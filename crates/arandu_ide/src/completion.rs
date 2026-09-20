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

use arandu_middle::types::ArType;
use arandu_middle::{SymbolId, SymbolKind};
use arandu_query::{AnalysisSnapshot, SourceFile};
use arandu_semantics::TypeCheckResult;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::presentation::{display_type, prefix_at, symbol_at, symbol_presentation, typecheck};

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
    "int", "uint", "float", "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64",
    "bool", "byte", "char", "str", "any", "void", "Err",
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

// ── General completion ──────────────────────────────────────────────

fn general_completions(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    tc: &TypeCheckResult,
    offset: u32,
    prefix: &str,
    prefix_l: &str,
) -> Vec<CompletionItem> {
    let mut best: FxHashMap<String, Candidate> = FxHashMap::default();

    for symbol in tc.symbols.iter() {
        // Members are only meaningful after `.`.
        if matches!(
            symbol.kind,
            SymbolKind::Field
                | SymbolKind::EnumVariant
                | SymbolKind::AssociatedFunc
                | SymbolKind::NamespaceMember
        ) {
            continue;
        }
        let rank = match symbol.kind {
            SymbolKind::Local | SymbolKind::Param => {
                // Not yet declared at this point in the file.
                if symbol.span.start > offset {
                    continue;
                }
                RANK_LOCAL
            }
            SymbolKind::Func
            | SymbolKind::ExternFunc
            | SymbolKind::Const
            | SymbolKind::ImportValue => RANK_VALUE,
            SymbolKind::Struct
            | SymbolKind::Enum
            | SymbolKind::Interface
            | SymbolKind::TypeAlias
            | SymbolKind::ImportType => RANK_TYPE,
            SymbolKind::TypeParam => {
                if symbol.span.start > offset {
                    continue;
                }
                RANK_TYPE
            }
            SymbolKind::Module => RANK_MODULE,
            _ => continue,
        };
        let name = symbol.name.as_str();
        if !accepted(prefix, prefix_l, name) {
            continue;
        }
        let presentation = symbol_presentation(snap, source, tc, symbol);
        let mut item = CompletionItem::new(name, value_kind(symbol.kind));
        item.detail = Some(presentation.signature);
        item.documentation = presentation.documentation;
        offer(&mut best, name, rank, symbol.span.start, item);
    }

    for (keyword, doc) in KEYWORDS {
        if !accepted(prefix, prefix_l, keyword) {
            continue;
        }
        let mut item = CompletionItem::new(*keyword, CompletionKind::Keyword);
        item.detail = Some((*doc).to_string());
        offer(&mut best, keyword, RANK_KEYWORD, 0, item);
    }

    finish(best)
}

// ── Type-position completion ────────────────────────────────────────

fn type_position_completions(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    tc: &TypeCheckResult,
    text: &str,
    offset: u32,
    prefix: &str,
    prefix_l: &str,
) -> Option<Vec<CompletionItem>> {
    if !is_type_position(text, offset) {
        return None;
    }
    let mut best: FxHashMap<String, Candidate> = FxHashMap::default();

    for ty in PRIMITIVE_TYPES {
        if !accepted(prefix, prefix_l, ty) {
            continue;
        }
        let mut item = CompletionItem::new(*ty, CompletionKind::TypeParameter);
        item.detail = Some("primitive type".to_string());
        offer(&mut best, ty, 1, 0, item);
    }

    for symbol in tc.symbols.iter() {
        if !symbol.kind.is_type() {
            continue;
        }
        if symbol.kind == SymbolKind::TypeParam && symbol.span.start > offset {
            continue;
        }
        let name = symbol.name.as_str();
        if !accepted(prefix, prefix_l, name) {
            continue;
        }
        let presentation = symbol_presentation(snap, source, tc, symbol);
        let mut item = CompletionItem::new(name, value_kind(symbol.kind));
        item.detail = Some(presentation.signature);
        item.documentation = presentation.documentation;
        offer(&mut best, name, 0, symbol.span.start, item);
    }

    if best.is_empty() {
        return None;
    }
    Some(finish(best))
}

/// Heuristic: the cursor sits right after `:` in a declaration context.
fn is_type_position(text: &str, offset: u32) -> bool {
    let Some(before) = text.get(..(offset as usize).min(text.len())) else {
        return false;
    };
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let trimmed = before[line_start..].trim_end();
    let Some(head) = trimmed.strip_suffix(':') else {
        return false;
    };
    // `::` is a path separator, not a type annotation.
    if head.ends_with(':') {
        return false;
    }
    let words: Vec<&str> = head
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
        .collect();
    let is_decl = words.iter().any(|word| {
        matches!(
            *word,
            "func" | "struct" | "enum" | "interface" | "const" | "type" | "extern" | "let"
        )
    });
    if !is_decl {
        return false;
    }
    // `let p = Pair { left: ▮` is a struct literal: a value is expected.
    let struct_decl = words
        .iter()
        .any(|word| matches!(*word, "struct" | "enum" | "interface"));
    !(head.contains('{') && !struct_decl)
}

