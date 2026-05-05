# Layout: `size_of`, `align_of`, `ox_size_of`, and `ox_transmute`

## Requirements

Compile-time reasoning about value layout — how many bytes a type occupies, how it's aligned, when bits can be moved between two types — is missing from oxide's typeck. The immediate need is `ox_transmute`: a primitive that bit-copies a value of one type into another, used by `ox_alloc<T>` (spec/16, the typed alloc wrapper in `stdlib/mem.ox`) to construct a `*mut T` from a `*mut u8` without going through a cross-pointee `as` (rejected by spec/12).

The companion need is `ox_size_of<T>() -> usize`: a user-callable intrinsic that returns the size in bytes of any concrete `T`. Without it, callers can't write `malloc(ox_size_of::<T>())` and have to either hardcode widths (brittle, non-generic) or use `ox_transmute` to bypass the type-system entirely.

`ox_transmute` is the *named* safety hole. Callers must write it explicitly, which makes the unsafety visible at the call site. This is strictly weaker than Rust's `unsafe { }` block; it's the v0 substitute until `unsafe` lands.

**Naming convention** (applies throughout this spec): every function we author in our own bundled stdlib carries the `ox_` prefix — `ox_transmute`, `ox_size_of`, `ox_alloc`, `ox_alloc_zeroed`, `ox_dealloc`, `ox_realloc`. C-binding stdlib files (`stdio.ox`, `stdlib.ox`, `string.ox`) keep the C names verbatim because those identifiers must match the linker symbols. The prefix kills cross-namespace collisions (e.g. our typed `ox_realloc<T>` and C `realloc` coexist freely) and makes "is this our wrapper or a C function?" instantly visible at use sites. Internal Rust-side helpers in `src/typeck/ty.rs` (`size_of`, `align_of`, `substitute_ty`) carry no prefix because they are not Oxide-source identifiers.

This spec defines:

1. Internal `size_of(typeck, TyId) -> Option<u64>` and `align_of(typeck, TyId) -> Option<u64>` helpers (Rust API on `TypeckResults`, in `src/typeck/ty.rs`).
2. The `ox_transmute<Src, Dst>(x: Src) -> Dst` intrinsic (Oxide-source, declared body-less in `stdlib/intrinsics.ox`).
3. The `ox_size_of<T>() -> usize` intrinsic (Oxide-source, declared body-less in `stdlib/intrinsics.ox`).

## Subset-of-Rust constraint

`ox_transmute` matches Rust's `mem::transmute` semantics:

