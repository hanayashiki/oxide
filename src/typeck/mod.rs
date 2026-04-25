//! Type checker — derives real types from HIR. Owns the `TyArena`,
//! resolves primitive type names, runs HM-style inference per-fn, and
//! produces side-tables that codegen consumes.

mod check;
mod error;
mod ty;

pub use check::{TypeckResults, check};
pub use error::TypeError;
pub use ty::{FnSig, InferId, PrimTy, TyArena, TyId, TyKind};
