# Generic functions

## Requirements

Oxide today has no way to write a function that operates over an unknown type. Every concrete-typed allocation that wants `*mut T` for user-defined `T` must either go through the fixed `*mut u8` from `malloc` and convert via `as` (rejected by spec/12) or via a `transmute` (introduced in spec/17). Both routes need a helper that takes a type *parameter*:

```rust
fn alloc<T>(size: usize) -> *mut T { transmute(malloc(size)) }
let m: *mut HashMap = alloc(24);
```

Without generic functions, every typed-allocation, every typed-buffer reinterpretation, every container would re-implement the same cast/transmute escape inline. Solving it once via generics is the principled path.

This spec adds **generic functions only**. Generic structs/enums, trait bounds, and `where` clauses are explicitly out of scope for v0.

## Subset-of-Rust constraint

Anything we accept must parse and mean the same thing in Rust. Specifically:

- `fn name<T, U>(args) -> ret { body }` — same syntax, same scoping rule (params in scope inside the body's type positions only).
- `name::<T, U>(args)` — turbofish at call sites.
- Inferred type-args via the existing Infer machinery; equivalent to Rust's elided type-args.
- Monomorphization semantics: each distinct `(fn, type-args)` produces an independent instance.

Where we differ, we accept *fewer* programs:

- No trait bounds. `T: Display` is a parse error.
- No `where` clauses.
- No generic ADTs (`struct Vec<T>` is a parse error).
- Implicit bound is `T: Sized` only (no `Copy`, since oxide has no Copy infrastructure). `T = [U]` (unsized array) is rejected at the call site.

## Acceptance

```rust
fn id<T>(x: T) -> T { x }                        // ✓ identity
fn alloc<T>(size: usize) -> *mut T { ... }       // ✓ generic over T
let p: *mut i32 = alloc(4);                      // ✓ T inferred from let-binding
let q = alloc::<i32>(4);                         // ✓ T given via turbofish
```

```rust
fn alloc<T: Sized>(size: usize) -> *mut T { ... }   // ✗ trait bounds rejected (parse)
struct Vec<T> { ... }                                // ✗ generic ADTs rejected (parse)
fn main() { let p = alloc(4); }                      // ✗ E0256: T unconstrained (existing CannotInfer)
fn main() { alloc::<i32, u8>(4); }                   // ✗ E0275: arity mismatch
fn marker<T>() {}
fn main() { marker::<[i32]>() }                      // ✗ E0269: T = [i32] is unsized (Sized obligation)
```

## Position in the pipeline

A new monomorphization pass slots **between typeck and codegen**:

```
parse → hir → typeck → mono → codegen
```

The mono pass reads `HirProgram` + `TypeckResults`, walks reachable instances starting from non-generic entry points, and emits a `MonoResults` table that codegen consumes per-instance.

## Surface syntax

- **Declaration**: `fn name<T, U, ...>(params) -> ret { body }`. Type params come between the fn name and the param list. Empty list `<>` is accepted (matches Rust) and is semantically equivalent to a non-generic fn — `fn name<>() {}` parses and behaves identically to `fn name() {}`. Same for turbofish: `name::<>(args)` is accepted and behaves identically to `name(args)`.
- **Turbofish**: `name::<T, U>(args)` at call sites. The `::` is mandatory (matches Rust); `name<T, U>(args)` is parsed as a comparison expression.
- **Type-param uses**: inside the body's type positions only, by name. No nested generic scopes.

The parser is uniform — it accepts `<T, U>` on every fn item including extern declarations. Semantic rejection of generic externs happens at HIR (see Errors).

## HIR

- New newtype `TyParamId` (per `define_index_type!` convention).
- New arena on `HirProgram`: `IndexVec<TyParamId, TyParamInfo>`, where `TyParamInfo { owner: FnId, idx_in_owner: u32, name: String, span: Span }`.
- `HirFn.generic_params: Vec<TyParamId>` (declaration order).
- `HirTyKind::Param(TyParamId)` — new variant.
- `HirExprKind::Call.type_args: Vec<HirTy>` — new field, empty for non-turbofish calls.

Resolution: when `HirTyKind::Named(name)` is being lowered inside a fn whose `generic_params` includes `name`, lower to `HirTyKind::Param(tpid)` instead of falling through to the ADT/prim lookup. Resolution lives in `src/hir/lower/ty.rs`.

**Generic-extern recovery**: when the prescan/lower pass encounters `extern "C" fn f<T>(...)`, it emits `HirError::GenericExternFn` but leaves the fn signature *intact* in `HirProgram` — `f.generic_params`, `f.is_extern`, and any `Param` references in `f.params` / `f.ret_ty` are preserved unchanged. Clearing `generic_params` would orphan the `Param` refs in the signature (typeck would have nowhere to resolve them), creating cascading errors. The post-HIR invariant we *do* enforce by error reporting (not by signature mutation) is: clean HIR → `is_extern ⇒ generic_params.is_empty()`. The driver short-circuits before invoking typeck/mono if any `HirError` was emitted, so downstream phases never observe the contradictory state.

## Typeck rules

- New `TyKind::Param(TyParamId)` (same id type as HIR).
- New `FnSig.generic_params: Vec<TyParamId>`.
- `resolve_ty` handles `HirTyKind::Param(tpid) → TyKind::Param(tpid)`.
- **Param-in-`expr_tys` contract**: `TyKind::Param(tpid)` is permitted to appear in `TypeckResults.expr_tys[eid]` and `TypeckResults.local_tys[lid]` for any expression/local inside a *generic* fn's body. `resolve_fully` deliberately doesn't substitute Param leaves (it only resolves Infer); substitution is mono's responsibility (Phase 2 body walk via the local subst map). Other consumers of `expr_tys` (diagnostic renderers, future analyses) must either substitute Param via a known subst map or render Param as the source name of the corresponding type-param (e.g., `hir.ty_params[tpid].name`) for human display. This is the dual to "Infer may not appear in `expr_tys` post-finalize" — Param is the generic-fn-body counterpart of what Infer is for the inference-in-flight phase.
- `infer_call` on a generic callee:
  1. Allocate one fresh `InferId` per `FnSig.generic_params` entry; collect into `fresh: Vec<TyId>`.
  2. If turbofish args are present, equate each fresh Infer with the user-supplied resolved type *first*. Arity mismatch → E0275.
  3. **For each fresh type-arg, push `Obligation::Sized { ty, pos: SizedPos::TypeArg, span: call_span }` onto `Inferer.obligations`.** This enforces the implicit `T: Sized` bound at the call site. Body-phase: at finalize, the obligation resolves via `resolve_fully`; if the resolved type is `Array(_, None)` the existing `discharge_sized` emits E0269 `UnsizedArrayAsValue`. This fills a pre-existing gap — decl-time `Sized` checks (`decl.rs:199`) only see `Param(_)` for generic-fn parameters and pass through; the call-site is the only point where the substitution becomes concrete and the unsized case can actually be detected.
  4. Build the substitution `subst: HashMap<TyParamId, TyId>` inline as `zip(callee_sig.generic_params, fresh).collect()`, then call `substitute_ty` on each of `callee_sig.params` and `callee_sig.ret`. Unify the substituted types with arg/expected-return types.
  5. Record the `(HExprId, fresh: Vec<TyId>)` pair on a per-fn buffer `Inferer.call_type_args`. The `Vec<TyId>` may carry unresolved Infer vars at this point.
- In `Checker::finalize` for this fn (same place that resolves `expr_tys` and `local_tys` via `resolve_fully`, while the Inferer is still alive): walk `inf.call_type_args`, run `resolve_fully` on every TyId, insert the resolved tuples into module-wide `TypeckResults.call_type_args`. The transient buffer dies with the Inferer.
- **Unconstrained type params** are caught by the *existing* `CannotInfer` mechanism (E0256). The fresh Infer vars minted in step 1 are normal Bindings — if no unification touched them by the end of the body, finalize emits E0256 pointing at `creation_span` (the call site we passed when minting the var). No new error code; no separate walk over `call_type_args`. The `subst` map is local to `infer_call` and dies at the end of the call.
- **Unused type params** (`fn f<T>() {}` where T appears in neither params nor return) are accepted at typeck without warning. Oxide's typeck is permissive at declaration sites and strict at use sites; the error fires naturally at call sites via E0256 — a non-turbofish call has nothing to bind the fresh Infer to.

A new low-level helper `substitute_ty(tys: &mut TyArena, ty: TyId, subst: &HashMap<TyParamId, TyId>) -> TyId` mirrors the existing `resolve_fully` (`check.rs:571–592`) — same recursion shape; leaf case swaps `Infer`-via-bindings for `Param`-via-map. This is the *primitive* for one-type substitution; the higher-level "create an Instance for this (fid, args) pair" operation lives in mono as `instantiate` (see below). Typeck doesn't call `instantiate` because, at typeck time, the args are still Infer vars — `Instance`s require concrete types.

## Monomorphization

New module `src/mono/mod.rs`. New newtype `InstId` (per `define_index_type!`).

The pass is a **fixpoint walk** over the call graph: instantiation cascades — instantiating `f<i32>` whose body calls `g<T>` triggers `instantiate(g, [i32])`, which may in turn cascade further. The walk drains a work queue until no new instances are discovered. **Dedup is required for termination, not just performance**: same-args recursion (`f<T>() { f<T>() }`) only terminates because the `(FnId, Vec<TyId>)` lookup short-circuits the second visit.

### Data model

**Single-arena contract** (load-bearing for dedup correctness): one `TyArena` exists for the whole compilation. It is created by typeck and lives on `TypeckResults.tys`. Mono borrows it mutably for the duration of the pass — `substitute_ty` interns new types into it as it walks Param-bearing trees (e.g. `*mut T` substituted with `T = i32` produces `*mut i32`, which may be a fresh interning if no prior code referenced `*mut i32`). All TyIds inside `MonoResults` (in `Instance.type_args` / `Instance.params` / `Instance.ret`, in dedup keys, in `call_targets`) reference this single arena; codegen continues to resolve them through `typeck.tys` post-mono. **No second arena is created at any phase.** This invariant is what makes the dedup key `(FnId, Vec<TyId>)` work — structurally identical types interned through one hash-consed arena have equal TyIds, so same-args recursion terminates and `alloc<i32>` called from 17 different sites produces exactly one Instance.

Following the codebase's process-struct + result-struct convention (`Lowerer → HirProgram` at `src/hir/lower/lowerer.rs:135`; `Checker → TypeckResults` at `src/typeck/check.rs:413`):

```rust
/// Public output. Codegen consumes this.
#[derive(Clone, Debug)]
pub struct MonoResults {
    pub instances: IndexVec<InstId, Instance>,
    pub call_targets: HashMap<HExprId, InstId>,
}

pub struct Instance {
    pub fid: FnId,
    pub type_args: Vec<TyId>,                  // parallel to callee FnSig.generic_params; also the dedup key
    pub params: Vec<TyId>,                     // substituted parameter types — pre-baked for codegen Phase 1
    pub ret: TyId,                             // substituted return type — pre-baked for codegen Phase 1
    pub mangled: String,                       // LLVM symbol — owned here so InstId→name can never desync
    pub depth: u32,                            // cascade depth from nearest entry point
    pub origin: InstanceOrigin,
}

pub enum InstanceOrigin {
    /// Seeded directly (e.g., `main`, any reachable non-generic fn).
    EntryPoint,
    /// Discovered while walking `parent`'s body at this call site.
    InstantiatedAt { parent: InstId, call_span: Span },
}

/// Top-level entry; mirrors `check(hir) -> (TypeckResults, Vec<TypeError>)`.
/// `typeck` is taken by `&mut` because `substitute_ty` interns new types
/// into `typeck.tys` as it walks Param-bearing trees — see TyArena
/// contract below.
pub fn monomorphize(
    hir: &HirProgram,
    typeck: &mut TypeckResults,
) -> (MonoResults, Vec<MonoError>) {
    let mut cx = MonoCtx::new(hir, typeck);
    cx.seed_entry_points();
    while let Some(inst_id) = cx.work_queue.pop_front() {
        cx.walk_body(inst_id);
    }
    cx.finish()
}

/// Process struct. Owns both result-bound state and transient state;
/// `finish(self)` steals the durable fields.
struct MonoCtx<'a> {
    hir: &'a HirProgram,
    typeck: &'a mut TypeckResults,             // mut because substitute_ty interns into typeck.tys

    // ---- result-bound (stolen at finish) ----
    instances: IndexVec<InstId, Instance>,
    call_targets: HashMap<HExprId, InstId>,

    // ---- transient (dropped at finish) ----
    /// Dedup index: same (FnId, type_args) → same InstId. Required for
    /// termination — without it, recursive generic fns loop forever
    /// even when type_args repeat.
    instance_map: HashMap<(FnId, Vec<TyId>), InstId>,
    /// Bodies awaiting walk. Each instance walked exactly once.
    work_queue: VecDeque<InstId>,
    /// Hard cap on cascade depth from any entry point. Default 256
    /// (matches rustc's `recursion_limit`). Configurable later.
    depth_limit: u32,

    // ---- errors (returned alongside MonoResults) ----
    errors: Vec<MonoError>,
}

impl<'a> MonoCtx<'a> {
    fn finish(self) -> (MonoResults, Vec<MonoError>) {
        (
            MonoResults {
                instances: self.instances,
                call_targets: self.call_targets,
            },
            self.errors,
        )
    }
}
```

### `instantiate` — the canonical creation point

```rust
fn instantiate(
    cx: &mut MonoCtx,
    fid: FnId,
    type_args: Vec<TyId>,
    origin: InstanceOrigin,
) -> Option<InstId> {
    if let Some(&id) = cx.instance_map.get(&(fid, type_args.clone())) {
        return Some(id);                        // dedup → terminates the cascade
    }
    let depth = match &origin {
        InstanceOrigin::EntryPoint => 0,
        InstanceOrigin::InstantiatedAt { parent, .. } => cx.instances[*parent].depth + 1,
    };
    if depth > cx.depth_limit {
        // Only InstantiatedAt can reach this branch — EntryPoint's depth
        // is 0 by construction and 0 > limit (limit ≥ 1) is false.
        // The match makes that invariant explicit at the call site rather
        // than hiding it behind an `origin.span()` helper that would have
        // to lie about EntryPoint.
        let span = match &origin {
            InstanceOrigin::InstantiatedAt { call_span, .. } => call_span.clone(),
            InstanceOrigin::EntryPoint => unreachable!(
                "EntryPoint cannot exceed depth_limit (depth = 0)"
            ),
        };
        cx.errors.push(MonoError::DivergentMonomorphization {
            chain: cx.walk_origin_chain(&origin),
            span,
        });
        return None;                            // failure admitted in the type
    }
    let sig = cx.typeck.fn_sig(fid);
    // Build a local subst map to pre-substitute the signature. The map
    // dies at the end of this call — only the substituted result lives
    // on `Instance`. Codegen Phase 2 rebuilds an equivalent map locally
    // when it needs to substitute body-internal expression types.
    let subst: HashMap<TyParamId, TyId> =
        sig.generic_params.iter().copied().zip(type_args.iter().copied()).collect();
    let params: Vec<TyId> = sig.params.iter()
        .map(|&p| substitute_ty(&mut cx.typeck.tys, p, &subst))
        .collect();
    let ret = substitute_ty(&mut cx.typeck.tys, sig.ret, &subst);
    let mangled = format_inst(cx.hir, fid, &type_args, &cx.typeck.tys, NameStyle::Mangle);
    let inst = Instance { fid, type_args: type_args.clone(), params, ret, mangled, depth, origin };
    let id = cx.instances.push(inst);
    cx.instance_map.insert((fid, type_args), id);
    cx.work_queue.push_back(id);
    Some(id)
}
```

The caller never builds substitution maps and never touches `instance_map` / `work_queue`. One call handles dedup, depth-overflow detection, signature pre-substitution, mangling, and queue scheduling. The transient `subst` lives only inside `instantiate`; it is *not* stored on `Instance` because:

- Codegen Phase 1 (declare) reads `instance.params` and `instance.ret` directly — no substitution needed.
- Codegen Phase 2 (define) does need a substitution map for body-internal expression types, but rebuilds it locally per body from `(callee_sig.generic_params, instance.type_args)` — cheap, scoped to one body walk.
- Storing `subst` on every `Instance` would duplicate information already implied by `(fid, type_args)`.

This keeps the `Instance` lean and the responsibility clear: `Instance.params` / `Instance.ret` are mono's *output*; the HashMap is mono's *machinery*, regenerated when needed.

### Algorithm

1. **Seed** (`seed_entry_points`): for each non-generic fn with a body (`main` and any reachable non-generic fn), call `instantiate(cx, fid, vec![], InstanceOrigin::EntryPoint)`.
2. **Drain** (`walk_body`): pop an `InstId` from `work_queue`. Build a local `subst` map for this instance from `(callee_sig.generic_params, instance.type_args)`. Walk the body's call exprs. For each call to a generic fn:
   a. **Invariant check**: assert `!hir.fns[callee_fid].is_extern` (or equivalently, `callee_sig.generic_params.is_empty() || !is_extern`). The contract from §HIR ("clean HIR → `is_extern ⇒ generic_params.is_empty()`") guarantees this; if it fails here, the driver let dirty HIR through, and mono panics. No graceful recovery.
   b. Read `TypeckResults.call_type_args[call_eid]` (the typeck-recorded type-args for this call).
   c. Substitute the local `subst` into those type-args via `substitute_ty` (handles `T` → concrete chains in cascading calls).
   d. Call `instantiate(cx, callee_fid, resolved_args, InstanceOrigin::InstantiatedAt { parent: inst_id, call_span })`.
   e. If `instantiate` returned `Some(id)`, record `call_targets[call_eid] = id`. If it returned `None` (depth-overflow; error already pushed to `cx.errors`), skip the insertion and continue with the next call — we want to surface as many errors as possible per build, not bail on the first one.

For calls to **non-generic** callees (extern or otherwise), `walk_body` does not invoke `instantiate` and does not insert into `call_targets`. Codegen's `emit_call` falls back to `fn_decls[fid]` for these — see §Codegen.
3. Repeat until `work_queue` is empty.
4. **Mangling** runs at `instantiate` time and stores the result on `Instance.mangled` — see §Mangling below.

### Naming (mangle and display)

A single function `format_inst` produces the string form of an instance. It walks `type_args` recursively and dispatches on a `NameStyle` parameter:

```rust
pub enum NameStyle {
    /// LLVM-symbol form. Injective, deterministic, parseable in principle.
    /// `f__$mutptr_$i32`. Stored on `Instance.mangled`.
    Mangle,
    /// Human-readable form for diagnostics. `f<*mut i32>`.
    /// Used by E0278's chain rendering and any other user-facing
    /// instance reference.
    Display,
}

pub fn format_inst(
    hir: &HirProgram,
    fid: FnId,
    type_args: &[TyId],
    tys: &TyArena,
    style: NameStyle,
) -> String;
```

`Instance.mangled` is computed at `instantiate` time as `format_inst(_, _, _, _, NameStyle::Mangle)`. The diagnostic emitter calls the same fn with `NameStyle::Display` when rendering E0278's breadcrumb chain.

Both styles share three properties (the mangle is the strict version):

- **Injective** — distinct Instances produce distinct strings (only checked for `Mangle`; `Display` doesn't need to be injective since LLVM doesn't see it, but it happens to be).
- **Deterministic** — same source → same string across builds and machines (snapshot stability).
- **Parseable in principle** — context-free; given a name, structure can be recovered. (Only applies meaningfully to `Mangle`.)

#### Encoding (both styles)

**Top-level format**:

- **Non-generic fns are not mangled.** The LLVM symbol is the source name verbatim — `main` stays `main`, helpers keep their declared identifier. `format_inst` short-circuits on `type_args.is_empty()` and returns the source name unchanged for both styles. This guarantees the linker finds `main` where it expects, and debuggers/profilers/`nm` show source names for any symbol that didn't require monomorphization.
- **Generic fn instances**:
  - `Mangle`: `<source_name>__<arg1>__<arg2>__...` with `__` as the separator.
  - `Display`: `<source_name><<arg1>, <arg2>, ...>` with `, ` as the separator and angle brackets around the list.
- **Extern fns**: never appear in `MonoResults.instances`. Codegen emits them directly via FnId → source name lookup.

The Mangle decoration *only* appears on fns that genuinely needed monomorphization. A program with no generics produces an LLVM module whose symbol table is identical to a hand-written equivalent — no `__$` suffixes anywhere.

**Type encoding**, recursive — both styles share the same recursion shape, only the leaf/builder formatting differs:

| `TyKind` | `Mangle` | `Display` |
|----------|----------|-----------|
| `Prim(i8)` … `Prim(usize)` … `Prim(bool)` | `$i8`, `$u8`, …, `$usize`, `$bool` | `i8`, `u8`, …, `usize`, `bool` |
| `Unit` | `$unit` | `()` |
| `Never` | `$never` | `!` |
| `Ptr(T, Const)` | `$constptr_<rec(T)>` | `*const <rec(T)>` |
| `Ptr(T, Mut)` | `$mutptr_<rec(T)>` | `*mut <rec(T)>` |
| `Array(T, Some(N))` | `$array_<rec(T)>_$n<N>` | `[<rec(T)>; <N>]` |
| `Adt(aid)` | bare source name of the ADT | bare source name of the ADT |
| `Error` | `$error` | `<error>` |
| `Param(_)` / `Infer(_)` / `Fn(..)` / `Array(_, None)` | `unreachable!` — none reach mono on a successful path | `unreachable!` |

`rec(T)` means recursive call to `format_inst` on the inner type with the same style.

Every `$<prefix>_` (Mangle) introduces a structural type-builder with known arity: 1-ary for `$constptr` / `$mutptr`, 2-ary for `$array` (type then integer). The arity-known property is what makes the Mangle encoding context-free.

**Why integers need the `$n` prefix in Mangle**: Oxide identifiers match `[a-zA-Z_][a-zA-Z0-9_]*`, so `Foo_10` is a legal ADT name. Without an integer marker, `$array_Foo_10` could parse as either `[Foo; 10]` or `[Foo_10; <missing length>]`. The `$n` prefix removes the ambiguity (`$array_Foo_$n10` vs `$array_Foo_10_$n0` for the two cases). The Display form uses Rust syntax (`[Foo; 10]`) which is unambiguous via the `;` delimiter.

#### Examples

| Instance | `Mangle` | `Display` |
|----------|----------|-----------|
| `id<i32>` | `id__$i32` | `id<i32>` |
| `id<*mut S>` | `id__$mutptr_S` | `id<*mut S>` |
| `f<i32, *const S>` | `f__$i32__$constptr_S` | `f<i32, *const S>` |
| `g<*const *mut S>` | `g__$constptr_$mutptr_S` | `g<*const *mut S>` |
| `h<[S; 10]>` | `h__$array_S_$n10` | `h<[S; 10]>` |
| `nested<*mut [i32; 4]>` | `nested__$mutptr_$array_$i32_$n4` | `nested<*mut [i32; 4]>` |
| `main` (no type-args) | `main` | `main` |

#### Discrimination of `*const` vs `*mut`

Both styles distinguish `*const T` from `*mut T` even though LLVM's pointer type is opaque (their LLVM IR is identical). This is deliberate: the name is a faithful 1:1 encoding of the *source-level* instantiation, not of the lowered LLVM type. Benefits:

- Snapshot stability without coupling tests to LLVM type identity.
- Diagnostics can report the exact instantiation a symbol corresponds to.
- Future-proofs against const/mut pointers being treated differently at codegen later.

#### v0 limitations

- **Cross-module ADT name collisions**: two modules each defining `struct S` produce identical strings (`f__S` / `f<S>`). Not a problem for single-file or trivially-pathed programs; revisit when modules nest deeper. Future fix is to encode AdtId or fully-qualified path.
- **Symbol length**: deeply-nested types produce long Mangle names. No truncation in v0; if names exceed linker limits (~64KB on ELF, more elsewhere), revisit with a hash-suffix scheme.

### Termination and depth limit

- Same-args recursion (`f<T>() { f<T>() }`): dedup catches the second visit; the work queue empties.
- Different-args recursion that converges (`f<T>() { f<U>() }` for some bounded set of `T`/`U`): also terminates via dedup once the orbit closes.
- Different-args recursion that diverges (`f<T>() { f<*mut T>() }`): each cascade step mints a new `(fid, args)` pair, dedup never fires, depth grows monotonically. The depth check at `instantiate` catches it at depth 256 (default) and emits `E0278 DivergentMonomorphization` with a breadcrumb chain walked back via `Instance.origin` parent pointers.

The limit is **per cascade chain**, not global instance count. A program with 10,000 distinct generic instantiations across many independent chains compiles fine; only a single chain reaching depth 256 triggers the error. This matches Rust's `#![recursion_limit]` semantics.

## Codegen

**Pipeline contract**: codegen runs only on a clean `MonoResults`. The driver inspects the `Vec<MonoError>` returned by `monomorphize(...)` and aborts before codegen if any errors exist. Codegen does not handle mono failures — it trusts its input and panics on contract violations.

- Phase 1 (declare): walk `MonoResults.instances` instead of `hir.fns`. Use `instances[inst].mangled` as the LLVM symbol. Build the LLVM function type directly from `instance.params` / `instance.ret` via `lower_fn_type` — no substitution needed at this phase since mono already pre-baked them.
- Phase 2 (define): for each instance with a body, build a local `subst: HashMap<TyParamId, TyId>` from `(callee_sig.generic_params, instance.type_args)` once per body. For every `expr_tys[eid]` lookup, run `substitute_ty(_, expr_ty, &subst)` to replace `Param(_)` leaves with concrete TyIds. The map is scoped to the body walk and dropped at its end.
- `emit_call` lookup is split: if `mono.call_targets.get(call_eid)` returns `Some(inst_id)`, codegen uses the per-instance LLVM symbol (the generic-call path); otherwise it falls back to `self.fn_decls[fid]` (the non-generic path — extern fns, non-generic non-extern calls). The split is by `call_targets` membership, not by inspecting `is_extern` at the call site.
- Extern fns: emitted exactly once, never generic, no mangling. Source name preserved (`malloc` stays `malloc`). Calls to externs always take the `fn_decls` fallback path above.
- Reuse: dedup happens entirely at the mono level via the `(FnId, Vec<TyId>)` key. Codegen sees one symbol per `Instance`. Structurally-identical-but-distinct instances (e.g., `alloc<i32>` and `alloc<u32>`, both lowering to a 4-byte memcpy) emit two symbols in v0; LLVM's `mergefunc` pass at optimization time is the appropriate layer to collapse them, not us.
- Depth-overflow failures don't produce sentinel instances. `instantiate` returns `Option<InstId>`; on overflow it pushes a `MonoError::DivergentMonomorphization` and returns `None`. `walk_body` then skips the `call_targets` insertion for that call. Because codegen runs only on clean mono output (no `MonoError` emitted), every `call_targets` lookup it performs is guaranteed to hit — codegen never observes a missing entry, and there's no sentinel `Instance` to maintain.

## Errors

| Code | Variant | Owner | When |
|------|---------|-------|------|
| E0275 | `GenericArityMismatch { expected: usize, found: usize, span: Span }` | typeck | Turbofish supplies the wrong number of type-args. |
| E0278 | `DivergentMonomorphization { chain: Vec<(FnId, Vec<TyId>, Span)>, span: Span }` | mono | A cascade chain exceeded `depth_limit` (default 256). The `chain` is walked back via `Instance.origin` parent pointers and rendered as a breadcrumb list in the diagnostic. The chain entries are raw `(FnId, Vec<TyId>, Span)` triples; the diagnostic emitter formats each via `format_inst(_, _, _, _, NameStyle::Display)` — the same function used for `Instance.mangled`, just with the human-readable style. |
| E0XXX | `GenericExternFn { name: String, span: Span }` | hir | An `extern "C"` fn declaration carries `<T, U, ...>`. HIR-side error; not E0NNN-numbered (HIR errors don't follow that scheme — see Open Q3). |

Diagnostic format for E0278 (modeled on rustc E0275):

```
error[E0278]: monomorphization depth exceeded (limit: 256)
  --> src/main.ox:7:5
   |
 7 |     f::<*mut T>()
   |     ^^^^^^^^^^^^^
   |
   = note: instantiation chain (root → tip):
   = note:   main                       (depth 0)
   = note:   f<i32>                     (depth 1)
   = note:   f<*mut i32>                (depth 2)
   = note:   ...
   = note:   f<*mut^256 i32>            (depth 256)
   = note: each step discovers a strictly-larger type — dedup never converges.
   = note: restructure to use a fixed type parameter or factor through a non-generic helper.
```

## Out of scope (this round)

- Generic structs / enums.
- Trait bounds, `where` clauses.
- `sizeof<T>()` / `alignof<T>()` as user expressions (see spec/17 for the internal helpers).
- `unsafe { }` blocks.
- Variance, lifetimes, const generics.

## Out of scope (forever-ish)

- Higher-kinded types.
- Specialization.

## Worked examples

```rust
// alloc<T> — the motivating example. spec/17 defines `transmute`.
fn alloc<T>(size: usize) -> *mut T {
    transmute(malloc(size))
}

fn dealloc<T>(p: *mut T) {
    free(transmute(p))
}

fn main() -> i32 {
    let p: *mut i32 = alloc(4);     // T=i32 inferred from let-binding
    *p = 42;
    dealloc(p);
    0
}
```

```rust
// id<T> — pure identity, exercises Param threading without intrinsics.
fn id<T>(x: T) -> T { x }

fn main() -> i32 {
    let n = id::<i32>(7);            // T=i32 via turbofish
    n
}
```

## Open questions for spec iteration

1. **`transmute` declaration site**: spec/17 will define the intrinsic as a body-less generic. Does it have a source declaration (`fn transmute<Src, Dst>(x: Src) -> Dst;` in stdlib, requiring HIR to permit body-less non-extern), or is it compiler-known with no source presence? Recommend the latter; spec/17 will commit.
2. **HIR-side error code for `GenericExternFn`**: not a typeck error, lives in `HirError`. HIR errors don't follow the E0NNN scheme — confirm the convention.

---

# Generic structs (extension)

The sections below grow the spec to cover *generic record structs*, e.g.:

```rust
struct LinkedList<T> {
    value: T,
    next: *mut LinkedList<T>,
}

let mut head = LinkedList::<i32> { value: 0, next: null };
```

Every decision the generic-fn portion of this spec already locked down — single TyArena, Param-in-`expr_tys` contract, `substitute_ty` primitive, lean `Instance`, mangling shape, error-but-keep-signature recovery, driver short-circuit, out-of-scope list — is **preserved verbatim**. Where an existing section grows, the change is shown in diff form.

## Requirements (extension)

Once generic fns land, fns can be parametric but every ADT must be monomorphic. Every typed container (linked list, pair, stack, hash map) ends up either copy-pasted per element type or punted to `*mut u8` with manual casts. Solving it generically extends the same `T: Sized`-only model the fn portion already commits to.

This extension adds **generic record structs only**. Generic enums, generic unions, trait bounds, where clauses, HKT, specialization, const generics, variance, and lifetimes remain explicitly out of scope.

## Subset-of-Rust constraint (extension)

Anything we accept must parse and mean the same thing in Rust:

- `struct G<T, U> { f: ty1, g: ty2 }` — same syntax, same scoping rule (params in scope inside the field type positions only).
- `Name<T>` and `Name::<T>` in type positions (e.g., `*mut Name<i32>`, `[Name<u8>; 4]`) — the type grammar has no `<` operator, so no `::` is required.
- `Name::<T> { f: v }` — turbofish on struct literal, mandatory `::` (matches Rust; without `::`, `Name<T>{...}` parses as comparison and fails on the trailing `{`).
- `Name { f: v }` (no turbofish on a generic struct) — accepted when T is inferable from field-value types.
- Implicit bound is `T: Sized` only. `LinkedList<[i32]>` is rejected at the construction site (E0269), same path as `f::<[i32]>(...)`.

Where we differ, we accept *fewer* programs:

- No trait bounds on struct generic params (`struct Foo<T: Display>` is a parse error).
- No `where` clauses on struct decls.
- No generic enums or unions (enum/union themselves are reserved-but-unimplemented).
- No `impl<T>` blocks (no `impl` in oxide yet).

## Acceptance (extension)

```rust
struct LinkedList<T> {                                  // ✓ generic struct
    value: T,
    next: *mut LinkedList<T>,
}

let n: LinkedList<i32> = LinkedList::<i32> {            // ✓ turbofish form
    value: 0,
    next: null,
};

let m = LinkedList { value: 0i32, next: null };         // ✓ T inferred from field

struct Pair<T, U> { l: T, r: U }                        // ✓ multi-param
let p = Pair::<i32, *mut S> { l: 0, r: null };          // ✓
let q = Pair { l: 7i32, r: 'a' };                       // ✓ inferred
```

```rust
struct Vec<T: Sized> { ... }                            // ✗ trait bounds rejected (parse)
struct Wrap<T> where T: Sized { ... }                   // ✗ where clauses rejected (parse)
fn main() { let n = LinkedList { value: null, next: null }; } // ✗ E0256 — T unconstrained
fn main() { LinkedList::<i32, u8> { ... }; }            // ✗ E0275 — arity mismatch
struct B<T> { x: T }
fn main() { B::<[i32]> { x: ??? } }                     // ✗ E0269 — T = [i32] is unsized
```

## Position in the pipeline (no change)

The pipeline stays:

```
parse → hir → typeck → mono → codegen
```

Mono does **not** gain a struct-instantiation step. Generic structs are materialized lazily in codegen, keyed on the post-mono `(AdtId, Vec<TyId>)` pair. Rationale below.

## Surface syntax (extension)

Four cases:

- **Struct decl**: `struct G<T, U, ...> { f1: ty1, ... }`. Generic param list comes between the struct name and the opening brace. Empty list `<>` accepted (matches Rust); `struct G<> { }` parses identically to `struct G { }`. Trailing comma allowed inside `<>`.
- **Type position**: any `Named` type position may carry type-args — `*mut G<T>`, `[G<i32>; 4]`, `G<G<i32>>`. Type-args are recursive; every nesting may carry its own list. No `::` required (the type grammar has no `<` operator).
- **Struct lit turbofish**: `G::<T> { f: v }`. The `::` is mandatory (without it, `G<T>{...}` parses as comparison and fails on the trailing `{`). Empty `G::<>` accepted, equivalent to `G { ... }`.
- **Inferred form**: `G { f: v }`. Typeck mints fresh Infer vars per `G.generic_params` and lets field-value unification pin them — same machinery as fn-call inference. Failure mode: if no field unification pins T, finalize emits E0256 at the lit's span.

The choice to accept inferred form mirrors fn-call inference: same plumbing (fresh Infer + Sized obligation + finalize), same ergonomics, same failure-mode (E0256). No new error code.

## HIR (extension)

### Data model

```rust
pub struct HirAdt {
    pub name: String,
    pub kind: AdtKind,
    pub generic_params: Vec<HTyParamId>,    // NEW — empty for non-generic structs
    pub variants: IndexVec<VariantIdx, HirVariant>,
    pub span: Span,
}

pub enum TyParamOwner {                      // NEW
    Fn(FnId),
    Adt(HAdtId),
}

pub struct TyParamInfo {
    pub owner: TyParamOwner,                 // CHANGED from FnId
    pub idx_in_owner: u32,
    pub name: String,
    pub span: Span,
}

pub enum HirTyKind {
    // ...
    Adt(HAdtId, Vec<HirTy>),                 // CHANGED from Adt(HAdtId)
    // ...
}

pub enum HirExprKind {
    // ...
    StructLit {
        adt: HAdtId,
        type_args: Vec<HirTy>,               // NEW — empty for inferred form
        fields: Vec<HirStructLitField>,
    },
    // ...
}
```

`HirAdt.generic_params` is the source-of-truth for "how many type params does this ADT take" downstream — read by typeck (to size the substitution map), by codegen (to align args with params at field-type substitution).

`TyParamOwner` is an enum because the substitution machinery (`substitute_ty`, `resolve_fully`, mangler) doesn't dispatch on owner kind — it just looks up `pid` in `subst: HashMap<ParamId, TyId>`. A single `ParamId` arena keeps these consumers owner-agnostic. The owner tag is consulted only at diagnostic-rendering sites that want to attribute a Param to its declaring item.

`HirTyKind::Adt(haid, args)` carries the type-args that appear in source. A bare ADT name lowers to `Adt(haid, vec![])`. **Arity invariant**: `args.len() == hir.adts[haid].generic_params.len()` always — there is no "unsaturated ADT" representable as a `HirTyKind`.

`HirExprKind::StructLit.type_args` is empty for the inferred form (`G { ... }`) AND for explicit empty turbofish (`G::<> { ... }`); both collapse at the parser level.

### Multi-pass scanner re-ordering

`prescan_file` (the pass-1 ID-allocation phase of `src/hir/lower/scanner.rs`) currently mints `HTyParamId`s for fns at the `ItemKind::Fn` arm. **The struct arm at `ItemKind::Struct` gains the same shape**: walk `struct_decl.generic_params`, mint an `HTyParamId` per ident with `owner: TyParamOwner::Adt(haid)`, store the resulting `Vec<HTyParamId>` on `HirAdt.generic_params`. No new pass.

`seal_adts` (the pass-4 ADT field-type lowering pass) currently calls `ty::lower_ty(..., TyParamScope::empty(), ...)`. **It now builds a per-ADT `TyParamScope`** from `HirAdt.generic_params` and passes that scope to `lower_ty`, so a field type `value: T` resolves to `Param(htypid)` rather than falling through to `Named("T")`.

The pass-ordering invariant — type-param IDs allocated before any field-type lowering needs them — holds because pass 1 still runs strictly before pass 4. The 4-pass count stays at 4.

### `lower_ty` resolution rule

Today the Named arm has precedence Param > Adt > Named-unresolved (matches Rust's "type params shadow ADT names"). The new shape `ast::TypeKind::Named { name, type_args }` is handled:

- `name` resolves to a Param **and** `type_args` is empty: lower to `HirTyKind::Param(tpid)` (unchanged).
- `name` resolves to a Param **and** `type_args` is non-empty: emit `HirError::TypeParamWithArgs { name, span }`. Recovery: lower to `HirTyKind::Error`. Driver short-circuits before typeck (same shape as `GenericExternFn`).
- `name` resolves to an ADT: recursively lower each `type_arg` under the same `TyParamScope`. Result is `HirTyKind::Adt(haid, lowered_args)`. The recursion ensures `*mut G<T>` inside a generic fn body resolves T → `Param(htypid)` correctly.
- `name` doesn't resolve: fall through to `HirTyKind::Named(name)` (typeck's job to diagnose). Type-args on an unresolved name are dropped silently — primitives have arity 0, and the unresolved diagnostic at typeck takes precedence.

### Generic-extern-analog for ADTs

None needed. `extern "C"` blocks already reject non-fn items at `scanner.rs` (the existing `UnsupportedExternItem` path) before generic_params is even inspected. There is no shape analogous to `is_extern && !generic_params.is_empty()` for structs.

### New HIR error variant

```rust
HirError::TypeParamWithArgs { name: String, span: Span }
```

Fires when source writes `T<X>` for a name `T` already in scope as a generic param. Recovery: position becomes `HirTyKind::Error`, downstream Param-bearing trees that included it eventually reach typeck as `tys.error`. Driver short-circuits before typeck because this is a `HirError`. Not E0NNN-numbered (HIR errors don't follow that scheme).

## Typeck rules (extension)

### Data model

```rust
pub enum TyKind {
    // ...
    Adt(AdtId, Vec<TyId>),                  // CHANGED from Adt(AdtId)
    // ...
}

pub struct AdtDef {
    pub name: String,
    pub kind: AdtKind,
    pub generic_params: Vec<ParamId>,       // NEW — 1:1 with HIR HTyParamId via from_raw
    pub variants: IndexVec<VariantIdx, VariantDef>,
    pub partial: bool,
}
```

### Arity invariant

Every `Adt(aid, args)` interned in the arena satisfies `args.len() == cx.adts[aid].generic_params.len()`. There is no "uninitialized" or "short" form. For non-generic ADTs, `args` is always `vec![]`. For a generic ADT `LinkedList<T>` (arity 1), the two well-formed shapes are:

- **Decl-form**: `Adt(ll_aid, [Param(pid_T)])` — args are the Param leaves of the ADT's own generic_params. Phase 0 pre-interns this. Field types reference it via `*mut LinkedList<T>` lowering to `Ptr(Adt(ll_aid, [Param(pid_T)]), Mut)`.
- **Instantiated form**: `Adt(ll_aid, [i32])` — args are concrete TyIds (or fn-Params if we're inside a generic fn body).

Both shapes are arity-correct; they're distinct TyIds via hash-cons; the only difference is whether their args contain Param leaves. **The "have we substituted yet?" question is answered by Param-occurrence, not args-emptiness.** This preserves the Param-in-`expr_tys` contract verbatim.

### DAG invariant — decl-time cycle detection is sufficient

The TyId arena is a DAG by construction: hash-consing requires every sub-TyId to be interned strictly before its parent, so no `TyKind` can contain a TyId that points back to itself. The only structure re-used by name across distinct TyIds is `AdtId`. Therefore, value-typed cycles can only arise *through ADT identifiers*. Substitution (mono's `substitute_ty`, codegen's lazy materialization) inserts already-interned TyIds for `Param` leaves; never introduces a back-edge.

This separates two phenomena that are sometimes conflated:

- **Cycles** (a value-typed type contains itself) — caught at typeck decl phase by `check_recursive_adts` (see below). Reachability-agnostic: every ADT decl is checked regardless of whether `main` ever reaches it.
- **Divergent monomorphization** (unboundedly many *distinct* acyclic types, e.g., `f<T>() { f::<S<T>>() }`) — caught at mono via the existing `depth_limit` / E0278. Reachability-bounded: only fires for cascade chains rooted at a reachable entry point.

### `check_recursive_adts` — tri-color DFS over substituted types

The non-generic implementation walks each ADT's static decl edges via `walk_ty` over `field.ty`. With generic ADTs that walk over-approximates (`struct B<T> { y: *mut T } / struct A { x: B<A> }` is finite — `B<A>` = `{ y: *mut A }` — but a static walk of A's fields pushes `a` from B's args list without knowing B uses T only via Ptr). The correct check eagerly substitutes at each ADT step and uses a gray-set for cycle detection along the walk path:

```rust
fn check_field(
    cx: &mut Checker,
    start_aid: AdtId,
    ty: TyId,
    span: &Span,
    gray: &mut HashSet<AdtId>,                              // current walk path
    visited: &mut HashSet<(AdtId, Vec<TyId>)>,              // dedup
) {
    match cx.tys.kind(ty).clone() {
        TyKind::Adt(child, args) => {
            if gray.contains(&child) {
                cx.errors.push(TypeError::RecursiveAdt {
                    adt: cx.adts[start_aid].name.clone(),
                    span: span.clone(),
                });
                return;                                     // back-edge — cycle closes here
            }
            let key = (child, args.clone());
            if visited.contains(&key) { return; }
            visited.insert(key);
            gray.insert(child);

            let subst: HashMap<ParamId, TyId> = cx.adts[child].generic_params
                .iter().copied()
                .zip(args.iter().copied())
                .collect();
            for variant in &cx.adts[child].variants.clone() {
                for field in &variant.fields {
                    let substituted = cx.tys.substitute_ty(field.ty, &subst);
                    check_field(cx, start_aid, substituted, &field.span, gray, visited);
                }
            }

            gray.remove(&child);
        }
        TyKind::Ptr(_, _) => {}                             // pointer breaks cycle
        TyKind::Array(elem, Some(_)) => {
            check_field(cx, start_aid, elem, span, gray, visited);
        }
        _ => {}                                             // Prim / Param / Unit / Never / Fn / Infer / Error
    }
}
```

Driven once per ADT as the start. The "occurs check" is the `gray` set — checks occurrences on the *current walk path*, not history. Visited dedups branches.

**Termination guaranteed.** Gray's depth is bounded by `|adts|` — any second encounter of an aid (in any args shape) fires cycle and returns immediately. Visited grows monotonically; each new entry advances the walk by one substituted step, and growing-args divergence is absorbed by the gray-set: `struct S<T> { inner: S<*mut T> }` fires immediately at the first `Adt(s, [Ptr(Param, Mut)])` because `s ∈ gray` already.

**No false positives.** Ptr arms break cycles cleanly, even when the Ptr wraps a Param — because we walk the *substituted* body, not the static decl.

**Cost**: O(|adts|² × |fields per ADT|) calls to `substitute_ty`, each a hash-cons hit in practice. Acceptable at decl phase.

### Phase 0 — declaration form pre-intern

`alloc_partial_adts` (the existing pre-allocation pass) gains:

1. Allocate `ParamId` per ADT generic param via `ParamId::from_raw(htypid.raw())` (1:1 with HIR's IDs, same convention as `AdtId::from_raw(haid.raw())`).
2. Populate `AdtDef.generic_params` with the ParamId list.
3. Pre-intern the **declaration form** `Adt(aid, [Param(p0), Param(p1), ...])` — same TyId that field types reference via `*mut LinkedList<T>` etc., so hash-cons makes them shared.

For a non-generic ADT, the declaration form collapses to `Adt(aid, [])` — identical to today's pre-intern modulo the empty-vec wrapper.

### `resolve_ty` — Adt arm

```rust
HirTyKind::Adt(haid, args) => {
    let aid = AdtId::from_raw(haid.raw());
    let arg_tys: Vec<TyId> = args.iter()
        .map(|a| Self::resolve_ty(tys, errors, a))
        .collect();
    tys.intern(TyKind::Adt(aid, arg_tys))
}
```

`HirTyKind::Param(tpid) → TyKind::Param(pid)` is unchanged — the same translation works for both fn-Param and ADT-Param IDs (single ParamId arena).

### `substitute_ty` — Adt arm

The line that today reads `TyKind::Adt(_) => ty,` (with the comment anticipating this growth) becomes:

```rust
TyKind::Adt(aid, args) => {
    let new_args: Vec<TyId> = args.iter()
        .map(|&a| self.substitute_ty(a, subst))
        .collect();
    self.intern(TyKind::Adt(aid, new_args))
}
```

Param leaves nested inside args (e.g., `Adt(aid, [Param(T)])` substituting `T → i32`) are handled by recursion: `self.substitute_ty(Param(T), subst)` returns `i32`, the new args list is `[i32]`, the outer intern produces `Adt(aid, [i32])`.

### `resolve_fully` — Adt arm

Symmetric — recurse over args and re-intern. This is what carries Infer-resolution through args at finalize: `Adt(aid, [Infer(?T0)])` resolves to `Adt(aid, [i32])` if `?T0` got bound to `i32`.

### `infer_struct_lit` — generic shape

Mirrors `infer_call`'s generic branch:

```rust
fn infer_struct_lit(
    &mut self,
    inf: &mut Inferer,
    lit_eid: HExprId,
    aid: AdtId,
    type_args: &[HirTy],            // turbofish args (empty for inferred form)
    fields: &[HirStructLitField],
    lit_span: &Span,
) -> TyId {
    let adt_def = self.adts[aid].clone();
    let n_type_params = adt_def.generic_params.len();

    // (1) Arity check — same shape as infer_call.
    if !type_args.is_empty() && type_args.len() != n_type_params {
        inf.errors.push(TypeError::GenericArityMismatch {
            expected: n_type_params,
            found: type_args.len(),
            span: lit_span.clone(),
        });
        return self.tys.error;
    }

    // (2) Allocate fresh Infer per ADT generic param.
    let fresh: Vec<TyId> = (0..n_type_params)
        .map(|_| self.fresh_infer(inf, false, lit_span.clone()))
        .collect();

    // (3) Optional turbofish equate.
    for (i, hty) in type_args.iter().enumerate() {
        let user_ty = Self::resolve_ty(&mut self.tys, &mut inf.errors, hty);
        unify::equate(self, inf, fresh[i], user_ty, hty.span.clone());
    }

    // (4) Sized obligation per fresh — implicit T: Sized at construction.
    for &fresh_ty in &fresh {
        inf.obligations.push(Obligation::Sized {
            ty: fresh_ty,
            pos: SizedPos::TypeArg,
            span: lit_span.clone(),
        });
    }

    // (5) Build subst, substitute declared field types, unify with provided values.
    let subst: HashMap<ParamId, TyId> = adt_def.generic_params
        .iter().copied()
        .zip(fresh.iter().copied())
        .collect();
    let declared = &adt_def.variants[VariantIdx::from_raw(0)].fields;
    let mut seen: HashMap<String, Span> = HashMap::new();
    for provided in fields {
        let value_ty = self.infer_expr(inf, provided.value);
        // ... existing duplicate-field check unchanged ...
        match declared.iter().find(|f| f.name == provided.name) {
            Some(field_def) => {
                let target_ty = self.tys.substitute_ty(field_def.ty, &subst);
                unify::subtype(self, inf, value_ty, target_ty, /* span */);
            }
            None => { /* existing unknown-field path */ }
        }
    }
    // ... existing missing-field check ...

    // (6) Return result type. fresh is moved (not cloned); resolve_fully
    //     walks args at finalize and pins them to concrete types.
    self.tys.intern(TyKind::Adt(aid, fresh))
}
```

**No type-arg side-table needed.** Unlike fn calls — where the call expr's type is the *return*, so args have to be stored separately on `Inferer.call_type_args` — struct-lit type-args live directly in the result type `expr_tys[lit_eid] = Adt(aid, args)`. `resolve_fully`'s new Adt arm walks the args at finalize and resolves any Infer leaves to concrete types. Codegen reads `expr_tys[lit_eid]` and gets the concrete instantiation directly.

### `infer_field` — substitution at field access

For `p.value` where `p: *mut LinkedList<i32>`:

```rust
TyKind::Adt(aid, args) => {
    let adt_def = self.adts[aid].clone();
    match adt_def.variants[VariantIdx::from_raw(0)].fields.iter().find(|f| f.name == name) {
        Some(field_def) => {
            if args.is_empty() {
                field_def.ty                                        // non-generic fast path
            } else {
                let subst: HashMap<ParamId, TyId> = adt_def.generic_params
                    .iter().copied()
                    .zip(args.iter().copied())
                    .collect();
                self.tys.substitute_ty(field_def.ty, &subst)
            }
        }
        None => { /* existing NoFieldOnAdt path */ }
    }
}
```

The substitution **must** happen at typeck — leaving `Param(pid_T_Struct)` in `expr_tys` would violate the Param-in-`expr_tys` contract (Param leaves are permitted only when they reference a fn's generic_params, which the body subst at codegen Phase 2 substitutes; struct-Params are not in that subst and would survive into codegen, panicking `lower_ty(_, Param(_))`).

After substitution: inside a non-generic fn, the field type is concrete (e.g., `i32`); inside a generic fn, the field type may be a fn-Param (`Param(pid_T_make)`) — which is fine, it's caught by mono's body subst at codegen Phase 2. Either way, struct-Params don't escape `infer_field`.

### Param-in-`expr_tys` — extends naturally

The fn-only contract said "TyKind::Param may appear in `TypeckResults.expr_tys[eid]` for any expression inside a generic fn's body." With generic structs, `Param` may also appear *nested inside an Adt arg slot* — e.g., `Adt(wrap_aid, [Param(pid_T_make)])` for `Wrap { x: v }` inside `fn make<T>(v: T) -> Wrap<T>`. The contract extends verbatim: `resolve_fully` doesn't substitute Param leaves (only Infer); mono substitutes during codegen Phase 2 walk via the local subst map, now correctly recursing through Adt args via the new arm in `substitute_ty`.

### `call_type_args` — unchanged

The fn-call type-arg side-table keeps its name and shape. Struct lits don't need a parallel table because their type-args live in `expr_tys[lit_eid]` directly.

### Unconstrained type params — E0256 reuse

The fresh Infer vars allocated by `infer_struct_lit` are normal Bindings. If no field unification or turbofish pins them by finalize, the existing `CannotInfer` mechanism emits E0256 at `creation_span` (the `lit_span` we passed when minting the var). Same path as fn calls; no new code path.

## Mono (extension)

**Mono does not gain a struct-instantiation step.** Generic structs are materialized at codegen, keyed by `(AdtId, Vec<TyId>)` post-substitution. Justification:

1. Structs have no body to walk. The mono fixpoint walk drives instantiation by inspecting fn bodies and discovering callees. ADTs only have field types — they don't *invoke* anything.
2. `Adt(aid, args)` is materializable on its own. Given concrete args, codegen has all the information needed: the ADT's `generic_params`, the args, and `substitute_ty` to materialize each field. No prior instantiation is required.
3. Hash-cons dedup at the TyId level already works. Post-mono, every reference to `Adt(aid, [i32])` shares the same TyId. Codegen's lazy cache keyed on `(AdtId, Vec<TyId>)` deduplicates LLVM struct types automatically.
4. No new `Instance`-like struct needed. An `AdtInstance` would duplicate `(aid, args)`; kept off the books for simplicity.

### `Instance.params` / `.ret` — already cover Adt-arg substitution

When a generic fn `f<T>(x: G<T>)` is instantiated with `T = i32`:

- `instantiate(f_fid, [i32], _)` builds `subst = { pid_T → i32 }`.
- `substitute_ty` walks `params: [Adt(g_aid, [Param(T)])]` via the new Adt arm and produces `Adt(g_aid, [i32])`.
- `Instance.params = [Adt(g_aid, [i32])]`.

Codegen Phase 1 reads `Instance.params` directly; lazy ADT materialization handles the LLVM struct type the first time `Adt(g_aid, [i32])` is encountered.

### Cascade walk — only fns drive it

The work queue holds `InstId`s for fn instances. `walk_body` only inspects call expressions. ADT field types are walked indirectly — they appear in fn signatures and local annotations as already-substituted TyIds.

### Divergent monomorphization through structs — same E0278

```rust
struct Wrap<T> { x: T }
fn f<T>() { f::<Wrap<T>>() }
fn main() { f::<i32>() }
```

Cascade: `f<i32>` → `f<Wrap<i32>>` → `f<Wrap<Wrap<i32>>>` → ... Each step mints a new `(FnId, Vec<TyId>)` pair; dedup never fires; the existing `depth_limit = 256` catches at depth 256 and emits E0278 with the breadcrumb chain. **No new error code.** The chain renderer prints `f<Wrap<i32>>` correctly via the new Adt arm (when Display-rendering of Adt with args lands; until then, the chain shows `Adt(<raw>, [...])`).

## Mangling (extension)

The non-generic implementation chose `Adt(aid)` → `$adt<raw>` (AdtId raw integer) over the bare-source-name idea this spec originally floated, to dodge cross-module name collisions cleanly. Generic structs extend the same pattern:

| `TyKind` | Mangle (`mangle_inst`) | Display (`TyArena::render`) |
|----------|------------------------|------------------------------|
| `Adt(aid, [])` | `$adt<raw>` (preserved) | `Adt(<raw>)` (preserved) |
| `Adt(aid, args)` non-empty | `$adt<raw>$<rec(arg1)>$<rec(arg2)>...` | `Adt(<raw>, [<rec(arg1)>, <rec(arg2)>, ...])` |

Boundary: AdtId raw is digits-only, args always start with `$` — unambiguous. Demangling needs the registry for arity (same caveat as the bare-name scheme).

### Worked examples

| Instance / type | Mangle | Display |
|---|---|---|
| `LinkedList<i32>` (LinkedList aid raw = 7) | `$adt7$i32` | `Adt(7, [i32])` |
| `LinkedList<*mut i32>` | `$adt7$mutptr_$i32` | `Adt(7, [*mut i32])` |
| `Pair<LinkedList<i32>, u8>` (Pair aid raw = 8) | `$adt8$adt7$i32$u8` | `Adt(8, [Adt(7, [i32]), u8])` |
| `f<*mut LinkedList<i32>>` (fn instance, top-level) | `f__$mutptr_$adt7$i32` | — |

Properties (injective, deterministic, parseable-with-registry) preserved. Display column is mechanical; human-friendly `LinkedList<i32>` rendering is deferred to a future TypeckResults-aware printer (per the existing TODO at `ty.rs:410–412`). This extension does NOT extend `TyArena::render` to be name-aware.

## Codegen (extension)

### `AdtLlTypes` — keyed by `(AdtId, Vec<TyId>)`

```rust
pub type AdtLlTypes<'ctx> = HashMap<(AdtId, Vec<TyId>), StructType<'ctx>>;
```

Different generic instances need different LLVM struct types — `LinkedList<i32>` has `value: i32`, `LinkedList<u8>` has `value: i8`. Two layouts, two struct types. Keying by `(AdtId, Vec<TyId>)` mirrors mono's `(FnId, Vec<TyId>)` instance key. For non-generic ADTs the key is `(aid, vec![])` — one entry per ADT, identical to the IndexVec behavior modulo the empty-vec wrapper.

### `lower_adt_type` — lazy materialization

```rust
pub fn lower_adt_type<'ctx>(
    ctx: &'ctx Context,
    tcx: &TyArena,                             // interior-mut interning
    cache: &mut AdtLlTypes<'ctx>,
    adts: &IndexVec<AdtId, AdtDef>,
    aid: AdtId,
    args: &[TyId],
) -> StructType<'ctx> {
    let key = (aid, args.to_vec());
    if let Some(&st) = cache.get(&key) {
        return st;
    }
    // Cache-insert opaque struct BEFORE recursing into fields, so
    // self-referential types via pointer (`LinkedList<T>.next: *mut LinkedList<T>`)
    // hit the cache on recursion. Ptr lowers to opaque ptr (doesn't recurse
    // into pointee) so the recursion converges.
    let adt = &adts[aid];
    let display_name = render_adt_instance_name(adt, args, tcx);
    let opaque = ctx.opaque_struct_type(&display_name);
    cache.insert(key.clone(), opaque);

    // Build subst from the ADT's generic_params and the supplied args.
    let subst: HashMap<ParamId, TyId> = adt.generic_params
        .iter().copied()
        .zip(args.iter().copied())
        .collect();

    let fields_ll: Vec<BasicTypeEnum<'ctx>> = adt.variants[VariantIdx::from_raw(0)]
        .fields
        .iter()
        .map(|f| {
            let field_ty_concrete = tcx.substitute_ty(f.ty, &subst);
            lower_ty(ctx, tcx, cache, adts, field_ty_concrete)
        })
        .collect();
    opaque.set_body(&fields_ll, false);
    opaque
}
```

The cache insertion *before* recursing is load-bearing for self-referential types: when lowering `LinkedList<i32>`, the `next: *mut LinkedList<i32>` field lowers `*mut` to opaque LLVM `ptr` without recursing into the pointee.

### `lower_ty` — Adt arm

```rust
TyKind::Adt(aid, args) => {
    lower_adt_type(ctx, tcx, cache, adts, aid, &args).as_basic_type_enum()
}
```

The `Param(_)` panic in `lower_ty` is preserved exactly — post-mono, any `Param` reaching codegen is a bug.

### `field_index` / `field_gep` — unchanged

Field name → position is independent of type-args. `LinkedList<i32>.value` is at position 0 just as `LinkedList<u8>.value` is at position 0.

### `emit_field` and lvalue Field arm — substitute via args

For `p.value` access where `p: *mut LinkedList<i32>`:

1. Peel ptr → `Adt(ll_aid, [i32])`.
2. `field_index(ll_aid, "value") = 0`.
3. Look up `value`'s declared TyId: `Param(pid_T_LinkedList)`.
4. Build subst from `(adt.generic_params, args)` = `{pid_T_LinkedList → i32}`.
5. `substitute_ty` → `i32`.
6. `lower_ty` → LLVM `i32`.

For non-generic ADTs, `args.is_empty()`, the subst is empty, and `substitute_ty` passes the field type through unchanged — backward-compat.

### TyArena borrow

Mono already borrows `TypeckResults.tys` through interior-mut `substitute_ty`. Codegen does the same — `lower_adt_type` and the field-substitution path go through `tcx.substitute_ty(...)` on `&TyArena`. No new borrow contract.

## Errors (extension)

| Code | Variant | Owner | When |
|------|---------|-------|------|
| E0275 | `GenericArityMismatch` (existing) | typeck | Struct-lit turbofish supplies wrong number of type-args (e.g., `LinkedList::<i32, u8> { ... }`). Same shape as fn-call arity mismatch. |
| E0256 | `CannotInfer` (existing) | typeck | Struct-lit type-param unconstrained at finalize (e.g., `LinkedList { value: null, next: null }` — both fields are `*mut α`, T never pinned). |
| E0269 | `UnsizedArrayAsValue` (existing) | typeck | Struct-lit turbofish arg is `[T]` (e.g., `LinkedList::<[i32]> { ... }`). Same `Obligation::Sized { pos: SizedPos::TypeArg }` path as fn calls. |
| E0278 | `DivergentMonomorphization` (existing) | mono | Cascade through struct-wrapping fn (e.g., `f<T>() { f::<Wrap<T>>() }`). Same depth_limit. |
| (HIR) | `TypeParamWithArgs { name, span }` | hir | Source writes `T<X>` for a name `T` already in scope as a generic param. Recovery: type position becomes `HirTyKind::Error`, driver short-circuits. Not E0NNN-numbered. |

No new error codes for typeck. The only new shape is the HIR-side `TypeParamWithArgs`, following the same not-E0NNN convention as `GenericExternFn`.

## Out of scope (this round)

Same as the fn portion, restated for completeness:

- Generic enums and unions (enum/union themselves are reserved-but-unimplemented).
- Trait bounds on generic params (`T: Display`).
- `where` clauses.
- `impl<T>` blocks (no `impl` in oxide).
- Generic type aliases (no aliases in oxide).
- Specialization, HKT, const generics, variance, lifetimes — unchanged from the fn portion's list.
- Cross-module ADT name collisions — the AdtId-raw mangle scheme handles this cleanly (each `struct S` in different modules gets a different AdtId, hence a different mangle). The Display-side rendering would still need module-qualification when modules nest; deferred.

## Worked examples (extension)

### `LinkedList<T>` end-to-end

Source:

```rust
struct LinkedList<T> {
    value: T,
    next: *mut LinkedList<T>,
}

fn main() -> i32 {
    let mut head = LinkedList::<i32> { value: 0, next: null };
    head.value
}
```

After HIR:

- `HirAdt { name: "LinkedList", generic_params: [tpid_T], variants: [{ fields: [
  { name: "value", ty: HirTyKind::Param(tpid_T) },
  { name: "next", ty: HirTyKind::Ptr(HirTyKind::Adt(ll_haid, [HirTyKind::Param(tpid_T)]), Mut) },
  ] }] }`
- `HirFn { name: "main", generic_params: [], body: HBlockId(...) }`
- The struct lit lowers to `HirExprKind::StructLit { adt: ll_haid, type_args: [<i32 type-id>], fields: [...] }`

After typeck:

- `AdtDef { name: "LinkedList", generic_params: [pid_T], partial: false }`. Field types: `value: Param(pid_T)`, `next: Ptr(Adt(ll_aid, [Param(pid_T)]), Mut)`.
- `FnSig` for `main`: empty generic_params, `ret = i32`.
- `infer_struct_lit` for the literal: turbofish is `[i32]`; fresh = `[?T0]`; equate `?T0 = i32`; subst `{pid_T → ?T0}`; substitute fields (value's target = `?T0` = i32, next's target = `Ptr(Adt(ll_aid, [?T0]), Mut)`); unify `0: ?Tlit` with `i32`, `null: Ptr(α, Mut)` with `Ptr(Adt(ll_aid, [i32]), Mut)`.
- `head: Adt(ll_aid, [i32])` after finalize.
- `expr_tys[lit_eid] = Adt(ll_aid, [i32])`.

After mono:

- `MonoResults.instances` contains one `Instance` for `main` (entry point, no type_args). Nothing for the struct — it's not a fn.

Codegen:

- Phase 1: declare `main`. `lower_ty(i32)` → LLVM i32.
- Phase 2: walk `main`'s body.
  - At the struct lit, `expr_tys[lit_eid] = Adt(ll_aid, [i32])`. `lower_ty(_, Adt(ll_aid, [i32]))` → cache miss → `lower_adt_type(ll_aid, [i32])`:
    - Display name: `Adt(7, [i32])` (or whatever raw); opaque struct created and inserted.
    - Subst `{pid_T → i32}`.
    - Field 0 (`value: Param(pid_T)`): substitute → `i32` → LLVM i32.
    - Field 1 (`next: Ptr(Adt(ll_aid, [Param(pid_T)]), Mut)`): substitute → `Ptr(Adt(ll_aid, [i32]), Mut)`. `lower_ty` → LLVM `ptr` (opaque). No re-entry into `lower_adt_type`.
    - `set_body([i32, ptr], false)`.
  - Allocate stack slot for `head` typed as the materialized struct.
  - Field stores: `head.value ← 0`, `head.next ← null`.
  - `head.value` field load: `field_index(ll_aid, "value") = 0`; `field_gep` over the stack slot; `lower_ty(i32)` → load i32.

LLVM IR sketch:

```llvm
%"Adt(7, [i32])" = type { i32, ptr }

define i32 @main() {
entry:
  %head = alloca %"Adt(7, [i32])", align 8
  %head.value.gep = getelementptr inbounds %"Adt(7, [i32])", ptr %head, i32 0, i32 0
  store i32 0, ptr %head.value.gep, align 4
  %head.next.gep = getelementptr inbounds %"Adt(7, [i32])", ptr %head, i32 0, i32 1
  store ptr null, ptr %head.next.gep, align 8
  %v = load i32, ptr %head.value.gep, align 4
  ret i32 %v
}
```

### `Pair<T, U>` — multi-param inferred

```rust
struct Pair<T, U> { l: T, r: U }
fn main() -> i32 {
    let p = Pair { l: 7i32, r: 'a' };
    p.l
}
```

`infer_struct_lit`: `n_type_params = 2`, `fresh = [?T0, ?T1]`, no turbofish. Subst `{pid_T → ?T0, pid_U → ?T1}`. Field unifications: `7i32 ⊑ ?T0` → `?T0 ↪ i32`; `'a' (u8) ⊑ ?T1` → `?T1 ↪ u8`. Result: `Adt(pair_aid, [i32, u8])`.

LLVM: `%"Adt(8, [i32, u8])" = type { i32, i8 }`.

### Inference-vs-turbofish matrix

```rust
LinkedList::<i32> { value: 0, next: null }    // ✓ turbofish pins T = i32
LinkedList { value: 0i32, next: null }         // ✓ inferred from `value: 0i32`
LinkedList { value: 0, next: null }            // ✓ value: 0 is IntLit, defaults to i32 at finalize
LinkedList { value: null, next: null }         // ✗ E0256 — both fields are *mut α; T never pinned
```

The third case works because typeck's int-default mechanism fires at finalize and assigns `?T → i32` before the unconstrained-T check runs. The fourth case fails because `null` mints `Ptr(α, Mut)` without an int-default flag — finalize emits E0256 at the unconstrained binding's `creation_span`.

### `make<T>(v: T) -> Wrap<T>` — Param-in-Adt-args

```rust
struct Wrap<T> { x: T }
fn make<T>(v: T) -> Wrap<T> { Wrap { x: v } }
```

`infer_struct_lit` for `Wrap { x: v }`: fresh = `[?T0]`. Field unification: `v: Param(pid_T_make) ⊑ ?T0` → `?T0 ↪ Param(pid_T_make)`. Result: `Adt(wrap_aid, [?T0])` → resolve_fully → `Adt(wrap_aid, [Param(pid_T_make)])`.

So `expr_tys[lit_eid] = Adt(wrap_aid, [Param(pid_T_make)])` — a `Param` leaf nested inside the Adt's args. This is the Param-in-`expr_tys` contract extending to Adt arg slots. `resolve_fully` doesn't substitute Param (only Infer); mono's `instantiate(make, [i32], _)` substitutes via the new Adt arm in `substitute_ty`, producing `Adt(wrap_aid, [i32])` for the lit's TyId at codegen Phase 2.

## Open questions for spec iteration (extension)

3. **Display rendering of generic-Adt for diagnostics**: today `TyArena::render` prints `Adt(<raw>)` because it lacks AdtDef name access. Extending to `Adt(<raw>, [<args>])` is mechanical; human-friendly `LinkedList<i32>` rendering needs a TypeckResults-aware printer. Tracked by the existing TODO at `ty.rs:410–412`. Not a v0 blocker.
4. **`recursion_limit` configurability**: `depth_limit = 256` covers both pointer cascade (`f<T>() { f::<*mut T>() }`) and struct-wrap cascade (`f<T>() { f::<Wrap<T>>() }`). User-configurable limit deferred.
5. **Cross-module display rendering**: Mangle uses AdtId raw, so cross-module collisions are impossible at the symbol level. Display rendering would still want module-qualified names when modules nest deeper; deferred to the same future fix.
