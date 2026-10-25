//! Completion engine shared by the LSP and the in-browser wasm playground.
//!
//! Editor-agnostic on purpose: results carry a neutral [`CompletionKind`] and
//! already-ranked `sort_text`. The LSP adapter maps them to `lsp_types`; the
//! wasm bridge serializes them with serde.
//!
//! The engine is scope-aware where the snapshot allows it: locals and
//! parameters declared after the cursor are never offered, members come from
//! the receiver's declared type (struct fields, enum variants, methods and
//! interface methods), and declaration positions only suggest types.

use arandu_middle::SymbolKind;
use arandu_query::{AnalysisSnapshot, SourceFile};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::presentation::{prefix_at, typecheck};

mod annotation;
mod general;
mod member;

use annotation::{annotation_completions, is_annotation_completion};
use general::{general_completions, type_position_completions};
use member::member_completions;

/// Maximum number of completion items returned to avoid latency spikes.
pub const MAX_COMPLETION_ITEMS: usize = 200;

/// Editor-neutral completion item kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompletionKind {
    Keyword,
    Function,
    Method,
    Struct,
    Enum,
    EnumMember,
    Interface,
    Constant,
    Field,
    Variable,
    Module,
    Property,
    TypeParameter,
    Text,
}

/// Editor-neutral completion item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert_text: Option<String>,
    #[serde(default)]
    pub is_snippet: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_text: Option<String>,
}

impl CompletionItem {
    fn new(label: impl Into<String>, kind: CompletionKind) -> Self {
        Self {
            label: label.into(),
            kind,
            detail: None,
            documentation: None,
            insert_text: None,
            is_snippet: false,
            filter_text: None,
            sort_text: None,
        }
    }
}

// Grouping ranks used for `sort_text`. Lower sorts first.
const RANK_LOCAL: u8 = 0;
const RANK_MEMBER: u8 = 0;
const RANK_VALUE: u8 = 1;
const RANK_TYPE: u8 = 2;
const RANK_MODULE: u8 = 3;
const RANK_KEYWORD: u8 = 4;

/// Keywords offered at statement/expression positions, with a one-line hint.
const KEYWORDS: &[(&str, &str)] = &[
    ("func", "declare a function"),
    ("public", "expose an item to other modules and the runtime"),
    ("struct", "declare a struct type"),
    ("enum", "declare an enum type"),
    ("interface", "declare an interface (method set)"),
    ("const", "declare a compile-time constant"),
    (
        "comptime",
        "evaluate an admitted expression or block at compile time",
    ),
    ("type", "declare a type alias"),
    ("module", "declare the module identity"),
    ("import", "import another module"),
    ("as", "bind an import or type to a new name"),
    ("extern", "declare an external (C ABI) function block"),
    ("unsafe", "enter an unsafe context"),
    ("where", "constrain generic parameters"),
    ("catch", "handle a `Result` error"),
    ("is", "pattern-test a value"),
    ("impl", "attach behavior to a type"),
    ("let", "bind an immutable local"),
    (
        "guard",
        "early exit if condition is not met (guard <cond> else { <diverge> })",
    ),
    ("mut", "make a binding mutable"),
    ("set", "assign to a mutable place"),
    ("return", "return from the current function"),
    ("if", "conditional branch"),
    ("else", "alternative branch"),
    ("for", "iterate over a range or collection"),
    ("in", "introduce the iterable of a `for` loop"),
    ("while", "loop while the condition holds"),
    ("match", "pattern-match a value"),
    ("break", "exit the innermost loop"),
    ("continue", "skip to the next loop iteration"),
    ("defer", "run at scope exit"),
    ("errdefer", "run at scope exit on the error path"),
    ("async", "declare an asynchronous function"),
    ("await", "suspend until a future resolves"),
    ("own", "take ownership explicitly"),
    ("shared", "share a value across tasks"),
    ("ref", "borrow a value"),
    ("ptr", "raw pointer type"),
    ("self", "the receiver of a method"),
    ("true", "boolean true"),
    ("false", "boolean false"),
    ("nil", "the empty value for `Option`"),
];

/// Primitive/contextual type names accepted in type position.
const PRIMITIVE_TYPES: &[&str] = &[
    "int", "uint", "isize", "usize", "float", "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64",
    "f32", "f64", "bool", "byte", "char", "str", "any", "void", "Err",
];

/// Known top-level stdlib path roots for import completion (T3 tokens).
const IMPORT_ROOTS: &[&str] = &["std", "io", "err"];

/// Segments under `std.` that exist in the tree.
const STD_CHILDREN: &[(&str, &[&str])] = &[
    ("std", &["core", "alloc"]),
    (
        "std.core",
        &[
            "mem",
            "option",
            "result",
            "prelude",
            "intrinsics",
            "future",
            "pointer",
            "str",
            "cmp",
            "char",
            "ascii",
            "num",
            "slice",
            "iter",
        ],
    ),
    (
        "std.alloc",
        &["vec", "allocator_api", "gen_arena", "string"],
    ),
];

#[must_use]
pub fn completions(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    offset: u32,
) -> Vec<CompletionItem> {
    let prefix = prefix_at(text, offset);
    let prefix_l = prefix.to_ascii_lowercase();

    if is_annotation_completion(text, offset, &prefix) {
        return annotation_completions(text, offset, &prefix);
    }

    // W4 / T3.6: import path completion (`import std.core.▮` / `import std.▮`).
    if let Some(items) = import_path_completions(text, offset, &prefix) {
        return items;
    }

    let tc = typecheck(snap, source);

    // Member completion after `receiver.` (module aliases, struct fields,
    // enum variants, associated and interface methods).
    if let Some(items) = member_completions(snap, source, &tc, text, offset, &prefix, &prefix_l) {
        return items;
    }

    // Type position after `:` in a declaration.
    if let Some(items) =
        type_position_completions(snap, source, &tc, text, offset, &prefix, &prefix_l)
    {
        return items;
    }

    general_completions(snap, source, &tc, offset, &prefix, &prefix_l)
}

