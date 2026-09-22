//! Compiler diagnostic types for Arandu.
//!
//! A [`Diagnostic`] represents a single compiler message (error, warning, note,
//! or hint) and carries a machine-readable [`DiagCode`], a human-readable
//! message, a primary [`Span`], optional secondary [`Label`]s, notes, and
//! [`Hint`]s with optional code replacements.
//!
//! # Building a diagnostic
//! ```ignore
//! Diagnostic::error(DiagCode::T001CannotInferType, "cannot infer type", span)
//!     .with_label(extra_span, "declared here")
//!     .with_hint("add an explicit type annotation")
//! ```

pub mod codes;
pub mod registry;
pub mod render;

pub use arandu_base::source_registry::SourceRegistry;
pub use arandu_base::span::Span;
pub use codes::{DiagCode, diag_doc_diff};
use std::fmt;

/// Severity level of a compiler diagnostic.
///
/// Controls how the message is displayed and whether compilation is aborted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// A hard error that prevents successful compilation.
    Error,
    /// A potential issue that does not prevent compilation.
    Warning,
    /// Informational context attached to an error or warning.
    Note,
    /// A suggestion for how to fix an issue, optionally with a code replacement.
    Hint,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
            Severity::Note => write!(f, "note"),
            Severity::Hint => write!(f, "hint"),
        }
    }
}

/// Whether a diagnostic was produced by the user's code or by an internal compiler bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// The diagnostic describes a problem in the user's source code.
    User,
    /// The diagnostic describes an unexpected compiler bug (ICE — Internal Compiler Error).
    InternalCompilerError,
}

/// A suggested text replacement attached to a [`Hint`].
///
/// When present in a hint, editors and CLI can offer to automatically apply
/// the replacement to the source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeReplacement {
    /// The source span to replace.
    pub span: Span,
    /// The text that should replace the spanned region.
    pub new_text: String,
}

/// A human-readable suggestion attached to a [`Diagnostic`].
///
/// Hints optionally carry a [`CodeReplacement`] so that tooling can offer
/// a one-click fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    /// The hint message shown to the user.
    pub message: String,
    /// An optional automatic code fix for the suggestion.
    pub replacement: Option<CodeReplacement>,
}

impl From<String> for Hint {
    fn from(message: String) -> Self {
        Self {
            message,
            replacement: None,
        }
    }
}

impl From<&str> for Hint {
    fn from(message: &str) -> Self {
        Self {
            message: message.to_string(),
            replacement: None,
        }
    }
}

/// A secondary source location annotation attached to a [`Diagnostic`].
///
/// Labels highlight additional spans of code that are relevant to the main
/// diagnostic message (e.g. "value moved here", "declared here").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    /// The source span this label points to.
    pub span: Span,
    /// A short message describing why this span is relevant.
    pub message: String,
}

/// A single compiler diagnostic message.
///
/// Diagnostics are the primary output of every compiler pass. They carry a
/// machine-readable [`DiagCode`], a human-readable message, a primary source
/// [`Span`], optional secondary [`Label`]s, free-form notes, and [`Hint`]s
/// with optional code replacements.
///
/// Use the [`Diagnostic::error`], [`Diagnostic::warning`], etc. constructors
/// and chain [`with_label`](Diagnostic::with_label), [`with_note`](Diagnostic::with_note),
/// and [`with_hint`](Diagnostic::with_hint) to build a complete diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Machine-readable error code for tooling and tests.
    pub code: DiagCode,
    /// Display severity (controls abort behaviour and rendering colour).
    pub severity: Severity,
    /// Whether this is a user error or an internal compiler error.
    pub kind: DiagnosticKind,
    /// Primary human-readable message.
    pub message: String,
    /// Primary source location that the message refers to.
    pub span: Span,
    /// Secondary annotated spans for additional context.
    pub labels: Vec<Label>,
    /// Free-form explanatory notes appended after the main message.
    pub notes: Vec<String>,
    /// Actionable hints, optionally with automatic code replacements.
    pub hints: Vec<Hint>,
}

