//! Auto-import candidate discovery and canonical import insertion edits.
//!
//! Editor-agnostic: returns byte-offset [`TextEdit`]s so both `arandu_lsp` and
//! `arandu_web` can apply them without duplicating import-placement logic.

use arandu_middle::SymbolKind;
use arandu_middle::db::SourceDatabase;
use arandu_query::{AnalysisSnapshot, SourceFile};
use arandu_semantics::TypeCheckResult;
use rustc_hash::FxHashSet;

use super::{CompletionItem, CompletionKind, TextEdit, accepted, value_kind};

/// A single auto-import suggestion with the exact text edit to insert it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoImportCandidate {
    pub label: String,
    pub kind: CompletionKind,
    pub module_path: String,
    pub import_statement: String,
    pub edit: TextEdit,
}

/// Curated stdlib modules scanned for auto-import suggestions:
/// `(canonical_file_key, logical_module_path, module_alias, well_known_types)`.
const AUTO_IMPORT_STDLIB: &[(&str, &str, &str, &[&str])] = &[
    (
        "stdlib/std/collections.aru",
        "std.collections",
        "collections",
        &[
            "HashMap",
            "VecDeque",
            "BitSet",
            "SmallVec4",
            "Slab",
            "Arena",
            "ScratchArena",
            "Entry",
        ],
    ),
    ("stdlib/std/fs.aru", "std.fs", "fs", &[]),
    ("stdlib/std/env.aru", "std.env", "env", &[]),
    ("stdlib/std/path.aru", "std.path", "path", &["Path"]),
    (
        "stdlib/std/time.aru",
        "std.time",
        "time",
        &["Duration", "Instant"],
    ),
    ("stdlib/std/process.aru", "std.process", "process", &[]),
    ("stdlib/std/net.aru", "std.net", "net", &[]),
    ("stdlib/std/random.aru", "std.random", "random", &[]),
    ("stdlib/std/text.aru", "std.text", "text", &[]),
    ("stdlib/std/thread.aru", "std.thread", "thread", &[]),
    (
        "stdlib/std/sync/mutex.aru",
        "std.sync.mutex",
        "mutex",
        &["Mutex"],
    ),
    (
        "stdlib/std/sync/rwlock.aru",
        "std.sync.rwlock",
        "rwlock",
        &["RwLock"],
    ),
    (
        "stdlib/std/sync/cancel_token.aru",
        "std.sync.cancel_token",
        "cancel_token",
        &["CancelToken"],
    ),
    ("stdlib/alloc/rc.aru", "std.alloc.rc", "rc", &["Rc"]),
    ("stdlib/alloc/arc.aru", "std.alloc.arc", "arc", &["Arc"]),
    (
        "stdlib/core/cell.aru",
        "std.core.cell",
        "cell",
        &["Cell", "UnsafeCell"],
    ),
];

/// Computes the canonical byte offset where a new `import` statement should be
/// inserted, plus whether a leading blank line is needed (e.g. right after
/// `module ...` when no imports exist yet).
#[must_use]
pub fn import_insertion_point(text: &str) -> (u32, bool) {
    let mut offset = 0usize;
    let mut module_end: Option<usize> = None;
    let mut last_import_end: Option<usize> = None;

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let line_end = offset + line.len();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            offset = line_end;
            continue;
        }
        if trimmed.starts_with("module ") {
            module_end = Some(line_end);
            offset = line_end;
            continue;
        }
        if trimmed.starts_with("import ")
            || trimmed.starts_with("from ")
            || trimmed.starts_with("public use ")
            || trimmed.starts_with("internal use ")
        {
            last_import_end = Some(line_end);
            offset = line_end;
            continue;
        }
        // First non-header declaration reached.
        break;
    }

    if let Some(end) = last_import_end {
        return (u32::try_from(end).unwrap_or(u32::MAX), false);
    }
    if let Some(end) = module_end {
        return (u32::try_from(end).unwrap_or(u32::MAX), true);
    }
    (0, false)
}

fn make_import_edit(text: &str, import_statement: &str) -> TextEdit {
    let (pos, needs_leading_newline) = import_insertion_point(text);
    let prefix = if needs_leading_newline { "\n" } else { "" };
    let suffix = if pos as usize == text.len() && !text.is_empty() && !text.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let trailing = if pos == 0 && !text.is_empty() {
        "\n\n"
    } else {
        "\n"
    };
    TextEdit {
        start: pos,
        end: pos,
        new_text: format!("{suffix}{prefix}{import_statement}{trailing}"),
    }
}

