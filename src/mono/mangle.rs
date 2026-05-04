//! LLVM symbol mangling for instances. Produces stable, unique symbol
//! names per `(FnId, Vec<TyId>)` instance — the LLVM module's external
//! symbol table for the generic-instance space.
//!
//! Human-readable rendering (for diagnostic output) goes through
//! `TyArena::render` instead — no point duplicating that recursion
//! shape; this module is mangle-only.
//!
//! See spec/16_GENERIC.md §Naming.

use std::fmt::Write;

use crate::hir::{FnId, HirProgram};
use crate::typeck::{TyArena, TyId, TyKind};

/// Mangle an instance to its LLVM symbol name.
///
/// Non-generic short-circuit: when `type_args.is_empty()`, returns the
/// bare source name — `main` stays `main`, extern fns keep their
/// declared names, no decoration on programs that didn't use generics.
///
/// Generic form: `<name>__<arg1>__<arg2>...` with each arg going
/// through `mangle_ty`.
pub fn mangle_inst(hir: &HirProgram, fid: FnId, type_args: &[TyId], tys: &TyArena) -> String {
    let name = &hir.fns[fid].name;
    if type_args.is_empty() {
        return name.clone();
    }
    let mut out = name.clone();
    for &t in type_args {
        out.push_str("__");
        mangle_ty(&mut out, tys, t);
    }
    out
}

/// Recursive type → mangled symbol fragment.
///
/// Variant table per spec/16 §Naming:
/// - `Prim(i32)`           → `$i32`
/// - `Ptr(T, Const)`       → `$constptr_<rec(T)>`
/// - `Ptr(T, Mut)`         → `$mutptr_<rec(T)>`
/// - `Array(T, Some(N))`   → `$array_<rec(T)>_$n<N>`
/// - `Adt(aid)`            → `$adt<raw>` (identity-form; unique per
///                            compilation since AdtIds are unique)
/// - `Unit` / `Never`      → `$unit` / `$never`
/// - `Param(_)` / `Infer(_)` / `Fn(..)` / `Array(_, None)` → `unreachable!`
///                            (mono substitutes Param away before mangling;
///                            Infer is post-typeck poison; Fn never appears
///                            as a type-arg; unsized arrays are typeck-rejected
///                            at value positions.)
/// - `Error`               → `$error`
fn mangle_ty(out: &mut String, tys: &TyArena, ty: TyId) {
    match tys.kind(ty) {
        TyKind::Prim(p) => {
            out.push('$');
            out.push_str(p.name());
        }
        TyKind::Unit => out.push_str("$unit"),
        TyKind::Never => out.push_str("$never"),
        TyKind::Error => out.push_str("$error"),
        TyKind::Ptr(inner, mutability) => {
            use crate::parser::ast::Mutability;
            match mutability {
                Mutability::Const => out.push_str("$constptr_"),
                Mutability::Mut => out.push_str("$mutptr_"),
            }
            mangle_ty(out, tys, *inner);
        }
        TyKind::Array(elem, Some(n)) => {
            out.push_str("$array_");
            mangle_ty(out, tys, *elem);
            let _ = write!(out, "_$n{n}");
        }
        TyKind::Adt(aid, args) => {
            // Identity form — unique per compilation. AdtId raw is
            // digits-only, so the `$adt<digits>` boundary is unambiguous
            // even when followed by more `$`-prefixed type-arg encodings.
            // For non-generic ADTs `args` is empty and the suffix is
            // omitted; for generic ADTs each arg encoding starts with
            // `$`, self-delimiting. See spec/16_GENERIC.md §Mangling
            // (extension).
            let _ = write!(out, "$adt{}", aid.raw());
            for &arg in args {
                mangle_ty(out, tys, arg);
            }
        }
        TyKind::Param(_) | TyKind::Infer(_) | TyKind::Fn(_, _, _) | TyKind::Array(_, None) => {
            unreachable!(
                "mangle_inst::mangle_ty unreachable variant: {}",
                tys.render(ty)
            )
        }
    }
}
