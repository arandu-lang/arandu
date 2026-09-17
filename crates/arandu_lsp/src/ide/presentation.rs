//! LSP adapter over the shared presentation helpers in `arandu_ide`.
//!
//! The real logic (typecheck view, signature rendering, symbol resolution) is
//! editor-agnostic and lives in `arandu_ide`. Only [`markdown_documentation`]
//! is LSP-specific because it produces an `lsp_types::Documentation`.

use lsp_types::{Documentation, MarkupContent, MarkupKind};

pub use arandu_ide::presentation::{
    display_type, expr_symbol_at, symbol_at, symbol_presentation, typecheck,
};

#[cfg(test)]
pub use arandu_ide::presentation::prefix_at;

pub(crate) fn markdown_documentation(value: String) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    })
}
