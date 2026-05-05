//! Multi-pass declaration resolution. Lives as a child submodule of
//! `check` so it can read/write `Checker`'s private fields directly
//! without leaking visibility to siblings (`ty.rs`, `error.rs`).
//!
//! Three phases (see spec/08_ADT.md "Phase ordering and module layout"):
//!
//!   0    — Alloc `AdtDef` stubs (`partial: true`) keyed by `AdtId`,
//!           1:1 with HIR's `HAdtId`. Pre-intern `TyKind::Adt(aid)` so
//!           subsequent passes can reference Adts by identity even
//!           before their fields are resolved. This is the "partial
//!           construction" trick — graph nodes exist before edges.
//!
//!   0.5  — Walk each `HirAdt`, resolve every field's `HirTy → TyId`,
//!           backfill `AdtDef.variants`, flip `partial: false`.
//!           Recursive (`struct A { x: A }`) and mutual (`A → B`,
//!           `B → A`) references resolve cleanly because every AdtId
//!           is known after phase 0.
//!
//!   1    — Resolve fn signatures from source annotations. ADT names
//!           in fn types resolve to the pre-interned `Adt(aid)` from
//!           phase 0; primitive names resolve via `TyArena::from_prim_name`.

use index_vec::IndexVec;

use crate::hir::{HAdtId, HirConstValue};
use crate::reporter::Span;

use super::super::error::{ParamOrReturn, SizedPos, TypeError};
use super::super::ty::{
    AdtDef, AdtId, FieldDef, FnSig, ParamId, TyId, TyKind, VariantDef, subst_from,
};
use super::Checker;
use super::obligation::Obligation;

pub(super) fn resolve_decls(cx: &mut Checker<'_>) {
    alloc_partial_adts(cx);
    resolve_adt_fields(cx);
    check_recursive_adts(cx);
    resolve_fn_sigs(cx);
    resolve_consts(cx);
}

/// Phase 0 — push an `AdtDef` stub per HIR adt, allocate `ParamId`s
/// for its generic params, and pre-intern its **declaration form**
/// `Adt(aid, [Param(p0), Param(p1), ...])`. The `IndexVec` order is
/// HAdtId-aligned so `AdtId::from_raw(haid.raw())` is the lookup
/// throughout, and `ParamId::from_raw(htypid.raw())` mirrors HIR's
/// `HTyParamId` 1:1.
///
/// Pre-interning the declaration form (rather than `Adt(aid, [])`) is
/// load-bearing for non-generic and generic ADTs alike: field types
/// inside `LinkedList<T>`'s body reference `Adt(ll_aid, [Param(T)])`
/// directly via Phase 0.5's `resolve_ty`. Hash-cons makes the
/// declaration form and any field-type-recovery shape share TyIds.
/// For non-generic ADTs the args list is `[]` and the form collapses
/// to today's behavior. See spec/16_GENERIC.md §Typeck rules
/// (extension).
/// FIXME: do not assume HAdtId == AdtId raw!
fn alloc_partial_adts(cx: &mut Checker<'_>) {
    for hir_adt in cx.hir.adts.iter() {
        // 1:1 ParamId allocation, mirrors HTyParamId numbering.
        let generic_params: Vec<ParamId> = hir_adt
            .generic_params
            .iter()
            .map(|hid| ParamId::from_raw(hid.raw()))
            .collect();
        let aid = cx.adts.push(AdtDef {
            name: hir_adt.name.clone(),
            kind: hir_adt.kind,
            generic_params: generic_params.clone(),
            variants: IndexVec::new(),
            partial: true,
        });
        // Pre-intern the declaration form. Empty args for non-generic
        // ADTs; `[Param(p0), ...]` for generic ones.
        let arg_tys: Vec<TyId> = generic_params
            .iter()
            .map(|&pid| cx.tys.intern(TyKind::Param(pid)))
            .collect();
        let _ = cx.tys.intern(TyKind::Adt(aid, arg_tys));
    }
}

