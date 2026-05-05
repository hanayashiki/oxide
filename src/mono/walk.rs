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
use crate::typeck::{ParamId, TyId, subst_from};

use super::{InstanceOrigin, InstanceParent, MonoCtx, instantiate};

/// Walk a fn body for generic-call discovery. `parent` names the
/// cascade entry — `Inst(id)` when walking a generic instance's body,
/// `Fn(fid)` when walking a non-generic root. The cascade site uses
/// `parent` directly as the `InstanceOrigin.parent` for any generic
/// callee discovered in this body.
pub(super) fn walk_body(cx: &mut MonoCtx, parent: InstanceParent) {
    let (fid, subst) = match parent {
        InstanceParent::Fn(fid) => (fid, HashMap::new()),
        InstanceParent::Inst(id) => {
            let inst_fid = cx.instances[id].fid;
            // Borrow-zip — no clone of type_args. cx.instances and
            // cx.typeck are disjoint fields so the two `&` reads coexist.
            let subst = subst_from(
                &cx.typeck.fn_sig(inst_fid).generic_params,
                &cx.instances[id].type_args,
            );
            (inst_fid, subst)
        }
    };
    let body_id = match cx.hir.fns[fid].body {
        Some(b) => b,
        None => return, // foreign fn — nothing to walk
    };

    walk_block(cx, parent, body_id, &subst);
}

fn walk_block(
    cx: &mut MonoCtx,
    parent: InstanceParent,
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
        walk_expr(cx, parent, eid, subst);
    }
}

fn walk_expr(
    cx: &mut MonoCtx,
    parent: InstanceParent,
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
                        InstanceOrigin { parent, call_span },
                    );
                }
            }
            walk_expr(cx, parent, callee, subst);
            for a in args {
                walk_expr(cx, parent, a, subst);
            }
        }

        // Structural recursion. Variant names match HirExprKind in
        // src/hir/ir.rs. Atoms (IntLit, BoolLit, CharLit, StrLit, Null,
        // Local, Fn, Unresolved, Continue, Poison) are no-ops.
        HirExprKind::Block(bid) => walk_block(cx, parent, bid, subst),
        HirExprKind::If {
            cond,
            then_block,
            else_arm,
        } => {
            walk_expr(cx, parent, cond, subst);
            walk_block(cx, parent, then_block, subst);
            match else_arm {
                Some(HElseArm::Block(bid)) => walk_block(cx, parent, bid, subst),
                Some(HElseArm::If(eid)) => walk_expr(cx, parent, eid, subst),
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
                walk_expr(cx, parent, e, subst);
            }
            if let Some(e) = cond {
                walk_expr(cx, parent, e, subst);
            }
            if let Some(e) = update {
                walk_expr(cx, parent, e, subst);
            }
            walk_block(cx, parent, body, subst);
        }
        HirExprKind::Let { init, .. } => {
            if let Some(init) = init {
                walk_expr(cx, parent, init, subst);
            }
        }
        HirExprKind::Return(value) => {
            if let Some(v) = value {
                walk_expr(cx, parent, v, subst);
            }
        }
        HirExprKind::Break { expr } => {
            if let Some(e) = expr {
                walk_expr(cx, parent, e, subst);
            }
        }
        HirExprKind::Unary { expr, .. } => walk_expr(cx, parent, expr, subst),
        HirExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(cx, parent, lhs, subst);
            walk_expr(cx, parent, rhs, subst);
        }
        HirExprKind::Assign { target, rhs, .. } => {
            walk_expr(cx, parent, target, subst);
            walk_expr(cx, parent, rhs, subst);
        }
        HirExprKind::Field { base, .. } => walk_expr(cx, parent, base, subst),
        HirExprKind::Index { base, index } => {
            walk_expr(cx, parent, base, subst);
            walk_expr(cx, parent, index, subst);
        }
        HirExprKind::StructLit { fields, .. } => {
            for f in fields {
                walk_expr(cx, parent, f.value, subst);
            }
        }
        HirExprKind::AddrOf { expr, .. } => walk_expr(cx, parent, expr, subst),
        HirExprKind::Cast { expr, .. } => walk_expr(cx, parent, expr, subst),
        HirExprKind::ArrayLit(lit) => match lit {
            HirArrayLit::Elems(elems) => {
                for e in elems {
                    walk_expr(cx, parent, e, subst);
                }
            }
            HirArrayLit::Repeat { init, .. } => walk_expr(cx, parent, init, subst),
        },

        // Atoms — no recursion. `Const(_)` is an atom: its RHS lives
        // in `HirProgram.consts[cid].value` (a `HirConstValue`, not
        // an HExprId), so there's nothing for mono to walk into.
        // See spec/18_CONST.md.
        HirExprKind::IntLit(_)
        | HirExprKind::BoolLit(_)
        | HirExprKind::CharLit(_)
        | HirExprKind::StrLit(_)
        | HirExprKind::Null
        | HirExprKind::Local(_)
        | HirExprKind::Fn(_)
        | HirExprKind::Const(_)
        | HirExprKind::Unresolved(_)
        | HirExprKind::Continue
        | HirExprKind::Poison => {}
    }
}
