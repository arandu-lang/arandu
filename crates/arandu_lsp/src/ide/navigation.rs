//! References, document highlights, folding ranges, and selection ranges.

use arandu_base::LineIndex;
use arandu_middle::types::ArType;
use arandu_middle::SymbolId;
use arandu_query::{AnalysisSnapshot, SourceFile};
use arandu_semantics::TypeCheckResult;
use lsp_types::{
    DocumentHighlight, DocumentHighlightKind, FoldingRange, FoldingRangeKind, Location, Position,
    SelectionRange, Uri,
};

use super::presentation::{symbol_at, typecheck};
use crate::conv::{position_to_offset, span_to_range};

/// Type symbol targeted by "goto type definition" for `symbol`.
///
/// When the cursor sits on a type usage the type symbol is the symbol itself;
/// for values it is the underlying named type (unwrapping nullable/slice/
/// array/pointer/reference/result carriers).
#[must_use]
pub fn type_definition_symbol(tc: &TypeCheckResult, symbol: SymbolId) -> Option<SymbolId> {
    let kind = tc.symbols.try_get(symbol)?.kind;
    if kind.is_type() {
        return Some(symbol);
    }
    let ty = tc.type_info.decl_type(symbol)?;
    named_type_symbol(tc, &ty)
}

fn named_type_symbol(tc: &TypeCheckResult, ty: &ArType) -> Option<SymbolId> {
    use arandu_middle::types::ArType::*;
    match ty {
        Named(id, _) => Some(*id),
        Nullable(inner)
        | Slice(inner)
        | Array(_, inner)
        | Ptr(inner)
        | Ref(inner)
        | RefMut(inner)
        | Coroutine(inner)
        | Poll(inner)
        | Range(inner)
        | Option(inner) => named_type_symbol(tc, &tc.type_info.type_interner.resolve(*inner)),
        Result(ok, err) => {
            let ok_ty = tc.type_info.type_interner.resolve(*ok);
            named_type_symbol(tc, &ok_ty)
                .or_else(|| named_type_symbol(tc, &tc.type_info.type_interner.resolve(*err)))
        }
        _ => None,
    }
}

#[must_use]
pub fn references(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    position: Position,
    uri: &Uri,
    documents: &[super::types::DocSnap],
    include_declaration: bool,
) -> Vec<Location> {
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let tc = typecheck(snap, source);
    let program = arandu_query::passes::parse(&snap.db, source);
    let Some(sym) = symbol_at(&tc, offset).or_else(|| {
        program
            .as_ref()
            .as_ref()
            .ok()
            .and_then(|program| super::presentation::expr_symbol_at(program, &tc, offset))
    }) else {
        return Vec::new();
    };
    let mut locs = Vec::new();
    let collect_in_doc = |doc_source: SourceFile, doc_uri: &Uri, locs: &mut Vec<Location>| {
        let doc_text = doc_source.text(&snap.db);
        let doc_index = LineIndex::new(doc_text);
        let doc_tc = typecheck(snap, doc_source);
        for span in arandu_query::rename_occurrences(&snap.db, doc_source, sym) {
            let is_declaration = doc_tc.resolved.definitions.iter().any(|(key, symbol)| {
                *symbol == sym && key.start <= span.start && span.end <= key.end
            });
            if include_declaration || !is_declaration {
                locs.push(Location {
                    uri: doc_uri.clone(),
                    range: span_to_range(&doc_index, span),
                });
            }
        }
    };
    if documents.is_empty() {
        collect_in_doc(source, uri, &mut locs);
    } else {
        let mut saw_current = false;
        for doc in documents {
            if doc.source == source {
                saw_current = true;
            }
            collect_in_doc(doc.source, &doc.uri, &mut locs);
        }
        if !saw_current {
            collect_in_doc(source, uri, &mut locs);
        }
    }
    locs.sort_by(|a, b| {
        a.uri
            .as_str()
            .cmp(b.uri.as_str())
            .then_with(|| a.range.start.line.cmp(&b.range.start.line))
            .then_with(|| a.range.start.character.cmp(&b.range.start.character))
            .then_with(|| a.range.end.line.cmp(&b.range.end.line))
            .then_with(|| a.range.end.character.cmp(&b.range.end.character))
    });
    locs.dedup_by(|a, b| a.uri == b.uri && a.range == b.range);
    locs
}