/// Phase 0.5 — backfill each AdtDef's variants/fields with resolved
/// TyIds. ADT-typed fields hit the pre-interned identity from phase 0;
/// primitive-typed fields resolve through `TyArena::from_prim_name`;
/// unknown type names emit `UnknownType` and resolve to `tys.error`.
fn resolve_adt_fields(cx: &mut Checker<'_>) {
    for raw in 0..cx.adts.len() {
        let aid = AdtId::from_raw(raw as u32);
        let haid = HAdtId::from_raw(raw as u32);
        // Sanity: AdtId/HAdtId numbering is 1:1 by phase 0 construction.
        debug_assert_eq!(cx.adts[aid].name, cx.hir.adts[haid].name);

        let mut variants: IndexVec<_, VariantDef> = IndexVec::new();

        for variant in cx.hir.adts[haid].variants.iter() {
            let variant_name = variant.name.clone();

            let mut fields: IndexVec<_, FieldDef> = IndexVec::new();
            for field in variant.fields.iter() {
                // Clone the field's HirTy so we don't keep a borrow on
                // cx.hir while we call resolve_ty (which takes
                // &mut cx.tys / &mut cx.errors).
                let ty = Checker::resolve_ty(&mut cx.tys, &mut cx.errors, &field.ty);
                // Field types are concrete; push the Sized obligation
                // with the resolved TyId. Discharged at `finish()`. See
                // spec/09_ARRAY.md "E0261".
                cx.decl_obligations.push(Obligation::Sized {
                    ty,
                    pos: SizedPos::Field,
                    span: field.ty.span.clone(),
                });
                fields.push(FieldDef {
                    name: field.name.clone(),
                    ty,
                    span: field.span.clone(),
                });
            }
            variants.push(VariantDef {
                name: variant_name,
                fields,
            });
        }

        cx.adts[aid].variants = variants;
        cx.adts[aid].partial = false;
    }
}

/// Phase 0.6 — reject ADTs whose field-type graph contains a cycle
/// without a `Ptr` indirection layer.
///
/// **Algorithm**: tri-color DFS over **substituted** types, driven once
/// per ADT as the start. For each `Adt(child, args)` encountered,
/// substitute `child`'s decl-time fields with `args` and recurse into
/// the substituted body. The "occurs check" is a `gray` set of AdtIds
/// on the current walk path — encountering an aid already in `gray`
/// is a back-edge and emits `RecursiveAdt`. A `visited` set on
/// `(AdtId, Vec<TyId>)` deduplicates branches.
///
/// **Termination**: gray's depth is bounded by `|adts|` (any second
/// encounter of an aid fires cycle and returns immediately); visited
/// grows monotonically, but each new entry advances the walk by one
/// substituted step. Growing-args divergence (e.g., `struct S<T> {
/// inner: S<*mut T> }`) is absorbed by the gray-set: the very first
/// visit to S, while still gray, fires cycle on the inner `Adt(s, _)`
/// regardless of args shape.
///
/// **Why "over substituted types" and not the static decl graph**: the
/// naive walk-static-edges approach over-rejects valid programs like
/// `struct B<T> { y: *mut T } / struct A { x: B<A> }` (B uses T only
/// via Ptr, so `B<A>` = `{ y: *mut A }` is finite — but a static walk
/// of A's fields would push `a` from B's args list without knowing
/// that B uses T pointer-only). Substituting B's body before walking
/// makes the Ptr arm cleanly break the cycle.
///
/// `Ptr(_, _)` skips (pointer breaks cycle); `Array(T, Some(_))` walks
/// `T`; every other leaf TyKind is a non-edge. See spec/16_GENERIC.md
/// §Typeck rules (extension), spec/08_ADT.md "Recursive type
/// rejection".
fn check_recursive_adts(cx: &mut Checker<'_>) {
    use std::collections::HashSet;

    // Track ADTs already known to participate in a cycle; skip them as
    // start nodes to avoid noisy duplicate diagnostics for mutual
    // cycles (`A → B → A` would otherwise emit two errors, one for
    // each start).
    let mut already_flagged: HashSet<AdtId> = HashSet::new();

    for raw in 0..cx.adts.len() {
        let start_aid = AdtId::from_raw(raw as u32);
        if already_flagged.contains(&start_aid) {
            continue;
        }
        let mut gray: HashSet<AdtId> = HashSet::new();
        let mut visited: HashSet<(AdtId, Vec<TyId>)> = HashSet::new();
        gray.insert(start_aid);
        let fields_to_walk: Vec<(TyId, Span)> = cx.adts[start_aid]
            .variants
            .iter()
            .flat_map(|v| v.fields.iter().map(|f| (f.ty, f.span.clone())))
            .collect();
        let errors_before = cx.errors.len();
        for (field_ty, field_span) in fields_to_walk {
            check_field(
                cx,
                start_aid,
                field_ty,
                &field_span,
                &mut gray,
                &mut visited,
            );
        }
        // If this start emitted any cycle, the entire SCC participating
        // in the cycle is now in `visited` (transitively, every
        // (aid, args) we explored). Mark those AdtIds as already
        // flagged so we don't re-report the cycle from another entry
        // point.
        if cx.errors.len() > errors_before {
            for (aid, _) in &visited {
                already_flagged.insert(*aid);
            }
        }
    }

    /// `cur_outer_aid` is the *most recently entered* ADT — the one
    /// whose substituted body we're currently walking. When a back-edge
    /// fires, the diagnostic is attributed to this ADT (not to
    /// `start_aid`) so the message points at the cycle's discovery
    /// frame, matching the prior tri-color-DFS attribution. For the
    /// initial call from the start ADT's own fields, `cur_outer_aid`
    /// is `start_aid`.
    fn check_field(
        cx: &mut Checker<'_>,
        cur_outer_aid: AdtId,
        ty: TyId,
        span: &Span,
        gray: &mut std::collections::HashSet<AdtId>,
        visited: &mut std::collections::HashSet<(AdtId, Vec<TyId>)>,
    ) {
        // Clone to release the `&cx.tys` borrow before any recursive
        // call that might mutate `cx.tys` via `substitute_ty`.
        let kind = cx.tys.kind(ty).clone();
        match kind {
            TyKind::Adt(child, args) => {
                if gray.contains(&child) {
                    // Back-edge — cycle closes here. Attribute to the
                    // current outer ADT.
                    let name = cx.adts[cur_outer_aid].name.clone();
                    cx.errors.push(TypeError::RecursiveAdt {
                        adt: name,
                        span: span.clone(),
                    });
                    return;
                }
                let key = (child, args.clone());
                if visited.contains(&key) {
                    return;
                }
                visited.insert(key);
                gray.insert(child);

                let child_subst = subst_from(&cx.adts[child].generic_params, &args);
                let child_field_tys: Vec<(TyId, Span)> = cx.adts[child]
                    .variants
                    .iter()
                    .flat_map(|v| v.fields.iter().map(|f| (f.ty, f.span.clone())))
                    .collect();
                for (field_ty, field_span) in child_field_tys {
                    let substituted = cx.tys.substitute_ty(field_ty, &child_subst);
                    // `cur_outer_aid` advances to `child` for the
                    // recursive walk of child's body — that's the
                    // ADT whose body discovered the next edge.
                    check_field(cx, child, substituted, &field_span, gray, visited);
                }

                gray.remove(&child);
            }
            TyKind::Ptr(_, _) => {
                // Pointer breaks the cycle.
            }
            TyKind::Array(elem, Some(_)) => {
                check_field(cx, cur_outer_aid, elem, span, gray, visited);
            }
            // Prim, Unit, Never, Param, Infer, Fn, Array(_, None), Error
            // don't contribute edges.
            _ => {}
        }
    }
}

