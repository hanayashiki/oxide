//! LLVM codegen via inkwell. Consumes typecheck results + HIR and
//! produces a verified `inkwell::module::Module`.
//!
//! Pipeline position: Source → tokens → AST → HIR → typeck → **codegen**.
//! Codegen never runs inference; it reads `TypeckResults::expr_tys`
//! and `local_tys` for every type decision.

mod lower;
mod ty;

pub use lower::codegen;
