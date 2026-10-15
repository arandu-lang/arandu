//! Name resolution pass for Arandu.
//!
//! Resolves identifiers to [`SymbolId`]s, builds the [`SymbolTable`], and
//! populates [`ResolvedNames`] (the mapping from AST nodes to their symbols).
//!
//! **Production** entry: [`resolve_imports_and_bodies`] with a Salsa
//! [`SourceDbLoader`].
//! **Tests** should prefer the same function with [`EmptyModuleLoader`] via
//! [`resolve_for_test`] so import/prelude logic is not forked.

pub mod import_path;
pub mod module_loader;
pub mod name_resolution;

pub use import_path::{LogicalImport, canonicalize_import_path, logical_import};
pub use module_loader::{EmptyModuleLoader, ModuleLoader, SourceDbLoader};
pub use name_resolution::{
    BodySelection, HeaderResolution, resolve_bodies_with_poll, resolve_for_test,
    resolve_headers_with_poll, resolve_imports_and_bodies, resolve_imports_and_bodies_with_poll,
    resolve_local, resolve_local_with_poll, resolve_selected_body_with_poll,
};

pub use arandu_middle::{
    CodeReplacement, DiagCode, Diagnostic, DocCommentMap, Label, NodeKey, ResolutionResult,
    ResolvedNames, ScopeId, Severity, Symbol, SymbolId, SymbolKind, SymbolTable,
};
