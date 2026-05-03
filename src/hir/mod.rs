//! HIR — name-resolved IR consumed by typeck.

mod error;
mod ir;
mod lower;
pub mod pretty;

pub use error::*;
pub use ir::*;
pub use lower::{lower, lower_program};
