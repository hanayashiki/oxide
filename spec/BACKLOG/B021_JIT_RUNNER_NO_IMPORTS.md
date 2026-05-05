# B021 — JIT runner ignores `import` items, blocking runtime tests for stdlib functions

## Original report

Surfaced 2026-05-05 while landing `ox_ptr_eq` in `stdlib/mem.ox`
(B009 / B011 / B012 closure work). Codegen and typeck snapshots
exist for `ox_ptr_eq`; runtime verification is missing because of
this gap.

## The gap

`tests/common/mod.rs::jit_run_with_ir` (the helper that powers
`tests/jit_snapshot.rs`) calls `Builder::from_inline(...)`, which
explicitly ignores `import` items per its docstring at
`src/builder/driver.rs:208`:

```rust
/// Single-file Builder. Suitable for `--emit lex|ast` and any unit
/// inspection that doesn't need the loader. `import` items in
/// `src` are ignored.
pub fn from_inline(...) { ... }
```

Consequence: a JIT fixture that `import "mem.ox";` (or any other
bundled stdlib file) HIR-errors with `unresolved name`, and the
`expect("compile clean")` in `jit_run_with_ir` panics with `Hir`.

This blocks runtime fixtures for any function that lives in the
bundled stdlib — `ox_ptr_eq`, `ox_alloc`, `ox_dealloc`,
`ox_realloc`, `ox_alloc_zeroed`, plus anything future stdlib work
adds.

## Concrete missing tests

Three obvious runtime checks for `ox_ptr_eq` that should land:

```rust
import "mem.ox";

fn main() -> i32 {
    let p: *const u8 = null;
    if ox_ptr_eq(p, null) { 1 } else { 0 }      // expected 1
}

fn main() -> i32 {
    let mut x: i32 = 0;
    let mut y: i32 = 0;
    let p: *const i32 = &x;
    let q: *const i32 = &y;
    if ox_ptr_eq(p, q) { 1 } else { 0 }         // expected 0
}

fn main() -> i32 {
    let mut x: i32 = 0;
    let p: *const i32 = &x;
    let q: *const i32 = &x;
    if ox_ptr_eq(p, q) { 1 } else { 0 }         // expected 1
}
```

Today these have to be skipped. Codegen + typeck snapshots cover
*lowering correctness*; this gap is specifically about *runtime
correctness* of the lowered IR.

## Severity

**Low** — codegen snapshots already verify the IR shape (two
`ptrtoint` + `icmp eq i64`), and an end-to-end stack bug would
likely surface in some other JIT test that doesn't need imports.
The cost is "we don't directly check that null/null returns true,
distinct/distinct returns false, aliased returns true."

## Fix sketch

Add a sibling helper `jit_run_vfs` in `tests/common/mod.rs` that
mirrors `render_codegen`'s shape — uses `vfs_for_fixture` to mount
the fixture and then `Builder::from_root` (instead of
`from_inline`):

```rust
pub unsafe fn jit_run_vfs<R: Copy + 'static>(
    file_name: &str, src: &str, entry: &str
) -> (String, R) {
    let (host, root) = vfs_for_fixture(file_name, src);
    let sess = Session::for_test(&host);
    let mut tapper = NoopTapper;
    let ctx = Context::create();
    let module = {
        let mut b = Builder::from_root(sess, root, &mut tapper);
        b.codegen(&ctx, "jit").expect("codegen failed (compile clean expected)")
    };
    // ... rest identical to jit_run_with_ir from line 524 down
}
```

Then update `tests/jit_snapshot.rs::jit_i32_snapshot` to dispatch
through `jit_run_vfs`. The bundled stdlib auto-mounts via
`VfsHost::new` (per `src/loader/host.rs:101`), so `import` resolves
without any extra plumbing.

Alternative shape: keep `jit_run_with_ir` for inline-single-file
tests, route imports through the new helper. The two shapes are
symmetric with `render_codegen` (vfs) vs the rest of `from_inline`
callers, so both belong.

After the helper lands, write the three `ox_ptr_eq_*.ox` fixtures
above and delete this backlog item.

## Related

- B009 / B011 / B012 (closed in the same PR that surfaced this) —
  the user-facing soundness fixes. This is the runtime-test gap
  that closure left open.
- `tests/common/mod.rs::render_codegen` — the existing pattern this
  fix should mirror.
