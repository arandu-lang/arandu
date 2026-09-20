//! Native deterministic archive packaging and validation for Arandu releases (RFC 0020).

pub mod tar;
pub mod validator;
pub mod zip;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    TarGz,
    Zip,
}

impl ArchiveKind {
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?;
        if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if name.ends_with(".zip") {
            Some(Self::Zip)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArchiveOptions {
    pub source: PathBuf,
    pub output: PathBuf,
    pub epoch: u64,
    pub root: Option<String>,
}

pub fn create_archive(opts: &ArchiveOptions) -> Result<(), String> {
    let kind = ArchiveKind::from_path(&opts.output).ok_or_else(|| {
        format!(
            "unrecognized archive extension for '{}'; expected .tar.gz or .zip",
            opts.output.display()
        )
    })?;

    match kind {
        ArchiveKind::TarGz => tar::create_tar_gz(opts),
        ArchiveKind::Zip => zip::create_zip(opts),
    }
}

pub fn validate_archive(
    archive: &Path,
    root: &str,
    version: &str,
    target: &str,
) -> Result<validator::ValidationStats, String> {
    let kind = ArchiveKind::from_path(archive).ok_or_else(|| {
        format!(
            "unrecognized archive extension for '{}'; expected .tar.gz or .zip",
            archive.display()
        )
    })?;

    match kind {
        ArchiveKind::TarGz => validator::validate_tar_gz(archive, root, version, target),
        ArchiveKind::Zip => validator::validate_zip(archive, root, version, target),
    }
}
