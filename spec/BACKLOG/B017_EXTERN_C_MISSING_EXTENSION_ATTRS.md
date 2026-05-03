# B017 — `extern "C"` calls miss `zeroext`/`signext` parameter attributes (ABI bug)

## Original report

Surfaced by the soundness audit on 2026-05-03.

## The bug

Codegen never calls `FunctionValue::add_attribute` anywhere —
`grep -rn 'add_attribute\|signext\|zeroext\|byval\|sret' src/codegen/`
returns only comment mentions. Function declarations in
`src/codegen/lower.rs:54` and the body lowering go to LLVM with raw
integer types (i1, i8, i16) and no extension hints.

For the AArch64 AAPCS (Linux + macOS) and Microsoft x64 ABIs, narrow
integer parameters and bool must arrive in the register zero- or
sign-extended to the slot width per the ABI contract. Without
`zeroext`/`signext` attributes, LLVM is free to leave the upper bits
unspecified. C callees compiled by clang/gcc — which assume the
extension was done — read garbage from the upper bits.

x86_64 SysV (Linux+macOS) is mostly forgiving for `int`-sized values,
but `_Bool` requires bits 1-7 to be zero per psABI § 3.2.3. Without
`zeroext` on a bool param, LLVM may emit `mov al, $cond`, leaving
bits 1-7 of `eax` undefined; a C caller that does `if (b)` reads the
full register and may take the wrong branch.

## Failing cases

```rust
extern "C" {
    fn c_takes_bool(b: bool) -> i32;       // declared `declare i32 @c_takes_bool(i1)` — no zeroext
    fn c_takes_i8(x: i8) -> i32;            // no signext
    fn c_takes_u8(x: u8) -> i32;            // no zeroext
}
fn main() -> i32 { c_takes_bool(true) }
```

LLVM IR emitted (paraphrased):

```llvm
declare i32 @c_takes_bool(i1)               ; should be: declare i32 @c_takes_bool(i1 zeroext)
declare i32 @c_takes_i8(i8)                 ; should be: declare i32 @c_takes_i8(i8 signext)
declare i32 @c_takes_u8(i8)                 ; should be: declare i32 @c_takes_u8(i8 zeroext)
```

clang's lowering of the equivalent C declarations always includes
the attribute. Without it, calling these functions from `extern "C"`
Oxide code may pass garbage in the high bits of the register on
AAPCS aarch64 and Win64.

## Severity

**Medium** — silent miscompilation when calling C from Oxide on
aarch64 (Linux/macOS) and Windows. x86_64 SysV is largely forgiving
except for `_Bool`. Real bug, but only visible at the C boundary.

## Fix sketch

In the fn declaration pass (`src/codegen/lower.rs:54`), walk
parameters and the return:

```rust
for (idx, &p) in sig.params.iter().enumerate() {
    if let Some(prim) = as_prim(tcx, p) {
        match prim {
            PrimTy::Bool | PrimTy::U8 | PrimTy::U16 => {
                fn_value.add_attribute(AttributeLoc::Param(idx as u32), zeroext_attr);
            }
            PrimTy::I8 | PrimTy::I16 => {
                fn_value.add_attribute(AttributeLoc::Param(idx as u32), signext_attr);
            }
            _ => {}
        }
    }
}
```

Same shape for the return slot at `AttributeLoc::Return`. Apply
unconditionally — it's the C ABI for `extern "C"` and the default
ccc on internal fns. clang adds them on every fn; we should match.

## Related

- spec/06_LLVM_CODEGEN.md (no current mention of ABI attributes;
  should add a brief note).
- spec/15_VARIADIC.md §"C ABI promotion" — variadic args have their
  own promotion rules (i8/i16 → i32, f32 → f64) that the frontend
  must do, separate from this attribute issue.

## Out of scope

- The variadic-arg promotion rules above (separate gap; file as its
  own backlog if/when it surfaces).
- Struct-by-value calling convention (known issue, out of audit
  scope).