impl Diagnostic {
    /// Creates a user-facing error diagnostic.
    pub fn error(code: DiagCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            severity: Severity::Error,
            kind: DiagnosticKind::User,
            message: message.into(),
            span,
            labels: Vec::new(),
            notes: Vec::new(),
            hints: Vec::new(),
        }
    }

    /// Creates a user-facing warning diagnostic.
    pub fn warning(code: DiagCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            severity: Severity::Warning,
            kind: DiagnosticKind::User,
            message: message.into(),
            span,
            labels: Vec::new(),
            notes: Vec::new(),
            hints: Vec::new(),
        }
    }

    /// Creates a user-facing informational note.
    pub fn note(code: DiagCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            severity: Severity::Note,
            kind: DiagnosticKind::User,
            message: message.into(),
            span,
            labels: Vec::new(),
            notes: Vec::new(),
            hints: Vec::new(),
        }
    }

    /// Creates a user-facing hint (suggestion).
    pub fn hint(code: DiagCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            severity: Severity::Hint,
            kind: DiagnosticKind::User,
            message: message.into(),
            span,
            labels: Vec::new(),
            notes: Vec::new(),
            hints: Vec::new(),
        }
    }

    /// Returns `true` if this diagnostic represents an Internal Compiler Error (ICE).
    #[must_use]
    pub fn is_ice(&self) -> bool {
        self.kind == DiagnosticKind::InternalCompilerError || self.code.is_ice()
    }

    /// Creates an Internal Compiler Error (ICE) diagnostic.
    ///
    /// ICEs indicate a bug in the compiler itself, not in the user's code.
    /// They are always rendered as errors regardless of the provided code's category.
    pub fn ice(code: DiagCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            severity: Severity::Error,
            kind: DiagnosticKind::InternalCompilerError,
            message: message.into(),
            span,
            labels: Vec::new(),
            notes: Vec::new(),
            hints: Vec::new(),
        }
    }

    /// Attaches a secondary source label to this diagnostic.
    #[must_use]
    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    /// Appends a free-form explanatory note to this diagnostic.
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Appends a plain-text hint (no code replacement).
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hints.push(Hint {
            message: hint.into(),
            replacement: None,
        });
        self
    }

    /// Appends a hint with an optional automatic code replacement.
    #[must_use]
    pub fn with_hint_replacement(mut self, hint: Hint) -> Self {
        self.hints.push(hint);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_span() -> Span {
        Span::new(0, 0, 0)
    }

    fn registry_with(content: &str) -> SourceRegistry {
        let mut reg = SourceRegistry::new();
        reg.register("test.arandu", content);
        reg
    }

    // ── Severity Display ──

    #[test]
    fn severity_display() {
        assert_eq!(Severity::Error.to_string(), "error");
        assert_eq!(Severity::Warning.to_string(), "warning");
        assert_eq!(Severity::Note.to_string(), "note");
        assert_eq!(Severity::Hint.to_string(), "hint");
    }

    // ── DiagCode ──

    #[test]
    fn diag_code_as_str() {
        for (code, expected) in &[
            (DiagCode::LX001UnterminatedString, "LX001"),
            (DiagCode::P001UnexpectedToken, "P001"),
            (DiagCode::M001UnresolvedImport, "M001"),
            (DiagCode::N001UndefinedValue, "N001"),
            (DiagCode::T001CannotInferType, "T001"),
            (DiagCode::L001LoweringUnresolvedSymbol, "L001"),
            (DiagCode::G001GenericInstantiationCycle, "G001"),
            (DiagCode::O001UseAfterMove, "O001"),
            (DiagCode::W001VariableAssignedNotUsed, "W001"),
            (DiagCode::U001FeatureNotSupported, "U001"),
            (DiagCode::ICELX001, "ICE-LX-001"),
        ] {
            assert_eq!(code.as_str(), *expected, "mismatch for {code:?}");
        }
    }

    #[test]
    fn diag_code_display_matches_as_str() {
        let codes = [
            DiagCode::T002IncompatibleAssignment,
            DiagCode::T003IncompatibleCallArg,
            DiagCode::N003RedefinedName,
            DiagCode::ICEP001,
        ];
        for code in &codes {
            assert_eq!(code.to_string(), code.as_str());
        }
    }

    // ── Hint ──

    #[test]
    fn hint_from_string() {
        let h: Hint = "hello".to_string().into();
        assert_eq!(h.message, "hello");
        assert_eq!(h.replacement, None);
    }

    #[test]
    fn hint_from_str() {
        let h: Hint = "hello".into();
        assert_eq!(h.message, "hello");
        assert_eq!(h.replacement, None);
    }

    // ── Diagnostic builder: error ──

    #[test]
    fn diagnostic_error_builder() {
        let d = Diagnostic::error(DiagCode::T001CannotInferType, "oops", dummy_span());
        assert_eq!(d.code, DiagCode::T001CannotInferType);
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.kind, DiagnosticKind::User);
        assert_eq!(d.message, "oops");
        assert_eq!(d.span, dummy_span());
    }

    #[test]
    fn diagnostic_warning_builder() {
        let d = Diagnostic::warning(
            DiagCode::W001VariableAssignedNotUsed,
            "unused",
            dummy_span(),
        );
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.kind, DiagnosticKind::User);
    }

    #[test]
    fn diagnostic_note_builder() {
        let d = Diagnostic::note(DiagCode::N001UndefinedValue, "info", dummy_span());
        assert_eq!(d.severity, Severity::Note);
        assert_eq!(d.kind, DiagnosticKind::User);
    }

    #[test]
    fn diagnostic_hint_builder() {
        let d = Diagnostic::hint(
            DiagCode::T005OperatorNotApplicable,
            "try cast",
            dummy_span(),
        );
        assert_eq!(d.severity, Severity::Hint);
        assert_eq!(d.kind, DiagnosticKind::User);
    }

    #[test]
    fn diagnostic_ice_builder() {
        let d = Diagnostic::ice(DiagCode::ICET001, "internal", dummy_span());
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.kind, DiagnosticKind::InternalCompilerError);
    }

    // ── Diagnostic builder: with_* ──

    #[test]
    fn diagnostic_with_label() {
        let d = Diagnostic::error(
            DiagCode::T002IncompatibleAssignment,
            "type mismatch",
            dummy_span(),
        )
        .with_label(Span::new(0, 2, 5), "here");
        assert_eq!(d.labels.len(), 1);
        assert_eq!(d.labels[0].message, "here");
        assert_eq!(d.labels[0].span, Span::new(0, 2, 5));
    }

    #[test]
    fn diagnostic_with_multiple_labels() {
        let d = Diagnostic::error(
            DiagCode::T002IncompatibleAssignment,
            "type mismatch",
            dummy_span(),
        )
        .with_label(Span::new(0, 2, 5), "a")
        .with_label(Span::new(0, 6, 8), "b");
        assert_eq!(d.labels.len(), 2);
    }

    #[test]
    fn diagnostic_with_note() {
        let d = Diagnostic::error(
            DiagCode::T002IncompatibleAssignment,
            "type mismatch",
            dummy_span(),
        )
        .with_note("consider adding a cast");
        assert_eq!(d.notes, vec!["consider adding a cast"]);
    }

    #[test]
    fn diagnostic_with_hint() {
        let d = Diagnostic::error(
            DiagCode::T002IncompatibleAssignment,
            "type mismatch",
            dummy_span(),
        )
        .with_hint("try using `as`");
        assert_eq!(d.hints.len(), 1);
        assert_eq!(d.hints[0].message, "try using `as`");
        assert_eq!(d.hints[0].replacement, None);
    }

    #[test]
    fn diagnostic_with_hint_replacement() {
        let hint = Hint {
            message: "replace with int".to_string(),
            replacement: Some(CodeReplacement {
                span: Span::new(0, 0, 3),
                new_text: "int".to_string(),
            }),
        };
        let d = Diagnostic::error(DiagCode::T010InvalidCast, "bad cast", dummy_span())
            .with_hint_replacement(hint);
        assert_eq!(d.hints.len(), 1);
        assert_eq!(d.hints[0].message, "replace with int");
        assert!(d.hints[0].replacement.is_some());
    }

    #[test]
    fn diagnostic_starts_empty() {
        let d = Diagnostic::error(DiagCode::T001CannotInferType, "x", dummy_span());
        assert!(d.labels.is_empty());
        assert!(d.notes.is_empty());
        assert!(d.hints.is_empty());
    }

    // ── format_for_cli: no registry ──

    #[test]
    fn format_no_registry() {
        let d = Diagnostic::error(
            DiagCode::T001CannotInferType,
            "cannot infer type of `x`",
            dummy_span(),
        );
        let out = d.format_for_cli(&SourceRegistry::new());
        assert_eq!(out, "T001: cannot infer type of `x`\n  --> 1:1");
    }

    #[test]
    fn format_with_registry() {
        let reg = registry_with("let x = 1;");
        let d = Diagnostic::warning(
            DiagCode::W001VariableAssignedNotUsed,
            "unused variable `x`",
            Span::new(0, 4, 5),
        );
        let out = d.format_for_cli(&reg);
        // Source: "let x = 1;" — byte 4 = line 1, col 5 (1-based)
        assert_eq!(out, "W001: unused variable `x`\n  --> test.arandu:1:5");
    }

    #[test]
    fn format_with_label() {
        let reg = registry_with("let x: int = 5;");
        let d = Diagnostic::error(
            DiagCode::T002IncompatibleAssignment,
            "type mismatch",
            Span::new(0, 0, 3),
        )
        .with_label(Span::new(0, 8, 11), "expected `int`");
        let out = d.format_for_cli(&reg);
        assert!(out.contains("expected `int`"));
        assert!(out.contains("label: 1:9-1:12"));
    }

    #[test]
    fn format_with_note() {
        let d = Diagnostic::error(
            DiagCode::T002IncompatibleAssignment,
            "mismatch",
            dummy_span(),
        )
        .with_note("try casting");
        let out = d.format_for_cli(&SourceRegistry::new());
        assert!(out.contains("note: try casting"));
    }

    #[test]
    fn format_with_hint() {
        let d = Diagnostic::error(
            DiagCode::T002IncompatibleAssignment,
            "mismatch",
            dummy_span(),
        )
        .with_hint("use `as`");
        let out = d.format_for_cli(&SourceRegistry::new());
        assert!(out.contains("hint: use `as`"));
    }

    #[test]
    fn format_with_hint_replacement() {
        let hint = Hint {
            message: "replace with int".to_string(),
            replacement: Some(CodeReplacement {
                span: Span::new(0, 0, 4),
                new_text: "int".to_string(),
            }),
        };
        let reg = registry_with("let x: str = 5;");
        let d = Diagnostic::error(
            DiagCode::T010InvalidCast,
            "invalid cast",
            Span::new(0, 0, 0),
        )
        .with_hint_replacement(hint);
        let out = d.format_for_cli(&reg);
        assert!(out.contains("replacement: at 1:1-1:5 with \"int\""));
    }

    #[test]
    fn format_ice_code_prefix() {
        let d = Diagnostic::ice(DiagCode::ICET001, "internal error", dummy_span());
        let out = d.format_for_cli(&SourceRegistry::new());
        assert!(out.starts_with("ICE-T-001: internal error"));
    }

    #[test]
    fn format_no_trailing_newline() {
        let d = Diagnostic::error(
            DiagCode::P001UnexpectedToken,
            "unexpected token",
            dummy_span(),
        );
        let out = d.format_for_cli(&SourceRegistry::new());
        assert!(
            !out.ends_with('\n'),
            "output should not have trailing newline"
        );
    }

    // ── Display ──

    #[test]
    fn display_outputs_message() {
        let d = Diagnostic::error(DiagCode::T001CannotInferType, "x", dummy_span());
        let display = d.to_string();
        assert_eq!(display, "x");
    }

    // ── miette::Diagnostic ──

    #[test]
    fn miette_diagnostic_url_and_help() {
        use miette::Diagnostic as _;

        let user_diag = Diagnostic::error(
            DiagCode::T002IncompatibleAssignment,
            "mismatch",
            dummy_span(),
        )
        .with_note("consider explicit cast")
        .with_hint("change type to int");

        assert_eq!(
            user_diag.url().map(|u| u.to_string()),
            Some("https://arandu-lang.dev/docs/errors/T002".to_string())
        );

        let help_text = user_diag.help().map(|h| h.to_string()).expect("help text");
        assert!(help_text.contains("note: consider explicit cast"));
        assert!(help_text.contains("change type to int"));

        let ice_diag = Diagnostic::ice(DiagCode::ICET001, "internal error", dummy_span());
        assert!(ice_diag.url().is_none());
    }
}
