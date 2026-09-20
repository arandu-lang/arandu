//! Canonical, byte-deterministic `.tar.gz` archive generator (RFC 0020).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::ArchiveOptions;
use flate2::GzBuilder;
use tar::{Builder, EntryType, Header};

pub fn create_tar_gz(opts: &ArchiveOptions) -> Result<(), String> {
    let source = opts.source.canonicalize().map_err(|e| {
        format!(
            "failed to canonicalize source '{}': {e}",
            opts.source.display()
        )
    })?;

    let root_name = match &opts.root {
        Some(r) => r.clone(),
        None => source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "source directory has no valid name".to_string())?
            .to_string(),
    };

    let paths = collect_and_sort_paths(&source)?;

    if let Some(parent) = opts.output.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "failed to create parent directory '{}': {e}",
                parent.display()
            )
        })?;
    }

    let raw_out = fs::File::create(&opts.output).map_err(|e| {
        format!(
            "failed to create output archive '{}': {e}",
            opts.output.display()
        )
    })?;

    let gz = GzBuilder::new()
        .mtime(opts.epoch as u32)
        .write(raw_out, flate2::Compression::best());

    let mut tar = Builder::new(gz);
    tar.mode(tar::HeaderMode::Deterministic);

    for path in &paths {
        let relative = path
            .strip_prefix(&source)
            .map_err(|e| format!("path strip error: {e}"))?;

        let arcname = if relative.as_os_str().is_empty() {
            root_name.clone()
        } else {
            let rel_str = relative
                .components()
                .map(|c| c.as_os_str().to_str().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("/");
            format!("{root_name}/{rel_str}")
        };

        let meta = fs::symlink_metadata(path)
            .map_err(|e| format!("failed to read metadata for '{}': {e}", path.display()))?;

        let mut header = Header::new_gnu();
        header.set_mtime(opts.epoch);
        header.set_uid(0);
        header.set_gid(0);
        header
            .set_username("")
            .map_err(|e| format!("set username error: {e}"))?;
        header
            .set_groupname("")
            .map_err(|e| format!("set groupname error: {e}"))?;

        if meta.file_type().is_symlink() {
            header.set_entry_type(EntryType::Symlink);
            header.set_mode(0o777);
            header.set_size(0);
            let target = fs::read_link(path)
                .map_err(|e| format!("failed to read symlink '{}': {e}", path.display()))?;
            header
                .set_link_name(&target)
                .map_err(|e| format!("failed to set link name: {e}"))?;
            header.set_cksum();
            tar.append_data(&mut header, &arcname, io::empty())
                .map_err(|e| format!("failed to append symlink '{arcname}': {e}"))?;
        } else if meta.is_dir() {
            header.set_entry_type(EntryType::Directory);
            header.set_mode(0o755);
            header.set_size(0);
            header.set_cksum();
            tar.append_data(&mut header, &arcname, io::empty())
                .map_err(|e| format!("failed to append dir '{arcname}': {e}"))?;
        } else {
            header.set_entry_type(EntryType::Regular);
            let mode = if arcname.contains("/bin/") || arcname.ends_with("/bin") {
                0o755
            } else {
                0o644
            };
            header.set_mode(mode);
            header.set_size(meta.len());
            header.set_cksum();
            let mut file = fs::File::open(path)
                .map_err(|e| format!("failed to open file '{}': {e}", path.display()))?;
            tar.append_data(&mut header, &arcname, &mut file)
                .map_err(|e| format!("failed to append file '{arcname}': {e}"))?;
        }
    }

    let gz = tar
        .into_inner()
        .map_err(|e| format!("failed to finish tar: {e}"))?;
    gz.finish()
        .map_err(|e| format!("failed to finish gzip compression: {e}"))?;

    Ok(())
}

fn collect_and_sort_paths(source: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    paths.push(source.to_path_buf());
    collect_dir_recursive(source, &mut paths)?;

    // Sort entries lexicographically by relative POSIX path
    paths.sort_by(|a, b| {
        let rel_a = a.strip_prefix(source).unwrap_or(a);
        let rel_b = b.strip_prefix(source).unwrap_or(b);
        let str_a = rel_a
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let str_b = rel_b
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        str_a.cmp(&str_b)
    });

    Ok(paths)
}

fn collect_dir_recursive(dir: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir)
        .map_err(|e| format!("failed to read directory '{}': {e}", dir.display()))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry error: {e}"))?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)
            .map_err(|e| format!("metadata error on '{}': {e}", path.display()))?;

        paths.push(path.clone());
        if meta.is_dir() && !meta.file_type().is_symlink() {
            collect_dir_recursive(&path, paths)?;
        }
    }
    Ok(())
}
