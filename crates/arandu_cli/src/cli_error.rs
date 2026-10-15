use std::path::PathBuf;

#[derive(Debug)]
pub enum CliSuccess {
    Done,
    ProgramExit(i32),
}

impl CliSuccess {
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Done => 0,
            Self::ProgramExit(code) => code,
        }
    }
}

#[derive(Debug)]
pub enum CliFailure {
    Usage {
        message: String,
    },
    Operational {
        operation: &'static str,
        context: Option<PathBuf>,
        source: String,
    },
    Diagnostics {
        diagnostics: Vec<arandu_middle::Diagnostic>,
        source_path: Option<PathBuf>,
    },
}

impl std::fmt::Display for CliFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage { message } => formatter.write_str(message),
            Self::Operational {
                operation,
                context,
                source,
            } => {
                write!(formatter, "{operation}")?;
                if let Some(path) = context {
                    write!(formatter, " {}", path.display())?;
                }
                write!(formatter, ": {source}")
            }
            Self::Diagnostics { diagnostics, .. } => {
                for (index, diagnostic) in diagnostics.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str("\n")?;
                    }
                    write!(formatter, "{}: {}", diagnostic.code, diagnostic.message)?;
                }
                Ok(())
            }
        }
    }
}

impl CliFailure {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Usage { .. } => 2,
            Self::Operational { .. } | Self::Diagnostics { .. } => 1,
        }
    }

    #[must_use]
    pub fn usage(message: impl Into<String>) -> Self {
        Self::Usage {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn operational(
        operation: &'static str,
        context: Option<PathBuf>,
        source: impl Into<String>,
    ) -> Self {
        Self::Operational {
            operation,
            context,
            source: source.into(),
        }
    }

    #[must_use]
    pub fn diagnostics(
        diagnostics: impl IntoIterator<Item = arandu_middle::Diagnostic>,
        source_path: Option<PathBuf>,
    ) -> Self {
        Self::Diagnostics {
            diagnostics: diagnostics.into_iter().collect(),
            source_path,
        }
    }

    pub fn render(&self) {
        match self {
            Self::Usage { message } => eprintln!("{message}"),
            Self::Operational {
                operation,
                context,
                source,
            } => match context {
                Some(path) => eprintln!("error: {operation} {}: {source}", path.display()),
                None => eprintln!("error: {operation}: {source}"),
            },
            Self::Diagnostics {
                diagnostics,
                source_path,
            } => {
                let path = source_path
                    .as_deref()
                    .unwrap_or_else(|| std::path::Path::new(""));
                let source = std::fs::read_to_string(path).unwrap_or_default();
                for diagnostic in diagnostics {
                    let mut diag = diagnostic.clone();
                    if diag.is_ice() {
                        attach_ice_report_metadata(&mut diag, source_path.as_deref());
                    }
                    let source_len = source.len() as u32;
                    if diag.span.end > source_len || diag.span.start > source_len {
                        diag.span.start = diag.span.start.min(source_len);
                        diag.span.end = diag.span.end.min(source_len);
                    }
                    for label in &mut diag.labels {
                        if label.span.end > source_len || label.span.start > source_len {
                            label.span.start = label.span.start.min(source_len);
                            label.span.end = label.span.end.min(source_len);
                        }
                    }
                    let named_source =
                        miette::NamedSource::new(path.to_string_lossy(), source.clone());
                    let report = miette::Report::new(diag).with_source_code(named_source);
                    eprintln!("{report:?}");
                }
            }
        }
    }
}

/// Enriches an ICE diagnostic with compiler version info, platform target, and a pre-filled issue URL.
fn attach_ice_report_metadata(
    diag: &mut arandu_middle::Diagnostic,
    source_path: Option<&std::path::Path>,
) {
    let phase = diag.code.as_str();
    let version = env!("CARGO_PKG_VERSION");
    let target_os = std::env::consts::OS;
    let target_arch = std::env::consts::ARCH;

    let path_display = source_path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());

    let title = format!("ICE in {}: {}", phase, diag.message);
    let title_encoded = urlencoding_simple(&title);
    let body = format!(
        "### Internal Compiler Error Report\n\n\
        - **Arandu Version:** `{version}`\n\
        - **Platform:** `{target_os} / {target_arch}`\n\
        - **Phase / Code:** `{phase}`\n\
        - **Source:** `{path_display}`\n\n\
        ### Message\n```\n{}\n```\n",
        diag.message
    );
    let body_encoded = urlencoding_simple(&body);
    let issue_url = format!(
        "https://github.com/arandu-lang/arandu/issues/new?title={title_encoded}&body={body_encoded}"
    );

    diag.notes
        .push("this is an internal compiler bug, not an error in your code".to_string());
    diag.notes
        .push(format!("arandu v{version} ({target_os}-{target_arch})"));
    diag.hints.push(arandu_middle::Hint {
        message: format!("we would appreciate a bug report: {issue_url}"),
        replacement: None,
    });
}

fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

pub type CliResult = Result<CliSuccess, CliFailure>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_arandu_failure_classes() {
        assert_eq!(CliSuccess::Done.exit_code(), 0);
        assert_eq!(CliSuccess::ProgramExit(42).exit_code(), 42);
        assert_eq!(CliFailure::usage("bad flag").exit_code(), 2);
        assert_eq!(
            CliFailure::operational("read", None, "missing").exit_code(),
            1
        );
        assert_eq!(CliFailure::diagnostics([], None).exit_code(), 1);
    }

    #[test]
    fn ice_attaches_clean_notes_and_issue_url() {
        let mut diag = arandu_middle::Diagnostic::ice(
            arandu_middle::DiagCode::ICET001,
            "type resolution loop",
            arandu_base::Span::new(0, 10, 20),
        );
        attach_ice_report_metadata(&mut diag, Some(std::path::Path::new("src/main.aru")));
        assert!(
            diag.notes
                .iter()
                .any(|n| n.contains("internal compiler bug"))
        );
        assert!(diag.hints.iter().any(|h| {
            h.message
                .contains("https://github.com/arandu-lang/arandu/issues/new")
        }));
    }

    #[test]
    fn urlencoding_handles_special_characters() {
        assert_eq!(urlencoding_simple("hello world"), "hello+world");
        assert_eq!(urlencoding_simple("a/b:c"), "a%2Fb%3Ac");
    }
}
