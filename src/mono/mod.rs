//! Monomorphization — Phase D of the compiler pipeline. Walks HIR
//! per-instance to discover the full instantiation graph reachable from
//! non-generic entry points; substitutes fn signatures at instantiation
//! time; produces `MonoResults` for codegen to consume.
//!
//! Architectural commitments (see spec/16_GENERIC.md §Monomorphization
//! and the post-review plan):
//!
//! - **Mono only tracks generic instantiations.** Non-generic non-extern
//!   fns are walked at root by `seed_entry_points` to discover the first
//!   generic calls (cascade roots), but **no Instance is created** for
//!   them. Codegen emits non-generic fns directly from `hir.fns` and
//!   dispatches calls to them via `fn_decls[fid]`.
//!
//! - **Discovery walk**: `walk_body` is a structural HIR visitor that
//!   dispatches at `Call(Fn(callee_fid), args)`. When the callee is
//!   generic, it substitutes `typeck.call_type_args[parent_eid]` through
//!   the caller's `subst` to produce ground type-args, then cascades
//!   into `instantiate`. The walker carries an `InstanceParent` that
//!   names the cascade entry: `Inst(p)` when walking another generic
//!   instance's body, `Fn(fid)` when walking a non-generic root.
//!
//! - **Cascade termination**: `instance_map` is populated *before* a
//!   body is walked, so self-recursive calls (`fn rec<T>(x:T)->T { rec(x) }`)
//!   hit the cache and terminate. Keys are always non-empty —
//!   non-generic dedup is unnecessary because non-generic fns aren't
//!   instantiated by mono.
//!
//! - **Signature substitution at instantiation**: `Instance.params` and
//!   `Instance.ret` are computed once at `instantiate` time. Body-internal
//!   types (expr_tys/local_tys) are NOT pre-substituted; codegen does
//!   that lazily through `typeck.substitute_ty(...)`.
//!
//! - **Per-call-site resolution at codegen**: there is no `call_targets`
//!   side-table. At each generic call, codegen substitutes
//!   `typeck.call_type_args[parent_eid]` through `fx.subst` and looks up
//!   `mono.instance_map[(fid, resolved_args)]`. The same syntactic call
//!   site naturally resolves to different instances under different
//!   parents (`outer<i32>` vs `outer<i64>` body, same eid, different
//!   resolved args).
//!
//! - **Single-arena contract**: mono takes `&mut TypeckResults` so
//!   `typeck.substitute_ty(...)` can intern re-built structural kinds
//!   into `typeck.tys`. Every TyId in `MonoResults` references that
//!   one arena.

mod mangle;
mod walk;

use std::collections::{HashMap, VecDeque};

use index_vec::IndexVec;

use crate::hir::{FnId, HirProgram, Intrinsic};
use crate::reporter::Span;
use crate::typeck::{TyId, TypeckResults, layout, subst_from};

pub use mangle::mangle_inst;

index_vec::define_index_type! { pub struct InstId = u32; }

/// Default cap on instantiation-cascade depth. Overflow → `MonoError::DivergentMonomorphization`
/// (E0278). Mirrors rustc's `recursion_limit` of 256.
const DEFAULT_DEPTH_LIMIT: u32 = 256;

/// Public output of monomorphization. Consumed by codegen.
#[derive(Debug)]
pub struct MonoResults {
    pub instances: IndexVec<InstId, Instance>,
    /// `(fid, ground_type_args) → InstId`, populated by `mono::instantiate`.
    /// Each **generic** call site plus its containing instantiation
    /// corresponds to one entry. Keys are always non-empty: non-generic
    /// fns are not Instances under this model; codegen dispatches them
    /// through `fn_decls[fid]` instead. See spec/16_GENERIC.md
    /// §Monomorphization.
    pub instance_map: HashMap<(FnId, Vec<TyId>), InstId>,
}

#[derive(Debug, Clone)]
pub struct Instance {
    pub fid: FnId,
    pub type_args: Vec<TyId>,
    /// Parameter types substituted at instantiation. Drives Phase 1 LLVM declaration.
    pub param_tys: Vec<TyId>,
    /// Substituted at instantiation. Drives Phase 1 LLVM declaration
    /// and Phase 2 return-type emission.
    pub ret_ty: TyId,
    pub mangled: String,
    pub depth: u32,
    pub origin: InstanceOrigin,
    /// What codegen should do for this instance — Call (normal lowering),
    /// SizeOf { size } (emit i64 const), or Transmute (structural dispatch
    /// on (Src, Dst) shapes). Stamped by `instantiate` after the
    /// per-instance validity check. See spec/17_LAYOUT.md §Per-instance
    /// operation.
    pub operation: InstanceOperation,
}

