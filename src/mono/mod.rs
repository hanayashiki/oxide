//! Monomorphization — Phase D of the compiler pipeline. Walks HIR
//! per-instance to discover the full instantiation graph reachable from
//! non-generic entry points; substitutes fn signatures at instantiation
//! time; produces `MonoResults` for codegen to consume.
//!
//! Architectural commitments (see spec/16_GENERIC.md §Monomorphization
//! and the post-review plan):
//!
//! - **Discovery walk**: `walk_body` is a structural HIR visitor that
//!   dispatches at `Call(Fn(callee_fid), args)`. When the callee is
//!   generic, it substitutes `typeck.call_type_args[parent_eid]` through
//!   the caller's `subst` to produce ground type-args, then cascades
//!   into `instantiate`.
//!
//! - **Cascade termination**: `instance_map` is populated *before* a
//!   body is walked, so self-recursive calls (`fn rec<T>(x:T)->T { rec(x) }`)
//!   hit the cache and terminate.
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
//! - **Single-arena contract**: mono takes `&TypeckResults` (immutable);
//!   `typeck.substitute_ty(...)` interns through interior-mutable
//!   `TyArena`. Every TyId in `MonoResults` references `typeck.tys`.

mod mangle;
mod walk;

use std::collections::{HashMap, VecDeque};

use index_vec::IndexVec;

use crate::hir::{FnId, HirProgram};
use crate::reporter::Span;
use crate::typeck::{ParamId, TyId, TypeckResults};

pub use mangle::mangle_inst;

index_vec::define_index_type! { pub struct InstId = u32; }

/// Default cap on instantiation-cascade depth. Overflow → `MonoError::DivergentMonomorphization`
/// (E0278). Mirrors rustc's `recursion_limit` of 256.
const DEFAULT_DEPTH_LIMIT: u32 = 256;

/// Public output of monomorphization. Consumed by codegen.
#[derive(Debug)]
pub struct MonoResults {
    pub instances: IndexVec<InstId, Instance>,
    /// `(fid, ground type_args) → InstId`. Written by mono's `instantiate`,
    /// read by codegen's `emit_call`: codegen takes the call site's
    /// `typeck.call_type_args[parent_eid]`, substitutes through the caller's
    /// `fx.subst` to ground them, and looks up the instance here. The same
    /// call site under different parents (`outer<i32>` vs `outer<i64>`)
    /// resolves to different instances.
    pub instance_map: HashMap<(FnId, Vec<TyId>), InstId>,
}

#[derive(Debug)]
pub struct Instance {
    pub fid: FnId,
    pub type_args: Vec<TyId>,
    /// Substituted at instantiation. Drives Phase 1 LLVM declaration.
    pub params: Vec<TyId>,
    /// Substituted at instantiation. Drives Phase 1 LLVM declaration
    /// and Phase 2 return-type emission.
    pub ret: TyId,
    pub mangled: String,
    pub depth: u32,
    pub origin: InstanceOrigin,
}

#[derive(Clone, Debug)]
pub enum InstanceOrigin {
    /// Seeded by `seed_entry_points` — a non-generic non-extern fn with
    /// a body. Depth = 0.
    EntryPoint,
    /// Pushed via cascade from `parent`'s body. Depth = parent.depth + 1.
    InstantiatedAt { parent: InstId, call_span: Span },
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
}

/// Module entry. Walks HIR per-instance starting from non-generic
/// entry points; produces MonoResults. Uses the default depth limit
/// (`DEFAULT_DEPTH_LIMIT`).
pub fn monomorphize(
    hir: &HirProgram,
    typeck: &TypeckResults,
) -> (MonoResults, Vec<MonoError>) {
    monomorphize_with_limit(hir, typeck, DEFAULT_DEPTH_LIMIT)
}

/// Same as `monomorphize` but with an explicit depth limit. Tests use
/// this to exercise the overflow path with a smaller bound — the
/// 256-entry default produces a multi-hundred-KB diagnostic snapshot
/// dominated by `*mut`-chains, which is unreadable.
pub fn monomorphize_with_limit(
    hir: &HirProgram,
    typeck: &TypeckResults,
    depth_limit: u32,
) -> (MonoResults, Vec<MonoError>) {
    let mut cx = MonoCtx::new(hir, typeck);
    cx.depth_limit = depth_limit;
    cx.seed_entry_points();
    while let Some(inst_id) = cx.work_queue.pop_front() {
        walk::walk_body(&mut cx, inst_id);
    }
    cx.finish()
}

