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
