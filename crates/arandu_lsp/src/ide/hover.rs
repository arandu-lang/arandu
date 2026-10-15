//! Hover information generation and annotation documentation.

use arandu_base::LineIndex;
use arandu_query::{AnalysisSnapshot, SourceFile};
use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};

use super::presentation::{symbol_at, symbol_presentation, typecheck};
use crate::conv::{position_to_offset, span_to_range};

#[must_use]
pub fn hover(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    position: Position,
) -> Option<Hover> {
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    if let Some(hover) = annotation_hover(text, offset, &index) {
        return Some(hover);
    }
    let tc = typecheck(snap, source);
    let parsed = arandu_query::passes::parse(&snap.db, source);
    let program = (**parsed).as_ref().ok();
    if let Some(value) =
        program.and_then(|program| arandu_ide::comptime::value_at(program, &tc, offset))
    {
        return Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: comptime_markdown(&value),
            }),
            range: Some(span_to_range(&index, value.span)),
        });
    }
    let sym = symbol_at(&tc, offset)
        .or_else(|| super::presentation::expr_symbol_at(program?, &tc, offset))?;
    let symbol = tc.symbols.try_get(sym)?;
    let presentation = symbol_presentation(snap, source, &tc, symbol);
    let mut md = format!("```arandu\n{}\n```", presentation.signature);
    if let Some(documentation) = presentation.documentation {
        md.push_str("\n\n");
        md.push_str(&documentation);
    }
    let extra =
        program.and_then(|program| arandu_ide::comptime::arguments_at(program, &tc, offset));
    if let Some(extra) = &extra {
        md.push_str("\n\n");
        md.push_str(&comptime_markdown(extra));
    }
    let range = span_to_range(
        &index,
        extra.as_ref().map_or(symbol.span, |extra| extra.span),
    );
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: md,
        }),
        range: Some(range),
    })
}

fn comptime_markdown(value: &arandu_ide::comptime::ComptimePresentation) -> String {
    let values = value
        .values
        .iter()
        .map(|value| format!("= {value}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("```arandu\n{values}\n```\n\n*(evaluated at compile time)*")
}

pub(crate) fn annotation_hover(text: &str, offset: u32, index: &LineIndex) -> Option<Hover> {
    let offset = usize::try_from(offset).ok()?.min(text.len());
    let bytes = text.as_bytes();
    let mut start = offset;
    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }
    let mut end = offset;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    if start == 0 || bytes.get(start - 1) != Some(&b'@') || start == end {
        return None;
    }
    let (spec, legacy) = arandu_semantics::attributes::annotation_spec(&text[start..end])?;
    let targets = spec
        .targets
        .iter()
        .map(|target| target.description())
        .collect::<Vec<_>>()
        .join(", ");
    let mut value = format!(
        "```arandu\n@{}\n```\n\n{}\n\n**Targets:** {}\n\n**Arguments:** {}",
        spec.canonical_name,
        spec.summary,
        targets,
        spec.arguments.synopsis()
    );
    if legacy {
        value.push_str(&format!(
            "\n\nThis spelling is deprecated; use `@{}`.",
            spec.canonical_name
        ));
    }
    let span = arandu_base::Span::new(0, u32::try_from(start).ok()?, u32::try_from(end).ok()?);
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(span_to_range(index, span)),
    })
}
