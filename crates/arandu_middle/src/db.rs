use std::path::PathBuf;
use std::sync::Arc;

use crate::layout::DataLayout;

pub type FileId = u32;

#[salsa::input]
pub struct SourceFile {
    pub file_id: FileId,
    pub text: Arc<str>,
    pub path: Arc<PathBuf>,
}

/// Salsa input for the compilation target's canonical data layout.
///
/// Defaults to `DataLayout::host()`; the CLI sets it from `--layout=` before
/// running any semantic query. Type checking, MIR lowering and backends read
/// pointer width / alignment classes from here instead of assuming the host.
#[salsa::input]
pub struct TargetConfig {
    pub data_layout: DataLayout,
    pub identity: TargetIdentity,
}

/// The common database trait used by middle-end crates (resolve, typeck)
/// to request data from the Salsa database without knowing about `arandu_query`.
pub trait SourceDatabase: salsa::Database {
    fn source_file_by_id(&self, file_id: FileId) -> Option<SourceFile>;
    fn exported_symbols(&self, file: SourceFile) -> Arc<crate::ExportedSymbolTable>;

    /// Package-scoped exports are queried separately to preserve public early-cutoff.
    fn internal_symbols(&self, file: SourceFile) -> Arc<crate::ExportedSymbolTable>;

    /// Compare package identity for two registered files. Missing package metadata fails closed.
    fn same_package(&self, current_file: FileId, imported_file: SourceFile) -> bool;

    /// Retrieves the exact lexical span of a symbol for diagnostics (prevents Span from breaking early cutoff).
    fn symbol_span(&self, symbol_id: crate::SymbolId) -> crate::Span;

    /// Maps a module import path to a Salsa SourceFile.
    fn resolve_module_path(&self, path: &str) -> Option<SourceFile>;

    /// Whether imports are interpreted under a discovered package manifest.
    fn package_mode(&self) -> bool {
        false
    }
}

/// Diagnostic accumulator for Salsa.
#[salsa::accumulator]
pub struct DiagnosticsAccumulator(pub crate::Diagnostic);

/// Explicit platform identity supplied by the compilation driver. Semantic
/// queries never inspect the environment to obtain these values.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TargetIdentity {
    pub os: String,
    pub arch: String,
    /// Original triple when the driver supplied one. Empty for [`Self::host`].
    pub triple: String,
}

impl TargetIdentity {
    /// Decode the platform fields of an explicitly supplied target triple.
    /// The driver also supplies its ABI layout; this never guesses that ABI.
    pub fn from_triple(triple: &str) -> Result<Self, &'static str> {
        let mut parts = triple.split('-');
        let arch = parts
            .next()
            .filter(|part| !part.is_empty())
            .ok_or("target triple has no architecture")?;
        let rest: Vec<_> = parts.collect();
        let short_wasi = matches!(arch, "wasm32" | "wasm64")
            && rest.len() == 1
            && matches!(rest[0], "wasi" | "wasip1" | "wasip2");
        if (rest.len() < 2 && !short_wasi) || rest.iter().any(|part| part.is_empty()) {
            return Err("target requires architecture/vendor/OS or a WASI target spelling");
        }
        let os = rest
            .iter()
            .rev()
            .find_map(|part| match *part {
                "linux" => Some("linux"),
                "windows" => Some("windows"),
                "darwin" | "macos" => Some("macos"),
                "wasi" | "wasip1" | "wasip2" => Some("wasi"),
                "freebsd" => Some("freebsd"),
                "netbsd" => Some("netbsd"),
                "openbsd" => Some("openbsd"),
                "android" => Some("android"),
                "ios" => Some("ios"),
                "none" => Some("none"),
                _ => None,
            })
            .or_else(|| {
                rest.last()
                    .filter(|part| **part == "unknown")
                    .map(|_| "unknown")
            })
            .ok_or("target triple has an unsupported OS identity")?;
        Ok(Self {
            os: os.into(),
            arch: arch.into(),
            triple: triple.to_owned(),
        })
    }

    /// `none` is the freestanding OS token (`thumbv7em-none-eabihf`).
    /// Wasm stays `wasi` or `unknown` and is not bare-metal.
    #[must_use]
    pub fn is_bare_metal(&self) -> bool {
        self.os == "none"
    }

    /// Native driver default; call outside semantic queries.
    #[must_use]
    pub fn host() -> Self {
        Self {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            triple: String::new(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod target_identity_tests {
    use super::TargetIdentity;

    #[test]
    fn explicit_triples_preserve_architecture_and_recognize_platform_suffixes() {
        for (triple, arch, os) in [
            ("x86_64-unknown-linux-gnu", "x86_64", "linux"),
            ("aarch64-apple-darwin", "aarch64", "macos"),
            ("aarch64-linux-android", "aarch64", "android"),
            ("x86_64-pc-windows-msvc", "x86_64", "windows"),
            ("wasm32-wasi", "wasm32", "wasi"),
            ("wasm32-wasip1", "wasm32", "wasi"),
            ("wasm32-wasip2", "wasm32", "wasi"),
            ("wasm32-unknown-unknown", "wasm32", "unknown"),
            ("thumbv7em-none-eabihf", "thumbv7em", "none"),
            ("riscv32imc-unknown-none-elf", "riscv32imc", "none"),
        ] {
            let identity = TargetIdentity::from_triple(triple).expect("supported target identity");
            assert_eq!(identity.arch, arch);
            assert_eq!(identity.os, os);
            assert_eq!(identity.triple, triple);
            assert_eq!(identity.is_bare_metal(), os == "none");
        }
        let host = TargetIdentity::host();
        assert!(host.triple.is_empty());
        assert!(!host.is_bare_metal());
        for invalid in [
            "",
            "wasm32",
            "-unknown-linux",
            "x86_64--linux",
            "x86_64-unknown-newos",
        ] {
            assert!(TargetIdentity::from_triple(invalid).is_err());
        }
    }
}
