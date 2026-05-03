# B019 — Bounds-check / repeat-loop counter hardcoded `i64_type()` (latent for non-64-bit `usize`)

## Original report

Surfaced by the soundness audit on 2026-05-03.

## The bug

Two codegen sites assume `usize == i64` directly rather than going
through `lower_prim(Usize)`:

### `emit_bounds_check` (`src/codegen/lower.rs:273-278`)

```rust
fn emit_bounds_check(&self, fx: &FnCodegenContext<'ctx>, idx: IntValue<'ctx>, n: u64) {
    let i64_ty = self.ctx.i64_type();
    let n_v = i64_ty.const_int(n, false);
    let cmp = self
        .builder
        .build_int_compare(IntPredicate::UGE, idx, n_v, "bounds.cmp")
        ...
}
```

`idx` is the index expression's value, typed `usize` per spec/09. On
a future 32-bit-`usize` target, `idx` would be i32 while `n_v` is
i64 — type mismatch in `build_int_compare`, LLVM verifier reject.

### `emit_repeat_loop` (`src/codegen/lower.rs:378-381`)

```rust
let i64_ty = self.ctx.i64_type();
let zero = i64_ty.const_zero();
let one = i64_ty.const_int(1, false);
let n_v = i64_ty.const_int(n, false);
```

Same assumption — the iteration counter `i` is fixed at i64.

## Failing case

Not currently reachable on any supported target (v0 fixes
`usize == i64`). This is a latent foot-gun for the documented
"future target awareness" comment at `src/codegen/ty.rs:91-94`. The
day someone adds 32-bit-target support and flips the single arm in
`lower_prim`, both sites silently emit wrong-width IR.

## Severity

**Low** — latent only. Dormant on supported targets, but a
self-described tripwire.

## Fix sketch

Replace `self.ctx.i64_type()` with `lower_prim(self.ctx, PrimTy::Usize)`
at both sites. Single source of truth.

Even simpler at `emit_bounds_check`: derive from the operand —
`let usize_ty = idx.get_type();`. Drops the `lower_prim` dependency
too.

## Related

- spec/09_ARRAY.md §"New primitives" describes the `usize == i64`
  v0 assumption.
- spec/06_LLVM_CODEGEN.md should note `lower_prim(Usize)` as the
  canonical width source.

## Out of scope

- Adding 32-bit-target support. Out of v0.
