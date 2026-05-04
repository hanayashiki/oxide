//! Body-walker for mono — discovery only.
//!
//! Job: find every generic call reachable from this instance's body,
//! substitute its type-args through the caller's `subst`, cascade
//! `instantiate` (which populates `instance_map` for codegen to read
//! later).
//!
//! Non-jobs:
//!   - Substituting body-internal expression or local types into
//!     side-tables — codegen does that lazily through
//!     `typeck.substitute_ty(...)` at each `ty_of(fx, eid)` read.
//!   - Maintaining a per-call-site `(eid → InstId)` cache — codegen
//!     re-resolves through `mono.instance_map` at emit time, which
//!     naturally handles the same-eid-from-different-parents case
//!     (`outer<i32>` body vs `outer<i64>` body, same call eid,
//!     different `fx.subst` → different InstId).

use std::collections::HashMap;

use crate::hir::{HBlockId, HElseArm, HExprId, HirArrayLit, HirExprKind};
use crate::typeck::{ParamId, TyId};

use super::{InstId, InstanceOrigin, MonoCtx, instantiate};

pub(super) fn walk_body(cx: &mut MonoCtx, inst_id: InstId) {
    let fid = cx.instances[inst_id].fid;
    let body_id = match cx.hir.fns[fid].body {
        Some(b) => b,
        None => return, // foreign fn — nothing to walk
    };

    // Local subst for THIS instance's body. Empty for non-generic.
    // Borrow-zip — no clone of type_args. cx.instances and cx.typeck
    // are disjoint fields so the two `&` reads coexist.
    let subst: HashMap<ParamId, TyId> = cx
        .typeck
        .fn_sig(fid)
        .generic_params
        .iter()
        .copied()
        .zip(cx.instances[inst_id].type_args.iter().copied())
        .collect();

    walk_block(cx, inst_id, body_id, &subst);
}

fn walk_block(
    cx: &mut MonoCtx,
    inst_id: InstId,
    bid: HBlockId,
    subst: &HashMap<ParamId, TyId>,
) {
    // Snapshot the items list so we don't keep a borrow on cx.hir.blocks
    // while recursing (the recursion calls back into &mut cx).
    let items: Vec<HExprId> = cx.hir.blocks[bid]
        .items
        .iter()
        .map(|it| it.expr)
        .collect();
    for eid in items {
        walk_expr(cx, inst_id, eid, subst);
    }
}