/// Collects auto-import candidates matching `prefix` (for completion) or an
/// exact `exact_name` (for missing-import quick-fixes).
#[must_use]
pub fn collect_auto_imports(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    tc: &TypeCheckResult,
    text: &str,
    prefix: &str,
    prefix_l: &str,
    exact_only: bool,
    types_only: bool,
) -> Vec<AutoImportCandidate> {
    if !exact_only && prefix.len() < 2 {
        return Vec::new();
    }

    // Names already available in scope (local decls, explicit imports, prelude).
    let in_scope: FxHashSet<&str> = tc
        .symbols
        .iter()
        .filter(|sym| {
            !matches!(
                sym.kind,
                SymbolKind::Field
                    | SymbolKind::EnumVariant
                    | SymbolKind::AssociatedFunc
                    | SymbolKind::NamespaceMember
            )
        })
        .map(|sym| sym.name.as_str())
        .collect();

    let mut seen_labels: FxHashSet<String> = FxHashSet::default();
    let mut out = Vec::new();

    let matches_query = |candidate: &str| -> bool {
        if exact_only {
            candidate == prefix
        } else {
            accepted(prefix, prefix_l, candidate)
        }
    };

    for &(file_key, module_path, module_alias, fallback_types) in AUTO_IMPORT_STDLIB {
        // 1. Module alias suggestion (`fs` -> `import std.fs`, `env` -> `import std.env`)
        if !types_only
            && !in_scope.contains(module_alias)
            && matches_query(module_alias)
            && seen_labels.insert(module_alias.to_string())
        {
            let stmt = format!("import {module_path}");
            out.push(AutoImportCandidate {
                label: module_alias.to_string(),
                kind: CompletionKind::Module,
                module_path: module_path.to_string(),
                edit: make_import_edit(text, &stmt),
                import_statement: stmt,
            });
        }

        // 2. Named type suggestions from live Salsa `exported_symbols` when registered,
        //    or the curated fallback list in lightweight single-file snapshots.
        if let Some(imported_file) = snap.db.resolve_module_path(file_key) {
            if imported_file == source {
                continue;
            }
            let exports = arandu_query::passes::exported_symbols(&snap.db, imported_file);
            for (name, &(_, kind)) in &exports.symbols {
                if !kind.is_type() || name.contains('.') {
                    continue;
                }
                if in_scope.contains(name.as_str()) || !matches_query(name) {
                    continue;
                }
                if !seen_labels.insert(name.clone()) {
                    continue;
                }
                let stmt = format!("from {module_path} import {{ {name} }}");
                out.push(AutoImportCandidate {
                    label: name.clone(),
                    kind: value_kind(kind),
                    module_path: module_path.to_string(),
                    edit: make_import_edit(text, &stmt),
                    import_statement: stmt,
                });
            }
        } else {
            for &ty_name in fallback_types {
                if in_scope.contains(ty_name) || !matches_query(ty_name) {
                    continue;
                }
                if !seen_labels.insert(ty_name.to_string()) {
                    continue;
                }
                let stmt = format!("from {module_path} import {{ {ty_name} }}");
                out.push(AutoImportCandidate {
                    label: ty_name.to_string(),
                    kind: CompletionKind::Struct,
                    module_path: module_path.to_string(),
                    edit: make_import_edit(text, &stmt),
                    import_statement: stmt,
                });
            }
        }
    }

    collect_workspace_auto_imports(
        snap,
        source,
        text,
        &in_scope,
        &mut seen_labels,
        &matches_query,
        types_only,
        &mut out,
    );

    out.sort_by(|a, b| {
        a.label
            .cmp(&b.label)
            .then_with(|| a.module_path.cmp(&b.module_path))
    });
    out
}

/// Converts a canonical module key like `"self/models/user.aru"` or
/// `"web/router.aru"` into its dotted logical import path (`"self.models.user"`,
/// `"web.router"`).
fn logical_module_path_from_key(key: &str) -> Option<String> {
    let trimmed = key.strip_suffix(".aru").unwrap_or(key);
    if trimmed.is_empty() || trimmed.starts_with("stdlib/") {
        return None;
    }
    let normalized = trimmed.replace('\\', "/");
    let path = normalized.trim_start_matches("./").replace('/', ".");
    if path.is_empty() { None } else { Some(path) }
}