/// Mono context — transient state during the cascade walk. `finish()`
/// consumes self by-value to produce `MonoResults`.
pub(crate) struct MonoCtx<'a> {
    pub(crate) hir: &'a HirProgram,
    pub(crate) typeck: &'a TypeckResults,
    pub(crate) instances: IndexVec<InstId, Instance>,
    pub(crate) instance_map: HashMap<(FnId, Vec<TyId>), InstId>,
    pub(crate) work_queue: VecDeque<InstId>,
    pub(crate) depth_limit: u32,
    pub(crate) errors: Vec<MonoError>,
}

impl<'a> MonoCtx<'a> {
    fn new(hir: &'a HirProgram, typeck: &'a TypeckResults) -> Self {
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

    /// Seed the work queue with one `EntryPoint` instance per non-generic,
    /// non-extern, body-having fn. Generic non-extern fns enter the
    /// instance set only via cascade. Externs are never seeded.
    ///
    /// V0 over-approximation: every reachable non-generic fn is seeded,
    /// not just `main`-reachable. Dead-code elimination is a future pass
    /// that can prune mono's complete instance graph; mono itself owns
    /// the full graph regardless.
    fn seed_entry_points(&mut self) {
        for (fid, hir_fn) in self.hir.fns.iter_enumerated() {
            if hir_fn.is_extern {
                continue;
            }
            if hir_fn.body.is_none() {
                continue;
            }
            let sig = self.typeck.fn_sig(fid);
            if !sig.generic_params.is_empty() {
                continue;
            }
            instantiate(self, fid, Vec::new(), InstanceOrigin::EntryPoint);
        }
    }

    /// Walk parent pointers from the offending origin back to the root
    /// EntryPoint. Each chain entry is `(FnId, Vec<TyId>, Span)`.
    /// Output is in root → tip order.
    pub(crate) fn walk_origin_chain(
        &self,
        origin: &InstanceOrigin,
    ) -> Vec<(FnId, Vec<TyId>, Span)> {
        // Walk tip → root, then reverse.
        let mut tip_to_root: Vec<(FnId, Vec<TyId>, Span)> = Vec::new();
        let mut cur = origin.clone();
        loop {
            match cur {
                InstanceOrigin::EntryPoint => break,
                InstanceOrigin::InstantiatedAt { parent, call_span } => {
                    let p = &self.instances[parent];
                    tip_to_root.push((p.fid, p.type_args.clone(), call_span));
                    cur = p.origin.clone();
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
pub(crate) fn instantiate(
    cx: &mut MonoCtx,
    fid: FnId,
    type_args: Vec<TyId>,
    origin: InstanceOrigin,
) -> Option<InstId> {
    // Probe by reference. The `(fid, type_args.clone())` clone is the
    // cost of using `(FnId, Vec<TyId>)` as a HashMap key without the
    // `raw_entry` API or a `Box<[TyId]>` wrapper. Acceptable for v0;
    // revisit if monomorphization is ever a hot path.
    if let Some(&id) = cx.instance_map.get(&(fid, type_args.clone())) {
        return Some(id);
    }

    let depth = match &origin {
        InstanceOrigin::EntryPoint => 0,
        InstanceOrigin::InstantiatedAt { parent, .. } => cx.instances[*parent].depth + 1,
    };
    if depth > cx.depth_limit {
        let span = match &origin {
            InstanceOrigin::InstantiatedAt { call_span, .. } => call_span.clone(),
            InstanceOrigin::EntryPoint => unreachable!(
                "EntryPoint cannot exceed depth_limit (depth = 0)"
            ),
        };
        cx.errors.push(MonoError::DivergentMonomorphization {
            chain: cx.walk_origin_chain(&origin),
            span,
            limit: cx.depth_limit,
        });
        return None;
    }

    // Borrow-zip subst from sig.generic_params + type_args; no clone.
    let sig = &cx.typeck.fn_sig(fid);
    let subst: HashMap<ParamId, TyId> = sig
        .generic_params
        .iter()
        .copied()
        .zip(type_args.iter().copied())
        .collect();
    let params: Vec<TyId> = sig
        .params
        .iter()
        .map(|&p| cx.typeck.substitute_ty(p, &subst))
        .collect();
    let ret = cx.typeck.substitute_ty(sig.ret, &subst);
    let mangled = mangle_inst(cx.hir, fid, &type_args, &cx.typeck.tys);

    // Push the instance moving `type_args` into Instance, then re-borrow
    // it from cx.instances[id].type_args for the instance_map insert
    // key — second clone avoided.
    let id = cx.instances.push(Instance {
        fid,
        type_args,
        params,
        ret,
        mangled,
        depth,
        origin,
    });
    let key_args = cx.instances[id].type_args.clone();
    cx.instance_map.insert((fid, key_args), id);
    cx.work_queue.push_back(id);
    Some(id)
}
