//! Web diagnostic types and position calculation for the Arandu Web Playground.

use arandu_base::LineIndex;
use serde::{Deserialize, Serialize};

/// Diagnostic format consumed by the web editor (Monaco / CodeMirror).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebDiagnostic {
    /// 1-indexed line number.
    pub line: u32,
    /// 1-indexed column number.
    pub column: u32,
    /// Length of the diagnostic span in bytes/characters.
    pub length: u32,
    /// Severity level.
    pub severity: WebSeverity,
    /// Canonical diagnostic code (e.g. "T001", "P005").
    pub code: Option<String>,
    /// Primary error or warning message.
    pub message: String,
    /// Supplementary notes and help text.
    pub notes: Vec<String>,
}

/// Diagnostic severity for Monaco Editor markers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WebSeverity {
    Error,
    Warning,
    Info,
}

/// Convert an internal compiler diagnostic into a web-friendly diagnostic using `LineIndex`.
#[must_use]
pub fn convert_diagnostic(
    diag: &arandu_middle::Diagnostic,
    line_index: &LineIndex,
) -> WebDiagnostic {
    let span = diag.span;
    let (line, column) = line_index.line_col(span.start);
    let length = span.end.saturating_sub(span.start).max(1);

    let severity = match diag.severity {
        arandu_middle::Severity::Error => WebSeverity::Error,
        arandu_middle::Severity::Warning => WebSeverity::Warning,
        arandu_middle::Severity::Note | arandu_middle::Severity::Hint => WebSeverity::Info,
    };

    let code = Some(diag.code.as_str().to_string());
    let mut notes = diag.notes.clone();
    for hint in &diag.hints {
        notes.push(format!("Hint: {}", hint.message));
    }

    WebDiagnostic {
        line,
        column,
        length,
        severity,
        code,
        message: diag.message.clone(),
        notes,
    }
}

/// Calculate 1-indexed line and column from a byte offset in UTF-8 source.
#[must_use]
pub fn offset_to_line_col(source: &str, byte_offset: usize) -> (u32, u32) {
    let index = LineIndex::new(source);
    index.line_col(byte_offset as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_to_line_col_at_start() {
        assert_eq!(offset_to_line_col("hello", 0), (1, 1));
    }

    #[test]
    fn offset_to_line_col_multiline() {
        let text = "func main(): void {\n    return\n}";
        assert_eq!(offset_to_line_col(text, 20), (2, 1));
        assert_eq!(offset_to_line_col(text, 24), (2, 5));
    }
}
