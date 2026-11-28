//! Diagnostic formatting and rendering (CLI output and miette integration).

use crate::{Diagnostic, Severity};
use arandu_base::source_registry::SourceRegistry;
use std::fmt;

impl Diagnostic {
    #[must_use]
    pub fn format_for_cli(&self, registry: &SourceRegistry) -> String {
        use std::fmt::Write;
        let mut out = String::new();

        let (filepath, start_line, start_col) =
            if let Some(file) = registry.get_file(self.span.file_id) {
                let (line, col) = file.line_index.line_col(self.span.start);
                (&file.path[..], line, col)
            } else {
                ("", 1, 1)
            };

        let file_prefix = if filepath.is_empty() {
            String::new()
        } else {
            format!("{filepath}:")
        };

        // Format code prefix based on ICE vs regular error
        let code_prefix = self.code.as_str();

        let _ = writeln!(out, "{}: {}", code_prefix, self.message);
        let _ = writeln!(out, "  --> {}{}:{}", file_prefix, start_line, start_col);

        for label in &self.labels {
            let (l_start_line, l_start_col, l_end_line, l_end_col) =
                if let Some(file) = registry.get_file(label.span.file_id) {
                    let (s_line, s_col) = file.line_index.line_col(label.span.start);
                    let (e_line, e_col) = file.line_index.line_col(label.span.end);
                    (s_line, s_col, e_line, e_col)
                } else {
                    (1, 1, 1, 1)
                };
            let _ = writeln!(
                out,
                "  label: {}:{}-{}:{} {}",
                l_start_line, l_start_col, l_end_line, l_end_col, label.message
            );
        }
        for note in &self.notes {
            let _ = writeln!(out, "  note: {note}");
        }
        for hint in &self.hints {
            let _ = writeln!(out, "  hint: {}", hint.message);
            if let Some(ref rep) = hint.replacement {
                let (r_start_line, r_start_col, r_end_line, r_end_col) =
                    if let Some(file) = registry.get_file(rep.span.file_id) {
                        let (s_line, s_col) = file.line_index.line_col(rep.span.start);
                        let (e_line, e_col) = file.line_index.line_col(rep.span.end);
                        (s_line, s_col, e_line, e_col)
                    } else {
                        (1, 1, 1, 1)
                    };
                let _ = writeln!(
                    out,
                    "  replacement: at {}:{}-{}:{} with '{}'",
                    r_start_line, r_start_col, r_end_line, r_end_col, rep.new_text
                );
            }
        }

        // Remove trailing newline
        if out.ends_with('\n') {
            out.pop();
        }
        if out.ends_with('\r') {
            out.pop();
        }

        out
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Diagnostic {}

impl miette::Diagnostic for Diagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(self.code.as_str()))
    }

    fn severity(&self) -> Option<miette::Severity> {
        match self.severity {
            Severity::Error => Some(miette::Severity::Error),
            Severity::Warning => Some(miette::Severity::Warning),
            Severity::Note => Some(miette::Severity::Advice),
            Severity::Hint => Some(miette::Severity::Advice),
        }
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        let primary = std::iter::once(miette::LabeledSpan::new_primary_with_span(
            self.primary_label.as_deref().cloned(),
            miette::SourceSpan::new(
                (self.span.start as usize).into(),
                (self.span.end.saturating_sub(self.span.start)) as usize,
            ),
        ));
        let secondary = self.labels.iter().map(|label| {
            miette::LabeledSpan::new_with_span(
                Some(label.message.clone()),
                miette::SourceSpan::new(
                    (label.span.start as usize).into(),
                    (label.span.end.saturating_sub(label.span.start)) as usize,
                ),
            )
        });
        Some(Box::new(primary.chain(secondary)))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        if self.notes.is_empty() && self.hints.is_empty() {
            None
        } else {
            let mut parts = Vec::new();
            for note in &self.notes {
                parts.push(format!("note: {note}"));
            }
            for hint in &self.hints {
                parts.push(hint.message.clone());
                if let Some(rep) = &hint.replacement {
                    parts.push(format!("quick fix: replace with '{}'", rep.new_text));
                }
            }
            Some(Box::new(parts.join("\n")))
        }
    }

    fn url<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        if self.is_ice() {
            None
        } else {
            Some(Box::new(format!(
                "https://github.com/arandu-lang/arandu/blob/main/docs/errors/{}.md",
                self.code.as_str()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DiagCode, Span};

    #[test]
    fn miette_uses_a_short_primary_label_and_preserves_secondary_labels() {
        let diagnostic = Diagnostic::error(
            DiagCode::P001UnexpectedToken,
            "Expected an expression here, but found `}`.",
            Span::new(1, 4, 5),
        )
        .with_primary_label("unexpected token here")
        .with_label(Span::new(1, 0, 3), "expression starts here");

        let labels = miette::Diagnostic::labels(&diagnostic)
            .expect("diagnostic should expose source labels")
            .collect::<Vec<_>>();
        assert_eq!(labels[0].label(), Some("unexpected token here"));
        assert_ne!(labels[0].label(), Some(diagnostic.message.as_str()));
        assert_eq!(labels[1].label(), Some("expression starts here"));
    }

    #[test]
    fn miette_leaves_unannotated_primary_spans_without_inline_text() {
        let diagnostic = Diagnostic::error(
            DiagCode::P001UnexpectedToken,
            "Expected an expression here, but found `}`.",
            Span::new(1, 4, 5),
        );

        let label = miette::Diagnostic::labels(&diagnostic)
            .and_then(|mut labels| labels.next())
            .expect("diagnostic should expose the primary source span");
        assert_eq!(label.label(), None);
    }
}