fn walk_expr(
    cx: &mut MonoCtx,
    inst_id: InstId,
    eid: HExprId,
    subst: &HashMap<ParamId, TyId>,
) {
    // Clone the kind to release the &cx.hir borrow before recursing
    // (intrinsic NLL workaround — the match arms call &mut cx).
    let kind = cx.hir.exprs[eid].kind.clone();
    match kind {
        HirExprKind::Call { callee, args, .. } => {
            // Only direct Fn callees in v0. The Fn(callee_fid) discriminator
            // decides whether we cascade.
            if let HirExprKind::Fn(callee_fid) = cx.hir.exprs[callee].kind {
                let has_generics = !cx.typeck.fn_sig(callee_fid).generic_params.is_empty();
                if has_generics {
                    debug_assert!(
                        !cx.hir.fns[callee_fid].is_extern,
                        "extern + generic rejected at typeck (per spec/16); \
                         mono should never see a generic extern callee",
                    );
                    // Resolve the call's type-args through the caller's subst.
                    // Clone the typeck-recorded args out first so the
                    // `&mut typeck` for substitute_ty doesn't fight with
                    // the `&Vec<TyId>` borrow into `call_type_args`.
                    let typeck_args: Vec<TyId> = cx
                        .typeck
                        .call_type_args
                        .get(&eid)
                        .cloned()
                        .unwrap_or_default();
                    let resolved_args: Vec<TyId> = typeck_args
                        .iter()
                        .map(|&t| cx.typeck.substitute_ty(t, subst))
                        .collect();
                    let call_span = cx.hir.exprs[eid].span.clone();
                    // Cascade. instance_map is populated as a side-effect
                    // of instantiate. Overflow (None) pushes an error;
                    // driver short-circuits on errors before codegen runs.
                    let _ = instantiate(
                        cx,
                        callee_fid,
                        resolved_args,
                        InstanceOrigin::InstantiatedAt {
                            parent: inst_id,
                            call_span,
                        },
                    );
                }
            }
            walk_expr(cx, inst_id, callee, subst);
            for a in args {
                walk_expr(cx, inst_id, a, subst);
            }
        }

        // Structural recursion. Variant names match HirExprKind in
        // src/hir/ir.rs. Atoms (IntLit, BoolLit, CharLit, StrLit, Null,
        // Local, Fn, Unresolved, Continue, Poison) are no-ops.
        HirExprKind::Block(bid) => walk_block(cx, inst_id, bid, subst),
        HirExprKind::If {
            cond,
            then_block,
            else_arm,
        } => {
            walk_expr(cx, inst_id, cond, subst);
            walk_block(cx, inst_id, then_block, subst);
            match else_arm {
                Some(HElseArm::Block(bid)) => walk_block(cx, inst_id, bid, subst),
                Some(HElseArm::If(eid)) => walk_expr(cx, inst_id, eid, subst),
                None => {}
            }
        }
        // Loop has init/cond/update header slots (covers while/for/loop) —
        // a generic call in any of these would be missed if we only
        // walked body.
        HirExprKind::Loop {
            init,
            cond,
            update,
            body,
            ..
        } => {
            if let Some(e) = init {
                walk_expr(cx, inst_id, e, subst);
            }
            if let Some(e) = cond {
                walk_expr(cx, inst_id, e, subst);
            }
            if let Some(e) = update {
                walk_expr(cx, inst_id, e, subst);
            }
            walk_block(cx, inst_id, body, subst);
        }
        HirExprKind::Let { init, .. } => {
            if let Some(init) = init {
                walk_expr(cx, inst_id, init, subst);
            }
        }
        HirExprKind::Return(value) => {
            if let Some(v) = value {
                walk_expr(cx, inst_id, v, subst);
            }
        }
        HirExprKind::Break { expr } => {
            if let Some(e) = expr {
                walk_expr(cx, inst_id, e, subst);
            }
        }
        HirExprKind::Unary { expr, .. } => walk_expr(cx, inst_id, expr, subst),
        HirExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(cx, inst_id, lhs, subst);
            walk_expr(cx, inst_id, rhs, subst);
        }
        HirExprKind::Assign { target, rhs, .. } => {
            walk_expr(cx, inst_id, target, subst);
            walk_expr(cx, inst_id, rhs, subst);
        }
        HirExprKind::Field { base, .. } => walk_expr(cx, inst_id, base, subst),
        HirExprKind::Index { base, index } => {
            walk_expr(cx, inst_id, base, subst);
            walk_expr(cx, inst_id, index, subst);
        }
        HirExprKind::StructLit { fields, .. } => {
            for f in fields {
                walk_expr(cx, inst_id, f.value, subst);
            }
        }
        HirExprKind::AddrOf { expr, .. } => walk_expr(cx, inst_id, expr, subst),
        HirExprKind::Cast { expr, .. } => walk_expr(cx, inst_id, expr, subst),
        HirExprKind::ArrayLit(lit) => match lit {
            HirArrayLit::Elems(elems) => {
                for e in elems {
                    walk_expr(cx, inst_id, e, subst);
                }
            }
            HirArrayLit::Repeat { init, .. } => walk_expr(cx, inst_id, init, subst),
        },

        // Atoms — no recursion.
        HirExprKind::IntLit(_)
        | HirExprKind::BoolLit(_)
        | HirExprKind::CharLit(_)
        | HirExprKind::StrLit(_)
        | HirExprKind::Null
        | HirExprKind::Local(_)
        | HirExprKind::Fn(_)
        | HirExprKind::Unresolved(_)
        | HirExprKind::Continue
        | HirExprKind::Poison => {}
    }
}
