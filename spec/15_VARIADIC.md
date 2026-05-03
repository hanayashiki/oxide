# C Variadics (calling side)

## Requirements

Oxide can declare and call `extern "C"` functions today, but the parser
rejects the trailing `...` token, so a faithful `printf` declaration is
a syntax error. Every C-interop path that wants formatted output, file
descriptors with `O_CREAT` mode, `execl`, or any of the dozens of
variadic libc surfaces is currently unreachable without a hand-written
fixed-arity wrapper.

This spec adds **calling-side** support: declaring and *calling* C
variadic functions. **Defining-side** variadics (writing
`fn my_log(...) { va_arg!(args) }` in pure Oxide) stay rejected — see
"Out of scope" below for the rationale, which mirrors Rust's own
8-year-and-counting nightly status for `c_variadic`.

After this lands, the canonical `printf` example from spec/14_MODULES.md
becomes legal end-to-end:

```rust
import "stdio.ox";

fn main() -> i32 {
    printf("hello %d\n", 42);
    0
}
```

## Acceptance

```rust
extern "C" {
    fn printf(fmt: *const [u8], ...) -> i32;
    fn open(path: *const [u8], flags: i32, ...) -> i32;
}

fn main() -> i32 {
    printf("x = %d\n", 42);                 // 1 fixed + 1 variadic
    printf("a=%d b=%d c=%d\n", 1, 2, 3);    // 1 fixed + 3 variadic
    printf("hi\n");                          // 1 fixed + 0 variadic — legal
    open("/tmp/f", 0o102, 0o644);            // 2 fixed + 1 variadic
    0
}
```

Rejected — defining-side variadics:

```rust
fn my_log(level: i32, ...) {}
//                    ^^^ E0271 — `...` only allowed in `extern "C"` declarations
```

Rejected — variadic arg of a non-promotable type:

```rust
struct Point { x: i32, y: i32 }

extern "C" { fn printf(fmt: *const [u8], ...) -> i32; }

fn bad() {
    let p = Point { x: 1, y: 2 };
    printf("%d\n", p);
//                 ^ E0272 — struct value cannot be passed to a variadic parameter
}
```

Rejected — too few arguments (fewer than fixed-param count):

```rust
extern "C" { fn printf(fmt: *const [u8], ...) -> i32; }

fn bad() {
    printf();
//  ^^^^^^^^ E0253 — expected at least 1 argument, found 0
}
```

## Design Overview

- **One new token, one HIR flag, one type-system flag.** A `DotDotDot`
  token at the lexer; an `is_variadic: bool` on `HirFn`; a
  `c_variadic: bool` on `FnSig` and a third tuple element on
  `TyKind::Fn`. That's the entire surface of the change.
- **Calls are the only thing that becomes more permissive.** Today
  `args.len() == params.len()` is required at every call site. After
  this spec, variadic callees relax that to `args.len() >= params.len()`,
  with trailing args type-checked **standalone** (no expected type) and
  required to be a *promotable* type.
- **Codegen flips one bool, adds one promotion pass.** The trailing
  `false` in `lower_fn_type` becomes `sig.c_variadic`; `emit_call`
  splits its arg loop into fixed-then-variadic phases and runs an
  integer-extend on each variadic arg whose type is narrower than `i32`.
  No backend-internal machinery (register save area, `%al`, `va_start`)
  is touched — that's all LLVM's job once `isVarArg=true`.
- **No `va_list`, no `va_arg!`, no defining-side anything.** The `...`
  token only appears in `extern "C"` blocks, period. The user can
  *call* `printf` but cannot *write* a variadic fn themselves.

## Subset-of-Rust constraint

The accepted spelling

```rust
extern "C" { fn printf(fmt: *const [u8], ...) -> i32; }
```

parses and means the same in Rust on stable — no nightly feature flag
required. **Calling** C variadics has been stable in Rust since 1.0.