/// Per-instance codegen dispatch decision. Stamped by `mono::instantiate`
/// after substituting params/ret and running any per-intrinsic validity
/// checks. Codegen reads `instance.operation` and pattern-matches; it
/// never re-derives the kind from `HirFn::intrinsic`.
///
/// See spec/17_LAYOUT.md §Per-instance operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstanceOperation {
    /// Default: codegen does normal call lowering (Pass 1 declares the
    /// LLVM symbol, Pass 2 emits the body, `emit_call` issues a `call`
    /// instruction).
    Call,
    /// `ox_size_of<T>()` instance — `size` is the precomputed
    /// `size_of(typeck, T_substituted)` from the layout helper. Codegen
    /// emits a single `i64 <size>` constant at the call site, no LLVM
    /// declare for this instance.
    SizeOf { size: u64 },
    /// `ox_transmute<Src, Dst>(x)` instance — marker only; codegen
    /// dispatches structurally on `(instance.params[0] kind,
    /// instance.ret kind)`. Size equality is already enforced by the
    /// per-instance E0276 check at mono time, so codegen needn't recheck.
    /// No LLVM declare for this instance.
    Transmute,
}

/// Bookkeeping for the call-site that produced an `Instance`. Used by
/// the depth check (`instantiate`) and E0278 chain rendering
/// (`walk_origin_chain`).
///
/// `parent` distinguishes the two cascade entry shapes:
///  - `Inst(p)`: we're inside another generic instance's body when the
///    call fired. Depth = `p.depth + 1`. Chain walk continues at `p`.
///  - `Fn(fid)`: we're inside a non-generic body (the cascade root).
///    Depth = 0. Chain walk terminates here, pushing `(fid, vec![],
///    call_span)` so the diagnostic still names the source non-generic
///    fn.
#[derive(Clone, Debug)]
pub struct InstanceOrigin {
    pub parent: InstanceParent,
    /// Span of the call expression that triggered this instantiation —
    /// the cascade-edge span, **not** the parent fn's decl span.
    pub call_span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum InstanceParent {
    /// Cascade hop from another generic instance.
    Inst(InstId),
    /// Cascade entry from a non-generic body. The non-generic fn itself
    /// is not an Instance under this model — codegen emits it directly
    /// from `hir.fns` and dispatches via `fn_decls[fid]`.
    Fn(FnId),
}

#[derive(Debug)]
pub enum MonoError {
    /// Cascade exceeded `depth_limit`. Carries the breadcrumb chain
    /// (root → tip) for diagnostic rendering and the `limit` value
    /// for the message. E0278 — see spec/16_GENERIC.md §Errors.
    ///
    /// Depth semantics: when overflow fires, the *failing* instance
    /// hasn't been pushed yet, so the chain ends at the immediate parent.
    /// `chain.len()` equals `depth_limit + 1` (root + N depth steps).
    DivergentMonomorphization {
        chain: Vec<(FnId, Vec<TyId>, Span)>,
        span: Span,
        limit: u32,
    },
    /// `ox_transmute::<Src, Dst>(x)` was called with `size_of(Src) !=
    /// size_of(Dst)` after substitution. E0276 — see
    /// spec/17_LAYOUT.md §Errors. Span is the call expression of the
    /// failing instance.
    TransmuteSizeMismatch {
        src: TyId,
        dst: TyId,
        src_size: u64,
        dst_size: u64,
        span: Span,
    },
}

/// Walks HIR per-instance starting from non-generic
/// entry points; produces MonoResults.
pub fn monomorphize(hir: &HirProgram, typeck: &mut TypeckResults) -> (MonoResults, Vec<MonoError>) {
    monomorphize_with_limit(hir, typeck, DEFAULT_DEPTH_LIMIT)
}

/// Same as `monomorphize` but with an explicit depth limit. Tests use
/// this to exercise the overflow path with a smaller bound — the
/// 256-entry default produces a multi-hundred-KB diagnostic snapshot
/// dominated by `*mut`-chains, which is unreadable.
pub fn monomorphize_with_limit(
    hir: &HirProgram,
    typeck: &mut TypeckResults,
    depth_limit: u32,
) -> (MonoResults, Vec<MonoError>) {
    let mut cx = MonoCtx::new(hir, typeck);
    cx.depth_limit = depth_limit;
    cx.seed_entry_points();
    while let Some(inst_id) = cx.work_queue.pop_front() {
        walk::walk_body(&mut cx, InstanceParent::Inst(inst_id));
    }
    cx.finish()
}

/// Mono context — transient state during the cascade walk. `finish()`
/// consumes self by-value to produce `MonoResults`.
pub(crate) struct MonoCtx<'a> {
    pub(crate) hir: &'a HirProgram,
    pub(crate) typeck: &'a mut TypeckResults,
    pub(crate) instances: IndexVec<InstId, Instance>,
    pub(crate) instance_map: HashMap<(FnId, Vec<TyId>), InstId>,
    pub(crate) work_queue: VecDeque<InstId>,
    pub(crate) depth_limit: u32,
    pub(crate) errors: Vec<MonoError>,
}

