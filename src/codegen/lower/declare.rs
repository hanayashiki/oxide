//! Phase 1 — declare every reachable LLVM symbol before any body is
//! emitted. Two passes:
//!
//!   - Pass A walks `hir.fns` and adds one declaration per non-generic
//!     fn (extern keeps its verbatim source name; non-generic non-extern
//!     uses the source name as the LLVM symbol — `mangle_inst(_, _, &[])`
//!     short-circuits to the source name, so no mangling is necessary
//!     here).
//!
//!   - Pass B walks `mono.instances` (now generic-only) and adds one
//!     declaration per instance (mangled name).
//!
//! After Phase 1, every reachable LLVM symbol exists. Phase 2 emits
//! each instance's body — including self-recursive ones — without
//! ordering concerns.
//!
//! Two lookup tables run side by side:
//!   - `fn_decls: IndexVec<FnId, Option<FunctionValue>>` — the
//!     FnId-keyed table consulted by `emit_call`'s non-generic /
//!     extern dispatch path. Populated for extern fns and for
//!     non-generic non-extern fns directly from `hir.fns` (Pass A).
//!   - `inst_decls: IndexVec<InstId, Option<FunctionValue>>` — the
//!     InstId-keyed table consulted by `emit_call`'s generic dispatch
//!     path. Populated for every generic `Instance` mono produced
//!     (Pass B); intrinsic instances push `None` to keep the InstId →
//!     idx correspondence intact.

use crate::hir::FnId;
use crate::mono::InstanceOperation;
use crate::typeck::TyId;

use super::Codegen;

impl<'a, 'ctx> Codegen<'a, 'ctx> {
    pub(super) fn declare_all(&mut self) {
        // Phase 1 Pass A — every non-generic fn (extern + non-extern with
        // body). Declared with their verbatim source names: extern fns
        // resolve against external object files; non-generic non-extern
        // fns share the source name as the LLVM symbol because
        // `mangle_inst(_, _, &[])` collapses to the source name.
        //
        // Snapshot signatures into an owned `Vec` first: the emission loop
        // calls `self.lower_fn_type(...)` which needs `&mut self`,
        // conflicting with the `&FnSig` read borrow that
        // `self.typeck_results.fn_sig(fid)` would hand out inline.
        let non_generic_fns: Vec<(FnId, Vec<TyId>, TyId, bool)> = self
            .hir
            .fns
            .iter_enumerated()
            .filter(|(fid, h)| {
                // Non-extern fns must have a body to be declared here; bodyless
                // non-extern fns are HIR-rejected upstream.
                if !h.is_extern && h.body.is_none() {
                    return false;
                }
                // Drop generic non-extern (handled by Pass B). Generic externs
                // are typeck-rejected (E0212) so they wouldn't reach codegen,
                // but the filter is symmetric for clarity.
                self.typeck_results.fn_sig(*fid).generic_params.is_empty()
            })
            .map(|(fid, _)| {
                let sig = self.typeck_results.fn_sig(fid);
                (fid, sig.params.clone(), sig.ret, sig.c_variadic)
            })
            .collect();
        for (fid, params, ret, c_variadic) in non_generic_fns {
            let fn_ty = self.lower_fn_type(&params, ret, c_variadic);
            let hir_fn = &self.hir.fns[fid];
            let fnv = self.module.add_function(&hir_fn.name, fn_ty, None);
            // Attach LLVM param names for non-extern fns (debug-friendly).
            if !hir_fn.is_extern {
                for (i, pv) in fnv.get_param_iter().enumerate() {
                    let lid = hir_fn.params[i];
                    pv.set_name(&self.hir.locals[lid].name);
                }
            }
            self.fn_decls[fid] = Some(fnv);
        }

        // Phase 1 Pass B — generic instances. Each `Instance` from mono
        // produces one declaration with its mangled name and substituted
        // signature. The FunctionValue lands in `inst_decls[inst_id]` for
        // the generic dispatch path at `emit_call`. Non-generic fns are
        // **not** in mono.instances under the redesigned model.
        //
        // **Intrinsic instances are NOT declared.** When `inst.operation !=
        // Call`, codegen synthesizes the IR inline at the call site
        // (`emit_call` dispatches on `inst.operation`). To preserve the
        // `InstId → idx` correspondence in `inst_decls`, we still push an
        // entry — `None` instead of `Some(fnv)` — so downstream lookups
        // index correctly. See spec/17_LAYOUT.md §Intrinsic recognition
        // (Symbol emission).
        //
        // Snapshot the instance signatures so the &mut self call to
        // lower_fn_type doesn't conflict with the &mono iteration borrow.
        let inst_sigs: Vec<(InstanceOperation, FnId, Vec<TyId>, TyId, bool, String)> = self
            .mono
            .instances
            .iter()
            .map(|inst| {
                let c_variadic = self.hir.fns[inst.fid].is_variadic;
                (
                    inst.operation.clone(),
                    inst.fid,
                    inst.param_tys.clone(),
                    inst.ret_ty,
                    c_variadic,
                    inst.mangled.clone(),
                )
            })
            .collect();
        for (op, fid, param_tys, ret_ty, c_variadic, mangled) in inst_sigs {
            if op != InstanceOperation::Call {
                self.inst_decls.push(None);
                continue;
            }
            let fn_ty = self.lower_fn_type(&param_tys, ret_ty, c_variadic);
            let fnv = self.module.add_function(&mangled, fn_ty, None);
            // Attach LLVM param names (debug-friendly).
            let hir_fn = &self.hir.fns[fid];
            for (i, pv) in fnv.get_param_iter().enumerate() {
                let lid = hir_fn.params[i];
                pv.set_name(&self.hir.locals[lid].name);
            }
            self.inst_decls.push(Some(fnv));
        }
    }
}