The rejection of `fn f(...) { ... }` is also Rust-compatible: that
spelling is gated behind `#![feature(c_variadic)]` (issue
[#44930](https://github.com/rust-lang/rust/issues/44930)) and is
nightly-only. We follow Rust's stable-only posture exactly.

## Position in the pipeline

```
Source ─▶ tokens ─▶ AST ─▶ HIR ─▶ typeck ─▶ codegen
            ╰── DotDotDot ──╯ │       │         │
                              │       │         ╰── isVarArg=true,
                              │       │             arg promotion (sext/zext)
                              │       ╰── arity rule, promotability check
                              ╰── HirFn.is_variadic
```

## Lexer (`src/lexer/`)

A new token `DotDotDot` (`...`). Two reasons over piggybacking on the
existing `Dot` / `DotDot` pair:

- The 3-char operator dispatch in `src/lexer/scan.rs:346-358` runs
  *before* the 2-char arm at `:362-389`, so longest-match falls out
  for free as long as the token exists.
- Reusing `DotDot` plus an explicit `Dot` would force the parser to
  re-tokenize on the fly — the lexer's longest-match invariant should
  carry the burden, not the parser.

Concrete edits:

- `src/lexer/token.rs:63-64` — add `DotDotDot` after `DotDot`.
- `src/lexer/scan.rs:349-351` — add a 3-char arm matching
  `('.', '.', '.')` → `DotDotDot`.

`..` (range, when added) and `..=` continue to lex as `DotDot` and a
hypothetical `DotDotEq`; `...` is its own token. No grammatical
ambiguity — Oxide has no `..=` today and ranges are out of v0.

## Parser (`src/parser/`)

### AST

`src/parser/ast.rs:73-79` — `FnDecl` gains:

```rust
pub struct FnDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    pub is_variadic: bool,        // NEW
    pub ret_ty: Option<Ty>,
    pub body: Option<Block>,
    pub span: Span,
}
```

`Param` (line 90-95) is **unchanged**. The `...` token does not
allocate a `Param`; it's a property of the signature, not a parameter.
This mirrors Rust's calling-side spelling (bare `...`, no name) and
keeps later layers from having to special-case a synthetic param.

### Grammar

```
FnDecl     ::= 'fn' Ident '(' ParamList? ')' RetTy? FnTail
ParamList  ::= Param (',' Param)* (',' '...')?           # `...` only after at least one param, with comma
Param      ::= ('mut')? Ident ':' Type
FnTail     ::= Block | ';'
```

Forbidden shapes, all rejected by the parser:

| Shape | Reject reason |
|---|---|
| `fn f(...)` | `...` requires at least one fixed param. C's "old-style" decl trap. |
| `fn f(a, ..., b)` | `...` must be the last entry. |
| `fn f(a, ...,)` | No trailing comma after `...`. |
| `fn f(... )` (no comma) | Comma before `...` is mandatory; mirrors Rust's stable form. |

These are hard parse errors with a span pointing at the `...` token, on
the same diagnostic surface as other parser errors (no new error code
infrastructure — the existing parse-error pathway suffices).

### Body-vs-`...` validation

Variadic *definitions* (`...` on a fn that has a body) are a parse-time
error. The parser already distinguishes "extern fn decl" (body must be
absent) from "regular fn" (body must be present); add one more
condition:

> If `is_variadic` is set, require `is_extern_decl`. Otherwise emit
> **E0271 — `...` only allowed in `extern "C"` declarations**, with the
> primary span on the `...` token and a help line: *"variadic Oxide
> functions are not supported; use `extern "C"` to call a C variadic."*

Concrete edits:

- `src/parser/parse/syntax.rs:737-747` — `params_parser` recognises a
  trailing `, ...` and returns `(Vec<Param>, is_variadic: bool)`.
- `src/parser/parse/syntax.rs:761-781` — `fn_decl_parser` threads the
  flag into `FnDecl::is_variadic`.
- `src/parser/parse/syntax.rs:806-847` — `extern_block_parser` is
  unchanged structurally; it just passes through whatever
  `fn_decl_parser` produced. The body-vs-`...` validation lives in
  `fn_decl_parser` itself, since that's where both `is_variadic` and
  body-presence are known.

## HIR (`src/hir/`)

### `HirFn`

`src/hir/ir.rs:63-78` — add one field next to `is_extern`:

```rust
pub struct HirFn {
    pub name: Ident,
    pub params: Vec<LocalId>,
    pub ret_ty: HirTy,
    pub body: Option<HirBlockId>,
    pub is_extern: bool,
    pub is_variadic: bool,        // NEW
    pub span: Span,
}
```

### Lowering

`src/hir/lower.rs:152-173` — `register_fn_stub` takes the variadic
flag from `FnDecl::is_variadic` and stores it on the `HirFn`. The
flag flows alongside `is_extern` with no new logic.

`prescan_items` at `:124-148` does not need to change — it already
threads the `FnDecl` through to `register_fn_stub`.

The parser has already enforced `is_variadic ⇒ is_extern`, so the HIR
layer can rely on that invariant without re-checking. (Defensive
assertion on debug builds is fine but not required.)

## Typeck (`src/typeck/`)

### `TyKind::Fn` and `FnSig`

`src/typeck/ty.rs` — both gain a `c_variadic: bool` flag.

```rust
// src/typeck/ty.rs:31
TyKind::Fn(Vec<TyId>, TyId, bool /* c_variadic */)

// src/typeck/ty.rs:101-117
pub struct FnSig {
    pub params: Vec<TyId>,
    pub ret: TyId,
    pub partial: bool,
    pub c_variadic: bool,     // NEW
}
```

#### Why a tuple element on `TyKind::Fn`, not an `FnTy` struct

`TyKind::Fn` is matched in two or three places (`infer_call`, the
fn-callable rendering paths, debug printers). The pattern-match churn
is bounded — adding a third tuple element costs less than introducing
a new struct type and threading interning through it. v0 has no fn
pointers as values; the `Fn` shape is short-lived, mostly resolved at
call sites.

`FnSig` (the declaration-side companion) gets the same field name
(`c_variadic`) for symmetry. The two carry redundant information today
(every `Fn(...)` derives from a `FnSig`), but they're independent
structures and we keep them in sync the same way they're already kept
in sync for `params` and `ret`.

#### Why the field name is `c_variadic` and not `is_variadic`

`is_variadic` would be ambiguous — Oxide may someday grow Rust-style
generic variadic tuples, slice-spreading, or other "variadic" features
that have nothing to do with the C ABI's promotion rules. `c_variadic`
pins the meaning to "C-ABI default-argument-promoted variadic," which
is the only kind we'll ever support. Mirrors Rust's own
`#![feature(c_variadic)]` naming.

`HirFn.is_variadic` keeps the simpler name because at the HIR layer
there's no other variadic to disambiguate against. The rename happens
at the typeck boundary.

### `infer_call` (`src/typeck/check.rs:1431-1466`)

The arity check at `:1443-1450` becomes conditional on `c_variadic`:

```text
infer_call(callee, args):
    callee_ty := infer_expr(callee), then resolve
    arg_tys   := args.map(infer_expr)
    match callee_ty:
        Fn(param_tys, ret_ty, c_variadic):
            n_fixed = param_tys.len()
            if c_variadic:
                if args.len() < n_fixed:
                    emit WrongArgCount { expected: n_fixed, found: args.len(),
                                         at_least: true }
                    return ret_ty
            else:
                if args.len() != n_fixed:
                    emit WrongArgCount { expected: n_fixed, found: args.len(),
                                         at_least: false }
                    return ret_ty
            for (arg, pty) in args[..n_fixed].zip(param_tys):
                coerce(ty_of(arg), pty, span_of(arg))
            for arg in args[n_fixed..]:
                check_variadic_promotable(ty_of(arg), span_of(arg))
            ret_ty
        ...
```

#### `WrongArgCount` rendering (`src/typeck/error.rs:20-25`)

Add an `at_least: bool` field to the existing variant. Diagnostic
rendering reads:

- `at_least: false` → "expected N arguments, found M" (today's
  behavior).
- `at_least: true`  → "expected at least N arguments, found M".

No new error code — the diagnostic is still **E0253**. Renaming a
field on an existing variant is the smallest delta that captures the
new semantics.

#### `check_variadic_promotable`

A new typeck function (no obligation infrastructure — direct check at
the call site, since variadic args have no expected type to defer
against):

| Resolved arg type | Verdict | Note |
|---|---|---|
| `Prim(I8 / I16 / U8 / U16 / Bool)` | ✓ | promoted to i32 in codegen |
| `Prim(I32 / U32 / I64 / U64 / Isize / Usize)` | ✓ | unchanged |
| `Ptr(_, _)` (any pointee, any mutability) | ✓ | lowers to `ptr` |
| `Array(_, _)` | ✗ E0272 | unsized: nothing to pass; sized: matches C's "no array by value" |
| `Adt(_)` | ✗ E0272 | matches C: structs by value forbidden through `...` |
| `Unit` / `Never` / `Fn(_, _, _)` | ✗ E0272 | not values in any C ABI sense |
| `Infer(_)` | ✗ E0272 | unresolved at this point — fail loud rather than guess; same posture as `CannotInfer` |
| `Error` | (silently accept) | poison; upstream already errored |

Rejected with new error:

> **E0272** — `TypeError::VariadicArgUnsupported { found: TyId, span: Span }`
>
> Diagnostic: *"cannot pass `<found>` through a variadic parameter"*,
> with a help line listing accepted types: *"variadic args must be an
> integer, pointer, or `bool` — wrap structs/arrays in a `*const T` if
> you mean to pass by reference."*

#### Float types (forward-looking)

`PrimTy` has no `F32` / `F64` in v0. When floats land:

- `F32` → variadic-promoted to `F64` at codegen via `fpext`.
- `F64` → unchanged.

Both are accepted by `check_variadic_promotable`. No further design
work; the parallel to `i8 → i32` is exact.

### Length-erasure for the format-string parameter

The fixed `fmt: *const [u8]` parameter accepts a string literal
(`*const [u8; N]`) via the existing length-erasure coercion (see
spec/09 "Coercions"). **No new code path** — `printf("...", ...)`
type-checks the format string with the same machinery `puts(...)` uses
today.

### String literals in variadic position

`printf("%s\n", "name")` passes a literal in a variadic slot. There is
no expected type for variadic args, so no coercion fires; the literal
keeps its `*const [u8; N]` type. `check_variadic_promotable` accepts
it (it's a `Ptr(_, _)`), and codegen lowers all pointer types to LLVM
`ptr` regardless of pointee shape. **No special case** — the design
falls out from the existing rules.

(One ergonomic consequence: a literal is passed to `%s` as a bare
pointer to a NUL-terminated byte sequence, exactly matching C's
contract. The trailing NUL added by the StrLit codegen path — see
spec/07 "String literal emission" — preserves the contract.)

## Codegen (`src/codegen/`)

### `lower_fn_type` (`src/codegen/ty.rs:108-130`)

The trailing `false` in the two `fn_type` calls (lines 126 and 128)
becomes `sig.c_variadic`:

```rust
if is_void_ret(tcx, sig.ret) {
    ctx.void_type().fn_type(&params, sig.c_variadic)
} else {
    lower_ty(ctx, tcx, adt_ll, sig.ret).fn_type(&params, sig.c_variadic)
}
```

That single change makes `declare i32 @printf(ptr, ...)` come out
correctly instead of `declare i32 @printf(ptr)`.

### `emit_call` (`src/codegen/lower.rs:1293-1345`)

Today the arg loop (`:1309-1319`) treats every arg uniformly. It
becomes a two-phase loop:

```text
n_fixed = callee_sig.params.len()        // from typeck FnSig

for (i, arg) in args.enumerate():
    arg_ty   := typeck.type_of_expr(arg)
    arg_op   := emit_expr(arg)?
    arg_val  := match (i >= n_fixed, kind(arg_ty)):
        (true,  Prim(I8 | I16))            => sext(arg_op, i32)
        (true,  Prim(U8 | U16 | Bool))     => zext(arg_op, i32)
        // when floats land:
        // (true, Prim(F32))               => fpext(arg_op, f64)
        (true,  _)                         => load_value(arg_op, arg_ty)
        (false, _)                         => /* existing fixed-arg path:
                                                array byval or load_value */
    arg_vals.push(arg_val)

self.builder.build_call(fnv, &arg_vals, "call")
```

The `false` branch is exactly today's logic at `:1311-1318`
(sized-array byval slot, otherwise load_value); no behavioural change.
The `true` branch is the only new code path.

### Promotion helper

A new internal function `promote_for_variadic(self, op, ty) ->
BasicValueEnum`:

| Source type | Operation | inkwell call |
|---|---|---|
| `Prim(I8 / I16)` | sign-extend to i32 | `build_int_s_extend(v, i32_ty, "sext")` |
| `Prim(U8 / U16 / Bool)` | zero-extend to i32 | `build_int_z_extend(v, i32_ty, "zext")` |
| `Prim(I32 / U32 / I64 / U64 / Isize / Usize)` | unchanged | `load_value` only |
| `Ptr(_, _)` | unchanged | `load_value` only |
| anything else | unreachable | typeck E0272 |

The sext/zext helpers are the same ones `emit_cast` uses at
`src/codegen/lower.rs:1379` and `:1381` — no new inkwell wiring.

The choice of sign-extend for `I8`/`I16` and zero-extend for the
unsigned + bool family mirrors the C standard's default argument
promotion: signed-narrow types promote to `int` via sign extension;
unsigned-narrow and `_Bool` zero-extend.

### What inkwell handles for free

- The textual-IR shape `call i32 (ptr, ...) @printf(...)` (function
  type repeated at the call site) is emitted automatically by
  `build_call` whenever the callee's `FunctionType` has
  `isVarArg=true`. We don't construct the type-suffix manually.
- Caller-side `%al` (number of vector regs used), register-save-area
  spill, `va_start`/`va_end` lowering, the per-target argument
  classification — all done by the LLVM x86_64 backend once it sees
  `isVarArg=true` on the function type. Front-end never thinks about
  any of this.

### Arrays in variadic position

Sized arrays don't reach `promote_for_variadic` because typeck rejects
`Array(_, _)` at E0272. If a future relaxation allowed `*const [T; N]`
(already accepted as a `Ptr(_, _)`) to pass through, the existing
codegen path lowers it to `ptr` and the variadic promotion is a no-op.
No change needed.

### Worked IR

For:

```rust
extern "C" { fn printf(fmt: *const [u8], ...) -> i32; }

fn main() -> i32 {
    let c: u8 = 65;
    printf("c = %d\n", c);
    0
}
```

Emitted IR (allocas hoisted; matches the codegen patterns of
spec/07_POINTER.md "String literal emission"):

```llvm
@.str.0 = private unnamed_addr constant [8 x i8] c"c = %d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
allocas:
  %c.0.slot = alloca i8, align 1
  br label %body
body:
  store i8 65, ptr %c.0.slot, align 1
  %c.load = load i8, ptr %c.0.slot, align 1
  %c.prom = sext i8 %c.load to i32              ; variadic-arg promotion
  %call   = call i32 (ptr, ...) @printf(ptr @.str.0, i32 %c.prom)
  ret i32 0
}
```

For `printf("hi\n")` (no variadic args):

```llvm
@.str.1 = private unnamed_addr constant [4 x i8] c"hi\0A\00", align 1
%call = call i32 (ptr, ...) @printf(ptr @.str.1)
```

The `(ptr, ...)` type marker survives at the call site even with zero
trailing args, because the function's `isVarArg` flag is a property of
the *type*, not of any one call.

## Errors

| Code | Variant | Layer | Trigger |
|---|---|---|---|
| **E0271** | `TypeError::VariadicOnlyExtern` *(parse-time, see note)* | parser | `...` in a non-extern fn declaration |
| **E0272** | `TypeError::VariadicArgUnsupported { found, span }` | typeck | unpromotable type at a variadic-arg position |
| E0253 (existing) | `TypeError::WrongArgCount { expected, found, at_least, span }` | typeck | extended with `at_least` field; renders "at least N" when callee is variadic |

E0271 is reported via the parser's diagnostic channel; the existing
parse-error infrastructure carries it. We don't introduce a parser-side
error type — we use the existing parse-error pathway and reserve E0271
in `src/typeck/error.rs` as a comment for cross-spec discoverability.
(Compare to the `extern "C"` body-must-be-absent rule, which is also
parse-enforced today.)

E0272 is the only genuinely new typeck variant.

## Out of scope

- **Defining-side variadics** — writing `fn my_log(...)`, declaring
  `va_list`, calling a `va_arg!`-shaped intrinsic. This requires
  per-target ABI lowering: clang-style `TargetInfo.cpp` work that
  emits the platform's `va_list` struct layout, the register save
  area branch sequence, and the `gp_offset`/`fp_offset` cursor walks.
  On SysV x86_64 alone that's a few hundred lines of front-end code
  reproducing what clang already does. Rust's nightly `c_variadic`
  feature has been unstable since 2017 for the same cost/benefit
  reason. Oxide is a teaching language; we punt indefinitely.

- **Format-string type-checking.** `printf("%d", "hello")` compiles
  cleanly — no `-Wformat`-style analysis. The format string is opaque
  bytes to the compiler. Same UB story as C.

- **Float promotion.** `PrimTy` has no F32/F64 in v0. The promotion
  table reserves slots; codegen wiring lands when floats land.

- **`%al`, register save area, `va_start`/`va_end` lowering.** All
  backend-internal — LLVM emits them when it sees `isVarArg=true` on
  the function type. Our front-end is unaware. This is the load-bearing
  reason calling-side is cheap and defining-side is expensive.

- **Pointer-to-variadic-fn values.** `let p = printf;` (taking the
  address of a variadic fn into a local) — out, same as fn pointers in
  general are out of v0.

- **Multiple variadic ABIs.** Only `extern "C"` is supported; an
  `extern "system"` or `extern "win64"` distinction would mean wiring
  through inkwell's `set_call_conventions`. Defer until typeck
  recognizes more than one ABI string.

## What this unblocks

- The `import "stdio.ox";` example in spec/14_MODULES.md compiles
  end-to-end (currently the `printf` declaration in the bundled
  `stdio.ox` is itself a syntax error).
- libc's full surface — `printf`, `fprintf`, `sprintf`, `snprintf`,
  `scanf`, `open` (with `O_CREAT` mode), `execl`, `execlp`,
  `fcntl`, `ioctl`, `syslog`, `dprintf`, `fnmatch` family — becomes
  callable.
- A "hello world" tutorial that uses `printf` with a format specifier
  becomes the canonical first example, replacing the slightly-awkward
  `puts("hello world")` form from spec/07_POINTER.md.
