use super::{
    Candidate, CompletionItem, CompletionKind, KEYWORDS, PRIMITIVE_TYPES, RANK_KEYWORD, RANK_LOCAL,
    RANK_MODULE, RANK_TYPE, RANK_VALUE, accepted, finish, offer, value_kind,
};
use crate::presentation::symbol_presentation;
use arandu_middle::SymbolKind;
use arandu_query::{AnalysisSnapshot, SourceFile};
use arandu_semantics::TypeCheckResult;
use rustc_hash::FxHashMap;

// ── General completion ──────────────────────────────────────────────

pub(super) fn general_completions(
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

pub(super) fn type_position_completions(
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
