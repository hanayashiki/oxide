# B006 — Pointer mutability laundering at if-arm and ArrayLit boundaries (soundness hole)

## Status
Open. Surfaced by the soundness audit on 2026-05-03 after the StrLit
migration landed (commit `36a8cbf`). Pre-existing bug; not introduced
by the migration.

## The bug

`unify_with_ctx`'s Ptr-Ptr arm (`src/typeck/check.rs:737-739`) is loose
on outer mutability — `*mut T ~ *const T` accepted at unify, with the
directional `*mut → *const` subtype rule deferred to `coerce`'s
discharge. That assumption holds at every site that goes through
`coerce`. It does **not** hold at two sites that use plain `unify`
without queuing an obligation:

- **ArrayLit element unify** (`src/typeck/check.rs:1218`):
  ```rust
  self.unify_with(inf, ti, t0, elem_span, MismatchCtx::ArrayLitElement { i });
  ```
  The first element's type `t0` becomes the array's element type; later
  elements unify into it shape-blind. No coerce obligation queued.
- **`if`/`else` / `match` arm coalesce** (`src/typeck/check.rs:1602`,
  `unify_arms` body):
  ```rust
  self.unify(inf, a, b, span);
  ```
  Then `join_never` returns `then_ty` as the if-expr's type
  (`src/typeck/check.rs:1619`). Direction-blind.

Result: a `*mut` value and a `*const` value can be coalesced into the
more permissive of the two arm/element orderings, with no diagnostic.
The result type carries `*mut`, but the runtime byte-pattern is the
`*const` pointer at one of the inputs — yielding write access to
memory that was never writable.

## Failing case (verified — exit code 138 = SIGBUS on macOS)

```rust
extern "C" { fn puts(s: *const [u8]) -> i32; }

fn main() -> i32 {
    let mut buf: [u8; 3] = [104, 105, 0];      // "hi\0", writable
    let rw: *mut   [u8; 3] = &mut buf;
    let ro: *const [u8; 3] = "Hi";              // .rodata

    // First-element-wins: arr typed [*mut [u8; 3]; 2].
    // ro silently flows in as *mut.
    let arr = [rw, ro];

    let bad: *mut [u8; 3] = arr[1];             // typed *mut, points to .rodata
    (*bad)[0] = 88;                              // write to .rodata → SIGBUS
    puts(ro); 0
}
```

Same hole via if/else:

```rust
let p = if cond { rw } else { ro };             // typed *mut [u8; 3]
(*p)[0] = 88;                                    // .rodata write
```

## Why coerce isn't enough at these sites

Spec/05 lists "mid-block expression-statement, else-less `if` then-arm"
as `coerce` sites — both have a *fixed-direction* (the slot type is
known at the time of the unify). An if-with-else has **no fixed slot**
at the unify point; the result type is *derived* from the arms. The
current code picks the first arm's type, which silently determines
direction. Same for ArrayLit — the first element wins.

The right rule is one of:

1. **Compute a true LUB.** When arms / elements disagree on outer mut
   at any Ptr layer, drop to `Const` (the less permissive). Mirrors
   spec/09 §"Arm-coalesce sloppy subtyping (residual)" but for the
   mut tag rather than the length tag.
2. **Coerce both arms into a fresh Infer.** Each arm runs through
   `coerce(arm_ty, ?T)` so the existing direction enforcement fires
   per-arm. The Infer pins to the first non-Infer side; the discharge
   then validates the other arm's direction.

Option (1) is the cleaner model — a real LUB respects the sloppy-
subtyping precedent and gives the user a less-permissive but sound
type. Option (2) is more surgical but tangles the unify logic with
coerce in a new place. Recommend (1).

## Adjacent: spec/09's documented "sloppy subtyping" note

Spec/09 already documents a residual hole in arm coalescing for
length erasure (mixed `Some/None` Some-arm wins → result type claims
length). That note is **information loss**, not safety violation —
the subsequent indexing operation may run past the end at runtime
(C semantics). The mut-laundering bug above is **safety violation**
— it produces a typed write through what was a `*const` pointer.
The two should be addressed together; the gap-closing rule is the
same shape (LUB at coalesce sites), just applied to a different tag.

## Related code paths to audit

- Anywhere `unify` / `unify_with` is called outside of `coerce` — see
  the inventory in commit `36a8cbf`'s plan: `unify_arms`,
  ArrayLit elements, IndexNotUsize, return-from-coerce.
- `IndexNotUsize` (`src/typeck/check.rs:929`) unifies idx-ty against
  `usize`. Both sides are non-Ptr in any reachable program; not a
  source of mut laundering.
- `unify_with` direct call from `infer_array_lit` at line 1218 is the
  ArrayLit case above — already covered.

## Out of scope

- Dropping pointer-mutability from the type system entirely (Rust
  borrow-checker style). Out of v0.
- Adding `as` cast for pointers. Already deferred (spec/12).