impl<'a> MonoCtx<'a> {
    fn new(hir: &'a HirProgram, typeck: &'a mut TypeckResults) -> Self {
        Self {
            hir,
            typeck,
            instances: IndexVec::new(),
            instance_map: HashMap::new(),
            work_queue: VecDeque::new(),
            depth_limit: DEFAULT_DEPTH_LIMIT,
            errors: Vec::new(),
        }
    }

    /// Walk every non-generic non-extern body to discover the first
    /// generic calls (cascade roots). No Instance is created for the
    /// non-generic fn itself — codegen will emit it directly from
    /// `hir.fns`. Each generic call discovered seeds the cascade with
    /// `parent: Fn(non_generic_fid)`.
    fn seed_entry_points(&mut self) {
        // Snapshot the eligible fids first to release the `&self.hir`
        // borrow before `walk_body` (which needs `&mut self`).
        let roots: Vec<FnId> = self
            .hir
            .fns
            .iter_enumerated()
            .filter_map(|(fid, h)| {
                if h.is_extern || h.body.is_none() {
                    return None;
                }
                if !self.typeck.fn_sig(fid).generic_params.is_empty() {
                    return None;
                }
                Some(fid)
            })
            .collect();
        for fid in roots {
            walk::walk_body(self, InstanceParent::Fn(fid));
        }
    }

    /// Walk parent pointers from the offending origin back to the
    /// non-generic source fn. Each chain entry is `(FnId, Vec<TyId>,
    /// Span)`. Output is in root → tip order.
    ///
    /// `Fn(fid)` terminates the walk and pushes `(fid, vec![],
    /// call_span)` so the chain still names the non-generic source.
    /// `Inst(p)` pushes `p`'s identity and continues at `p.origin`.
    pub(crate) fn walk_origin_chain(
        &self,
        origin: &InstanceOrigin,
    ) -> Vec<(FnId, Vec<TyId>, Span)> {
        // Walk tip → root, then reverse.
        let mut tip_to_root: Vec<(FnId, Vec<TyId>, Span)> = Vec::new();
        let mut cur = origin.clone();
        loop {
            match cur.parent {
                InstanceParent::Inst(p) => {
                    let inst = &self.instances[p];
                    tip_to_root.push((inst.fid, inst.type_args.clone(), cur.call_span));
                    cur = inst.origin.clone();
                }
                InstanceParent::Fn(fid) => {
                    tip_to_root.push((fid, Vec::new(), cur.call_span));
                    break;
                }
            }
        }
        tip_to_root.reverse();
        tip_to_root
    }

    fn finish(self) -> (MonoResults, Vec<MonoError>) {
        (
            MonoResults {
                instances: self.instances,
                instance_map: self.instance_map,
            },
            self.errors,
        )
    }
}