#[must_use]
pub fn document_highlights(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    position: Position,
) -> Vec<DocumentHighlight> {
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let Ok(target) = arandu_query::prepare_rename(&snap.db, source, offset) else {
        return Vec::new();
    };
    arandu_query::rename_occurrences(&snap.db, source, target.symbol)
        .into_iter()
        .map(|span| DocumentHighlight {
            range: span_to_range(&index, span),
            // Resolution currently records identity, not access mode. Text is
            // preferable to inventing read/write semantics from source text.
            kind: Some(DocumentHighlightKind::TEXT),
        })
        .collect()
}

#[must_use]
pub fn folding_ranges(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
) -> Vec<FoldingRange> {
    let tree = arandu_query::passes::syntax_tree(&snap.db, source);
    let index = LineIndex::new(text);
    let file_id = *source.file_id(&snap.db);
    let mut ranges = Vec::new();

    for node in tree.root().descendants() {
        if node.kind() == arandu_parser::SyntaxKind::BLOCK {
            push_folding_range(
                &mut ranges,
                &index,
                file_id,
                u32::from(node.text_range().start()),
                u32::from(node.text_range().end()),
                None,
            );
        }
    }
    for token in tree
        .root()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        if token.kind() == arandu_parser::SyntaxKind::COMMENT {
            push_folding_range(
                &mut ranges,
                &index,
                file_id,
                u32::from(token.text_range().start()),
                u32::from(token.text_range().end()),
                Some(FoldingRangeKind::Comment),
            );
        }
    }
    ranges.sort_by_key(|range| {
        (
            range.start_line,
            range.start_character,
            range.end_line,
            range.end_character,
        )
    });
    ranges.dedup_by(|left, right| {
        left.start_line == right.start_line
            && left.start_character == right.start_character
            && left.end_line == right.end_line
            && left.end_character == right.end_character
    });
    ranges
}

fn push_folding_range(
    ranges: &mut Vec<FoldingRange>,
    index: &LineIndex,
    file_id: u32,
    start: u32,
    end: u32,
    kind: Option<FoldingRangeKind>,
) {
    let range = span_to_range(index, arandu_base::Span::new(file_id, start, end));
    if range.start.line >= range.end.line {
        return;
    }
    ranges.push(FoldingRange {
        start_line: range.start.line,
        start_character: Some(range.start.character),
        end_line: range.end.line,
        end_character: Some(range.end.character),
        kind,
        collapsed_text: None,
    });
}

#[must_use]
pub fn selection_ranges(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    positions: &[Position],
) -> Vec<SelectionRange> {
    let tree = arandu_query::passes::syntax_tree(&snap.db, source);
    let root = tree.root();
    let index = LineIndex::new(text);
    let file_id = *source.file_id(&snap.db);
    positions
        .iter()
        .map(|&position| {
            let offset = position_to_offset(&index, position, text);
            let token = root
                .token_at_offset(offset.into())
                .right_biased()
                .or_else(|| root.token_at_offset(offset.into()).left_biased());
            let mut byte_ranges = Vec::new();
            if let Some(token) = token {
                byte_ranges.push((
                    u32::from(token.text_range().start()),
                    u32::from(token.text_range().end()),
                ));
                byte_ranges.extend(token.parent().into_iter().flat_map(|parent| {
                    parent.ancestors().map(|node| {
                        (
                            u32::from(node.text_range().start()),
                            u32::from(node.text_range().end()),
                        )
                    })
                }));
            } else {
                byte_ranges.push((0, u32::try_from(text.len()).unwrap_or(u32::MAX)));
            }
            byte_ranges.retain(|(start, end)| start < end && *start <= offset && offset <= *end);
            byte_ranges.dedup();

            let mut parent = None;
            for (start, end) in byte_ranges.into_iter().rev() {
                parent = Some(Box::new(SelectionRange {
                    range: span_to_range(&index, arandu_base::Span::new(file_id, start, end)),
                    parent,
                }));
            }
            parent.map_or_else(
                || SelectionRange {
                    range: lsp_types::Range::new(position, position),
                    parent: None,
                },
                |range| *range,
            )
        })
        .collect()
}
