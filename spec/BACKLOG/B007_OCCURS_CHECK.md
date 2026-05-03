# B007 — Missing occurs check in `bind_infer_checked` (compiler crash)

## Status
Open. Surfaced by the soundness audit on 2026-05-03.

## The bug

`bind_infer_checked` (`src/typeck/check.rs:840-874`) binds
`id := target` without testing whether `target` mentions `Infer(id)`.
Once a cyclic type is constructed, downstream walks through the
union-find chain (`resolve_fully`, `discharge_ptr_inner_eq`,
diagnostic rendering) recurse infinitely.

## Failing case (verified — stack overflow)

```rust
fn main() -> i32 {
    let mut p = null;     // p: *mut α  (fresh α)
    p = &mut p;            // RHS type: *mut *mut α
                           //   coerce(*mut *mut α, *mut α) →
                           //     eager unify recurses Ptr-Ptr →
                           //     unify(*mut α, α) →
                           //     bind_infer_checked(α, *mut α)
                           //   → α := *mut α (cycle)
    0
}
```

```text
$ cargo run -- -f cycle.ox
thread 'main' has overflowed its stack
fatal runtime error: stack overflow, aborting
```

## Severity

Compiler crash on a clean source — not strictly type-system unsoundness
(the program never executes), but a denial-of-service shape that
should produce a graceful diagnostic. The example is plausible enough
for a learner to type by accident.

## Fix sketch

Add an occurs check inside `bind_infer_checked`:

```rust
fn occurs_in(&self, inf: &Inferer, id: InferId, ty: TyId) -> bool {
    let resolved = self.resolve(inf, ty);
    match self.tys.kind(resolved).clone() {
        TyKind::Infer(other) => other == id,
        TyKind::Ptr(pointee, _) => self.occurs_in(inf, id, pointee),
        TyKind::Array(elem, _) => self.occurs_in(inf, id, elem),
        TyKind::Fn(params, ret) => {
            params.iter().any(|p| self.occurs_in(inf, id, *p))
                || self.occurs_in(inf, id, ret)
        }
        // ADTs and primitives don't carry Infer (ADTs are nominal;
        // primitives are leaves).
        _ => false,
    }
}
```

In `bind_infer_checked`, before binding:

```rust
if self.occurs_in(inf, id, target) {
    inf.errors.push(TypeError::CannotInfer { span });
    inf.bindings[id] = Some(self.tys.error);
    return;
}
```

Or introduce a dedicated `TypeError::CyclicType { span }` variant for
the better diagnostic.

## Adjacent

- The cycle also breaks `discharge_ptr_inner_eq` (recursion at
  `src/typeck/check.rs:425`). Once the bind path is guarded, that
  recursion is safe by construction (no cycles can form). No separate
  fix needed there — it's a downstream consequence.
- `resolve_fully` would similarly loop. Same story: guarding the
  binding closes the source.
- Worth a stress test: deep but acyclic Ptr-Infer chains
  (`α := *mut β; β := *mut γ; ...`) should keep working — only
  self-cycles are rejected.

## Out of scope

- General termination analysis for the inference engine. The simple
  occurs check is sufficient for the type kinds we have today.
