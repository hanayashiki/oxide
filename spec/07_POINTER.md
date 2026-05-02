# Requirements

Currently, type only works for primitive ones, no `*mut u8`, `*const u8`.

We need to be able to handle pointers to some extent, but first of all lets only consider dealing with pointer as an atomic value, and try to pass the `*const u8` to `puts`. Full pointer support needs memory layout support which is too big right now.

## Acceptance

```
extern "C" {
    fn puts(s: *const [u8]) -> i32;
}

extern "C" {
    fn multi_ptr(s: *const *const *const [u8]);
}

fn main() -> i32 {
    puts("hello world");

    0
}
```

Anatomy:

1. String literal `"hello world"` has type `*const [u8; 12]` —
   pointer to a sized byte array; `N = byte_len + 1` counts the
   trailing `\0` (matches C's `char[12]` for the same literal).
   Codegen emits a private `[12 x i8]` global in `.rodata`. Unlike
   Rust's UTF-8 `str`, Oxide bytes are bytes — no Unicode invariant.

2. **String literals are C-style null-terminated.** This is a
   deliberate divergence from Rust and an alignment with C: every
   string literal emits a trailing `\0` byte that is *counted* in
   the type's length (`char[12]` for `"hello world"`, mirroring C).
   The pointer handed to FFI is to the first byte; the consumer
   (e.g. `puts`, `printf`) walks the bytes until it sees `\0`. There
   is no separate length field at runtime, no `&str` fat pointer, no
   `CString` wrapper type. A bare `"..."` literal *is* the C string.

   Rationale: the only string consumers we care about today are C
   ABI functions, and they all expect NUL-terminated `char*`.
   Carrying a Rust-style length around would be dead weight that
   we'd just have to strip at every FFI boundary.

   Note that the FFI parameter is spelled `*const [u8]` — pointer
   to *unsized* byte sequence — not `*const u8`. See "`*const T` vs
   `*const [T]`" below for the distinction; the StrLit's
   `*const [u8; N]` reaches the parameter slot via the existing
   length-erasure coercion (`*const [T; N] → *const [T]`).

3. **Pointer types: loose unify + strict coercion.** Mutability is a
   permission, not a structural property, so we keep it out of unify
   and check it as a separate step at every use site (call argument,
   `let x: T = ...`, assignment RHS, return value).

   - **Unification (shape only):** `*α₁ T₁  ~  *α₂ T₂` succeeds iff
     `T₁ ~ T₂`. The mutability tags `α₁`, `α₂` are *ignored* at this
     stage. This keeps inference clean — a type variable that flows
     into a `*mut` site at one place and a `*const` site at another
     unifies without drama.

   - **Coercion check (at use sites, after unify):** when an actual
     pointer flows into an expected pointer slot, we run:

     ```
     coerce(*α_actual T_actual, *α_expected T_expected):
       T_actual is structurally identical to T_expected
         (recursive equality, including ALL inner mutability tags)
       AND  α_actual ≤ α_expected
         where mut ≤ const, mut ≤ mut, const ≤ const, const ≰ mut
     ```

   In plain English: **only the outermost mutability is droppable;
   every layer below it must match exactly.** So
   `*mut u8 → *const u8` ✓, but `*mut *mut u8 → *mut *const u8` ✗
   (inner mut → inner const is unsound — it lets you launder
   const-ness once we add deref; see the worked example in the
   discussion notes).

   `*const T → *mut T` is forbidden in *every* position — that would
   forge write access. Only `as` casts grant that, and `as` is out
   of scope for v0.

4. **StrLit type: `*const [u8; N]`.** The HIR payload still holds
   the source string (no NUL); codegen appends the `\0` and emits a
   `[N x i8]` private global with `N = byte_len + 1` (NUL counted).
   Typeck assigns the literal expression the type
   `*const [u8; N]` — pointer to a sized byte array.

   Two properties of this typing:

   - **Length is in the type.** Lengths shift around between
     literals (`"hi"` → `*const [u8; 3]`, `"bye"` → `*const [u8; 4]`),
     so two literals of different sizes don't unify directly.
     Workaround for arm-coalesce / `=` reassignment: bind through a
     `*const [u8]` (unsized) local first.
   - **Immutability is encoded structurally** by the outer
     `*const`. A bare `[u8; N]` place (the variant in earlier drafts
     of this spec) would have admitted `let mut s = "aa"; s[0] = b'b';`
     where the rebinding-mut + array-element-write would write
     through `.rodata`. The pointer wrapper makes that statically
     impossible without involving `as`. Rust's `b"..."` of type
     `&'static [u8; N]` makes the same call.

   `&"hello"` stays rejected (E0208) because StrLit remains a
   non-place expression. The canonical form is already a pointer;
   `&"hello"` would produce `*const *const [u8; N]`, which is
   rarely what anyone wants. If you do want a double-pointer, bind
   to a local first: `let s = "hi"; let p = &s;`.

   FFI compatibility: extern signatures should be spelled with
   `*const [u8]` (sequence pointer; see "`*const T` vs `*const [T]`"
   below). The literal's `*const [u8; N]` reaches the parameter
   slot via the existing length-erasure coercion
   (`*const [T; N] → *const [T]`; spec/09 "Coercions"). The pre-
   migration spelling `*const u8` no longer accepts a string literal
   — `*const T` strictly means "pointer to a single T".

   Incidental consequences of the type carrying the array layer:

   - `"hi"[0]` is now valid (returns `u8`). Index already unwraps
     `Array(u8, _)` after `auto_deref_ptr`; no new code path.
     Indexing through a string literal is read-only (the outer
     `*const` propagates through the auto-deref, so `s[0] = 1` errors
     as `MutateImmutable`).
   - In if/match arms with mismatched-length literals (e.g.
     `if c { "hi" } else { "bye" }`), the strict Some/Some length
     check fires and rejects with E0265. Workaround: bind each
     literal to a `*const [u8]` local first.

   See `09_ARRAY.md` "Arm-coalesce sloppy subtyping" for the
   one residual asymmetry around arm coalescing of mixed
   sized / unsized arms.

### `*const T` vs `*const [T]` semantics

`*const T` is a pointer to **a single `T`**. `*const [T]` is a
pointer to a **sequence** of `T` (length not statically known).
`*const [T; N]` is a pointer to a sequence of statically-known
length. C's `char *` semantically maps to Oxide `*const [u8]`,
not `*const u8` — `char *` is the address of a sequence, just
like `int *` is the address of a sequence in idiomatic C even when
the type doesn't say so.

Codegen lowers all three to opaque LLVM `ptr`, so the distinction
is typeck-only and free at runtime. The point of the distinction
is what the type system lets you do:

- Through a `*const u8`: deref to `u8` (read one byte). No
  indexing — there's no array layer to index into.
- Through a `*const [u8]`: index `p[i]` (returns `u8`). No
  bounds check at runtime — the length is not in the type.
- Through a `*const [u8; N]`: index `p[i]` (with a static
  bounds check at compile time when `i` is a const). Length-
  erasure coerces this to `*const [u8]` at use sites.

Pointer-to-sequence is the right type for *any* C function that
takes a buffer (`read`, `write`, `puts`, `perror`, `system`,
`memcpy`, …). Single-byte pointers (`*const u8` / `*mut u8`)
are for the rare case where you actually mean "address of one
byte" (e.g. atomic reads of a flag byte).

5. **Pointer access (`*ptr` rvalue / `*ptr = v` lvalue) and the
   `null` literal are now specified — see "Null literal" and
   "Deref operator (`*p`)" below.** Pointer **arithmetic**
   (`p + 1`, `p - q`, `*(p + 1)`) remains deferred — it lands later
   via methods (`ptr.add(n)`, `ptr.offset(n)`) once struct methods
   land. The C-style infix-`+`-on-pointer form is intentionally not
   the chosen syntax; see the deref section's "Out of scope".

## Codegen

LLVM (since opaque pointers landed in 15) doesn't track pointee types or
mutability on the pointer — there's exactly one `ptr` type. So the entire
mutability story stays inside typeck and never reaches codegen.

- **Pointer type lowering:** any `TyKind::Ptr { .. }` lowers to LLVM
  `ptr` (`ctx.ptr_type(AddressSpace::default())`), regardless of pointee
  or mutability. Multi-level pointers like `*const *const u8` flatten to
  the same `ptr` — depth lives only in the type-system.

- **String literal emission:** each `HirExprKind::StrLit(s)` becomes one
  module-level constant global:

  ```llvm
  @.str.N = private unnamed_addr constant [LEN+1 x i8] c"...\00", align 1
  ```

  - `LEN+1`: the source-level byte count plus one trailing `\0`. The
    terminator is added here at codegen, *not* stored in the HIR payload
    (point 4 above).
  - `private` linkage: module-local, never exported to the linker.
  - `unnamed_addr`: lets the linker merge identical literals across
    translation units (we don't intern at codegen time — `unnamed_addr`
    handles dedup at link time, which is good enough for v0).
  - `constant`: lands in `.rodata`.

  The expression evaluates to the global's `PointerValue` directly. No
  GEP `[N x i8], ptr, 0, 0` needed — under opaque pointers the global
  *is* a `ptr`. Counter for the `N` suffix lives on the codegen state
  as a `Cell<u32>` (the codegen struct is `&self` everywhere via
  inkwell's interior mutability — we follow that pattern).

- **What flows through unchanged:** since pointer values are just
  `BasicValueEnum::PointerValue` (wrapped as `Operand::Value` by
  `emit_expr`), the existing call-arg, let-init, and local-load paths
  all work without touching them. The places that *would* break
  (`emit_binary`, compound `op=` in `emit_assign`) force
  `.into_int_value()`. Pointer arithmetic and compound mutation on
  raw pointers remain unsupported in v0, so those paths are statically
  unreachable on pointer-typed expressions. (Pointer **deref** *is*
  now supported — see the "Deref operator" section below; `emit_unary`
  gains a `Deref` arm that returns `Operand::Place(loaded_ptr)`, and
  the existing `store_into` / `load_value` machinery dispatches the
  load/memcpy/passthrough decision per the consumer's context.)

- **Linking:** no new build-system work. The existing `compile.sh` flow
  (`cc hello.o -o hello`) already pulls libc / libSystem by default, so
  `puts` resolves with no extra flags.

### Worked example

Source:

```rust
extern "C" { fn puts(s: *const [u8]) -> i32; }

fn main() -> i32 {
    puts("hello world");
    0
}
```

Emitted IR:

```llvm
@.str.0 = private unnamed_addr constant [12 x i8] c"hello world\00", align 1

declare i32 @puts(ptr)

define i32 @main() {
allocas:
  br label %body
body:
  %call = call i32 @puts(ptr @.str.0)
  ret i32 0
}
```

## Null literal

A typed null pointer literal expressible without generics, casts, or
library plumbing. Lets pure-Oxide programs interoperate with the C
ABI surface where "absent" is signalled via `NULL` (`getenv`, `fopen`,
`malloc`, most of `<unistd.h>` / `<stdlib.h>`).

### Subset-of-Rust caveat

`null` is an Oxide-reserved keyword. **Rust has no such keyword** —
in Rust source, `null` parses as an identifier and fails name
resolution. Reserving it here is an *additive* divergence: Oxide
accepts source Rust rejects (as unresolved-name), not the reverse.

C++ chose `nullptr` (over the shorter `null`) specifically to avoid
colliding with C's pre-existing `NULL` macro. Oxide is greenfield —
no `NULL` macro to dodge — so `null` is the cleaner pick: shorter, no
"ptr" suffix, matches the convention in modern non-C-lineage
languages (Java, C#, Kotlin's `null`; Swift's / Go's `nil`). The
token was already reserved in the lexer (`TokenKind::KwNull`) since
spec/01_LEXER.md, so this section just connects an existing
reservation to its semantics.

The typing rule (see "Typeck changes" below) uses **inference**
instead of C++'s `std::nullptr_t` implicit-conversion mechanism —
same role, mechanism that fits Oxide's existing `Infer` + loose-unify
model.

The "subset of Rust" constraint from `10_ADDRESS_OF.md` is therefore
softened by exactly one keyword. No further keyword additions are
anticipated.

### Acceptance

```rust
extern "C" {
    fn puts(s: *const [u8]) -> i32;
    fn write(fd: i32, buf: *mut [u8], n: usize) -> isize;
}

fn main() -> i32 {
    let s: *const u8 = null;        // *mut α → *const u8 (α=u8, Mut→Const)
    let buf: *mut u8 = null;        // *mut α → *mut u8   (α=u8, Mut→Mut)
    puts(null);                     // each `null` is its own α; α=[u8] here
    0
}
```

The same `null` token may flow into a `*const T` slot and a `*mut T`
slot in the same scope, so long as each occurrence is its own
expression — every `null` gets a fresh inference variable for the
pointee.

### Lexer / AST changes

- New keyword token: `TokenKind::KwNull`. Lexed by exact match
  alongside the other reserved words. Reserved unconditionally; no
  contextual-keyword behavior.
- New AST expression: `ExprKind::Null` (no payload). Slots into the
  atom layer of the Pratt builder alongside other literals.

### HIR changes

- New `HirExprKind::Null` (no payload).
- Lowering: AST `Null` → HIR `Null`; trivial, no operands.
- **Place rule.** `Null` is **not** a place. `compute_is_place`
  falls through its catch-all arm and returns `false`. Same posture
  as `IntLit`, `StrLit`, `AddrOf`. Operations that require a place —
  `null = v`, `&null` — error via the existing structural rules
  (`InvalidAssignTarget`, `AddrOfNonPlace`).

### Typeck changes

```text
infer_null() -> TyId:
    let α = fresh_infer_var()
    intern(TyKind::Ptr(α, Mutability::Mut))
```

That's the entirety of the new typing rule. **No new error variant.**

The choice of outer `Mut` is load-bearing:

- The existing coerce rule allows `*mut T → *const T` outer but
  forbids `*const T → *mut T` (only `as` casts grant write access,
  and `as` is out of v0). Typing `null` as `*mut α` therefore
  lets it flow into both `*const T` and `*mut T` slots.
- Typing as `*const α` would block `*mut T` slots — and most C-ABI
  surfaces that take `T*` map to `*mut T` in Oxide.

Pointee `α` is a **fresh inference variable per `null` expression**.
Its shape gets pinned at the use site by the existing loose-unify
rule (`*α₁ T₁  ~  *α₂ T₂` succeeds iff `T₁ ~ T₂`, mutability ignored
at unify); strict coerce then runs over the substituted type as
usual. Two distinct `null` expressions never share `α`.

If `α` is never pinned (`let x = null;` with no constraining use),
it stays `Infer(_)` at finalize. The existing `resolve_fully`
leftover-infer path triggers the standard "type cannot be inferred"
error — no new infrastructure. A diagnostic refinement ("type
annotation needed: null's pointee cannot be inferred") is welcome
polish but not spec-binding.

### Codegen

```text
emit_expr(Null) -> BasicValueEnum:
    ctx.ptr_type(AddressSpace::default()).const_null().into()
```

Opaque LLVM `ptr` null. Mutability is typeck-only and doesn't reach
codegen, same as every other pointer value.

### Worked examples

#### Single use site pins α

```rust
let p: *const u8 = null;
```

Unify `*mut α  ~  *const u8` (mut ignored) → `α = u8`. Coerce
`*mut u8 → *const u8` (outer `Mut → Const`) ✓. `p` typed as
`*const u8`.

```llvm
%p.0.slot = alloca ptr, align 8
store ptr null, ptr %p.0.slot, align 8
```

#### Multiple use sites, each fresh

```rust
puts(null);
puts(null);
```

Two distinct `null` AST nodes, two independent `α` variables, both
pinned to `u8` from `puts`'s parameter type.

#### Bound-then-used (compatible)

```rust
let p = null;
puts(p);          // pins p's α to u8
read_const(p);    // also expects *const u8; existing coerce passes
```

#### Bound-then-used (conflicting inner mut) — load-bearing edge

```rust
fn use_a(p: *const *const u8) {}
fn use_b(p: *const *mut u8) {}
fn bad() {
    let n = null;       // *mut α
    use_a(n);              // α unifies with *const u8; n: *mut *const u8
    use_b(n);              // ERROR: coerce *mut *const u8 → *const *mut u8
                           //        inner *const → *mut forbidden
}
```

This is the right answer — letting it through would launder inner
mutability and violate soundness. Workaround: call `null` twice —
`use_a(null); use_b(null);` — each call gets its own α.

#### Unconstrained α

```rust
let x = null;       // α never pinned; resolve_fully leaves Infer(_);
                       // existing leftover-infer error fires
```

#### Wrong shape (non-pointer slot)

```rust
let x: i32 = null;   // ERROR: cannot unify *mut α with i32
                        // (existing TypeMismatch)
```

### Out of scope (this round)

- **Compile-time null check.** `*null = v` typechecks fine;
  runtime UB.
- **Pointer equality** (`p == null`). No `==` on pointers in v0;
  add later via a dedicated small spec.
- **Optional / `Option<*const T>` modeling.** We use raw nullable
  pointers, not optional types; Rust's `Option<NonNull<T>>` niche
  optimization isn't applicable.

## Deref operator (`*p`)

The companion to `&` / `&mut` (spec/10): produces a place from a
pointer, enabling reads and writes through pointer-addressed storage.
Closes the basic pointer-operator pair so pure-Oxide code can mutate
memory it doesn't own a `let`-binding for.

### Subset-of-Rust constraint

`*p` parses identically in Rust with the same meaning (read or write
through a raw pointer). Rust's borrow checker does not apply — same
posture as `&` / `&mut`. The shape matches Rust's `unsafe { *p }` /
`unsafe { *p = v }` minus the `unsafe` block; Oxide has no `unsafe`
keyword in v0, so raw-pointer ops are just legal.

### Acceptance

```rust
fn main() -> i32 {
    let mut x: i32 = 0;
    let p: *mut i32 = &mut x;
    *p = 42;            // lvalue write
    let v: i32 = *p;    // rvalue read
    v - 42              // = 0
}
```

Rejection (immutable target):

```rust
fn bad() {
    let mut x: i32 = 0;
    let p: *const i32 = &x;
    *p = 1;             // E0263 MutateImmutable { Assign }
}
```

Composition with field projection through an explicit deref:

```rust
struct Point { x: i32, y: i32 }
fn write_field() {
    let mut p = Point { x: 0, y: 0 };
    let q: *mut Point = &mut p;
    (*q).x = 7;         // OK — outer mut governs through Deref → Field
}
```

### Position in the pipeline

```
Source ─▶ tokens ─▶ AST ─▶ HIR ─▶ typeck ─▶ codegen
                                ╰─── `*` deref operator added in this section ───╯
```

### AST changes (`src/parser/`)

- `UnOp` (in `src/parser/ast.rs`) gains a `Deref` variant. Today
  it's `Neg | Not | BitNot`; `Deref` is the natural fourth — same
  shape, no payload. (`AddrOf` got a dedicated `ExprKind::AddrOf`
  variant because it carries a `Mutability` payload; `Deref`
  doesn't, so it folds into `UnOp` cleanly.)
- Grammar: `DerefExpr ::= '*' UnaryExpr`. Slots into the prefix-unary
  level **13** alongside `&` / `-` / `!` / `~` in
  `src/parser/parse/syntax.rs`, right next to the `&` arm.

### Token disambiguation

`*` lexes as `TokenKind::Star`. Today it's used as binary
multiplication (`Mul`, level 11). The Pratt builder distinguishes
prefix `Star` (the new arm at level 13) from infix `Star` (existing
`Mul`) by position — at the start of an atom slot, `Star` parses as
prefix `Deref`; mid-expression, as `Mul`. Same precedent spec/10
set for `Amp` (prefix `&` vs infix `BitAnd`).

`**p` (deref of deref) parses cleanly. Unlike `&&` (which the lexer
greedily tokenizes as `AndAnd`), `**` lexes as two separate `Star`
tokens, so `**p` becomes `Deref(Deref(p))` with no special handling.

#### Precedence note

`*p.x` parses as `*(p.x)` — field access (postfix, level 12) binds
tighter than prefix `*` (level 13). To say "deref then field", write
`(*p).x`. This matches Rust verbatim.

### What the AST does *not* add

- A `*p as *const T`-style coercion through `as`. Out of v0.
- Deref of non-pointer expressions (e.g., `*5`). Caught at typeck —
  see `DerefNonPointer` below.

### HIR changes (`src/hir/`)

- `UnOp` (in `src/hir/ir.rs`) gains a `Deref` variant, mirroring the
  AST.
- Lowering: the existing AST `UnOp` lowering arm in
  `src/hir/lower.rs` picks up `Deref` for free — it's a unit-variant
  unary op.
- **Place rule.** `compute_is_place` returns `true` for
  `Unary { op: UnOp::Deref, .. }`. The placeholder comment at
  `src/hir/ir.rs:122-124` (which already names this variant as a
  pending place-producer) becomes the implemented arm.
- The operand of `Deref` does **not** need to be a place itself —
  `*returns_a_pointer()` is fine; the result *is* a place because
  the pointer addresses storage. (Compare to `AddrOf`, whose operand
  must be a place but whose result is not.)
- No new HIR error. Pointer-ness and unsized-pointee rejection are
  typeck concerns; HIR doesn't have types.

### Typeck changes (`src/typeck/`)

Three additions in `src/typeck/check.rs`:

#### 1. `infer_unary` for `Deref`

```text
infer_unary(UnOp::Deref, expr) -> TyId:
    inner_ty = infer_expr(expr)
    match resolve(inner_ty):
        Ptr(pointee, _) -> match resolve(pointee):
            Array(_, None) -> emit DerefUnsized { found: pointee, span };
                              return error_ty
            _              -> pointee
        _               -> emit DerefNonPointer { found: inner_ty, span };
                           return fresh_infer()    // poison-bounded
```

Two new error variants:

- `TypeError::DerefNonPointer { found: TyId, span: Span }` (E0264) —
  fires when the operand isn't a pointer.
- `TypeError::DerefUnsized { found: TyId, span: Span }` (E0265 — or
  reuse E0269 "unsized in value position" if its scope fits) — fires
  when the pointee is `[T]`. `*p` for `p: *const [T]` would
  materialize a value of unsized type, which Oxide forbids in value
  position. Workaround: index through the pointer directly (`p[i]`);
  the existing `Ptr(Array(_, None), _)` arm of `emit_index_place`
  handles this without ever materializing `[T]` as a value.

#### 2. `place_mutability` for `Deref`

Extends the match in `place_mutability` (`check.rs:1277`):

```text
Unary { op: Deref, expr } => match resolve(expr_tys[expr]):
    Ptr(_, m) -> Some(m)         // outer mut of operand's ptr type
    _         -> None            // typeck already emitted DerefNonPointer
```

This is the rule promised by `spec/10_ADDRESS_OF.md` line 240. **One
peel** — the pointer's *outer* mutability — *not* the recursive
`auto_deref_ptr` peel-to-innermost. Rationale: `*p` for
`p: *const *mut T` produces a place of type `*mut T` whose
write-permission is governed by the *outer* `*const`, because writing
to `*p` modifies the location `p` addresses. (See the design note
below for a worked trace showing the auto-deref and explicit-deref
rules give consistent results when composed.)

#### 3. No change to `infer_assign`

`infer_assign` already calls `place_mutability` on its target, so it
picks up the new `Deref` arm automatically. `*p = v` on `*const T`
errors as the existing `MutateImmutable { op: Assign }` (E0263);
`&mut *p` on `*const T` → `MutateImmutable { op: BorrowMut }`. **No
new mutability-error variants.**

### Codegen (`src/codegen/lower.rs`)

Codegen leans on the `Operand` abstraction (`Value` | `Place` |
`Unit`) introduced in commit `82d16cf`. With `store_into` doing
unified Value→store / Place→memcpy / Unit→no-op dispatch, and
`load_value` doing Value-passthrough / Place→load, **the deref
codegen needs no per-pointee-shape branching**. It just produces an
`Operand::Place(loaded_ptr)` and lets the consumer's existing
machinery handle the rest.

#### Rvalue (`emit_unary` arm for `UnOp::Deref`)

```text
emit_unary(UnOp::Deref, expr):
    inner_op = emit_expr(expr)?              // expr: Ptr(T, _)
    inner_ty = ty_of(expr)                   // the pointer type
    ptr = load_value(inner_op, inner_ty, "load").into_pointer_value()
    Some(Operand::Place(ptr))
```

The deref's storage *is* at the loaded pointer. Wrapping it as
`Operand::Place(ptr)` defers the load/memcpy/passthrough decision
to the consumer:

- `let v = *p;` (let-init) — `store_into(v.slot, Place(ptr),
  pointee_ty)` does memcpy of `sizeof(pointee_ty)`. Works uniformly
  for primitives, structs, and sized arrays.
- `*p` flowing into a binary op — `load_value(Place(ptr),
  pointee_ty, "deref")` emits the `build_load`. The "load only when
  needed" property comes for free.
- `(*p)[i]` — `emit_index_place` calls `emit_expr(base)` and
  destructures the resulting `Operand`; `Place(p) => p` (lower.rs
  line 929) hands the loaded pointer to the existing
  `while let TyKind::Ptr` peel-and-GEP loop unchanged.
- `(*p).x` — `emit_field`'s place path triggers because
  `is_place(Deref) == true` (per the HIR rule above); it calls
  `lvalue(Deref(p))` (see below) for the GEP base.
- `&*p` — `AddrOf`'s arm calls `lvalue(Deref(p))`; the result is
  the same loaded pointer, wrapped back as `Operand::Value(ptr)`.

The "arrays-as-places everywhere" invariant is preserved without an
explicit branch: a deref of `*const [T; N]` produces
`Operand::Place(ptr)`; consumers that expect a sized-array place
(let-init memcpy, `(*p)[i]` indexing, `&(*p)`) accept it directly,
exactly as they accept `Operand::Place` from `Local(lid)` for
array-typed locals.

#### Lvalue (`lvalue` arm for `Unary { op: UnOp::Deref, expr }`)

```text
lvalue(Deref(expr)):
    inner_op = emit_expr(expr).expect("Deref operand should not
                                       diverge in lvalue position")
    inner_ty = ty_of(expr)
    load_value(inner_op, inner_ty, "deref").into_pointer_value()
```

Same loaded pointer as the rvalue arm computes; the lvalue function
returns it as a `PointerValue` directly for callers (`emit_assign`,
`emit_field`, `emit_index_place`, `AddrOf`) that need the slot ptr.
No load of the pointee. The two arms can share a small helper if
desired; the spec only requires that they agree on "the deref's
storage address is `load_value(emit_expr(operand))`."

#### Why no pointee-shape branch

Pre-`Operand` (before `82d16cf`), `emit_expr` returned a
`BasicValueEnum`, which forced every consumer to know whether it was
holding an SSA value or a slot pointer. The deref rvalue had to
decide eagerly: load the pointee for primitives/structs, but return
the raw pointer for sized arrays (to preserve place form). With
`Operand`, the place-vs-value tag is part of the return; deref
always returns Place; consumers route via the unified
`store_into` / `load_value` helpers; no branching needed.

#### Defensive backstops

- **Unsized pointee.** `*p` for `p: *const [T]` is rejected at
  typeck (`DerefUnsized`). If somehow it reached codegen, the
  consumer chain would panic at `lower_ty(Array(_, None))`
  (`src/codegen/ty.rs:71-73` — `unreachable!("Array(_, None) is not
  a value type; typeck E0269 should have rejected")`) the moment
  any `store_into` or `load_value` tried to touch the pointee's
  size. The deref arm itself doesn't need to peek at the pointee;
  the existing guard in `lower_ty` is sufficient.
- **Non-pointer operand.** Rejected at typeck (`DerefNonPointer`).
  If it reached codegen, `into_pointer_value()` would panic on the
  non-pointer `BasicValueEnum`.
- **Unit / Never / Fn / Infer / Error pointees.** Typeck rejects
  these as pointers in value position upstream. Same upstream
  guard.

### Pre-existing codegen gap: `p.a` / `p[i]` auto-deref asymmetry

While reviewing for this section, an inconsistency surfaced that's
worth recording but **does not block** this work:

- `emit_index_place` (`lower.rs:912`) auto-derefs through arbitrary
  `Ptr` depth via a `while let TyKind::Ptr` loop. So `p[i]` for
  `p: *mut [T; N]` (or `*const *mut [T; N]`, etc.) works in codegen.
- `emit_field` (`lower.rs:720`) and the `Field` arm of `lvalue`
  (`lower.rs:680`) **do not** have this loop. They panic on a
  non-`Adt` base. So `s.x` for `s: *mut Point` typechecks fine (the
  acceptance test
  `tests/snapshots/typeck/acceptance_field_assign_through_mut_ptr.ox`
  exists) but would ICE if it ever hit codegen — and indeed there's
  no JIT test covering struct-field-through-pointer.

This section's deref work doesn't have to fix that gap. Once
explicit `*p` is implemented, **`(*p).a` works through codegen
without any new auto-deref machinery**: `Deref`'s lvalue returns the
operand pointer; `Field`'s `base_ty` after typeck is `Adt(Point)`;
the existing emit_field Adt arm handles it.

**Follow-up after deref lands** (separate spec / PR): a HIR
normalization pass can rewrite implicit auto-deref into explicit
`Deref` nodes — turning `Field { base: p, .. }` (where `p: *Point`)
into `Field { base: Deref(p), .. }`. That closes the latent codegen
gap and lets `auto_deref_ptr` retire. Out of scope for this section.

### Subtlety: outer-mut for explicit Deref vs innermost-mut for auto-deref

| place form                  | mut rule                                       |
|-----------------------------|------------------------------------------------|
| `(*p)` (explicit Deref)     | outer mut of `p`'s pointer type                |
| `s.a` / `p[i]` (auto-deref) | `auto_deref_ptr` peels all → **innermost** mut |

The two rules give consistent answers when composed because
Field/Index recursion in `place_mutability` terminates at the
explicit `Deref` node (via the existing `_ → recurse` arm at
`check.rs:1294`), at which point the outer-mut rule takes over.

#### Worked trace for `p: *mut *const Struct`

- `(*p).a = v` — HIR `Field { base: Deref(p), name: a }`:
  - `place_mutability(Field)`: `base_ty = *const Struct` (pointer);
    `auto_deref_ptr` peels one → innermost = `Const`; returns
    `Some(Const)`. **Blocked.** ✓
- `*p = new_const_ptr` — HIR `Assign(Deref(p), ...)`:
  - `place_mutability(Deref(p))`: outer of `*mut *const Struct` =
    `Mut`. **Allowed.** ✓ (Writes a new `*const Struct` to where `p`
    points.)

The asymmetry is by design: writing to `*p` only changes what `p`
itself addresses, while writing to `(*p).a` changes the *deeper*
storage that the inner pointer addresses.

### Composition cases

Under the `Operand` model, the deref's emit always returns
`Operand::Place(loaded_ptr)`; consumers dispatch via `store_into` /
`load_value` / `lvalue` as needed. The bullets below describe
user-visible behavior; each composes without per-pointee-shape
branching in the deref arm itself.

- `*p = v` on `*mut T` where T is primitive — basic write. `lvalue`
  computes the loaded pointer; `store_into(ptr, Value(v), T)`
  builds the store.
- `let v = *p;` on `*const T` where T is primitive — let-init calls
  `store_into(v.slot, Place(loaded_ptr), T)`, which dispatches to
  `emit_memcpy(sizeof T)`. Equivalent in observable behavior to
  load + store; LLVM optimizer collapses small fixed-size memcpys.
- `*p = v` on `*const T` — error E0263 `MutateImmutable { Assign }`.
- `(*p).x = v` and `(*p)[i] = v` — composition through Field/Index.
  After typeck, the inner `Deref(p)` produces a place whose
  `base_ty` resolves to the pointee (`Adt` or sized `Array`).
  Existing `emit_field` / `emit_index_place` arms handle these
  unchanged: `emit_field`'s place path triggers because
  `is_place(Deref) == true`; `emit_index_place` accepts the
  `Operand::Place(loaded_ptr)` from `emit_expr` directly.
- `*p` for `p: *const Point` (struct pointee) — emit_unary returns
  `Operand::Place(loaded_ptr)`. A consumer that needs the struct
  value (e.g., `let s = *p;`) memcpys via `store_into`; one that
  reads a field (`(*p).x`) GEPs without ever materializing the
  whole struct.
- `*p` for `p: *const [T; N]` (sized-array pointee) — same uniform
  path: `Operand::Place(loaded_ptr)`. The arrays-as-places
  invariant is preserved without a special branch — `(*p)[i]`
  indexes through it, `let q = *p;` memcpys, `&(*p)` returns the
  loaded ptr.
- `*p` for `p: *const [T]` (unsized-array pointee) — **rejected at
  typeck** (`DerefUnsized`). Workaround: use `p[i]` directly; the
  existing `Ptr(Array(_, None), _)` arm of `emit_index_place`
  handles unsized indexing without ever materializing `[T]`.
- `*p += 1` — compound assign: works because `*p` is a place; the
  existing compound-assign machinery (load + op + store through
  `lvalue(target)`) reuses the lvalue path.
- `&*p` — Address-of of a deref. `*p` is a place ⇒ `AddrOfNonPlace`
  doesn't fire; `AddrOf`'s arm calls `lvalue(Deref(p))` and wraps
  the result as `Operand::Value(ptr)`. Round-trip identity holds
  (with outer mut possibly weakened to const).
- `&mut *p` — same, but requires `place_mutability(*p) == Mut`
  (i.e., `p: *mut T`). On `*const T` →
  `MutateImmutable { BorrowMut }`.
- `**pp = v` for `pp: *mut *mut T` — multi-level: `place_mutability`
  recursion through nested `Unary { Deref }` arms; outer mut governs
  at each level. Each layer's `lvalue` does one load.
- `*p` where `p: i32` — error E0264 `DerefNonPointer` at typeck;
  codegen never sees it.

### Worked LLVM IR

For:

```rust
fn main() -> i32 {
    let mut x: i32 = 0;
    let p: *mut i32 = &mut x;
    *p = 42;
    *p
}
```

Lowered IR (allocas hoisted to entry):

```llvm
%x.0.slot = alloca i32, align 4
%p.1.slot = alloca ptr, align 8

store i32 0, ptr %x.0.slot, align 4
store ptr %x.0.slot, ptr %p.1.slot, align 8     ; let p = &mut x

%p.load = load ptr, ptr %p.1.slot, align 8      ; emit_expr(p) for `*p`
store i32 42, ptr %p.load, align 4              ; *p = 42  (lvalue path)

%p.load2 = load ptr, ptr %p.1.slot, align 8     ; emit_expr(p) for rvalue *p
%deref = load i32, ptr %p.load2, align 4        ; rvalue: build_load(i32, ...)
ret i32 %deref
```

For `(*q).x = 7` where `q: *mut Point`:

```llvm
%q.load = load ptr, ptr %q.0.slot, align 8      ; lvalue(Deref(q))
%fld.gep = getelementptr inbounds %Point, ptr %q.load, i32 0, i32 0
                                                ; existing Field-place GEP
store i32 7, ptr %fld.gep, align 4
```

For `*p` with `p: *const [i32; 3]` flowing into `(*p)[i]`:

```llvm
%p.load = load ptr, ptr %p.0.slot, align 8      ; emit_expr(p) load
                                                ; emit_unary(Deref) wraps
                                                ; this as Operand::Place(%p.load);
                                                ; emit_index_place destructures
                                                ; → cur_ptr = %p.load
; bounds check on %idx vs N=3
%idx.gep = getelementptr inbounds [3 x i32], ptr %p.load, i64 0, i64 %idx
%idx.load = load i32, ptr %idx.gep, align 4
```

For `let v: i32 = *p;` (primitive let-init via Operand::Place):

```llvm
%p.load = load ptr, ptr %p.0.slot, align 8      ; emit_unary(Deref)
                                                ; → Operand::Place(%p.load)
call void @llvm.memcpy.p0.p0.i64(ptr %v.slot, ptr %p.load, i64 4, i1 false)
                                                ; store_into dispatched to memcpy;
                                                ; equivalent to load+store; LLVM
                                                ; collapses small fixed-size cases.
```

### Errors summary

| Code  | Variant                                              | Layer  |
|-------|------------------------------------------------------|--------|
| E0264 | `TypeError::DerefNonPointer { found, span }`         | typeck |
| E0265 | `TypeError::DerefUnsized { found, span }` *(or reuse E0269)* | typeck |
| E0263 | `TypeError::MutateImmutable { op: Assign, span }`    | typeck |
| E0263 | `TypeError::MutateImmutable { op: BorrowMut, span }` | typeck |

E0263 is reused from `10_ADDRESS_OF.md` — same variant, same
discriminator — for `*p = v` on `*const T` and `&mut *p` on
`*const T`. The deref operator plugs into the existing
place-mutability machinery; no new mutability errors.

### Out of scope (this round)

- **Pointer arithmetic** in any form: `p + 1`, `p - q`, `*(p + 1)`.
  The C bug-magnet stays out. Lands later, almost certainly via
  methods (`ptr.add`, `ptr.offset`, `ptr.sub`) once struct methods
  land.
- **Pointer methods** (`ptr.add`, `ptr.is_null`, `ptr.read`,
  `ptr.write`). Whole feature deferred to the struct-method spec;
  that spec will define an intrinsic-method registry and these are
  its first citizens.
- **`as` casts between pointer types** — already out of v0.
- **Null-pointer checks at compile time.** `*null = v`
  typechecks; runtime UB.
- **Lifetime extension for `*temporary` / `&*temporary`** — Oxide
  has no lifetimes; doesn't apply.

### What this unblocks

Together with `&` / `&mut` (spec/10) and `null` (above), the
basic pointer-operator surface is complete. C-ABI functions taking
or returning pointers can now be called and consumed in pure-Oxide
code — no C glue required. The auto-deref-on-Field codegen gap
becomes addressable as a small follow-up (HIR rewrite to insert
explicit `Deref`).
