//! Type checker — derives real types from HIR. Owns the `TyArena`,
//! resolves primitive type names, runs HM-style inference per-fn, and
//! produces side-tables that codegen consumes.

mod check;
mod error;
mod ty;

pub use check::{TypeckResults, check};
pub use error::{MutateOp, TypeError};
pub use ty::{
    AdtDef, AdtId, ConstArena, ConstId, ConstKind, FieldDef, FnSig, InferId, PrimTy, TyArena, TyId,
    TyKind, VariantDef,
};
