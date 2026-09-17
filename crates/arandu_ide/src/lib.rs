//! Editor-agnostic IDE analysis over a frozen analysis snapshot.
//!
//! Pure queries: no Salsa writes, no filesystem access, no `lsp_types`. The
//! language server maps these results to LSP protocol types; the in-browser
//! wasm bridge serializes them to JSON for the playground. Keeping the engine
//! here is what makes both front-ends share one completion brain instead of
//! drifting apart.

pub mod completion;
pub mod presentation;

pub use completion::{
    CompletionItem, CompletionKind, MAX_COMPLETION_ITEMS, completions, import_path_completions,
};
pub use presentation::{
    ParameterPresentation, SymbolPresentation, display_type, expr_symbol_at, prefix_at, symbol_at,
    symbol_documentation, symbol_kind_name, symbol_presentation, typecheck,
};
