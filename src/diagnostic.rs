//! Errors reported to the user, and how they are printed.

use crate::source::{SourceMap, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// The program is invalid, and compilation stops.
    Error,
    /// The program is valid but probably contains a mistake. Compilation continues.
    Warning,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>, span: Span) -> Self {
        Diagnostic { severity: Severity::Error, message: message.into(), span }
    }

    pub fn warning(message: impl Into<String>, span: Span) -> Self {
        Diagnostic { severity: Severity::Warning, message: message.into(), span }
    }

    /// Formats the diagnostic with the source line it points at, for example:
    ///
    /// ```text
    /// error: invalid character '$'
    ///  --> main.jo:1:4
    ///   |
    /// 1 | fn $main() -> u8 { 0 }
    ///   |    ^
    /// ```
    pub fn render(&self, sources: &SourceMap) -> String {
        let file = sources.get(self.span.file);
        let (line, column) = file.line_col(self.span.start);
        let line_text = file.line_text(line);

        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let line_number = line.to_string();
        let gutter = " ".repeat(line_number.len());

        // Indent the markers using the same whitespace as the line, so tabs line up.
        let indent: String = line_text
            .chars()
            .take(column - 1)
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect();

        // Underline the part of the span on this line, with at least one marker so empty spans
        // (like the end of the file) are still visible.
        let span_chars = sources.text(self.span).lines().next().unwrap_or("").chars().count();
        let markers = "^".repeat(span_chars.max(1));

        format!(
            "{severity}: {message}\n\
             {gutter}--> {path}:{line}:{column}\n\
             {gutter} |\n\
             {line_number} | {line_text}\n\
             {gutter} | {indent}{markers}\n",
            message = self.message,
            path = file.path,
        )
    }
}
