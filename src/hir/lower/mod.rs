//! AST → HIR lowering. Two phases:
//!   1. `scanner::scan` (passes 1–3 + pass 4a per spec/14_MODULES.md)
//!      — allocate fn/ADT stubs, build per-file scopes, seal ADT
//!      field types.
//!   2. `lowerer` (pass 4b) — walk each fn body to populate
//!      `params`/`ret_ty`/`body` on the stubs, plus the
//!      `locals`/`exprs`/`blocks` arenas.

mod lowerer;
mod scanner;
mod ty;

pub use lowerer::{lower, lower_program};
