//! Pure AST → `HirTy` lowering. Used by both the scanner (sealing
//! ADT field types) and the lowerer (fn signatures, `let` annotations,
//! casts, struct literals). Reads only — no error sink today, since
//! the parser already rejected non-`IntLit` length slots and missing
//! type names degrade to `HirTyKind::Named` for typeck to resolve.

use crate::hir::ir::{HirConst, HirTy, HirTyKind, TyParamId};
use crate::parser::ast;

use super::scanner::ModuleScopeCtx;

/// Per-fn type-parameter lookup. Linear scan; small N (typical 0–4
/// per spec/16's "no trait bounds, no generic ADTs" v0 scope). Used
/// inside `lower_ty` so the `Named` arm can decide whether a name is
/// a generic param (→ `Param(tpid)`) or an ADT (→ `Adt(haid)`) or
/// neither (→ `Named(name)` for typeck to resolve as a primitive).
///
/// For ADT-field lowering (no enclosing fn) and any other type-position
/// outside a generic-fn body, pass `TyParamScope::empty()` — Param
/// resolution is silently skipped and behavior matches the pre-spec/16
/// codepath. See spec/16_GENERIC.md §HIR.
#[derive(Clone, Copy)]
pub(super) struct TyParamScope<'a>(pub &'a [(String, TyParamId)]);

impl<'a> TyParamScope<'a> {
    pub fn empty() -> Self {
        Self(&[])
    }
    pub fn lookup(&self, name: &str) -> Option<TyParamId> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, id)| *id)
    }
}

/// Lower a type-position AST node. ADT names that hit the scope
/// resolve to `HirTyKind::Adt(haid)`; misses stay `Named(name)` for
/// typeck (primitives, unresolved-on-purpose forward refs).
///
/// `ty_params` is the type-parameter scope of the enclosing fn (empty
/// outside fn bodies, e.g. for ADT field lowering). Names that match
/// a generic param resolve to `Param(tpid)` *before* falling through
/// to ADT/`Named`. Per Rust precedence, type-param scope wins over
/// ADT scope — see spec/16_GENERIC.md §HIR.
pub(super) fn lower_ty(
    ast: &ast::Module,
    scope: &ModuleScopeCtx, // FIXME: unify scope access
    ty_params: TyParamScope<'_>,
    tid: ast::TypeId,
) -> HirTy {
    let ty = &ast.types[tid];
    let span = ty.span.clone();
    let kind = match &ty.kind {
        ast::TypeKind::Named(id) => {
            if let Some(tpid) = ty_params.lookup(&id.name) {
                HirTyKind::Param(tpid)
            } else if let Some(haid) = scope.lookup_type(&id.name) {
                HirTyKind::Adt(haid)
            } else {
                HirTyKind::Named(id.name.clone())
            }
        }
        ast::TypeKind::Ptr { mutability, pointee } => {
            let pointee = Box::new(lower_ty(ast, scope, ty_params, *pointee));
            HirTyKind::Ptr {
                mutability: *mutability,
                pointee,
            }
        }
        ast::TypeKind::Array { elem, len } => {
            let elem = Box::new(lower_ty(ast, scope, ty_params, *elem));
            let len_const = len.map(|eid| extract_length_const(ast, eid));
            HirTyKind::Array(elem, len_const)
        }
    };
    HirTy { kind, span }
}

/// Extract a `HirConst` from an AST length expression. Per
/// spec/09_ARRAY.md "Length literal extraction", v0 only accepts a
/// bare `IntLit` token — and the parser already enforces that. This
/// is therefore a structural pattern match with no error path.
pub(super) fn extract_length_const(ast: &ast::Module, eid: ast::ExprId) -> HirConst {
    match &ast.exprs[eid].kind {
        ast::ExprKind::IntLit(n) => HirConst::Lit(*n),
        other => unreachable!("parser ensures length slot is IntLit; got {other:?}"),
    }
}