/// Scans package modules (both `self.*` with `internal` visibility and
/// `Arandu.toml` dependencies with `public` visibility) plus any other
/// non-stdlib workspace files registered in the snapshot.
#[allow(clippy::too_many_arguments)]
fn collect_workspace_auto_imports(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    in_scope: &FxHashSet<&str>,
    seen_labels: &mut FxHashSet<String>,
    matches_query: &impl Fn(&str) -> bool,
    types_only: bool,
    out: &mut Vec<AutoImportCandidate>,
) {
    let current_file_id = *source.file_id(&snap.db);
    let mut visited_files: FxHashSet<SourceFile> = FxHashSet::default();
    visited_files.insert(source);

    // 1. Package-mode bindings (`self.*` and declared dependencies).
    let mut module_entries: Vec<(String, SourceFile, bool)> = Vec::new();
    if let Some(map) = snap.db.package_module_map() {
        let mut by_file: rustc_hash::FxHashMap<SourceFile, String> =
            rustc_hash::FxHashMap::default();
        for (key, binding) in map.bindings(&snap.db).iter() {
            if binding.file == source {
                continue;
            }
            let Some(mod_path) = logical_module_path_from_key(key) else {
                continue;
            };
            // Prefer `self.*` over `<package_name>.*` when dual-root bindings exist.
            match by_file.get(&binding.file) {
                Some(existing) if existing.starts_with("self.") => {}
                Some(existing) if !mod_path.starts_with("self.") && existing <= &mod_path => {}
                _ => {
                    by_file.insert(binding.file, mod_path);
                }
            }
        }
        for (file, mod_path) in by_file {
            let same_pkg = snap.db.same_package(current_file_id, file);
            module_entries.push((mod_path, file, same_pkg));
        }
    }

    // 2. Non-package workspace files registered in the DB.
    for (path_key, file) in snap.db.registered_source_files() {
        if file == source || path_key.starts_with("stdlib/") {
            continue;
        }
        if module_entries
            .iter()
            .any(|(_, existing, _)| *existing == file)
        {
            continue;
        }
        if snap.db.is_trusted_stdlib_source(file) {
            continue;
        }
        let mod_path = arandu_query::passes::parse(&snap.db, file)
            .as_ref()
            .ok()
            .and_then(|prog| prog.module.as_ref().map(|m| m.path.join(".")))
            .or_else(|| logical_module_path_from_key(&path_key));
        if let Some(mod_path) = mod_path {
            let same_pkg = snap.db.same_package(current_file_id, file);
            module_entries.push((mod_path, file, same_pkg));
        }
    }

    module_entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (module_path, file, same_pkg) in module_entries {
        if !visited_files.insert(file) {
            continue;
        }

        // Offer the module alias itself (e.g. `user` -> `import self.models.user`).
        if !types_only
            && let Some(alias) = module_path.rsplit('.').next()
            && !alias.is_empty()
            && !in_scope.contains(alias)
            && matches_query(alias)
            && seen_labels.insert(format!("{alias}@{module_path}"))
        {
            let stmt = format!("import {module_path}");
            out.push(AutoImportCandidate {
                label: alias.to_string(),
                kind: CompletionKind::Module,
                module_path: module_path.clone(),
                edit: make_import_edit(text, &stmt),
                import_statement: stmt,
            });
        }

        let exports = arandu_query::passes::exported_symbols(&snap.db, file);
        let internal = same_pkg.then(|| arandu_query::passes::internal_symbols(&snap.db, file));

        let symbols_iter = exports.symbols.iter().chain(
            internal
                .iter()
                .flat_map(|table| table.internal_symbols.iter()),
        );

        for (name, &(_, kind)) in symbols_iter {
            if name.contains('.') {
                continue;
            }
            if types_only && !kind.is_type() {
                continue;
            }
            if !matches!(
                kind,
                SymbolKind::Struct
                    | SymbolKind::Enum
                    | SymbolKind::Interface
                    | SymbolKind::TypeAlias
                    | SymbolKind::Func
                    | SymbolKind::Const
            ) {
                continue;
            }
            if in_scope.contains(name.as_str()) || !matches_query(name) {
                continue;
            }
            if !seen_labels.insert(format!("{name}@{module_path}")) {
                continue;
            }
            let stmt = format!("from {module_path} import {{ {name} }}");
            out.push(AutoImportCandidate {
                label: name.clone(),
                kind: value_kind(kind),
                module_path: module_path.clone(),
                edit: make_import_edit(text, &stmt),
                import_statement: stmt,
            });
        }
    }
}

/// Exact-name lookup for LSP missing-import quick-fixes (`N001`/`N002`).
#[must_use]
pub fn auto_import_candidates_for_name(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    name: &str,
) -> Vec<AutoImportCandidate> {
    if name.is_empty() {
        return Vec::new();
    }
    let tc = crate::presentation::typecheck(snap, source);
    let lower = name.to_ascii_lowercase();
    collect_auto_imports(snap, source, &tc, text, name, &lower, true, false)
}

/// Converts an [`AutoImportCandidate`] into a [`CompletionItem`] carrying its
/// `additional_text_edits`.
#[must_use]
pub(super) fn to_completion_item(candidate: AutoImportCandidate) -> CompletionItem {
    let mut item = CompletionItem::new(candidate.label, candidate.kind);
    item.detail = Some(format!("auto-import: {}", candidate.import_statement));
    item.additional_text_edits = vec![candidate.edit];
    item
}
