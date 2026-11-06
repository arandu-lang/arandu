//! Wire format between `arandu build` and its daemon (HOT.3): one frame is
//! `[magic 4B][version 2B][length 4B][payload]`, the payload JSON. A frame
//! that does not start with the magic, carries another version or exceeds
//! [`MAX_PAYLOAD`] is rejected, and the client falls back to a batch build.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::output::{RenderStyle, Stream};

const MAGIC: [u8; 4] = *b"ARDN";
pub const VERSION: u16 = 1;
/// Diagnostics of a large failed build fit; a corrupt length cannot make
/// either side allocate without bound.
const MAX_PAYLOAD: u32 = 64 << 20;

pub fn write_frame(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    let payload = serde_json::to_vec(value).map_err(io::Error::other)?;
    let length = u32::try_from(payload.len())
        .ok()
        .filter(|length| *length <= MAX_PAYLOAD)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    let mut frame = Vec::with_capacity(10 + payload.len());
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&VERSION.to_le_bytes());
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&payload);
    writer.write_all(&frame)?;
    writer.flush()
}

pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> io::Result<T> {
    let mut header = [0; 10];
    reader.read_exact(&mut header)?;
    let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidData, message.to_owned());
    if header[..4] != MAGIC {
        return Err(invalid("not an Arandu daemon frame"));
    }
    if u16::from_le_bytes([header[4], header[5]]) != VERSION {
        return Err(invalid("daemon protocol version differs"));
    }
    let length = u32::from_le_bytes([header[6], header[7], header[8], header[9]]);
    if length > MAX_PAYLOAD {
        return Err(invalid("daemon frame exceeds its size limit"));
    }
    let mut payload = vec![0; length as usize];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(|error| invalid(&error.to_string()))
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// One `arandu build`: its arguments, as given, relative to `cwd`.
    Build {
        identity: String,
        cwd: PathBuf,
        args: Vec<String>,
        style: RenderStyle,
    },
    /// Ends the daemon once its builds finish.
    Shutdown { identity: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    /// Sent every second while the build runs; a silent daemon is gone.
    Heartbeat,
    /// The daemon runs another toolchain or environment and is shutting down.
    Mismatch,
    /// The build is one the daemon does not run (another backend).
    Unsupported,
    Output {
        stream: Stream,
        text: String,
    },
    Done(Outcome),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Outcome {
    Success { exit_code: i32 },
    Failure(FailureDto),
}

impl Outcome {
    pub fn of(result: &CliResult) -> Self {
        match result {
            Ok(success) => Self::Success {
                exit_code: match success {
                    CliSuccess::Done => 0,
                    CliSuccess::ProgramExit(code) => *code,
                },
            },
            Err(failure) => Self::Failure(FailureDto::of(failure)),
        }
    }

    pub fn into_result(self) -> CliResult {
        match self {
            Self::Success { exit_code: 0 } => Ok(CliSuccess::Done),
            Self::Success { exit_code } => Ok(CliSuccess::ProgramExit(exit_code)),
            Self::Failure(failure) => Err(failure.into_failure()),
        }
    }
}

/// A [`CliFailure`] that crosses the socket; the client renders it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FailureDto {
    Usage {
        message: String,
    },
    Operational {
        operation: String,
        context: Option<PathBuf>,
        source: String,
    },
    Diagnostics {
        diagnostics: Vec<DiagnosticDto>,
        source_path: Option<PathBuf>,
    },
}

impl FailureDto {
    fn of(failure: &CliFailure) -> Self {
        match failure {
            CliFailure::Usage { message } => Self::Usage {
                message: message.clone(),
            },
            CliFailure::Operational {
                operation,
                context,
                source,
            } => Self::Operational {
                operation: operation.to_string(),
                context: context.clone(),
                source: source.clone(),
            },
            CliFailure::Diagnostics {
                diagnostics,
                source_path,
            } => Self::Diagnostics {
                diagnostics: diagnostics.iter().map(DiagnosticDto::of).collect(),
                source_path: source_path.clone(),
            },
        }
    }

