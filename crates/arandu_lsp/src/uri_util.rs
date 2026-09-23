//! Helpers for `lsp_types::Uri` (lsp-types ≥ 0.97 replaced `url::Url`).
//!
//! The newtype no longer exposes `to_file_path` / `from_file_path`; we convert
//! via the `file://` string form with minimal percent-encoding.

use lsp_types::Uri;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Parse a URI string (`file:///…` or generic).
#[must_use]
pub fn parse_uri(s: &str) -> Option<Uri> {
    Uri::from_str(s).ok()
}

/// Convert a filesystem path to an LSP `file://` URI.
#[must_use]
pub fn uri_from_path(path: &Path) -> Option<Uri> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let normalized = normalized_path_text(&abs)?;
    let mut out = if normalized.starts_with('/') {
        String::from("file://")
    } else {
        String::from("file:///")
    };
    for &b in normalized.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b':' | b'.' | b'_' | b'-' | b'~' => {
                out.push(b as char)
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    Uri::from_str(&out).ok()
}

fn normalized_path_text(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    #[cfg(windows)]
    {
        let text = text.strip_prefix(r"\\?\UNC\").map_or_else(
            || {
                text.strip_prefix(r"\\?\")
                    .unwrap_or(text)
                    .replace('\\', "/")
            },
            |unc| format!("//{}", unc.replace('\\', "/")),
        );
        Some(text)
    }
    #[cfg(not(windows))]
    {
        Some(text.to_string())
    }
}

/// Convert an LSP URI to a filesystem path (best-effort for `file://`).
#[must_use]
pub fn path_from_uri(uri: &Uri) -> PathBuf {
    let s = uri.as_str();
    if let Some(rest) = s.strip_prefix("file://") {
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        decoded_file_path(rest)
    } else if let Some(rest) = s.strip_prefix("file:") {
        decoded_file_path(rest)
    } else {
        PathBuf::from(s)
    }
}

fn decoded_file_path(encoded: &str) -> PathBuf {
    let decoded = percent_decode(encoded);
    #[cfg(windows)]
    let decoded = {
        let decoded = decoded.strip_prefix("//?/UNC/").map_or_else(
            || decoded.strip_prefix("//?/").unwrap_or(&decoded).to_string(),
            |unc| format!("//{unc}"),
        );
        decoded
            .strip_prefix('/')
            .filter(|path| {
                let bytes = path.as_bytes();
                bytes.len() >= 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && bytes[2] == b'/'
            })
            .unwrap_or(&decoded)
            .to_string()
    };
    PathBuf::from(decoded)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_absolute_path() {
        let p = std::env::temp_dir().join("hello world.aru");
        let uri = uri_from_path(&p).expect("uri");
        assert!(uri.as_str().starts_with("file://"));
        assert!(
            uri.as_str().contains("hello%20world.aru"),
            "spaces in filesystem paths must be URI-escaped: {}",
            uri.as_str()
        );
        let back = path_from_uri(&uri);
        assert_eq!(back, p);
    }

    #[test]
    fn parse_file_uri() {
        let u = parse_uri("file:///home/user/a.aru").expect("parse");
        assert_eq!(path_from_uri(&u), PathBuf::from("/home/user/a.aru"));
    }

    #[cfg(windows)]
    #[test]
    fn standard_windows_file_uri_drops_uri_root_slash() {
        let uri = parse_uri("file:///C:/workspace/a.aru").expect("parse Windows URI");
        assert_eq!(path_from_uri(&uri), PathBuf::from("C:/workspace/a.aru"));
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_and_standard_windows_paths_have_one_uri_identity() {
        let standard = PathBuf::from("C:/workspace/a.aru");
        let verbatim = PathBuf::from(r"\\?\C:\workspace\a.aru");
        assert_eq!(uri_from_path(&standard), uri_from_path(&verbatim));
        assert_eq!(
            uri_from_path(&standard).expect("standard URI").as_str(),
            "file:///C:/workspace/a.aru"
        );
    }

    #[test]
    fn malformed_uri_is_rejected_without_panicking() {
        assert!(parse_uri("not a uri with spaces and %zz").is_none());
    }
}
