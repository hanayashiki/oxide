//! Multi-file fixture splitter.
//!
//! Splits a single `.ox` fixture into N virtual files using `/// <path>`
//! header lines. Test-only; production code never sees this format.
//!
//! ```text
//! /// main.ox
//! import "./util.ox";
//! fn main() -> i32 { add_one(41) }
//! /// util.ox
//! fn add_one(x: i32) -> i32 { x + 1 }
//! ```
//!
//! Each header begins a new virtual file; everything between two
//! headers (or between the last header and EOF) is that file's source.
//! A fixture with **zero** `///` headers is a single virtual file
//! whose path defaults to the fixture's own filename — back-compat
//! for every existing single-file fixture.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use oxide::loader::VfsHost;

#[derive(Debug)]
pub enum SplitError {
    /// Non-empty content appeared before the first `///` header. Either
    /// remove the leading content or add a header at the top of the
    /// fixture.
    LeadingContent { line: usize },
    /// Two headers named the same path.
    DuplicatePath {
        path: PathBuf,
        first_line: usize,
        dup_line: usize,
    },
}

/// Split `src` into virtual files keyed by `/// <path>` headers.
///
/// Lines numbers in `SplitError` are 1-indexed.
pub fn split_fixture(
    src: &str,
    default_name: &Path,
) -> Result<Vec<(PathBuf, String)>, SplitError> {
    let lines: Vec<&str> = src.lines().collect();

    // Find every header line and the path it names.
    let headers: Vec<(usize, PathBuf)> = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| parse_header(line).map(|p| (idx, p)))
        .collect();

    if headers.is_empty() {
        return Ok(vec![(default_name.to_path_buf(), src.to_string())]);
    }

    // No non-empty content before the first header.
    let first_header_idx = headers[0].0;
    for (idx, line) in lines[..first_header_idx].iter().enumerate() {
        if !line.trim().is_empty() {
            return Err(SplitError::LeadingContent { line: idx + 1 });
        }
    }

    // No duplicate paths.
    let mut seen: HashMap<PathBuf, usize> = HashMap::new();
    for (idx, path) in &headers {
        if let Some(&first) = seen.get(path) {
            return Err(SplitError::DuplicatePath {
                path: path.clone(),
                first_line: first + 1,
                dup_line: idx + 1,
            });
        }
        seen.insert(path.clone(), *idx);
    }

    // Build segments. Each segment owns the lines BETWEEN its header
    // and the next header (exclusive on both ends).
    let mut segments = Vec::with_capacity(headers.len());
    for i in 0..headers.len() {
        let (header_idx, path) = &headers[i];
        let content_start = header_idx + 1;
        let content_end = if i + 1 < headers.len() {
            headers[i + 1].0
        } else {
            lines.len()
        };
        let mut content = lines[content_start..content_end].join("\n");
        // `lines()` strips trailing newlines; restore one if there's any
        // content so the file ends cleanly.
        if !content.is_empty() {
            content.push('\n');
        }
        segments.push((path.clone(), content));
    }

    Ok(segments)
}

/// `parse_header("///  main.ox  ")` → `Some(PathBuf::from("main.ox"))`.
/// Returns `None` for non-header lines (regular code, regular comments,
/// `///` doc comments without a path token).
fn parse_header(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("///")?;
    // `////` and beyond → regular comment, not a header.
    if rest.starts_with('/') {
        return None;
    }
    // `///` must be followed by whitespace before the path token.
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let path = rest.trim();
    if path.is_empty() {
        // Bare `///   ` with no path — treat as a regular doc comment.
        return None;
    }
    Some(PathBuf::from(path))
}

/// Build a `VfsHost` from the segments produced by `split_fixture`.
pub fn build_vfs(segments: Vec<(PathBuf, String)>) -> VfsHost {
    VfsHost::new(segments.into_iter().collect())
}