    fn into_failure(self) -> CliFailure {
        match self {
            Self::Usage { message } => CliFailure::Usage { message },
            Self::Operational {
                operation,
                context,
                source,
            } => CliFailure::Operational {
                operation: operation.into(),
                context,
                source,
            },
            Self::Diagnostics {
                diagnostics,
                source_path,
            } => CliFailure::Diagnostics {
                diagnostics: diagnostics
                    .into_iter()
                    .map(DiagnosticDto::into_diagnostic)
                    .collect(),
                source_path,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanDto {
    pub file_id: u32,
    pub start: u32,
    pub end: u32,
}

impl From<arandu_middle::Span> for SpanDto {
    fn from(span: arandu_middle::Span) -> Self {
        Self {
            file_id: span.file_id,
            start: span.start,
            end: span.end,
        }
    }
}

impl From<SpanDto> for arandu_middle::Span {
    fn from(span: SpanDto) -> Self {
        Self::new(span.file_id, span.start, span.end)
    }
}

/// Every field of a diagnostic, so the client renders labels, notes, hints
/// and replacements exactly as a batch build would.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticDto {
    pub code: String,
    pub severity: SeverityDto,
    pub internal: bool,
    pub message: String,
    pub span: SpanDto,
    pub primary_label: Option<String>,
    pub labels: Vec<(SpanDto, String)>,
    pub notes: Vec<String>,
    pub hints: Vec<(String, Option<(SpanDto, String)>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeverityDto {
    Error,
    Warning,
    Note,
    Hint,
}

impl DiagnosticDto {
    fn of(diagnostic: &arandu_middle::Diagnostic) -> Self {
        use arandu_middle::Severity;
        use arandu_middle::diagnostics::DiagnosticKind;
        Self {
            code: diagnostic.code.as_str().to_owned(),
            severity: match diagnostic.severity {
                Severity::Error => SeverityDto::Error,
                Severity::Warning => SeverityDto::Warning,
                Severity::Note => SeverityDto::Note,
                Severity::Hint => SeverityDto::Hint,
            },
            internal: diagnostic.kind == DiagnosticKind::InternalCompilerError,
            message: diagnostic.message.clone(),
            span: diagnostic.span.into(),
            primary_label: diagnostic.primary_label.as_deref().cloned(),
            labels: diagnostic
                .labels
                .iter()
                .map(|label| (label.span.into(), label.message.clone()))
                .collect(),
            notes: diagnostic.notes.clone(),
            hints: diagnostic
                .hints
                .iter()
                .map(|hint| {
                    (
                        hint.message.clone(),
                        hint.replacement.as_ref().map(|replacement| {
                            (replacement.span.into(), replacement.new_text.clone())
                        }),
                    )
                })
                .collect(),
        }
    }

    /// A code this toolchain does not know cannot come from the same binary;
    /// it becomes an internal error that keeps the message.
    fn into_diagnostic(self) -> arandu_middle::Diagnostic {
        use arandu_middle::diagnostics::DiagnosticKind;
        use arandu_middle::{DiagCode, Severity};
        let code = DiagCode::ALL
            .iter()
            .copied()
            .find(|code| code.as_str() == self.code);
        arandu_middle::Diagnostic {
            code: code.unwrap_or(DiagCode::ICEL001),
            severity: match self.severity {
                SeverityDto::Error => Severity::Error,
                SeverityDto::Warning => Severity::Warning,
                SeverityDto::Note => Severity::Note,
                SeverityDto::Hint => Severity::Hint,
            },
            kind: if self.internal || code.is_none() {
                DiagnosticKind::InternalCompilerError
            } else {
                DiagnosticKind::User
            },
            message: self.message,
            span: self.span.into(),
            primary_label: self.primary_label.map(Box::new),
            labels: self
                .labels
                .into_iter()
                .map(|(span, message)| arandu_middle::Label {
                    span: span.into(),
                    message,
                })
                .collect(),
            notes: self.notes,
            hints: self
                .hints
                .into_iter()
                .map(|(message, replacement)| arandu_middle::Hint {
                    message,
                    replacement: replacement.map(|(span, new_text)| {
                        arandu_middle::CodeReplacement {
                            span: span.into(),
                            new_text,
                        }
                    }),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_reject_foreign_or_oversized_input() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &Response::Unsupported).unwrap();
        assert!(matches!(
            read_frame::<Response>(&mut bytes.as_slice()).unwrap(),
            Response::Unsupported
        ));

        let mut foreign = bytes.clone();
        foreign[0] = b'X';
        assert!(read_frame::<Response>(&mut foreign.as_slice()).is_err());
        let mut other_version = bytes.clone();
        other_version[4] = 0xFF;
        assert!(read_frame::<Response>(&mut other_version.as_slice()).is_err());
        let mut oversized = bytes.clone();
        oversized[6..10].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_frame::<Response>(&mut oversized.as_slice()).is_err());
        assert!(read_frame::<Response>(&mut &bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn a_diagnostic_crosses_the_socket_with_every_field() {
        use arandu_middle::{DiagCode, Diagnostic, Span};
        let diagnostic = Diagnostic::error(
            DiagCode::T004IncompatibleReturnType,
            "mismatched types",
            Span::new(3, 10, 14),
        )
        .with_primary_label("found bool")
        .with_label(Span::new(3, 1, 4), "expected int")
        .with_note("a note")
        .with_hint("a hint");
        let failure = CliFailure::diagnostics([diagnostic.clone()], Some(PathBuf::from("a.aru")));
        let dto = FailureDto::of(&failure);
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &dto).unwrap();
        let back = read_frame::<FailureDto>(&mut bytes.as_slice())
            .unwrap()
            .into_failure();
        let CliFailure::Diagnostics {
            diagnostics,
            source_path,
        } = back
        else {
            panic!("diagnostics failure");
        };
        assert_eq!(diagnostics, [diagnostic]);
        assert_eq!(source_path, Some(PathBuf::from("a.aru")));
    }
}