#[inline]
fn starts_with_ignore_case(s: &str, prefix_lower: &str) -> bool {
    if s.len() < prefix_lower.len() {
        return false;
    }
    s.chars()
        .zip(prefix_lower.chars())
        .all(|(a, b)| a.to_ascii_lowercase() == b)
}

#[inline]
fn accepted(prefix: &str, prefix_lower: &str, name: &str) -> bool {
    prefix.is_empty() || starts_with_ignore_case(name, prefix_lower)
}

fn value_kind(kind: SymbolKind) -> CompletionKind {
    match kind {
        SymbolKind::Func | SymbolKind::ExternFunc => CompletionKind::Function,
        SymbolKind::AssociatedFunc => CompletionKind::Method,
        SymbolKind::Const | SymbolKind::ConstParam => CompletionKind::Constant,
        SymbolKind::Local | SymbolKind::Param => CompletionKind::Variable,
        SymbolKind::EnumVariant => CompletionKind::EnumMember,
        SymbolKind::Field => CompletionKind::Field,
        SymbolKind::Module => CompletionKind::Module,
        SymbolKind::Struct => CompletionKind::Struct,
        SymbolKind::Enum => CompletionKind::Enum,
        SymbolKind::Interface => CompletionKind::Interface,
        SymbolKind::TypeAlias | SymbolKind::ImportType => CompletionKind::Struct,
        SymbolKind::TypeParam => CompletionKind::TypeParameter,
        SymbolKind::NamespaceMember | SymbolKind::ImportValue => CompletionKind::Text,
    }
}

// ── Ranking / collection helpers ────────────────────────────────────

struct Candidate {
    rank: u8,
    span_start: u32,
    item: CompletionItem,
}

/// Keep the most relevant definition per label.
///
/// Lower rank wins; on a tie the later declaration (larger `span_start`) wins,
/// so a shadowing local replaces an outer binding of the same name.
fn offer(
    best: &mut FxHashMap<String, Candidate>,
    key: &str,
    rank: u8,
    span_start: u32,
    item: CompletionItem,
) {
    match best.get(key) {
        Some(existing)
            if existing.rank < rank
                || (existing.rank == rank && existing.span_start >= span_start) => {}
        _ => {
            best.insert(
                key.to_string(),
                Candidate {
                    rank,
                    span_start,
                    item,
                },
            );
        }
    }
}

fn finish(best: FxHashMap<String, Candidate>) -> Vec<CompletionItem> {
    let mut items: Vec<(u8, CompletionItem)> = best
        .into_values()
        .map(|entry| (entry.rank, entry.item))
        .collect();
    items.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.label.cmp(&right.1.label))
    });
    items.truncate(MAX_COMPLETION_ITEMS);
    items
        .into_iter()
        .map(|(rank, mut item)| {
            item.sort_text = Some(format!("{rank:02}_{}", item.label.to_ascii_lowercase()));
            item
        })
        .collect()
}

fn finish_ranked(mut items: Vec<(u8, CompletionItem)>) -> Vec<CompletionItem> {
    items.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.label.cmp(&right.1.label))
    });
    items.dedup_by(|left, right| left.1.label == right.1.label);
    items.truncate(MAX_COMPLETION_ITEMS);
    items
        .into_iter()
        .map(|(rank, mut item)| {
            item.sort_text = Some(format!("{rank:02}_{}", item.label.to_ascii_lowercase()));
            item
        })
        .collect()
}

// ── Import paths ────────────────────────────────────────────────────

/// If the cursor is inside an `import …` path (not after `as`), suggest next segments.
#[must_use]
pub fn import_path_completions(
    text: &str,
    offset: u32,
    prefix: &str,
) -> Option<Vec<CompletionItem>> {
    let off = (offset as usize).min(text.len());
    let line_start = text[..off].rfind('\n').map(|index| index + 1).unwrap_or(0);
    let line = &text[line_start..off];
    let trimmed = line.trim_start();
    if !trimmed.starts_with("import ") && !trimmed.starts_with("import\t") {
        return None;
    }
    // After `as` → alias name, not path.
    if trimmed.contains(" as ") {
        return None;
    }
    let after_import = trimmed.strip_prefix("import")?.trim_start();
    // Quoted imports are free-form paths; skip special completion.
    if after_import.starts_with('"') {
        return None;
    }
    let parent = if after_import.is_empty() || after_import.ends_with('.') {
        after_import.trim_end_matches('.').to_string()
    } else if let Some(dot) = after_import.rfind('.') {
        after_import[..dot].to_string()
    } else {
        String::new()
    };
    let prefix_l = prefix.to_ascii_lowercase();
    let mut labels: Vec<&str> = Vec::new();
    if parent.is_empty() {
        labels.extend(IMPORT_ROOTS.iter().copied());
    } else {
        for (key, children) in STD_CHILDREN {
            if *key == parent.as_str() {
                labels.extend(children.iter().copied());
            }
        }
    }
    if labels.is_empty() {
        return None;
    }
    let mut items: Vec<CompletionItem> = labels
        .into_iter()
        .filter(|label| accepted(prefix, &prefix_l, label))
        .map(|label| {
            let mut item = CompletionItem::new(label, CompletionKind::Module);
            item.detail = Some("module path".to_string());
            item
        })
        .collect();
    items.sort_by(|left, right| left.label.cmp(&right.label));
    items.dedup_by(|left, right| left.label == right.label);
    if items.is_empty() { None } else { Some(items) }
}
