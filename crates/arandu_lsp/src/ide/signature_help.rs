//! Interactive parameter and signature help for function/method calls.

use arandu_base::LineIndex;
use arandu_middle::SymbolKind;
use arandu_query::{AnalysisSnapshot, SourceFile};
use lsp_types::{ParameterInformation, ParameterLabel, Position};

use super::presentation::{markdown_documentation, symbol_presentation, typecheck};
use crate::conv::position_to_offset;

#[must_use]
pub fn signature_help(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    position: Position,
) -> Option<lsp_types::SignatureHelp> {
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let context = call_context(snap, source, offset)?;

    let tc = typecheck(snap, source);
    let sym = arandu_ide::signature_help::callee_symbol(snap, source, &tc, &context)?;
    let symbol = tc.symbols.try_get(sym)?;
    if !matches!(
        symbol.kind,
        SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc
    ) {
        return None;
    }
    let presentation = symbol_presentation(snap, source, &tc, symbol);
    let active_parameter = (!presentation.parameters.is_empty()).then(|| {
        context
            .active_parameter
            .min(u32::try_from(presentation.parameters.len() - 1).unwrap_or(u32::MAX))
    });
    let parameters = presentation
        .parameters
        .into_iter()
        .map(|parameter| ParameterInformation {
            label: ParameterLabel::Simple(parameter.label),
            documentation: None,
        })
        .collect();
    Some(lsp_types::SignatureHelp {
        signatures: vec![lsp_types::SignatureInformation {
            label: presentation.signature,
            documentation: presentation.documentation.map(markdown_documentation),
            parameters: Some(parameters),
            active_parameter,
        }],
        active_signature: Some(0),
        active_parameter,
    })
}

pub(crate) use arandu_ide::signature_help::call_context;