- The only validity gate is `size_of(Src) == size_of(Dst)` (Rust-side helper, applied per instance).
- Alignment is *not* checked at the transmute site — the destination is materialized into a freshly-aligned slot in the caller's frame, so source alignment is irrelevant. Alignment-related UB manifests at deref/access of a misaligned pointer downstream, not at the transmute itself.
- Mutability change is allowed via ptr-to-ptr (including `*const T → *mut T` — yes, this is the soundness hole; that's the point).
- Aggregates are accepted (`ox_transmute<MyStruct, [u8; 8]>` works if both are 8 bytes).

We accept *fewer* programs than Rust:

- No `unsafe { }` requirement at the call site (we have no unsafe blocks). The function name `ox_transmute` is the safety acknowledgement.
- Signature must be exactly `<Src, Dst>(Src) -> Dst` — no const-generic transmute, no transmute-of-references with lifetime constraints (we have no references / lifetimes).

`ox_size_of` matches Rust's `core::mem::size_of` semantics with one important divergence:

- In Rust, `size_of::<T>()` is `const fn` and its result can flow into type-level positions (`const N: usize = size_of::<i32>(); let s: [u8; N];`).
- Oxide has no const-expr; `ox_size_of::<T>()` yields a **runtime `usize` value only**. It cannot appear in array length positions (`[u8; ox_size_of::<i32>()]` parses to a non-literal length and is rejected — array lengths are integer literals per spec/09). This is a deliberate v0 simplification; const-expr is a separate feature unscoped here. The intrinsic is still useful at runtime (e.g. `malloc(ox_size_of::<T>())`).

## Acceptance

```rust
import "intrinsics.ox";
import "stdlib.ox";

fn alloc<T>() -> *mut T { ox_transmute(malloc(ox_size_of::<T>())) }   // ✓ ptr→ptr same width
let n: i32 = ox_transmute::<u32, i32>(1u32);                           // ✓ same-width int reinterpret
let p_mut: *mut T = ox_transmute(p_const);                              // ✓ *const T → *mut T (the named hole)
struct S { a: i32, b: i32 }
let bytes: [u8; 8] = ox_transmute(s);                                   // ✓ aggregate→aggregate same size

let four: usize  = ox_size_of::<i32>();                                  // ✓ → 4
let eight: usize = ox_size_of::<*mut i32>();                             // ✓ → 8
let s_size: usize = ox_size_of::<S>();                                   // ✓ → 8
```

```rust
let bad = ox_transmute::<i32, i64>(1);                                  // ✗ E0276: 4 ≠ 8 bytes
```

## Position in the pipeline

- `size_of` / `align_of` are pure functions on `TypeckResults`, available to typeck, mono, and codegen passes.
- Both intrinsics live in `stdlib/intrinsics.ox`, body-less. HIR scanner permits the body-less form **iff** the fn is recognized via the file-gate + name-allowlist mechanism (§Intrinsic recognition); otherwise the existing E0209 `BodylessFnOutsideExtern` still fires.
- Typeck treats intrinsic fns as ordinary generic fns for unification and `call_type_args` recording — no special path.
- Mono treats intrinsic instances as ordinary instances for cascading. At instantiation time it **(a) runs the per-instance validity check** and **(b) stamps an `Instance::operation: InstanceOperation` field** (see §Per-instance operation) that records what codegen should do:
  - `ox_transmute`: size-equality check runs after substitution; on mismatch push E0276. Either way, stamp `operation = Transmute`. Means `ox_transmute<T, U>(x)` in a generic body type-checks *unconditionally*; the check fires per instantiation. Consequence: the error span is the call site of the *instantiation*, not the body of the generic — matches Rust's E0512 and is more actionable for users.
  - `ox_size_of`: call the Rust-side `size_of` helper on the substituted `T` and stamp `operation = SizeOf { size: n }`. The helper returning `None` is unreachable in v0 — typeck's E0269 `UnsizedArrayAsValue` rejects unsized type-args before mono runs, and every other type with no size (`Param`/`Infer`/`Fn`/`Error`) is caught earlier still. The arm panics with `unreachable!()` if it ever fires; see §Out of scope for the deferred `LayoutUnknown` error and its prerequisites.
  - Regular calls: stamp `operation = Call`.
- Codegen reads `Instance::operation` and dispatches — no name-string match, no recomputation of `size_of`, no second peek at `HirFn::intrinsic`. Intrinsic instances (`operation != Call`) are **not** declared in Pass 1 — no LLVM `declare` lines for them ever appear in the module. The `HirFn::intrinsic` field still exists, but only the HIR scanner reads it (to decide whether to fire E0209); mono and codegen route through `Instance::operation` instead.

## Intrinsic recognition

`stdlib/intrinsics.ox` content (the **entire** file — intrinsics only, nothing else):

```rust
fn ox_transmute<Src, Dst>(x: Src) -> Dst;
fn ox_size_of<T>() -> usize;
```

The implementation will add `intrinsics.ox` (and `mem.ox`, see §Bundled `mem.ox`) to `STDLIB_FILES` (`src/loader/host.rs`), alongside the existing `stdio.ox`, `stdlib.ox`, `string.ox` entries. Explicit-import only — the resolver short-circuits stdlib names to bundled paths per `is_stdlib_name`. The file requires no imports of its own: `usize`, generic parameters, and pointer types are all language-level primitives, not stdlib symbols.

**What does NOT go in intrinsics.ox**: regular fns with bodies. They go in `stdlib/mem.ox` — a sibling stdlib file shipping the convenience layer (`ox_alloc`, `ox_alloc_zeroed`, `ox_dealloc`, `ox_realloc`). intrinsics.ox is the *intrinsic* file — it exists specifically because intrinsics need bodyless declarations and a recognition gate. The split mirrors Rust's `core::intrinsics` (bodyless, compiler-recognized) vs `core::mem`/`alloc::alloc` (regular wrappers).

**HIR marker** (Rust-side enum, no prefix needed inside the compiler):

```rust
pub enum Intrinsic { Transmute, SizeOf }
// HirFn gains: pub intrinsic: Option<Intrinsic>
```

`HirFn::intrinsic` is set by the scanner when both gates pass and is the only HIR-level signal that this fn is an intrinsic. It is **read by the HIR scanner only** (to decide whether to fire E0209). Mono uses it as a cheap "should I compute a non-`Call` operation for this instance?" check and then stamps `Instance::operation` accordingly (see §Per-instance operation). After mono, codegen reads `Instance::operation` and never touches `HirFn::intrinsic` again.

**Scanner rule** (replaces the unconditional E0209 at `src/hir/lower/scanner.rs:261`): when a body-less non-`extern` fn is encountered, evaluate **both** gates:

1. **File gate**: the canonical file path is `PathBuf::from("intrinsics.ox")` (the bundled stdlib mount point — see `STDLIB_FILES` in `src/loader/host.rs`).
2. **Name gate**: `name_to_intrinsic(fn.name)` returns `Some(...)` (mapping defined below).

| File gate | Name gate | Outcome |
|---|---|---|
| ✓ | ✓ | set `intrinsic = Some(...)` via the name→variant map below, **skip** E0209 |
| ✓ | ✗ | still E0209 (a future contributor adding `fn ox_foo<T>();` to intrinsics.ox doesn't accidentally get a silent intrinsic — must extend the allowlist + `Intrinsic` enum + codegen dispatch in tandem) |
| ✗ | — | E0209 unconditionally (a user file named `intrinsics.ox` next to `main.ox` does NOT trigger; resolution treats stdlib `intrinsics.ox` as canonical, so user shadowing is impossible per `is_stdlib_name` at `src/loader/host.rs:135`) |

**Name → variant mapping** (the lookup performed when both gates pass):

```rust
fn name_to_intrinsic(name: &str) -> Option<Intrinsic> {
    match name {
        "ox_transmute" => Some(Intrinsic::Transmute),
        "ox_size_of"   => Some(Intrinsic::SizeOf),
        _              => None,
    }
}
```

`name_to_intrinsic` is the **single source of truth** for the allowlist — its `Some` arms are the recognized names, so a separate `INTRINSIC_NAMES: &[&str]` constant would be redundant. Adding a new intrinsic is two coordinated edits: add an `Intrinsic` variant + a `name_to_intrinsic` arm, and extend the codegen dispatch in `emit_call`. The list of intrinsic names that *must exist as declarations* lives in `stdlib/intrinsics.ox` itself — the compiler never enumerates them, it only recognizes them when it sees them.

Generic-extern restriction (E0212) does **not** apply: intrinsics are not extern. `<Src, Dst>` and `<T>` are legal.

**Why filename, not an attribute or extern ABI**: Oxide has no attribute syntax, and adding a new keyword (`intrinsic fn`) or a magic ABI string (`extern "oxide-intrinsic"`) is more parser surface than two intrinsics need. The filename gate is the cheapest mechanism that still gives source-visible marking (the intrinsics.ox file content). Migration path documented in §Future intrinsic surface.

**Symbol emission**: codegen Pass 1 (`src/codegen/lower.rs:105`) skips `inst_decls` for instances whose `operation != InstanceOperation::Call` (see §Per-instance operation). No `declare` lines for intrinsics ever appear in the module. `ox_transmute$T<id>$T<id>` and `ox_size_of$T<id>` mangled symbols never appear.

## Per-instance operation

`Instance` (in `src/mono/mod.rs`) gains an `operation` field that records what codegen should do for this instance:

```rust
pub enum InstanceOperation {
    /// Default: codegen does normal call lowering against this instance.
    Call,

    /// User-callable `ox_size_of<T>()`. The size is precomputed at mono
    /// time via the Rust-side `size_of` helper and stored here so codegen
    /// emits a single `i64 <size>` constant — no helper call from codegen.
    SizeOf { size: u64 },

    /// User-callable `ox_transmute<Src, Dst>(x)`. Marker only; codegen
    /// dispatches structurally on `(instance.params[0] kind, instance.ret kind)`
    /// per the table in §Codegen. Size equality is already enforced by
    /// the per-instance E0276 check at mono time, so codegen needn't recheck.
    Transmute,
}

pub struct Instance {
    pub fid: FnId,
    pub type_args: Vec<TyId>,
    pub params: Vec<TyId>,
    pub ret: TyId,
    pub mangled: String,
    pub depth: u32,
    pub origin: InstanceOrigin,
    pub operation: InstanceOperation,    // NEW
}
```

**Stamping rule** (in `instantiate()`, after substituting params/ret through the caller's subst map):

```rust
let operation = match hir.fns[fid].intrinsic {
    Some(Intrinsic::Transmute) => {
        let src_size = size_of(typeck, params[0]).unwrap_or_else(|| unreachable!());
        let dst_size = size_of(typeck, ret).unwrap_or_else(|| unreachable!());
        if src_size != dst_size {
            push E0276 { src_size, dst_size, .. };
        }
        InstanceOperation::Transmute
    }
    Some(Intrinsic::SizeOf) => {
        // size_of returning None on a post-substitution type-arg is
        // unreachable in v0: typeck's E0269 rejects unsized type-args
        // (Array(_, None)), and Param/Infer/Fn never reach mono after
        // substitution. Reaching the panic indicates a compiler bug,
        // not a user error.
        let n = size_of(typeck, type_args[0]).unwrap_or_else(|| unreachable!());
        InstanceOperation::SizeOf { size: n }
    }
    None => InstanceOperation::Call,
};
```

When the size-mismatch validity check fails (E0276), the instance is still pushed (codegen needs the `(fid, resolved_args)` key to be stable for diagnostics span lookup), the operation is still stamped, and the error short-circuits the driver before codegen runs — so the post-error instance is never observed at codegen.

**Why this design**:

- **Single computation per instance.** `size_of` is called exactly once per `ox_size_of` instance (at mono time). Codegen reads `SizeOf { size }` from the instance and emits the constant directly.
- **Codegen is pure dispatch.** `match instance.operation { Call => ..., SizeOf { size } => ..., Transmute => ... }`. No more `hir.fns[fid].intrinsic` lookup, no more `size_of(typeck, ...)` calls from codegen.
- **Validity is committed at mono.** Codegen can't accidentally disagree with the validation because there's no recomputation path to disagree on.
- **Pass 1 skip is uniform**: `if instance.operation != Call { skip declaring }`. Same code path handles all current and future intrinsics.
- **Adding a new intrinsic** is now four coordinated edits: (1) add `Intrinsic` variant + `name_to_intrinsic` arm, (2) add `InstanceOperation` variant with whatever payload codegen needs, (3) add stamping arm in `instantiate()`, (4) add codegen branch on the new operation.

## `size_of` and `align_of`

```rust
pub fn size_of(typeck: &TypeckResults, t: TyId) -> Option<u64> { ... }
pub fn align_of(typeck: &TypeckResults, t: TyId) -> Option<u64> { ... }
```

These are **Rust-side helpers** (in `src/typeck/ty.rs`), distinct from the user-callable `ox_size_of` intrinsic. They take `&TypeckResults` (not just `&TyArena`) because ADT field walks need `typeck.adts[aid]` and reuse `typeck.tys.substitute_ty(...)`.

Both return `None` when the size/alignment is unknown (unsized, unresolved, or non-value). The expectation is that they only get called on fully-concrete types (post-substitution); `Param` and `Infer` arguments yield `None`.

| `TyKind` | `size_of` (bytes) | `align_of` |
|----------|-------------------|------------|
| `Prim(i8/u8/bool)` | 1 | 1 |
| `Prim(i16/u16)` | 2 | 2 |
| `Prim(i32/u32)` | 4 | 4 |
| `Prim(i64/u64/usize/isize)` | 8 | 8 |
| `Ptr(_)` | 8 (target = x86_64 / aarch64) | 8 |
| `Array(t, Some(n))` | `size_of(t).checked_mul(n)?` | `align_of(t)` |
| `Adt(aid, args)` | C-layout per algorithm below | max of substituted-field `align_of` (1 for empty structs) |
| `Unit` / `Never` | 0 | 1 |
| `Array(_, None)` | None | None |
| `Param(_)` / `Infer(_)` / `Error` / `Fn(..)` | None | None |

**ADT layout algorithm** (matches LLVM's `StructLayout`, which matches the C ABI on x86_64 SysV / AArch64 AAPCS):

```
fn adt_size(aid, args) -> Option<u64>:
    fields = typeck.adts[aid].variants[0].fields    // single-variant struct
    offset = 0
    struct_align = 1
    for each field in declaration order:
        field_ty = typeck.tys.substitute_ty(field.ty, build_subst(adt_generic_params, args))
        field_size  = size_of(typeck, field_ty)?     // None propagates
        field_align = align_of(typeck, field_ty)?
        offset = round_up(offset, field_align)        // pad before field
        offset += field_size                          // place field
        struct_align = max(struct_align, field_align)
    Some(round_up(offset, struct_align))              // trailing pad to struct align
```

Mirror algorithm for `align_of`: walk fields, return `max(field_align, ..., 1)`. Empty structs get align 1, size 0.

Worked example: `struct { a: u8, b: u32, c: u8 }`. Pass: offset=0 → place `a` at 0 (size 1), offset=1 → round_up(1, 4)=4, place `b` at 4 (size 4), offset=8 → round_up(8, 1)=8, place `c` at 8 (size 1), offset=9 → struct_align=4 → round_up(9, 4)=12. Total: 12 bytes, align 4.

Fields are substituted through `args` via `substitute_ty` — same pattern `codegen/ty.rs:lower_adt_type` uses today.

**Recursive ADT detection**: cycles without `Ptr` indirection are already rejected at typeck (spec/08, B013). `size_of` recurses without a depth limit on accepted ADTs; pointers terminate descent.

**Target assumption**: v0 supports exactly two target triples — `aarch64-apple-darwin` (macOS on Apple Silicon, AAPCS64-Darwin ABI) and `x86_64-unknown-linux-gnu` (Linux on Intel/AMD 64, SysV ABI). For our v0 primitive set + C-layout ADTs, **both targets produce byte-identical layouts** — every numeric entry in the `size_of` / `align_of` table above holds verbatim on both. The two ABIs both use natural alignment, both have 64-bit pointers / `usize` / `isize`, and neither applies struct-packing or special-alignment rules at our layout's resolution. The check in the table — primitive widths from 1 to 8 bytes, `Ptr` always 8 bytes, ADT C-layout — is identical across the two.

Consequence: `size_of(typeck, ty)` does **not** take a `target_triple` parameter in v0. The numeric constants in the table are hardcoded against the assumption that pointer width = 8 bytes. The compiler's `config.target_triple: Option<Triple>` (in `src/config.rs:23`, `target_lexicon::Triple`) is consulted by codegen for emitting object files but is not threaded into the layout helper. If a 32-bit target (e.g. `i686-unknown-linux-gnu`) or any non-64-bit-pointer target is ever added, the helper signature grows a target parameter and the `Ptr` / `usize` / `isize` rows become `target.pointer_width().bytes()`-driven; no other rows change. v0 punts on cross-compilation to non-64-bit targets explicitly.

A sanity test asserts `size_of` matches `data_layout.get_store_size_of(...)` for representative types after codegen runs. This catches accidental drift between the helper's hardcoded values and LLVM's `data_layout` (which IS target-dependent and stamped per-module by `src/builder/target.rs::stamp_module`); on either supported target, the assertion holds because the two are computing the same numbers.

## The `ox_transmute` intrinsic

**Signature**:
```rust
fn ox_transmute<Src, Dst>(x: Src) -> Dst   // body-less; compiler-recognized
```

**Declaration**: in `stdlib/intrinsics.ox`. Recognition mechanism is specified in §Intrinsic recognition (file gate + name allowlist).

**Mono-time check** (per instance, after substitution):

```
size_of(Src) == size_of(Dst)   else E0276 TransmuteSizeMismatch
```

The Rust-side `size_of` helper is called on concrete types only — by the time mono runs, all `Param` slots are filled. No special-casing for `Ptr(Param)` is needed; `Ptr(_)` is uniformly 8 bytes for any concrete pointee. Errors are owned by `MonoError` (per-instance, after substitution), reusing the existing `MonoCtx::errors: Vec<MonoError>` sink in `src/mono/mod.rs`.

**Where the check fires**: at instantiation time, inside `instantiate()` (the entry point that pushes a new `Instance` into `MonoResults::instances`). After substituting `Src`/`Dst` through the caller's subst map and calling the Rust-side `size_of` helper, mono **stamps `InstanceOperation::Transmute`** on the instance regardless of outcome (so codegen has a uniform variant to match on); on size mismatch it also pushes E0276 onto `MonoCtx::errors`. The error short-circuits the driver before codegen runs.

Intrinsic instances go through the **same `(fid, resolved_args)` keying** as regular instances — the only differences are: (a) no LLVM `declare` in Pass 1 (because `operation != Call`), (b) inline IR emission in `emit_call` driven by `instance.operation`, (c) per-instance validity check + operation stamping at instantiation. See §Per-instance operation for the full stamping logic.

## The `ox_size_of` intrinsic

Disambiguation: this section is about the **user-callable** `ox_size_of<T>() -> usize` Oxide-source intrinsic. The internal Rust helper of the analogous name (`size_of(typeck, t) -> Option<u64>`) is specced in §`size_of` and `align_of`. The intrinsic delegates to the helper at mono time. Different namespaces (Oxide source vs Rust compiler internals), different prefixes, no collision.

**Signature**:
```rust
fn ox_size_of<T>() -> usize   // body-less; compiler-recognized
```

**Declaration**: in `stdlib/intrinsics.ox`. Recognition mechanism is specified in §Intrinsic recognition.

**Mono-time evaluation**: per instance, call the Rust-side `size_of(typeck, T_substituted)` helper (with `T_substituted = instance.type_args[0]`). The expected result is `Some(n)` — every reachable type-arg passes typeck's E0269 `Sized` obligation, so by the time mono runs `T` is concrete-sized. **Stamp `InstanceOperation::SizeOf { size: n }`** on the instance — this is the only place the size value is computed.

**Validity**: helper returning `None` is unreachable in v0 (typeck's E0269 catches unsized type-args before mono); the stamp arm panics with `unreachable!()` if it ever fires. A user-facing `LayoutUnknown` (E0277) is deferred until a reachable trigger lands — see §Out of scope.

**Codegen**: read `size` directly off `InstanceOperation::SizeOf { size }` and emit an `i64` constant at the call site in `emit_call`. No helper call from codegen, no call instruction emitted. The intrinsic's instance has no LLVM `declare` (per §Intrinsic recognition).

## Codegen

`emit_call` dispatches on `instance.operation` (no name-string match, no `HirFn::intrinsic` lookup):

```rust
match instance.operation {
    InstanceOperation::Call             => /* normal call lowering */,
    InstanceOperation::SizeOf { size }  => emit_i64_const(size),
    InstanceOperation::Transmute        => emit_transmute(instance.params[0], instance.ret, arg_value),
}
```

**`InstanceOperation::SizeOf { size }`**: emit an `i64` constant equal to `size`. No alloca, no call, no other IR. Single instruction.

**`InstanceOperation::Transmute`**: dispatch by `(Src kind, Dst kind)` where `Src = instance.params[0]` and `Dst = instance.ret`:

| `(Src, Dst)` | LLVM op |
|--------------|---------|
| `(Prim, Prim)` same width | no-op (or `bitcast` if LLVM int types differ) |
| `(Ptr, Ptr)` | no-op (LLVM `ptr` is opaque) |
| `(Ptr, Prim)` ptr-width int | `ptrtoint` |
| `(Prim, Ptr)` ptr-width int | `inttoptr` |
| **all other size-equal pairs** (incl. `(Adt, Adt)`, `(Adt, Array)`, `(Array, Adt)`, `(Adt, Prim)`, `(Prim, Adt)`, etc.) | spill src to a stack alloca, `bitcast` the alloca pointer to `*Dst`, load — equivalent to `llvm.memcpy.p0.p0.i64` of `size_of(Src)` bytes |

The first four rows are optimizations for shapes where LLVM has a direct op; the **alloca fallback is the catch-all** and correctly handles every size-equal pair. The size equality is already enforced by E0276 at mono time, so codegen doesn't recheck — and codegen doesn't call `size_of` either; the alloca-store-load uses LLVM types (which encode their own size), not numeric byte counts. This is why `ox_transmute<MyStruct, [u8; 8]>` and `ox_transmute<*mut T, MyOpaqueHandle>` both work without per-shape codegen branches.

Per-instance LLVM declarations are **not** emitted for any non-`Call` operation — codegen synthesizes the IR inline, so `ox_transmute$T<id>$T<id>` and `ox_size_of$T<id>` symbols never appear in the module.

## Errors

| Code | Variant | When |
|------|---------|------|
| E0276 | `TransmuteSizeMismatch { src: TyId, dst: TyId, src_size: u64, dst_size: u64, span: Span }` | Per-`ox_transmute`-instance: Rust-side `size_of(Src) != size_of(Dst)` after substitution. |

Lives on `MonoError` (mono-time errors). The existing `DivergentMonomorphization` (E0278) is unaffected. A `LayoutUnknown` (E0277) for `ox_size_of` is deferred — see §Out of scope.

Diagnostic format for E0276 (mirrors rustc E0512):
```
error[E0276]: cannot transmute between types of different sizes
  --> tests/snapshots/typeck/transmute_size_mismatch.ox:N:M
   |
N  |     let x = ox_transmute::<i32, i64>(1);
   |             ^^^^^^^^^^^^^^^^^^^^^^^^^^^^
   |
   = note: source type: `i32` (4 bytes)
   = note: target type: `i64` (8 bytes)
```

## Out of scope (this round)

- `ox_align_of<T>()` as a user-callable expression — no v0 consumer. The Rust-side `align_of(TyId)` helper is internal-only this round.
- `unsafe { }` blocks (the `ox_transmute` name itself is the safety acknowledgement in v0).
- Niche-occupied layouts (`Option<&T>` = pointer width via null-niche). All unions/enums in v0 are tag-prefixed C layout.
- `repr(C)` / `repr(packed)` / `repr(align(N))` attributes — implicit C layout only.
- Dependently-sized transmutes.
- Const-evaluable layout queries (`const N: usize = ox_size_of::<i32>();` — Oxide has no const-expr).
- `E0277 LayoutUnknown` — deferred until a reachable trigger exists. The current spec layer rejects unsized type-args at typeck (E0269) before mono ever runs, so `ox_size_of`'s layout-known check fires `unreachable!()` rather than a user-facing diagnostic. Lights up when `?Sized`, niche-occupied layouts, or dynamically-sized ADTs land.

## Out of scope (forever-ish)

- Stable cross-compilation `size_of` (the target-tuple machinery).

## Resolved decisions

(Replaces the prior "Open questions for spec iteration" section.)

1. **Declaration site of intrinsics**: stdlib source declaration in `stdlib/intrinsics.ox`. The HIR scanner relaxes E0209 `BodylessFnOutsideExtern` for fns matching the file-gate + name-allowlist combo specced in §Intrinsic recognition. Compiler-synthesized intrinsics (zero source presence) were considered but rejected — keeping the surface visible in source means users and contributors can read intrinsics.ox to see what's available.
2. **`align_of` use sites in v0**: keep the helper. No user-callable `ox_align_of` — internal-only this round. `align_of` is part of the layout vocabulary and the size-of code already walks the same field tree; the cost of dropping and re-adding later exceeds the cost of a tiny dead helper today.
3. **Naming convention**: every Oxide-source identifier we author in our own bundled stdlib carries the `ox_` prefix (`ox_transmute`, `ox_size_of`, `ox_alloc`, `ox_alloc_zeroed`, `ox_dealloc`, `ox_realloc`). C bindings keep their C names. Internal Rust helpers carry no prefix.

## Future intrinsic surface

The v0 mechanism (filename gate + name allowlist) is deliberately ceremony-free. When the intrinsic set grows past ~3–4 entries, or when an intrinsic legitimately needs to live outside `intrinsics.ox`, migrate to a source-visible marker — the cleanest options are `extern "oxide-intrinsic" { ... }` blocks (mirrors Rust's `extern "rust-intrinsic"`, reuses existing extern-block parsing) or a dedicated `intrinsic fn` keyword. Either drop-in replacement keeps the `Intrinsic` enum and the codegen dispatch — only the recognition site in `src/hir/lower/scanner.rs` changes.

## Bundled `mem.ox`

Companion stdlib file shipped alongside `intrinsics.ox`. Hosts typed wrappers around the C allocator. All exposed names carry the `ox_` prefix — no collisions with the C symbols imported from `stdlib.ox`.

| mem.ox API | Wraps | Notes |
|---|---|---|
| `ox_alloc<T>() -> *mut T` | `malloc(ox_size_of::<T>())` + `ox_transmute` | one-element allocation |
| `ox_alloc_zeroed<T>() -> *mut T` | `calloc(1, ox_size_of::<T>())` + `ox_transmute` | zero-initialized |
| `ox_dealloc<T>(p: *mut T)` | `free(ox_transmute(p))` | accepts any `*mut T` returned by alloc/realloc wrappers |
| `ox_realloc<T>(p: *mut T, n: usize) -> *mut T` | `realloc(ox_transmute(p), n * ox_size_of::<T>())` + `ox_transmute` | resize to `n` elements (count, not bytes) |

mem.ox content:

```rust
// stdlib/mem.ox
import "intrinsics.ox";   // ox_transmute, ox_size_of
import "stdlib.ox";       // malloc, calloc, realloc, free (C names)

fn ox_alloc<T>() -> *mut T {
    ox_transmute(malloc(ox_size_of::<T>()))
}

fn ox_alloc_zeroed<T>() -> *mut T {
    ox_transmute(calloc(1, ox_size_of::<T>()))
}

fn ox_dealloc<T>(p: *mut T) {
    free(ox_transmute(p))
}

fn ox_realloc<T>(p: *mut T, n: usize) -> *mut T {
    ox_transmute(realloc(ox_transmute(p), n * ox_size_of::<T>()))
}
```

Imports are non-transitive (spec/14): a user who writes `import "mem.ox";` gets the four typed wrappers but NOT direct `malloc` / `calloc` / `realloc` / `free` / `ox_transmute` / `ox_size_of`. mem.ox has no special compiler treatment — it's a regular stdlib file. The `ox_` prefix lets the typed wrappers and the C imports coexist cleanly inside mem.ox without name collisions.

**Soundness of `malloc`-backed allocation**: every v0 type has `align_of ≤ 8` (primitives top out at 8 bytes, ADTs use max-field alignment, no `repr(align)`); `malloc` returns ≥ `max_align_t` (16 bytes on x86_64/aarch64); strictly sufficient. When extended alignment lands (SIMD, `repr(align(N))`), mem.ox switches its raw allocators to `aligned_alloc(align_of::<T>(), size_of::<T>())` — `malloc` alone becomes unsound for T with `align_of > 16`.

## Worked examples

```rust
// The motivating case: ox_alloc<T> from mem.ox, body unfolded.
fn ox_alloc<T>() -> *mut T {
    // size_of(*mut u8) == size_of(*mut T) == 8 — passes the per-instance E0276 check
    ox_transmute(malloc(ox_size_of::<T>()))
}
```

```rust
// Aggregate transmute — Rust supports this, we do too.
struct S { a: i32, b: i32 }
fn s_to_bytes(s: S) -> [u8; 8] { ox_transmute(s) }
```

```rust
// Same-width int reinterpret — no LLVM op needed (typeck reinterprets bits).
fn u32_as_i32(x: u32) -> i32 { ox_transmute(x) }
```

```rust
// ox_size_of in a runtime expression.
fn n_i32_bytes(n: usize) -> usize { n * ox_size_of::<i32>() }   // ✓ → n * 4 at runtime
```