/// Phase 1 — resolve fn signatures. Bodies aren't touched; that's phase 2.
fn resolve_fn_sigs(cx: &mut Checker<'_>) {
    for (fid, hir_fn) in cx.hir.fns.iter_enumerated() {
        let is_extern = hir_fn.is_extern;
        let mut params = Vec::with_capacity(hir_fn.params.len());
        for &lid in &hir_fn.params {
            let local = &cx.hir.locals[lid];
            let ty = Checker::resolve_annotation(
                &mut cx.tys,
                &mut cx.errors,
                local.ty.as_ref(),
                &local.span,
            );
            cx.local_tys[lid] = ty;
            params.push(ty);
            // Sized check at param position. Always enqueue: a missing
            // annotation resolves to `tys.error`, and discharge is a
            // no-op on `Error`. Diagnostic span is the whole local
            // declaration. See spec/09_ARRAY.md.
            cx.decl_obligations.push(Obligation::Sized {
                ty,
                pos: SizedPos::Param,
                span: local.span.clone(),
            });
            // E0264: sized array by value at `extern "C"` boundary.
            // Unsized arrays are caught by the Sized obligation above
            // (E0269), so this only fires for `Array(_, Some(_))`.
            // See spec/09_ARRAY.md "ABI: array-by-value across extern C".
            if is_extern && matches!(cx.tys.kind(ty), TyKind::Array(_, Some(_))) {
                cx.errors.push(TypeError::ArrayByValueAtExternC {
                    which: ParamOrReturn::Param,
                    ty,
                    span: local.span.clone(),
                });
            }
        }
        let ret = match &hir_fn.ret_ty {
            Some(t) => {
                let span = t.span.clone();
                let ty = Checker::resolve_ty(&mut cx.tys, &mut cx.errors, t);
                cx.decl_obligations.push(Obligation::Sized {
                    ty,
                    pos: SizedPos::Return,
                    span: span.clone(),
                });
                if is_extern && matches!(cx.tys.kind(ty), TyKind::Array(_, Some(_))) {
                    cx.errors.push(TypeError::ArrayByValueAtExternC {
                        which: ParamOrReturn::Return,
                        ty,
                        span,
                    });
                }
                ty
            }
            // Rust-style implicit unit return — Unit is sized; no
            // obligation needed.
            None => cx.tys.unit,
        };
        // Convert HIR's `Vec<HTyParamId>` (per-fn declaration order) to
        // typeck's `Vec<ParamId>`. 1:1 raw correspondence — same shape
        // as `AdtId::from_raw(haid.raw())` at `resolve_ty`'s Adt arm.
        // See spec/16_GENERIC.md §HIR.
        let generic_params: Vec<ParamId> = hir_fn
            .generic_params
            .iter()
            .map(|&hid| ParamId::from_raw(hid.raw()))
            .collect();
        cx.fn_sigs[fid] = FnSig {
            params,
            ret,
            generic_params,
            partial: false,
            c_variadic: hir_fn.is_variadic,
        };
    }
}

