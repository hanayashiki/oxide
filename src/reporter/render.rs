use std::collections::HashMap;
use std::io::Write;
use std::ops::Range;

use ariadne::{Color, Config, Label as ArLabel, Report, ReportKind, Source};

use super::diagnostic::{Diagnostic, Severity};
use super::source_map::{FileId, SourceMap};

type ArSpan = (FileId, Range<usize>);

fn report_kind(severity: Severity) -> ReportKind<'static> {
    match severity {
        Severity::Error => ReportKind::Error,
        Severity::Warning => ReportKind::Warning,
        Severity::Note => ReportKind::Custom("note", Color::Cyan),
        Severity::Help => ReportKind::Custom("help", Color::Green),
    }
}

struct MapCache<'a> {
    map: &'a SourceMap,
    cache: HashMap<FileId, Source<String>>,
}

impl<'a> ariadne::Cache<FileId> for MapCache<'a> {
    type Storage = String;

    fn fetch(&mut self, id: &FileId) -> Result<&Source<String>, impl std::fmt::Debug> {
        let entry = self
            .cache
            .entry(*id)
            .or_insert_with(|| Source::from(self.map.get(*id).text.clone()));
        Ok::<_, String>(entry)
    }

    fn display<'b>(&self, id: &'b FileId) -> Option<impl std::fmt::Display + 'b> {
        Some(self.map.get(*id).path.display().to_string())
    }
}

pub fn emit(
    diag: &Diagnostic,
    sources: &SourceMap,
    out: &mut dyn Write,
    color: bool,
) -> std::io::Result<()> {
    let primary = diag
        .labels
        .iter()
        .find(|l| l.primary)
        .or(diag.labels.first());

    // Anchor the report at the primary label (or 0..0 if none).
    let (anchor_file, anchor_range): (FileId, Range<usize>) = match primary {
        Some(l) => (l.file, l.span.start.offset..l.span.end.offset),
        None => (FileId(0), 0..0),
    };

    let mut builder =
        Report::<ArSpan>::build(report_kind(diag.severity), (anchor_file, anchor_range))
            .with_config(Config::new().with_color(color))
            .with_message(&diag.message);

    if let Some(code) = diag.code {
        builder = builder.with_code(code);
    }

    let primary_color = match diag.severity {
        Severity::Error => Color::Red,
        Severity::Warning => Color::Yellow,
        Severity::Note => Color::Cyan,
        Severity::Help => Color::Green,
    };
    for label in &diag.labels {
        let span = label.span.start.offset..label.span.end.offset;
        let mut al = ArLabel::new((label.file, span));
        if !label.message.is_empty() {
            al = al.with_message(&label.message);
        }
        if color {
            let c = if label.primary { primary_color } else { Color::Cyan };
            al = al.with_color(c);
        }
        builder = builder.with_label(al);
    }

    for note in &diag.notes {
        builder = builder.with_note(note);
    }
    for help in &diag.helps {
        builder = builder.with_help(help);
    }

    let report = builder.finish();
    let cache = MapCache {
        map: sources,
        cache: HashMap::new(),
    };
    report.write(cache, out)
}

pub fn emit_all(
    diags: &[Diagnostic],
    sources: &SourceMap,
    out: &mut dyn Write,
    color: bool,
) -> std::io::Result<()> {
    for d in diags {
        emit(d, sources, out, color)?;
    }
    Ok(())
}
