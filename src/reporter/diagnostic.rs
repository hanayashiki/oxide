use crate::lexer::Span;
use super::source_map::FileId;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Severity {
    Error,
    Warning,
    Note,
    Help,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Label {
    pub file: FileId,
    pub span: Span,
    pub message: String,
    pub primary: bool,
}

impl Label {
    pub fn primary(file: FileId, span: Span, message: impl Into<String>) -> Self {
        Self { file, span, message: message.into(), primary: true }
    }

    pub fn secondary(file: FileId, span: Span, message: impl Into<String>) -> Self {
        Self { file, span, message: message.into(), primary: false }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Option<&'static str>,
    pub message: String,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
    pub helps: Vec<String>,
}

impl Diagnostic {
    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: Some(code),
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
        }
    }

    pub fn with_label(mut self, label: Label) -> Self {
        self.labels.push(label);
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.helps.push(help.into());
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}