/// Phase 1.5 — resolve `const NAME: Type = LITERAL;` items.
///
/// For each const in `hir.consts`:
/// 1. Lower the annotation into a `TyId` via `Checker::resolve_ty`.
/// 2. Verify the literal's kind matches the annotation (E0250 type
///    mismatch on miss). Int → any integer prim; Bool → bool; Char
///    → u8; Str → `*const [u8; N+1]`. The shape mirrors
///    `infer_expr`'s literal arms (`StrLit` in particular). For
///    `Int`, the annotation pins the width — there's no inference
///    variable since consts have no body.
/// 3. Push a `Sized` obligation so unsized annotations (`[u8]`)
///    fire E0269 at decl time.
///
/// Writes the annotation TyId into `cx.const_tys[cid]` regardless of
/// the literal-vs-annotation match — downstream `infer_expr` reads
/// this for `HirExprKind::Const(cid)`. See spec/18_CONST.md.
fn resolve_consts(cx: &mut Checker<'_>) {
    for raw in 0..cx.hir.consts.len() {
        let cid = crate::hir::ConstId::from_raw(raw as u32);
        let hc = &cx.hir.consts[cid];
        let ann_span = hc.ty.span.clone();
        // Clone to release the borrow on hir before calling resolve_ty.
        let hir_ty = hc.ty.clone();
        let value = hc.value.clone();
        let annotated = Checker::resolve_ty(&mut cx.tys, &mut cx.errors, &hir_ty);
        cx.const_tys[cid] = annotated;

        // Sized obligation — pushed even if the type-check below
        // fails, so unsized annotations are flagged consistently
        // (matches the field/param posture).
        cx.decl_obligations.push(Obligation::Sized {
            ty: annotated,
            pos: SizedPos::LetBinding,
            span: ann_span.clone(),
        });

        // Build the literal's "intrinsic" type so we can equate.
        let lit_ty: TyId = match &value {
            HirConstValue::Int(_) => {
                // Any integer primitive matches an integer literal.
                // We don't materialize an Infer here (consts are
                // decl-phase, no Inferer yet); instead we accept any
                // annotation that resolves to an integer Prim and
                // bail with E0250 otherwise.
                if matches!(
                    cx.tys.kind(annotated),
                    TyKind::Prim(p) if p.is_integer()
                ) {
                    annotated
                } else {
                    // Use i32 as a stand-in "what would have fit" for
                    // the diagnostic. Same posture as infer_expr's
                    // int-default rule.
                    cx.tys.i32
                }
            }
            HirConstValue::Bool(_) => cx.tys.bool,
            HirConstValue::Char(_) => cx.tys.u8,
            HirConstValue::Str(s) => cx.tys.intern_str_lit(s),
        };

        cx.decl_obligations.push(Obligation::Coerce {
            actual: lit_ty,
            expected: annotated,
            span: hc.ty.span.clone(),
        });
    }
}
