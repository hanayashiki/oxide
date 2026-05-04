//! Pure AST → `HirTy` lowering. Used by both the scanner (sealing
//! ADT field types) and the lowerer (fn signatures, `let` annotations,
//! casts, struct literals). The error sink threads through for the
//! `TypeParamWithArgs` corner case (`T<X>` in source for a name `T`
//! that's already a generic param) — see spec/16_GENERIC.md §HIR
//! (extension).

use crate::hir::error::HirError;
use crate::hir::ir::{HirConst, HirTy, HirTyKind, HTyParamId};
use crate::parser::ast;

use super::scanner::ModuleScopeCtx;

/// Per-item type-parameter lookup (fn or ADT). Linear scan; small N
/// (typical 0–4). Used inside `lower_ty` so the `Named` arm can decide
/// whether a name is a generic param (→ `Param(tpid)`) or an ADT
/// (→ `Adt(haid, args)`) or neither (→ `Named(name)` for typeck to
/// resolve as a primitive).
///
/// Per Rust precedence, type-param scope wins over ADT scope. For
/// type-positions outside any item that introduces type params, pass
/// `TyParamScope(&[])`. See spec/16_GENERIC.md §HIR.
#[derive(Clone, Copy)]
pub(super) struct TyParamScope<'a>(pub &'a [(String, HTyParamId)]);

impl<'a> TyParamScope<'a> {
    pub fn lookup(&self, name: &str) -> Option<HTyParamId> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, id)| *id)
    }
}

/// Lower a type-position AST node.
///
/// Resolution rule for `Named { name, type_args }`:
///   1. If `name` resolves to a generic param **and** `type_args` is
///      empty: `HirTyKind::Param(tpid)`.
///   2. If `name` resolves to a generic param **and** `type_args` is
///      non-empty: emit `HirError::TypeParamWithArgs` (T<X> is
///      meaningless — type params have arity 0 in v0). Recovery:
///      `HirTyKind::Error`.
///   3. If `name` resolves to an ADT: `HirTyKind::Adt(haid, lowered_args)`,
///      recursively lowering each `type_args[i]` under the same scope.
///   4. Otherwise: `HirTyKind::Named(name)` — typeck resolves primitives
///      or fires `UnknownType`. `type_args` are dropped silently
///      (primitives have arity 0).
///
/// See spec/16_GENERIC.md §HIR (extension).
pub(super) fn lower_ty(
    ast: &ast::Module,
    scope: &ModuleScopeCtx, // FIXME: unify scope access
    ty_params: TyParamScope<'_>,
    errors: &mut Vec<HirError>,
    tid: ast::TypeId,
) -> HirTy {
    let ty = &ast.types[tid];
    let span = ty.span.clone();
    let kind = match &ty.kind {
        ast::TypeKind::Named { name, type_args } => {
            if let Some(tpid) = ty_params.lookup(&name.name) {
                if type_args.is_empty() {
                    HirTyKind::Param(tpid)
                } else {
                    errors.push(HirError::TypeParamWithArgs {
                        name: name.name.clone(),
                        span: span.clone(),
                    });
                    HirTyKind::Error
                }
            } else if let Some(haid) = scope.lookup_type(&name.name) {
                let lowered_args: Vec<HirTy> = type_args
                    .iter()
                    .map(|&tid| lower_ty(ast, scope, ty_params, errors, tid))
                    .collect();
                HirTyKind::Adt(haid, lowered_args)
            } else {
                // Unresolved — typeck will diagnose. Type-args on a
                // primitive (`i32<u8>`) are nonsense in v0; we drop
                // them silently and rely on typeck's `UnknownType`
                // path (or, for primitives, primitive-arity-0
                // semantics).
                HirTyKind::Named(name.name.clone())
            }
        }
        ast::TypeKind::Ptr { mutability, pointee } => {
            let pointee = Box::new(lower_ty(ast, scope, ty_params, errors, *pointee));
            HirTyKind::Ptr {
                mutability: *mutability,
                pointee,
            }
        }
        ast::TypeKind::Array { elem, len } => {
            let elem = Box::new(lower_ty(ast, scope, ty_params, errors, *elem));
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
