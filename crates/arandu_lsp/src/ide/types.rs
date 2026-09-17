//! Document snapshot types and structured diagnostic payload schemas.

use std::path::PathBuf;
use std::sync::Arc;

use arandu_query::SourceFile;
use lsp_types::Uri;
use serde::{Deserialize, Serialize};

/// Snapshot of open docs for multi-file IDE features.
#[derive(Clone)]
pub struct DocSnap {
    pub source: SourceFile,
    pub path: Arc<PathBuf>,
    pub uri: Uri,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticFixData {
    pub title: String,
    pub uri: Uri,
    pub range: lsp_types::Range,
    pub new_text: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticData {
    pub notes: Vec<String>,
    pub hints: Vec<String>,
    pub fixes: Vec<DiagnosticFixData>,
}

/// Maximum number of workspace symbols returned to the client.
pub const MAX_WORKSPACE_SYMBOLS: usize = 200;
