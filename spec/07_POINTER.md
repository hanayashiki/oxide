# Requirements

Currently, type only works for primitive ones, no `*mut u8`, `*const u8`.

We need to be able to handle pointers to some extent, but first of all lets only consider dealing with pointer as an atomic value, and try to pass the `*const u8` to `puts`. Full pointer support needs memory layout support which is too big right now.

## Acceptance

```
extern "C" {
    fn puts(s: *const u8) -> i32;
}

extern "C" {
    fn multi_ptr(s: *const *const *const u8);
}

fn main() -> i32 {
    puts("hello world");

    0
}
```

Anatomy:

1. String literal "hello world" is treated as "*const u8". Unlike "str" in rust is utf-8, Oxide does not have such luxury.

2. **String literals are C-style null-terminated.** This is a deliberate
   divergence from Rust and an alignment with C: every string literal
   emits a trailing `\0` byte that is *not* counted in the source-level
   length. So `"hello world"` is 11 visible characters but 12 bytes in
   the emitted data (`h e l l o ' ' w o r l d \0`). The pointer handed
   to FFI is to the first byte; the consumer (e.g. `puts`, `printf`)
   walks the bytes until it sees `\0`. There is no separate length
   field, no `&str` fat pointer, no `CString` wrapper type. A bare
   `"..."` literal *is* the C string.

   Rationale: the only string consumers we care about today are C ABI
   functions, and they all expect NUL-terminated `char*`. Carrying a
   Rust-style length around would be dead weight that we'd just have
   to strip at every FFI boundary. We are explicitly choosing the C
   model here, not Rust's.

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

4. StrLit still holds string (at compilation level), just get
   downleveled from utf-8 to bytes in type level. The `\0` terminator
   is appended at codegen time and is *not* stored in the AST/HIR
   string payload — so the source-level `.len()` of the literal stays
   honest if we ever expose it to user code.

   **Note: the "`*const u8` is the literal's type" model is
   scaffolding for the no-arrays-in-v0 era.** Both C (`char[6]`) and
   Rust (`&'static str`) keep an array layer that makes string
   literals places with a stable address. We collapsed that layer
   purely because arrays weren't available; one consequence is
   `&"hello"` is rejected today (see `10_ADDRESS_OF.md` "Subset
   gap"). Once arrays land (future `09_ARRAY.md`), the model
   transitions to:

   - `"hello"` has type `[u8; 6]` (matching C's `char[6]`; `N`
     counts the trailing `\0`).
   - `StrLit` becomes a place expression — codegen's existing
     private-global emission already gives it a stable address.
   - Existing FFI use sites continue to work via array-to-pointer
     decay (`[u8; N] → *const u8` at fn-arg / let-init position),
     so no source-level breakage.
   - `&"hello"` becomes legit and produces `*const [u8; N]`.

   The current spec (string-literal-IS-`*const u8`) stays in force
   until arrays land. Codegen needs no change at that point —
   `emit_str_lit`'s `[LEN+1 x i8]` global *is* the array layer; we
   just stop pretending it isn't.

5. Pointer access (*ptr) read and write is deferred. `*(ptr + 1)` too.

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
  `BasicValueEnum::PointerValue`, the existing call-arg, let-init, and
  local-load paths all work without touching them. The places that
  *would* break (`emit_binary`, `emit_unary`, compound `op=` in
  `emit_assign`) all force `.into_int_value()`, but v0 doesn't allow
  pointer arithmetic, deref, or compound mutation on pointers, so those
  paths are statically unreachable on pointer-typed expressions.

- **Linking:** no new build-system work. The existing `compile.sh` flow
  (`cc hello.o -o hello`) already pulls libc / libSystem by default, so
  `puts` resolves with no extra flags.

### Worked example

Source:

```rust
extern "C" { fn puts(s: *const u8) -> i32; }

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
