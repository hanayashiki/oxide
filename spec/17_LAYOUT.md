# Layout: `size_of`, `align_of`, and `transmute`

## Requirements

Compile-time reasoning about value layout — how many bytes a type occupies, how it's aligned, when bits can be moved between two types — is missing from oxide's typeck. The immediate need is `transmute`: a primitive that bit-copies a value of one type into another, used by `alloc<T>` (spec/16) to construct a `*mut T` from a `*mut u8` without going through a cross-pointee `as` (rejected by spec/12).

`transmute` is the *named* safety hole. Callers must write it explicitly, which makes the unsafety visible at the call site. This is strictly weaker than Rust's `unsafe { }` block; it's the v0 substitute until `unsafe` lands.

This spec defines:

1. Internal `size_of(TyId) -> Option<u64>` and `align_of(TyId) -> Option<u64>` helpers.
2. The `transmute<Src, Dst>(x: Src) -> Dst` intrinsic.

`size_of` and `align_of` are **not** user-callable expressions in v0 (no `sizeof::<T>()` surface syntax). They exist to support `transmute`'s validity check and future layout-aware features.

## Subset-of-Rust constraint

`transmute` matches Rust's `mem::transmute` semantics:

- The only validity gate is `size_of::<Src>() == size_of::<Dst>()`.
- Alignment is *not* checked at the transmute site — the destination is materialized into a freshly-aligned slot in the caller's frame, so source alignment is irrelevant. Alignment-related UB manifests at deref/access of a misaligned pointer downstream, not at the transmute itself.
- Mutability change is allowed via ptr-to-ptr (including `*const T → *mut T` — yes, this is the soundness hole; that's the point).
- Aggregates are accepted (`transmute<MyStruct, [u8; 8]>` works if both are 8 bytes).

We accept *fewer* programs than Rust:

- No `unsafe { }` requirement at the call site (we have no unsafe blocks). The function name `transmute` is the safety acknowledgement.
- Signature must be exactly `<Src, Dst>(Src) -> Dst` — no const-generic transmute, no transmute-of-references with lifetime constraints (we have no references / lifetimes).

## Acceptance

```rust
fn alloc<T>(size: usize) -> *mut T { transmute(malloc(size)) }   // ✓ ptr→ptr same width
let n: i32 = transmute::<u32, i32>(1u32);                         // ✓ same-width int reinterpret
let p_mut: *mut T = transmute(p_const);                            // ✓ *const T → *mut T (the named hole)
struct S { a: i32, b: i32 }
let bytes: [u8; 8] = transmute(s);                                 // ✓ aggregate→aggregate same size
```

```rust
let bad = transmute::<i32, i64>(1);                                // ✗ E0276: 4 ≠ 8 bytes
```

## Position in the pipeline

- `size_of` / `align_of` are pure functions in `src/typeck/ty.rs`, available to typeck and mono passes.
- The transmute size-equality check runs **post-monomorphization**. Each `transmute` instance's `(Src, Dst)` is concretized by the mono pass (spec/16); a per-instance check then asserts size equality.
- This means `transmute<T, U>(x)` in a generic body type-checks *unconditionally*. The check fires per instantiation. Consequence: the error span is the call site of the *instantiation*, not the body of the generic — matches Rust's E0512 and is more actionable for users.
- Codegen recognizes the transmute intrinsic by `FnId` (no name-string match) and emits the LLVM op directly, skipping the normal call lowering.

## `size_of` and `align_of`

```rust
pub fn size_of(tys: &TyArena, t: TyId) -> Option<u64> { ... }
pub fn align_of(tys: &TyArena, t: TyId) -> Option<u64> { ... }
```

Both return `None` when the size/alignment is unknown (unsized, unresolved, or non-value). The expectation is that they only get called on fully-concrete types (post-substitution); `Param` and `Infer` arguments yield `None`.

| `TyKind` | `size_of` (bytes) | `align_of` |
|----------|-------------------|------------|
| `Prim(i8/u8/bool)` | 1 | 1 |
| `Prim(i16/u16)` | 2 | 2 |
| `Prim(i32/u32)` | 4 | 4 |
| `Prim(i64/u64/usize/isize)` | 8 | 8 |
| `Ptr(_)` | 8 (target = x86_64 / aarch64) | 8 |
| `Array(t, Some(n))` | `size_of(t).checked_mul(n)?` | `align_of(t)` |
| `Adt(id)` | C-layout: sum of field sizes with each field aligned to its `align_of`, struct rounded up to its own alignment | max of field `align_of` (1 for empty structs) |
| `Unit` / `Never` | 0 | 1 |
| `Array(_, None)` | None | None |
| `Param(_)` / `Infer(_)` / `Error` / `Fn(..)` | None | None |

**Recursive ADT detection**: cycles without `Ptr` indirection are already rejected at typeck (spec/08, B013). `size_of` recurses without a depth limit on accepted ADTs; pointers terminate descent.

**Target assumption**: x86_64 SysV / AArch64 AAPCS conventions for v0 (both use natural-alignment). A sanity test asserts `size_of` matches `data_layout.get_store_size_of(...)` for representative types after codegen runs.

## The `transmute` intrinsic

**Signature**:
```rust
fn transmute<Src, Dst>(x: Src) -> Dst   // body-less; compiler-recognized
```

**Declaration**: TBD per spec/16 open question. Either declared in `stdlib/stdlib.ox` (requiring HIR to permit body-less non-extern fns) or compiler-synthesized with no source presence. **Recommend** the latter to keep the surface language minimal.

**Mono-time check** (per instance, after substitution):

```
size_of(Src) == size_of(Dst)   else E0276 TransmuteSizeMismatch
```

`size_of` is called on concrete types only — by the time mono runs, all `Param` slots are filled. No special-casing for `Ptr(Param)` is needed; `Ptr(_)` is uniformly 8 bytes for any concrete pointee.

## Codegen

`emit_call` recognizes the transmute intrinsic by `FnId` and dispatches by `(Src kind, Dst kind)`:

| `(Src, Dst)` | LLVM op |
|--------------|---------|
| `(Prim, Prim)` same width | no-op (or `bitcast` if LLVM int types differ) |
| `(Ptr, Ptr)` | no-op (LLVM `ptr` is opaque) |
| `(Ptr, Prim)` ptr-width int | `ptrtoint` |
| `(Prim, Ptr)` ptr-width int | `inttoptr` |
| any other pair, sizes equal | spill src to a stack alloca, `bitcast` the alloca pointer to `*Dst`, load — equivalent to `llvm.memcpy.p0.p0.i64` of `size_of(Src)` bytes |

The aggregate fallback handles `transmute<MyStruct, [u8; 8]>` and similar without case-explosion in codegen.

Per-instance LLVM declarations are not emitted for transmute — codegen synthesizes the IR inline, so `transmute$T<id>$T<id>` symbols never appear in the module.

## Errors

| Code | Variant | When |
|------|---------|------|
| E0276 | `TransmuteSizeMismatch { src: TyId, dst: TyId, src_size: u64, dst_size: u64, span: Span }` | Per-instance size check fails after substitution. |

Diagnostic format (mirrors rustc E0512):
```
error[E0276]: cannot transmute between types of different sizes
  --> tests/snapshots/typeck/transmute_size_mismatch.ox:N:M
   |
N  |     let x = transmute::<i32, i64>(1);
   |             ^^^^^^^^^^^^^^^^^^^^^^^^^
   |
   = note: source type: `i32` (4 bytes)
   = note: target type: `i64` (8 bytes)
```

## Out of scope (this round)

- `sizeof<T>()` / `alignof<T>()` as user-callable expressions.
- `unsafe { }` blocks (transmute name itself is the safety acknowledgement in v0).
- Niche-occupied layouts (Option<&T> = pointer width via null-niche). All unions/enums in v0 are tag-prefixed C layout.
- `repr(C)` / `repr(packed)` attributes — implicit C layout only.
- Dependently-sized transmutes.

## Out of scope (forever-ish)

- Stable cross-compilation `size_of` (the target-tuple machinery).

## Worked examples

```rust
// The motivating case: alloc<T> from spec/16, body unfolded.
fn alloc<T>(size: usize) -> *mut T {
    transmute(malloc(size))   // size_of(*mut u8) == size_of(*mut T) == 8 — passes per instance
}
```

```rust
// Aggregate transmute — Rust supports this, we do too.
struct S { a: i32, b: i32 }
fn s_to_bytes(s: S) -> [u8; 8] { transmute(s) }
```

```rust
// Same-width int reinterpret — no LLVM op needed.
fn u32_as_i32(x: u32) -> i32 { transmute(x) }
```

## Open questions for spec iteration

1. **Declaration site of transmute**: stdlib source declaration (requires HIR change for body-less non-extern) vs. compiler-synthesized (zero source presence, FnId minted by typeck). Recommend the latter.
2. **`align_of` use sites in v0**: `align_of` is defined here for completeness but no v0 caller uses it. Drop from this spec until a real consumer appears? Or keep as part of the layout vocabulary so the helper exists when needed.
