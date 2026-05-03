use super::diagnostic::{Diagnostic, Label};
use super::from_parse::from_parse_error;
use crate::loader::LoadError;

/// Map a `LoadError` into one or more diagnostics. `ImportParseFailed`
/// expands into one diagnostic per inner `ParseError` (delegating to
/// `from_parse_error`); the other variants produce a single
/// diagnostic each. Returns `Vec` so callers can iterate uniformly
/// regardless of variant.
pub fn from_load_error(err: &LoadError) -> Vec<Diagnostic> {
    match err {
        LoadError::ImportFileNotFound { raw, span } => vec![
            Diagnostic::error(
                "E0270",
                format!("cannot find imported file `{raw}`"),
            )
            .with_label(Label::primary(
                span.file,
                span.clone(),
                "import not found",
            ))
            .with_help(
                "imports resolve relative to the importing file, or against \
                 the bundled stdlib name table",
            ),
        ],
        LoadError::ImportParseFailed { file, errors, .. } => errors
            .iter()
            .map(|e| from_parse_error(e, *file))
            .collect(),
        LoadError::Io { path, span, source } => {
            let mut d = Diagnostic::error(
                "E_IO",
                format!("failed to read `{}`: {source}", path.display()),
            );
            if let Some(span) = span {
                d = d.with_label(Label::primary(
                    span.file,
                    span.clone(),
                    "imported here",
                ));
            }
            vec![d]
        }
    }
}
