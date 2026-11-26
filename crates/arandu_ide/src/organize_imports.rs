//! Organize imports action (`source.organizeImports`) shared across IDE clients.

use std::collections::{BTreeMap, BTreeSet};

use arandu_middle::{DiagCode, Span, Visibility};
use arandu_parser::{ImportDecl, ImportItem};
use arandu_query::{AnalysisSnapshot, SourceFile};

use crate::TextEdit;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ImportBucket {
    Std = 0,
    External = 1,
    Local = 2,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FormattedEntry {
    sort_key: String,
    rendered: String,
}

/// Organizes top-level imports in `text` into three sorted groups (`std.*`, external/package deps,
/// `self.*`/local), removes `W007` unused imports, merges sibling module imports into grouped
/// `import prefix.{a, b}` syntax, and preserves `public use`/`internal use` re-exports intact.
///
/// Returns `None` when the file has syntax errors, no imports, comments or non-import items inside
/// the import block, or when imports are already organized.
#[must_use]
pub fn organize_imports(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
) -> Option<TextEdit> {
    let parsed = arandu_query::passes::parse(&snap.db, source);
    let Ok(program) = parsed.as_ref() else {
        return None;
    };
    if program.imports.is_empty() {
        return None;
    }

    let first_start = program
        .imports
        .iter()
        .map(|imp| imp.span().start as usize)
        .min()?;
    let last_end = program
        .imports
        .iter()
        .map(|imp| imp.span().end as usize)
        .max()?;
    if first_start >= last_end || last_end > text.len() {
        return None;
    }

    // Bail out if non-import declarations (or comments) sit inside the import range.
    let mut interleaved = false;
    program.for_each_decl_recursive(|_, decl| {
        let s = decl.span().start as usize;
        if s >= first_start && s < last_end {
            interleaved = true;
        }
    });
    if interleaved || contains_comment(&text[first_start..last_end]) {
        return None;
    }

    let unused_spans: Vec<Span> = arandu_query::passes::resolve(&snap.db, source)
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagCode::W007UnusedImport)
        .map(|d| d.span)
        .collect();

    let is_unused = |span: Span| -> bool {
        unused_spans.iter().any(|u| {
            u == &span || (u.file_id == span.file_id && u.start >= span.start && u.end <= span.end)
        })
    };

    // Grouped module imports: prefix -> Vec<(name, Option<alias>)>
    let mut module_groups: BTreeMap<Vec<String>, BTreeSet<(String, Option<String>)>> =
        BTreeMap::new();
    // Single-segment module aliases: Vec<(name, alias)>
    let mut root_modules: BTreeSet<(String, String)> = BTreeSet::new();
    // Merged `from path import { ... }` entries: path -> Vec<(name, Option<alias>)>
    let mut named_imports: BTreeMap<Vec<String>, BTreeSet<(String, Option<String>)>> =
        BTreeMap::new();
    // External imports (`import "src" as alias` / `from "src" import { ... }`)
    let mut ext_aliases: BTreeSet<(String, String)> = BTreeSet::new();
    let mut ext_named: BTreeMap<String, BTreeSet<(String, Option<String>)>> = BTreeMap::new();
    // Preserved `public use` / `internal use` declarations verbatim.
    let mut reexports: Vec<String> = Vec::new();

    for import in &program.imports {
        match import {
            ImportDecl::ReExport {
                visibility,
                span,
                items,
                path,
            } => {
                if *visibility != Visibility::Private {
                    if let Some(slice) = text.get(span.start as usize..span.end as usize) {
                        reexports.push(slice.trim().to_string());
                    }
                } else {
                    collect_used_items(&unused_spans, path, items, &mut named_imports);
                }
            }
            ImportDecl::ModuleAlias { span, path, alias } => {
                if is_unused(*span) {
                    continue;
                }
                if path.len() >= 2 {
                    let prefix: Vec<String> = path[..path.len() - 1]
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
                    let leaf = path.last()?.to_string();
                    let alias_opt = (alias.as_str() != leaf).then(|| alias.to_string());
                    module_groups
                        .entry(prefix)
                        .or_default()
                        .insert((leaf, alias_opt));
                } else if let Some(single) = path.first() {
                    root_modules.insert((single.to_string(), alias.to_string()));
                }
            }
            ImportDecl::ModuleGroup { prefix, items, .. } => {
                let prefix_vec: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
                for item in items {
                    if is_unused(item.span) {
                        continue;
                    }
                    module_groups
                        .entry(prefix_vec.clone())
                        .or_default()
                        .insert((
                            item.name.to_string(),
                            item.alias.as_ref().map(|a| a.to_string()),
                        ));
                }
            }
            ImportDecl::Named { path, items, .. } => {
                collect_used_items(&unused_spans, path, items, &mut named_imports);
            }
            ImportDecl::ExternalAlias {
                span,
                source,
                alias,
            } => {
                if !is_unused(*span) {
                    ext_aliases.insert((source.to_string(), alias.to_string()));
                }
            }
            ImportDecl::ExternalNamed { source, items, .. } => {
                for item in items {
                    if is_unused(item.span) {
                        continue;
                    }
                    ext_named.entry(source.to_string()).or_default().insert((
                        item.name.to_string(),
                        item.alias.as_ref().map(|a| a.to_string()),
                    ));
                }
            }
        }
    }

    let mut buckets: [Vec<FormattedEntry>; 3] = [Vec::new(), Vec::new(), Vec::new()];

    for (name, alias) in root_modules {
        let bucket = if name == "std" {
            ImportBucket::Std
        } else {
            ImportBucket::Local
        };
        let rendered = if alias == name {
            format!("import {name}")
        } else {
            format!("import {name} as {alias}")
        };
        buckets[bucket as usize].push(FormattedEntry {
            sort_key: format!("0:{name}:{alias}"),
            rendered,
        });
    }

    for (prefix, items) in module_groups {
        if items.is_empty() {
            continue;
        }
        let bucket = classify_prefix(&prefix);
        let prefix_str = prefix.join(".");
        let rendered = if items.len() == 1 {
            let (leaf, alias_opt) = items.into_iter().next()?;
            match alias_opt {
                Some(alias) => format!("import {prefix_str}.{leaf} as {alias}"),
                None => format!("import {prefix_str}.{leaf}"),
            }
        } else {
            let formatted_items = items
                .into_iter()
                .map(|(leaf, alias_opt)| match alias_opt {
                    Some(alias) => format!("{leaf} as {alias}"),
                    None => leaf,
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("import {prefix_str}.{{{formatted_items}}}")
        };
        buckets[bucket as usize].push(FormattedEntry {
            sort_key: format!("0:{prefix_str}"),
            rendered,
        });
    }

    for (source_str, alias) in ext_aliases {
        buckets[ImportBucket::External as usize].push(FormattedEntry {
            sort_key: format!("0:{source_str}:{alias}"),
            rendered: format!("import \"{source_str}\" as {alias}"),
        });
    }

    for (path, items) in named_imports {
        if items.is_empty() {
            continue;
        }
        let bucket = classify_path(&path);
        let path_str = path.join(".");
        let formatted_items = format_named_items(items);
        buckets[bucket as usize].push(FormattedEntry {
            sort_key: format!("1:{path_str}"),
            rendered: format!("from {path_str} import {{ {formatted_items} }}"),
        });
    }

    for (source_str, items) in ext_named {
        if items.is_empty() {
            continue;
        }
        let formatted_items = format_named_items(items);
        buckets[ImportBucket::External as usize].push(FormattedEntry {
            sort_key: format!("1:{source_str}"),
            rendered: format!("from \"{source_str}\" import {{ {formatted_items} }}"),
        });
    }

    let mut sections: Vec<String> = Vec::new();
    for bucket in &mut buckets {
        if bucket.is_empty() {
            continue;
        }
        bucket.sort();
        let section = bucket
            .iter()
            .map(|e| e.rendered.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(section);
    }

    if !reexports.is_empty() {
        sections.push(reexports.join("\n"));
    }

    let new_text = sections.join("\n\n");
    if new_text.is_empty() {
        let mut end = last_end;
        if text.as_bytes().get(end) == Some(&b'\r') {
            end += 1;
        }
        if text.as_bytes().get(end) == Some(&b'\n') {
            end += 1;
        }
        return Some(TextEdit {
            start: first_start as u32,
            end: end as u32,
            new_text: String::new(),
        });
    }

    if new_text == text[first_start..last_end] {
        return None;
    }

    Some(TextEdit {
        start: first_start as u32,
        end: last_end as u32,
        new_text,
    })
}

fn collect_used_items(
    unused_spans: &[Span],
    path: &[arandu_middle::SmolStr],
    items: &[ImportItem],
    out: &mut BTreeMap<Vec<String>, BTreeSet<(String, Option<String>)>>,
) {
    let path_vec: Vec<String> = path.iter().map(|s| s.to_string()).collect();
    for item in items {
        let is_unused = unused_spans.iter().any(|u| {
            u == &item.span
                || (u.file_id == item.span.file_id
                    && u.start >= item.span.start
                    && u.end <= item.span.end)
        });
        if is_unused {
            continue;
        }
        out.entry(path_vec.clone()).or_default().insert((
            item.name.to_string(),
            item.alias.as_ref().map(|a| a.to_string()),
        ));
    }
}

fn format_named_items(items: BTreeSet<(String, Option<String>)>) -> String {
    items
        .into_iter()
        .map(|(name, alias_opt)| match alias_opt {
            Some(alias) => format!("{name} as {alias}"),
            None => name,
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn classify_prefix(prefix: &[String]) -> ImportBucket {
    match prefix.first().map(String::as_str) {
        Some("std") => ImportBucket::Std,
        Some("self") => ImportBucket::Local,
        _ => ImportBucket::External,
    }
}

fn classify_path(path: &[String]) -> ImportBucket {
    match path.first().map(String::as_str) {
        Some("std") => ImportBucket::Std,
        Some("self") => ImportBucket::Local,
        Some(_) if path.len() == 1 => ImportBucket::Local,
        _ => ImportBucket::External,
    }
}

fn contains_comment(slice: &str) -> bool {
    let bytes = slice.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        if in_string {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if bytes[i] == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*')
        {
            return true;
        }
        i += 1;
    }
    false
}
