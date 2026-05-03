//! Loader — discovers reachable `.ox` files starting from a root,
//! parses each, and feeds the set to HIR lowering.

pub mod host;

pub use host::{BuilderHost, MaterializeError, ResolveError, VfsHost};

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use index_vec::IndexVec;

use crate::lexer::lex;
use crate::parser::{ParseError, ast, parse};
use crate::reporter::{FileId, Span};

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

/// Errors emitted while discovering, reading, and parsing the file
/// graph. Per `spec/14_MODULES.md:545-579`, `ImportFileNotFound` is
/// E0270 and `ImportParseFailed` is E0271. `Io` covers the
/// permission-denied / read-error cases the spec leaves implicit.
#[derive(Debug)]
pub enum LoadError {
    /// E0270 — `import "<raw>";` did not resolve to any readable file.
    ImportFileNotFound { raw: String, span: Span },
    /// E0271 — file resolved and was read, but parsing produced
    /// errors. The inner `ParseError`s carry their own spans tied to
    /// `file`.
    ImportParseFailed {
        file: FileId,
        path: PathBuf,
        errors: Vec<ParseError>,
    },
    /// IO error reading a resolved file (permission, EOF, etc.). For
    /// the root file, `span` is `None`; for an imported file, the span
    /// points at the `import` token that pulled it in.
    Io {
        path: PathBuf,
        span: Option<Span>,
        source: io::Error,
    },
}

/// Discover and parse every file reachable from `root` by following
/// `import` edges. Single-pass recursive DFS: each file gets a
/// `FileId` on entry (so cycle-back edges resolve), and
/// `direct_imports` is patched as recursion returns.
///
/// Returns the loaded files (indexed by `FileId`), the root's
/// `FileId`, and any `LoadError`s collected during traversal. Parse
/// errors don't abort traversal — the file is recorded and the
/// errors are reported separately so partial graphs can still drive
/// later stages where useful.
pub fn load_program(
    host: &dyn BuilderHost,
    source_map: &mut crate::reporter::SourceMap,
    root: PathBuf,
) -> (IndexVec<FileId, LoadedFile>, FileId, Vec<LoadError>) {
    let mut visited: HashMap<PathBuf, FileId> = HashMap::new();
    let mut files: IndexVec<FileId, LoadedFile> = IndexVec::new();
    let mut errors: Vec<LoadError> = Vec::new();

    let root_fid = match dfs(
        host,
        source_map,
        &mut visited,
        &mut files,
        &mut errors,
        root.clone(),
        None,
    ) {
        Some(fid) => fid,
        None => {
            // Root unreadable. Return an empty result with the IO error
            // already recorded by `dfs`. Caller checks `errors` is empty
            // and bails before lowering.
            return (files, FileId::default(), errors);
        }
    };

    (files, root_fid, errors)
}

fn dfs(
    host: &dyn BuilderHost,
    source_map: &mut crate::reporter::SourceMap,
    visited: &mut HashMap<PathBuf, FileId>,
    files: &mut IndexVec<FileId, LoadedFile>,
    errors: &mut Vec<LoadError>,
    path: PathBuf,
    import_site: Option<Span>,
) -> Option<FileId> {
    // Mount-key dedup: if we've already entered this path (Done or
    // InProgress), return the FileId. The cycle-back edge is well-formed
    // because we mark the entry *before* recursing into children below.
    if let Some(&fid) = visited.get(&path) {
        return Some(fid);
    }

    let src = match host.read(&path) {
        Ok(s) => s,
        Err(e) => {
            errors.push(LoadError::Io {
                path: path.clone(),
                span: import_site,
                source: e,
            });
            return None;
        }
    };

    let fid = source_map.add(path.clone(), src.clone());
    visited.insert(path.clone(), fid);

    let tokens = lex(&src, fid);
    let (module, parse_errs) = parse(&tokens, fid);
    if !parse_errs.is_empty() {
        errors.push(LoadError::ImportParseFailed {
            file: fid,
            path: path.clone(),
            errors: parse_errs,
        });
    }

    // Push the LoadedFile slot now; recursion below mutates
    // direct_imports as children return.
    let pushed_fid = files.push(LoadedFile {
        file: fid,
        path: path.clone(),
        ast: module,
        direct_imports: Vec::new(),
    });
    debug_assert_eq!(pushed_fid, fid);

    // Walk imports in source order. We clone the import spec here to
    // release the borrow on `files[fid].ast` before recursing (recursion
    // may push to `files`, which would conflict with an active borrow).
    let imports: Vec<(String, Span)> = files[fid]
        .ast
        .root_items
        .iter()
        .filter_map(|iid| {
            let item = &files[fid].ast.items[*iid];
            if let ast::ItemKind::Import(imp) = &item.kind {
                Some((imp.path.clone(), imp.span.clone()))
            } else {
                None
            }
        })
        .collect();

    for (raw, imp_span) in imports {
        match host.resolve(&path, &raw) {
            Ok(canon) => {
                if let Some(child_fid) = dfs(
                    host,
                    source_map,
                    visited,
                    files,
                    errors,
                    canon,
                    Some(imp_span.clone()),
                ) {
                    files[fid].direct_imports.push(child_fid);
                }
                // If dfs returned None (read error already recorded),
                // skip this edge — child can't be referenced.
            }
            Err(_) => {
                errors.push(LoadError::ImportFileNotFound {
                    raw,
                    span: imp_span,
                });
            }
        }
    }

    Some(fid)
}
