# B020 — Integer literal silent truncation when out of range

## Original report

Surfaced by the soundness audit on 2026-05-03.

## The bug

`emit_int_lit` (`src/codegen/lower.rs:664-670`) calls inkwell's
`const_int(n, false)` which silently truncates `n` to the target
IntType's width:

```rust
fn emit_int_lit(&self, eid: HExprId, n: u64) -> Operand<'ctx> {
    let ty = self.ty_of(eid);
    match self.typeck_results.tys().kind(ty) {
        TyKind::Prim(p) => Operand::Value(lower_prim(self.ctx, *p).const_int(n, false).into()),
        other => panic!("int lit had non-prim type {:?}", other),
    }
}
```

There is no range check at typeck or codegen. `IntLit(1000)` typed
as `i8` becomes `i8 -24` (the low 8 bits of 1000) with no diagnostic.

## Failing cases (silent truncation)

```rust
fn weird() -> i8 { 1000 }       // emits `ret i8 -24`
fn main() -> i32 {
    let x: u8 = 256;             // emits `i8 0`
    x as i32                     // 0, not 256 (and the cast itself isn't validated; see B009)
}
```

## Severity

**Low** — silent miscompilation, but the program's semantics depend
on a literal that's obviously out of range. Real-world impact is
low; user-experience impact is medium (silent wrong answer).

## Fix sketch

Range-check `IntLit(n)` against the inferred prim type during typeck.
At the literal's typing site (the IntLit arm in `infer_expr`), once
the prim type is known:

```rust
let in_range = match prim_ty {
    PrimTy::I8  => n <= i8::MAX as u64,           // for unsigned magnitude only
    PrimTy::U8  => n <= u8::MAX as u64,
    PrimTy::I16 => n <= i16::MAX as u64,
    /* ... and the i64 max for I64/Isize, u64 always fits ... */
};
if !in_range {
    inf.errors.push(TypeError::IntLiteralOutOfRange { lit: n, ty, span });
}
```

Negative literals are currently parsed as `UnOp::Neg(IntLit(n))`, so
the literal arm sees the unsigned magnitude. The range check needs
to know about a surrounding `Neg` to validate `i8 = -128` correctly.
Two options: parse negative literals as a single signed `IntLit`, or
fold `Neg(IntLit(n))` separately and bound-check both magnitudes
(positive and negative).

## Related

- spec/12_AS.md (cast handling — explicit casts can also lose data,
  but those are user-requested; literals are user-implicit).
- B015 (array length not range-checked) is the same family at the
  type level.

## Out of scope

- Const-evaluation of arbitrary expressions. Range check applies to
  literal sources only.
