//! Loader — discovers reachable `.ox` files starting from a root,
//! parses each, and feeds the set to HIR lowering.
//!
//! `load_program` (file-graph DFS) lands in a follow-up. This module
//! currently exposes the host abstraction and the `LoadedFile` shape
//! that `lower_program` consumes; tests construct `LoadedFile`s
//! manually until the loader lands.

pub mod host;

pub use host::{BuilderHost, ResolveError, VfsHost};

use std::path::PathBuf;

use crate::parser::ast;
use crate::reporter::FileId;

/// A single file that participates in the compilation. Produced by
/// the loader (or assembled by tests). Each file has been lexed,
/// parsed, and registered with the session's `SourceMap`.
#[derive(Clone, Debug)]
pub struct LoadedFile {
    pub file: FileId,
    /// Canonical path. Used by `lower_program` to build a
    /// `path → FileId` map for resolving the file's own `import`
    /// items.
    pub path: PathBuf,
    pub ast: ast::Module,
    /// Direct imports of this file, resolved to the FileIds of other
    /// loaded files. The loader populates this; tests build it
    /// manually for now. Imports that didn't resolve (file not found,
    /// parse failure) don't appear here — those are reported by the
    /// loader as `LoadError`s.
    pub direct_imports: Vec<FileId>,
}
