//! Pure AST → `HirTy` lowering. Used by both the scanner (sealing
//! ADT field types) and the lowerer (fn signatures, `let` annotations,
//! casts, struct literals). Reads only — no error sink today, since
//! the parser already rejected non-`IntLit` length slots and missing
//! type names degrade to `HirTyKind::Named` for typeck to resolve.

use crate::hir::ir::{HirConst, HirTy, HirTyKind};
use crate::parser::ast;

use super::scanner::ModuleScopeCtx;

/// Lower a type-position AST node. ADT names that hit the scope
/// resolve to `HirTyKind::Adt(haid)`; misses stay `Named(name)` for
/// typeck (primitives, unresolved-on-purpose forward refs).
pub(super) fn lower_ty(
    ast: &ast::Module,
    scope: &ModuleScopeCtx,
    tid: ast::TypeId,
) -> HirTy {
    let ty = &ast.types[tid];
    let span = ty.span.clone();
    let kind = match &ty.kind {
        ast::TypeKind::Named(id) => match scope.lookup_type(&id.name) {
            Some(haid) => HirTyKind::Adt(haid),
            None => HirTyKind::Named(id.name.clone()),
        },
        ast::TypeKind::Ptr { mutability, pointee } => {
            let pointee = Box::new(lower_ty(ast, scope, *pointee));
            HirTyKind::Ptr {
                mutability: *mutability,
                pointee,
            }
        }
        ast::TypeKind::Array { elem, len } => {
            let elem = Box::new(lower_ty(ast, scope, *elem));
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