// ── Member completion ───────────────────────────────────────────────

/// `(receiver, receiver_start_offset)` when the cursor follows `receiver.`.
fn receiver_before_prefix(text: &str, offset: u32, prefix_len: usize) -> Option<(String, u32)> {
    let end = (offset as usize).min(text.len()).checked_sub(prefix_len)?;
    let before = text.get(..end)?.trim_end();
    let without_dot = before.strip_suffix('.')?;
    let bytes = without_dot.as_bytes();
    let mut start = without_dot.len();
    while start > 0 {
        let byte = bytes[start - 1];
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    let receiver = &without_dot[start..];
    if receiver.is_empty() {
        return None;
    }
    Some((receiver.to_string(), u32::try_from(start).ok()?))
}

fn member_completions(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    tc: &TypeCheckResult,
    text: &str,
    offset: u32,
    prefix: &str,
    prefix_l: &str,
) -> Option<Vec<CompletionItem>> {
    let (receiver, receiver_start) = receiver_before_prefix(text, offset, prefix.len())?;
    let global = tc.symbols.global_scope();

    if tc.symbols.lookup_module(global, &receiver).is_some() {
        return module_member_completions(snap, tc, &receiver, prefix, prefix_l);
    }

    let symbol = symbol_at(tc, receiver_start)
        .or_else(|| {
            // Name resolution does not always publish a reference entry for a
            // plain receiver use, so fall back to the most recent visible
            // declaration with the same name (a shadowing local wins).
            tc.symbols
                .iter()
                .filter(|candidate| candidate.name.as_str() == receiver)
                .filter(|candidate| candidate.span.start <= receiver_start)
                .filter(|candidate| {
                    matches!(
                        candidate.kind,
                        SymbolKind::Local | SymbolKind::Param | SymbolKind::Const
                    )
                })
                .max_by_key(|candidate| candidate.span.start)
                .map(|candidate| candidate.id)
        })
        .or_else(|| tc.symbols.lookup_value(global, &receiver))
        .or_else(|| tc.symbols.lookup_type(global, &receiver))?;
    let parent = parent_type_symbol(tc, symbol)?;
    let owner_kind = tc.symbols.try_get(parent).map(|symbol| symbol.kind);

    let mut items: Vec<(u8, CompletionItem)> = Vec::new();

    if owner_kind == Some(SymbolKind::Struct)
        && let Some(fields) = tc.type_info.struct_fields.get(&parent)
    {
        for field in fields.iter() {
            if !accepted(prefix, prefix_l, field.name.as_str()) {
                continue;
            }
            let ty = tc.type_info.type_interner.resolve(field.ty);
            let mut item = CompletionItem::new(field.name.to_string(), CompletionKind::Field);
            item.detail = Some(format!("{}: {}", field.name, display_type(tc, &ty)));
            items.push((RANK_MEMBER, item));
        }
    }

    if owner_kind == Some(SymbolKind::Enum) {
        for (variant, (owner, _shape)) in &tc.type_info.enum_variants {
            if *owner != parent {
                continue;
            }
            let Some(name) = tc.type_info.enum_variant_name(&tc.symbols, *variant) else {
                continue;
            };
            if !accepted(prefix, prefix_l, &name) {
                continue;
            }
            let mut item = CompletionItem::new(name, CompletionKind::EnumMember);
            item.detail = Some("variant".to_string());
            let tag = tc
                .type_info
                .enum_variant_tags
                .get(variant)
                .copied()
                .unwrap_or(0);
            items.push((u8::try_from(tag).unwrap_or(RANK_MEMBER), item));
        }
    }

    if owner_kind == Some(SymbolKind::Interface)
        && let Some(info) = tc.type_info.interfaces.get(&parent)
    {
        for method in &info.methods {
            if !accepted(prefix, prefix_l, method.name.as_str()) {
                continue;
            }
            let signature = tc.type_info.type_interner.resolve(method.sig_id);
            let mut item = CompletionItem::new(method.name.to_string(), CompletionKind::Method);
            item.detail = Some(display_type(tc, &signature));
            items.push((RANK_MEMBER, item));
        }
    }

    for ((owner, name), &member) in &tc.symbols.associated_members {
        if *owner != parent || !accepted(prefix, prefix_l, name.as_str()) {
            continue;
        }
        let mut item = CompletionItem::new(name.to_string(), CompletionKind::Method);
        if let Some(symbol) = tc.symbols.try_get(member) {
            let presentation = symbol_presentation(snap, source, tc, symbol);
            item.detail = Some(presentation.signature);
            item.documentation = presentation.documentation;
        }
        items.push((RANK_MEMBER + 1, item));
    }

    if items.is_empty() {
        return None;
    }
    Some(finish_ranked(items))
}

fn module_member_completions(
    snap: &AnalysisSnapshot,
    tc: &TypeCheckResult,
    alias: &str,
    prefix: &str,
    prefix_l: &str,
) -> Option<Vec<CompletionItem>> {
    let mut items: Vec<(u8, CompletionItem)> = Vec::new();
    for ((module, name), &member) in &tc.symbols.module_members {
        if module.as_str() != alias {
            continue;
        }
        let name_s = name.as_str();
        // Associated method keys are `Type.method`; only plain members apply here.
        if name_s.contains('.') || !accepted(prefix, prefix_l, name_s) {
            continue;
        }
        let symbol = tc.symbols.try_get(member);
        let mut item = CompletionItem::new(
            name_s,
            symbol.map_or(CompletionKind::Text, |symbol| value_kind(symbol.kind)),
        );
        let presentation = symbol.and_then(|symbol| {
            let member_source = snap.db.source_file_by_id(symbol.id.file_id)?;
            Some(symbol_presentation(snap, member_source, tc, symbol))
        });
        item.detail = Some(presentation.as_ref().map_or_else(
            || format!("from `{alias}`"),
            |presentation| format!("{} — from `{alias}`", presentation.signature),
        ));
        item.documentation = presentation.and_then(|presentation| presentation.documentation);
        items.push((RANK_MEMBER, item));
    }
    if items.is_empty() {
        return None;
    }
    Some(finish_ranked(items))
}

/// Resolve the nominal type symbol behind a receiver.
fn parent_type_symbol(tc: &TypeCheckResult, symbol: SymbolId) -> Option<SymbolId> {
    if let Some(definition) = tc.symbols.try_get(symbol)
        && matches!(
            definition.kind,
            SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Interface
        )
    {
        return Some(symbol);
    }
    let ty = tc.type_info.decl_type(symbol)?;
    named_symbol_of(tc, &ty)
}

fn named_symbol_of(tc: &TypeCheckResult, ty: &ArType) -> Option<SymbolId> {
    match ty {
        ArType::Named(symbol, _) => Some(*symbol),
        ArType::Ref(inner)
        | ArType::RefMut(inner)
        | ArType::Ptr(inner)
        | ArType::Nullable(inner) => {
            named_symbol_of(tc, &tc.type_info.type_interner.resolve(*inner))
        }
        _ => None,
    }
}

// ── Annotations ─────────────────────────────────────────────────────

fn is_annotation_completion(text: &str, offset: u32, prefix: &str) -> bool {
    usize::try_from(offset)
        .ok()
        .and_then(|offset| offset.checked_sub(prefix.len()))
        .and_then(|name_start| name_start.checked_sub(1))
        .is_some_and(|at| text.as_bytes().get(at) == Some(&b'@'))
}

fn annotation_target_after(
    text: &str,
    offset: u32,
) -> Option<arandu_semantics::attributes::AnnotationTarget> {
    let offset = usize::try_from(offset).ok()?.min(text.len());
    let tail = &text[offset..];
    let keyword = tail
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .find(|part| !part.is_empty() && !matches!(*part, "public" | "async"))?;
    match keyword {
        "func" => Some(arandu_semantics::attributes::AnnotationTarget::Function),
        "extern" => Some(arandu_semantics::attributes::AnnotationTarget::ExternBlock),
        "struct" => Some(arandu_semantics::attributes::AnnotationTarget::Struct),
        "enum" => Some(arandu_semantics::attributes::AnnotationTarget::Enum),
        "interface" => Some(arandu_semantics::attributes::AnnotationTarget::Interface),
        "const" => Some(arandu_semantics::attributes::AnnotationTarget::Const),
        "type" => Some(arandu_semantics::attributes::AnnotationTarget::TypeAlias),
        _ => None,
    }
}

fn annotation_completions(text: &str, offset: u32, prefix: &str) -> Vec<CompletionItem> {
    use arandu_semantics::attributes as attrs;

    let target = annotation_target_after(text, offset);
    let mut items: Vec<CompletionItem> = attrs::BUILTIN_ANNOTATIONS
        .iter()
        .filter(|spec| {
            spec.availability == attrs::AnnotationAvailability::Implemented
                && target.is_none_or(|target| spec.targets.contains(&target))
                && (prefix.is_empty()
                    || spec
                        .canonical_name
                        .to_ascii_lowercase()
                        .starts_with(&prefix.to_ascii_lowercase()))
        })
        .map(|spec| {
            let (insert_text, is_snippet) = match spec.arguments {
                attrs::AnnotationArguments::None => (None, false),
                attrs::AnnotationArguments::OneString => (
                    Some(format!("{}(\"${{1:library}}\")", spec.canonical_name)),
                    true,
                ),
                attrs::AnnotationArguments::OneStringOrIdent => {
                    (Some(format!("{}(\"${{1:C}}\")", spec.canonical_name)), true)
                }
                attrs::AnnotationArguments::EffectList => {
                    (Some(format!("{}(${{1:Pure}})", spec.canonical_name)), true)
                }
            };
            let mut item = CompletionItem::new(spec.canonical_name, CompletionKind::Property);
            item.detail = Some(format!("@{} — {}", spec.canonical_name, spec.summary));
            item.documentation = Some(spec.summary.to_string());
            item.insert_text = insert_text;
            item.is_snippet = is_snippet;
            item
        })
        .collect();
    items.sort_by(|left, right| left.label.cmp(&right.label));
    items
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
