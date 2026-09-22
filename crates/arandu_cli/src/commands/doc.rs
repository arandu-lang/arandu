//! Documentation command execution (`arandu doc`).

use std::fs;
use std::path::PathBuf;

use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::pipeline::fail_usage;
use crate::project::ProjectFlags;
use arandu_middle::layout::DataLayout;
use arandu_query::db::DatabaseImpl;

pub fn cmd_doc(args: &[String], _flags: &ProjectFlags, data_layout: DataLayout) -> CliResult {
    let mut format = "html";
    let mut out_dir = PathBuf::from("doc");
    let mut open = false;
    let mut target_path: Option<PathBuf> = None;

    for arg in &args[2..] {
        if let Some(fmt) = arg.strip_prefix("--format=") {
            format = fmt;
        } else if let Some(dir) = arg.strip_prefix("--out-dir=") {
            out_dir = PathBuf::from(dir);
        } else if arg == "--open" {
            open = true;
        } else if !arg.starts_with('-') && target_path.is_none() {
            target_path = Some(PathBuf::from(arg));
        } else {
            fail_usage(format!("unknown or unexpected option for doc: {arg}"));
        }
    }

    if !matches!(format, "html" | "json" | "md" | "markdown") {
        fail_usage(format!(
            "invalid format '{format}': expected 'html', 'json', or 'md'"
        ));
    }

    let raw_target = target_path.unwrap_or_else(|| PathBuf::from("."));
    // Resolve the path relative to cwd so that `arandu doc stdlib/` and
    // `arandu doc /abs/stdlib/` produce identical results regardless of how
    // the caller spells the path (fixes: relative path yields 0 overviews).
    let target = fs::canonicalize(&raw_target).map_err(|e| {
        CliFailure::operational(
            "resolve documentation target path",
            Some(raw_target.clone()),
            format!(
                "{e} — make sure the path exists (tried: {})",
                raw_target.display()
            ),
        )
    })?;
    fs::create_dir_all(&out_dir).map_err(|e| {
        CliFailure::operational(
            "create documentation output directory",
            Some(out_dir.clone()),
            e.to_string(),
        )
    })?;

    let mut db = DatabaseImpl::new();
    db.set_target_config(data_layout);

    // Documentation signatures resolve imported stdlib types through the
    // same registered source files as compilation. Point the DB at the root
    // before querying any module so names such as `io.IoError` render instead
    // of `<error>` in function signatures and struct fields.
    let stdlib_root = target
        .ancestors()
        .find(|path| arandu_query::is_stdlib_root(path));
    if let Some(root) = stdlib_root {
        db.set_stdlib_root(root.to_path_buf());
        if target.is_file() {
            crate::pipeline::register_stdlib_sources(&mut db, root);
        }
    }

    let mut files_to_doc = Vec::new();
    if target.is_file() {
        let text = fs::read_to_string(&target).map_err(|e| {
            CliFailure::operational("read source file", Some(target.clone()), e.to_string())
        })?;
        let key = target.to_string_lossy().to_string();
        let source_file = db
            .source_file_by_path(&key)
            .unwrap_or_else(|| db.new_file(key, text));
        files_to_doc.push((target.clone(), source_file));
    } else {
        let entries = arandu_query::scan_aru_entries(&target);
        if entries.is_empty() {
            return Err(CliFailure::operational(
                "discover documentation sources",
                Some(target),
                "no .aru source files found to document".to_string(),
            ));
        }
        for rel in entries {
            let full_path = target.join(&rel);
            if full_path.is_file() {
                let text = fs::read_to_string(&full_path).map_err(|e| {
                    CliFailure::operational(
                        "read source file",
                        Some(full_path.clone()),
                        e.to_string(),
                    )
                })?;
                let key = full_path.to_string_lossy().to_string();
                let source_file = db.new_file(key, text);
                files_to_doc.push((full_path, source_file));
            }
        }
    }

    let mut first_html_path: Option<PathBuf> = None;

    for (path, source_file) in files_to_doc {
        let module_doc = arandu_query::module_doc(&db, source_file);

        let ext = match format {
            "json" => "json",
            "md" | "markdown" => "md",
            _ => "html",
        };

        let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("index");

        let out_filename = if module_doc.name == "main" || module_doc.name == "unknown" {
            format!("{file_stem}.{ext}")
        } else {
            format!("{}.{ext}", module_doc.name.replace('/', "."))
        };

        let out_file_path = out_dir.join(&out_filename);

        let content = match format {
            "json" => arandu_doc::render_json(module_doc.as_ref(), true).map_err(|e| {
                CliFailure::operational(
                    "render json documentation",
                    Some(out_file_path.clone()),
                    e.to_string(),
                )
            })?,
            "md" | "markdown" => arandu_doc::render_markdown(module_doc.as_ref()),
            _ => {
                if first_html_path.is_none() {
                    first_html_path = Some(out_file_path.clone());
                }
                arandu_doc::render_html(module_doc.as_ref())
            }
        };

        fs::write(&out_file_path, content).map_err(|e| {
            CliFailure::operational(
                "write documentation file",
                Some(out_file_path.clone()),
                e.to_string(),
            )
        })?;

        println!(
            "Documentado `{}` -> {}",
            module_doc.name,
            out_file_path.display()
        );
    }

    if open && let Some(html_path) = first_html_path {
        println!("Abrindo {}", html_path.display());
        #[cfg(target_os = "linux")]
        let _ = std::process::Command::new("xdg-open")
            .arg(html_path)
            .spawn();
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(html_path).spawn();
        #[cfg(target_os = "windows")]
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", html_path.to_str().unwrap_or("")])
            .spawn();
    }

    Ok(CliSuccess::Done)
}
