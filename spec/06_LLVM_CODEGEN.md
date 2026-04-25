# LLVM Codegen

## Requirements

Goal: lower a typechecked `HirModule` + `TypeckResults` to LLVM IR
using [inkwell](https://github.com/TheDan64/inkwell). v0 covers
primitives, function definitions, calls, arithmetic / bitwise /
comparison / logical / shift operators, `let` bindings, assignment,
`if`/`else`, blocks-as-expressions, and `return`. Enough to JIT or
compile the acceptance program through `clang`/`lld`.

Acceptance:

```
fn add(a: i32, b: i32) -> i32 { a + b }
```

emits the LLVM module:

```llvm
define i32 @add(i32 %a, i32 %b) {
entry:
  %a.slot = alloca i32
  %b.slot = alloca i32
  store i32 %a, ptr %a.slot
  store i32 %b, ptr %b.slot
  %a.load = load i32, ptr %a.slot
  %b.load = load i32, ptr %b.slot
  %sum = add i32 %a.load, %b.load
  ret i32 %sum
}
```

(`mem2reg` collapses the slots into SSA later; we always emit the
canonical alloca-form first.)

## Position in the pipeline

Source ─▶ tokens ─▶ AST ─▶ HIR ─▶ typeck ─▶ **codegen (LLVM IR)** ─▶
object/exe.

Codegen is the first pass that produces LLVM artifacts. It consumes
the typed side-tables (`TypeckResults::expr_tys` / `local_tys` /
`fn_sigs`) — every expression's type is already resolved, so codegen
never runs inference.

## inkwell binding & LLVM version

```toml
inkwell = { version = "0.5", features = ["llvm18-0"] }
```

inkwell needs a system LLVM. On macOS:

```sh
brew install llvm@18
export LLVM_SYS_180_PREFIX=$(brew --prefix llvm@18)
```

The feature flag (`llvm18-0`) and `LLVM_SYS_*_PREFIX` env var must
agree. Pick a different LLVM only if the user already has one
installed; v0 targets LLVM 18 because it's the current Homebrew
stable.

## Module layout

```
src/codegen/
  mod.rs    — pub fn codegen; re-exports
  ty.rs     — TyId → BasicTypeEnum / FunctionType lowering
  lower.rs  — Codegen struct, fn/expr/block emission
```

## Public API

```rust
// src/codegen/mod.rs
use inkwell::context::Context;
use inkwell::module::Module;

pub fn codegen<'ctx>(
    ctx: &'ctx Context,
    hir: &HirModule,
    typeck_results: &TypeckResults,
    module_name: &str,
) -> Module<'ctx>;
```

The caller owns the `Context` (inkwell's lifetime root). The returned
`Module` is verified via `module.verify()` before return; verifier
failures panic — they indicate a typeck/codegen bug, not user error.

The single-function entry point is enough for tests, the example
binary, and a future driver. JIT execution and object emission are
out of scope for v0; tests inspect the IR string via
`module.print_to_string()`.

## Type lowering (`src/codegen/ty.rs`)

`lower_ty(tcx: &TyArena, ty: TyId) -> BasicTypeEnum<'ctx>`:

| `TyKind` | LLVM |
|---|---|
| `Prim(I8 / U8)` | `i8` |
| `Prim(I16 / U16)` | `i16` |
| `Prim(I32 / U32)` | `i32` |
| `Prim(I64 / U64)` | `i64` |
| `Prim(Bool)` | `i1` |
| `Unit` | not a value — see below |
| `Never` | not a value — see below |
| `Fn(_, _)` | `FunctionType` (separate path; not a `BasicTypeEnum`) |
| `Ptr(inner)` | `ptr` (opaque pointer; LLVM 15+) |
| `Infer / Error` | unreachable post-typeck — ICE |

Signedness lives on the *operations*, not the LLVM type — `i32` and
`u32` both lower to `i32`, but `Div` on a signed type emits `sdiv`
while on unsigned emits `udiv`. Codegen reads the operand's `TyId`
through `typeck_results.type_of_expr(eid)` to pick the right opcode.

`Unit` and `Never` are not first-class LLVM values. In return
position, `Unit` lowers to `void` and `Never` (which only appears
through `Return` / divergent expressions) is also represented by
`void`-returning functions or by the absence of a value. As an
expression value, `Unit` and `Never` simply don't materialize — the
generator returns `None` from `emit_expr` for them.

`fn_type(tcx, sig: &FnSig)`:

- Each `param` lowered via `lower_ty`.
- Return: if `sig.ret == tcx.unit` or `tcx.never`, build a `void`
  function type; else lower normally.

## Memory model: alloca + load/store for every local

Every function parameter and `let`-binding gets a dedicated
`alloca` slot in the entry block. Reads emit `load`; writes emit
`store`; `Assign` is just a store. This is the standard LLVM
front-end pattern — ugly until `mem2reg` runs, but it makes
mutability and address-of trivial and frees us from threading SSA
phis through expressions.

```text
entry:
  %a.slot = alloca i32          ; for param a
  store i32 %a, ptr %a.slot
  %x.slot = alloca i32          ; for `let x = ...`
  ...
```

Slots live in a `HashMap<LocalId, PointerValue<'ctx>>` on the
`Codegen` struct, populated:

- At fn entry for params (after the entry block is positioned).
- In `emit_let` for new bindings.

## Codegen state

```rust
struct Codegen<'a, 'ctx> {
    ctx: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    hir: &'a HirModule,
    typeck_results: &'a TypeckResults,

    /// Forward-declared once; bodies filled in pass 2.
    fn_decls: IndexVec<FnId, FunctionValue<'ctx>>,

    // Per-fn — reset on entering a new fn body.
    cur_fn: Option<FunctionValue<'ctx>>,
    locals: HashMap<LocalId, PointerValue<'ctx>>,
}
```

Two passes at module level, mirroring typeck:

1. **Declare**: walk `hir.fns`, build a `FunctionType` from each
   `fn_sig`, and add an empty `FunctionValue` to the module. Forward
   calls and recursion just work because all decls land before any
   body is emitted.
2. **Define**: for each fn, position the builder at a fresh `entry`
   block, alloca its params + store the incoming values, then
   `emit_block(body)` and emit the trailing `ret` instruction.

## Per-expression rules

`emit_expr(eid) -> Option<BasicValueEnum<'ctx>>` returns the produced
SSA value. `None` means "no value" — used for `Unit`, `Never`, and
any expression whose containing block has already been terminated
(see termination tracking below).

| `HirExprKind` | LLVM lowering |
|---|---|
| `IntLit(n)` | `iN_type().const_int(n, false)` for the resolved type |
| `BoolLit(b)` | `i1` const 0/1 |
| `CharLit(c)` | `i8` const |
| `StrLit(_)` | unreachable — typeck rejected with E0254 |
| `Local(lid)` | `load <ty>, ptr <slot>` |
| `Fn(fid)` | the `FunctionValue` as a callable; emitted only as a Call's callee |
| `Unresolved(_)` | unreachable — typeck rejected |
| `Unary { Neg, e }` | `0 - e` (signed types) / `0 - e` (unsigned wraps) |
| `Unary { Not, e }` | `xor i1 e, true` |
| `Unary { BitNot, e }` | `xor iN e, -1` |
| `Binary { Add, l, r }` | `add` |
| `Binary { Sub, l, r }` | `sub` |
| `Binary { Mul, l, r }` | `mul` |
| `Binary { Div, l, r }` | `sdiv` if signed, `udiv` if unsigned |
| `Binary { Rem, l, r }` | `srem` / `urem` |
| `Binary { BitAnd/Or/Xor, l, r }` | `and` / `or` / `xor` |
| `Binary { Shl, l, r }` | `shl` (rhs truncated/extended to lhs's width) |
| `Binary { Shr, l, r }` | `ashr` if signed, `lshr` if unsigned |
| `Binary { Eq/Ne, l, r }` | `icmp eq` / `icmp ne` |
| `Binary { Lt/Le/Gt/Ge, l, r }` | `icmp s{lt,le,gt,ge}` if signed, `u{...}` if unsigned |
| `Binary { And, l, r }` | short-circuit: cond br on l, evaluate r in own bb, phi i1 |
| `Binary { Or, l, r }` | symmetric short-circuit |
| `Assign { Eq, target, rhs }` | store rhs into target's slot; result = none (Unit) |
| `Assign { compound, target, rhs }` | load target, op, store back |
| `Call { callee, args }` | emit args, `build_call(callee_fn, args)` |
| `Index/Field` | unreachable — typeck rejected with E0255 |
| `Cast { expr, ty }` | int-to-int: `trunc` / `sext` / `zext` based on widths and source signedness; equal width → no-op |
| `If { cond, then, else? }` | see "Control flow" below |
| `Block(bid)` | recurse `emit_block(bid)` |
| `Return(val)` | emit val, build `ret`, mark current bb terminated |
| `Let { local, init }` | alloca slot in entry, emit init (if any), store; result = none |
| `Poison` | unreachable — typeck rejected |

Codegen reads `typeck_results.type_of_expr(eid)` for the result type
and `typeck_results.type_of_expr(operand)` for operand signedness on a per-op
basis. There is no inference here.

## Control flow

### Termination tracking

After emitting `ret` or `br`, the current basic block is "terminated"
— further `build_*` calls would produce invalid IR. Codegen consults
`builder.get_insert_block().unwrap().get_terminator().is_some()`
before each emit and short-circuits with `None` if true.

### `if`

```text
        cond_bb:    %cond = ...
                    cond br %cond, then_bb, else_bb
        then_bb:    <emit then-block>
                    [if produced value: store into result slot]
                    br merge_bb     ; only if not already terminated
        else_bb:    <emit else-block, or br merge_bb directly if no else>
                    ...
        merge_bb:   [if value-producing if: load from result slot]
```

`if` is value-producing iff its result type per typeck is not `Unit`
or `Never`. Value-producing case allocates a result slot at the entry
block on entry; each arm stores into it before branching to merge.

If both arms are divergent (typeck infers `Never`), the merge block
is omitted; the builder is left at a fresh "unreachable" block so
later code in the surrounding block can still emit valid IR (it'll
be dead, but well-formed).

### Short-circuit `&&` / `||`

```text
                eval lhs → %l
&&:             br_if %l, rhs_bb, end_bb
                rhs_bb: %r = eval rhs ; br end_bb
                end_bb: %res = phi [%l (false from lhs), %r (rhs taken)]
||:             br_if %l, end_bb, rhs_bb
                ...same shape, phi sources swapped...
```

Both produce `i1`. Codegen captures the `BasicBlock` that produced
the rhs value just before branching to `end_bb` (the rhs evaluation
itself may have introduced its own basic blocks via nested control
flow); the phi predecessor isn't necessarily `rhs_bb`.

### `return e`

Emit `e` (if any), build `ret`, mark current bb terminated, return
`None` from `emit_expr` (its type is `Never`).

## Errors

Codegen has no public error type. Typeck-clean input produces
typeck-clean output. Internal invariants (no `Infer` left, no
`Error`-typed expression that wasn't poisoned upstream, etc.) are
enforced by `panic!` on the unreachable arms above. The final
`module.verify()` catches IR-level mistakes and panics with the
verifier's own message — we want a loud failure, not a silent bad
module.

## Public API

```rust
// src/codegen/mod.rs
pub use lower::codegen;

// codegen returns a verified inkwell::module::Module; callers can
// print, JIT, or pass to `TargetMachine::write_to_file`.
```

## Worked example

`fn add(a: i32, b: i32) -> i32 { a + b }`:

1. Pass 1 declares `define i32 @add(i32, i32)` (empty body).
2. Pass 2 enters `add`:
   - Build `entry` block.
   - Alloca `%a.slot` (i32), store param 0 into it.
   - Alloca `%b.slot` (i32), store param 1 into it.
   - `emit_block(body)`:
     - Tail expr is `Binary(Add, Local(a), Local(b))`.
     - `Local(a)` → `load i32, ptr %a.slot` → `%a.load`.
     - `Local(b)` → `load i32, ptr %b.slot` → `%b.load`.
     - `Add` → `add i32 %a.load, %b.load` → `%sum`.
   - Tail value `%sum` is the body's value; emit `ret i32 %sum`.
3. `module.verify()` succeeds.

Output IR matches the acceptance block above (modulo names assigned
by inkwell's auto-numbering — tests assert structurally, not on
exact register names).

## Foreign functions (`extern "C"` blocks)

`extern "C" { fn name(...) -> ret; ... }` declares functions whose
definitions live outside Oxide source — typically a linked C runtime.
Each declaration is bodyless (HIR `body: None`, `is_extern: true`).

Pass 1 declares every foreign fn via `module.add_function(...)` —
identical to local fns. Pass 2 skips the foreign-fn loop entirely
(`if hir_fn.body.is_some()`), so the `FunctionValue` keeps an empty
basic-block list. LLVM prints that as a `declare` line:

```llvm
declare i32 @print_int(i32)
```

Calls against extern fns flow through the same `emit_call` path as
local-fn calls — the `Fn(fid)` callee resolves to `fn_decls[fid]`,
and the resulting `call` instruction emits a relocation entry that
the linker resolves against an external object file.

No special-casing for ABI in v0: every `FunctionValue` defaults to
LLVM's C calling convention, which is what `extern "C"` already
guarantees. Adding other ABIs (`"system"`, `"win64"`) would mean
calling `fnv.set_call_conventions(...)` after `add_function` — that
work waits until typeck distinguishes more than one ABI string.

## Out of scope (v0)

- Object code / bitcode emission. v0 stops at the in-memory `Module`;
  `module.print_to_string()` is the inspection surface. A later step
  will write an object file via inkwell's `TargetMachine` and shell
  out to the system-resolved `cc` (whatever `which cc` finds —
  clang on macOS, gcc on most Linux distros) to link it into an
  executable. We deliberately delegate to `cc` rather than calling
  `lld` directly so the host's normal C runtime + crt0 + libSystem /
  glibc paths get picked up for free.
- Optimization passes. `mem2reg` etc. would be one line, but skipping
  them keeps test IR readable.
- JIT execution. Trivial follow-up; not needed to validate codegen.
- Pointer types, address-of/deref, arrays, strings, structs — all
  rejected at typeck.
- Cast compatibility checks. Typeck is loose; codegen panics on a
  cast it can't lower (e.g. `i32 as bool` — bool is i1 but we don't
  emit `icmp ne, 0` automatically in v0). Acceptance tests only use
  int-to-int.
- Debug info, source-position metadata, attributes (`nounwind`,
  `noinline`, etc.).
- ABI considerations beyond what LLVM's default function-type
  lowering gives us — no `byval`, no SROA hints, no `align` overrides.
