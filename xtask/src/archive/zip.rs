//! Canonical, byte-deterministic `.zip` archive generator for Windows SDK (RFC 0020).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::ArchiveOptions;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

pub fn create_zip(opts: &ArchiveOptions) -> Result<(), String> {
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

    let files = collect_and_sort_files(&source)?;

    if let Some(parent) = opts.output.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "failed to create parent directory '{}': {e}",
                parent.display()
            )
        })?;
    }

    let file = fs::File::create(&opts.output).map_err(|e| {
        format!(
            "failed to create output zip archive '{}': {e}",
            opts.output.display()
        )
    })?;

    let mut zip = ZipWriter::new(file);

    // Convert epoch to Zip DateTime (min 1980-01-01)
    let dos_dt = epoch_to_zip_datetime(opts.epoch);

    for path in &files {
        let relative = path
            .strip_prefix(&source)
            .map_err(|e| format!("path strip error: {e}"))?;

        let rel_str = relative
            .components()
            .map(|c| c.as_os_str().to_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("/");
        let arcname = format!("{root_name}/{rel_str}");

        let mode = if arcname.contains("/bin/") {
            0o755
        } else {
            0o644
        };

        let options: SimpleFileOptions = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(mode)
            .last_modified_time(dos_dt);

        zip.start_file(&arcname, options)
            .map_err(|e| format!("failed to start zip entry '{arcname}': {e}"))?;

        let mut input = fs::File::open(path)
            .map_err(|e| format!("failed to open file '{}': {e}", path.display()))?;
        io::copy(&mut input, &mut zip).map_err(|e| {
            format!(
                "failed to stream file '{}' into zip entry '{arcname}': {e}",
                path.display()
            )
        })?;
    }

    zip.finish()
        .map_err(|e| format!("failed to finish zip archive: {e}"))?;

    Ok(())
}

fn collect_and_sort_files(source: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    collect_files_recursive(source, &mut files)?;

    files.sort_by(|a, b| {
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

    Ok(files)
}

fn collect_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir)
        .map_err(|e| format!("failed to read directory '{}': {e}", dir.display()))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry error: {e}"))?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)
            .map_err(|e| format!("metadata error on '{}': {e}", path.display()))?;

        if meta.is_file() {
            files.push(path);
        } else if meta.is_dir() && !meta.file_type().is_symlink() {
            collect_files_recursive(&path, files)?;
        }
    }
    Ok(())
}

fn epoch_to_zip_datetime(epoch: u64) -> DateTime {
    // clamp to at least 1980-01-01 00:00:00 (315532800)
    let epoch = epoch.max(315532800);
    // Simple calendar conversion from unix epoch (seconds since 1970-01-01)
    let secs = epoch;
    let days = (secs / 86400) as i64;
    let rem_secs = (secs % 86400) as u32;

    let hour = (rem_secs / 3600) as u8;
    let minute = ((rem_secs % 3600) / 60) as u8;
    let second = (rem_secs % 60) as u8;

    let (year, month, day) = days_to_civil(days);

    DateTime::from_date_and_time(year as u16, month, day, hour, minute, second).unwrap_or_default()
}

// Howard Hinnant's civil days to year/month/day
fn days_to_civil(days: i64) -> (i64, u8, u8) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + (if m <= 2 { 1 } else { 0 });
    (y, m as u8, d as u8)
}
