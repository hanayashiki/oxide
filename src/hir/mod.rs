//! HIR — name-resolved IR consumed by typeck.

mod ir;
mod lower;
pub mod pretty;

pub use ir::*;
pub use lower::lower;
