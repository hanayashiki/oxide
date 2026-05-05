//! Type checker — derives real types from HIR. Owns the `TyArena`,
//! resolves primitive type names, runs HM-style inference per-fn, and
//! produces side-tables that codegen consumes.

mod check;
mod error;
pub mod layout;
mod ty;

pub use check::{CastKind, TypeckResults, cast_kind, check};
pub use error::{IntegerSite, MutateOp, ParamOrReturn, SizedPos, TypeError};
pub use layout::{align_of, size_of};
pub use ty::{
    AdtDef, AdtId, FieldDef, FnSig, InferId, ParamId, PrimTy, TyArena, TyId, TyKind, VariantDef,
    subst_from,
};