/// The canonical creation point for an `Instance`. Returns the existing
/// `InstId` on cache hit (terminates self-recursion), pushes a new
/// instance + work-queue entry on miss, or returns `None` and records
/// a `DivergentMonomorphization` error on overflow.
///
/// **Invariant**: `fn_type_args` is always non-empty. Non-generic fns
/// don't reach this function under the redesigned model — they're
/// walked at root by `seed_entry_points` without producing an Instance.
pub(crate) fn instantiate(
    cx: &mut MonoCtx,
    fn_id: FnId,
    fn_type_args: Vec<TyId>,
    origin: InstanceOrigin,
) -> Option<InstId> {
    debug_assert!(
        !fn_type_args.is_empty(),
        "instantiate is for generic instances only; non-generic fns are HIR passthroughs"
    );

    // Check if there is already an instance for this `(fn_id, fn_type_args)`; if so, return it.
    // This is the cache hit that terminates self-recursive calls.
    if let Some(&id) = cx.instance_map.get(&(fn_id, fn_type_args.clone())) {
        return Some(id);
    }

    // Depth counts cascade hops including the initial Fn → generic
    // entry. The first generic from a non-generic root is depth 1
    // (matching today's behavior where the EntryPoint Instance was
    // depth 0 and its first cascaded child was depth 1) — preserves
    // the `recursion_limit = N` user-visible boundary across this
    // refactor.
    let depth = match origin.parent {
        InstanceParent::Inst(p) => cx.instances[p].depth + 1,
        InstanceParent::Fn(_) => 1,
    };
    if depth > cx.depth_limit {
        let span = origin.call_span.clone();
        cx.errors.push(MonoError::DivergentMonomorphization {
            chain: cx.walk_origin_chain(&origin),
            span,
            limit: cx.depth_limit,
        });
        return None;
    }

    // Snapshot sig pieces by value so the subsequent `&mut typeck`
    // substitution doesn't fight with the `&FnSig` read borrow.
    let (params_in, ret_in, subst) = {
        let sig = cx.typeck.fn_sig(fn_id);
        let subst = subst_from(&sig.generic_params, &fn_type_args);
        (sig.params.clone(), sig.ret, subst)
    };
    let params: Vec<TyId> = params_in
        .iter()
        .map(|&p| cx.typeck.substitute_ty(p, &subst))
        .collect();
    let ret = cx.typeck.substitute_ty(ret_in, &subst);
    let mangled = mangle_inst(cx.hir, fn_id, &fn_type_args, &cx.typeck.tys);

    // Compute the per-instance operation that codegen will dispatch on.
    // For intrinsic fns this is also where per-instance validity checks
    // fire (E0276 for transmute size mismatch). See spec/17_LAYOUT.md
    // §Per-instance operation.
    let operation = match cx.hir.fns[fn_id].intrinsic {
        Some(Intrinsic::Transmute) => {
            // post-substitution, both Src and Dst are fully concrete; if
            // size_of returns None it's a layout-helper bug, not user error.
            let src_size = layout::size_of(cx.typeck, params[0]).unwrap_or_else(|| {
                unreachable!(
                    "ox_transmute Src has no size after substitution: {}",
                    cx.typeck.tys.render(params[0])
                )
            });
            let dst_size = layout::size_of(cx.typeck, ret).unwrap_or_else(|| {
                unreachable!(
                    "ox_transmute Dst has no size after substitution: {}",
                    cx.typeck.tys.render(ret)
                )
            });
            if src_size != dst_size {
                cx.errors.push(MonoError::TransmuteSizeMismatch {
                    src: params[0],
                    dst: ret,
                    src_size,
                    dst_size,
                    span: origin.call_span.clone(),
                });
            }
            InstanceOperation::Transmute
        }
        Some(Intrinsic::SizeOf) => {
            let t = fn_type_args[0];
            let size = layout::size_of(cx.typeck, t).unwrap_or_else(|| {
                unreachable!(
                    "ox_size_of::<T>() with no size: T={}; typeck E0269 \
                     should have rejected unsized type-args before mono",
                    cx.typeck.tys.render(t)
                )
            });
            InstanceOperation::SizeOf { size }
        }
        None => InstanceOperation::Call,
    };

    // Push the instance moving `type_args` into Instance, then re-borrow
    // it from cx.instances[id].type_args for the instance_map insert
    // key — second clone avoided.
    let id = cx.instances.push(Instance {
        fid: fn_id,
        type_args: fn_type_args,
        param_tys: params,
        ret_ty: ret,
        mangled,
        depth,
        origin,
        operation,
    });
    let key_args = cx.instances[id].type_args.clone();
    cx.instance_map.insert((fn_id, key_args), id);
    cx.work_queue.push_back(id);
    Some(id)
}
