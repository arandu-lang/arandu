pub mod type_checker;

pub use type_checker::{
    EnumPayloadShape, TypeCheckResult, TypeChecker, TypeInfo, body_item_symbols,
    check::check_bodies, check::check_signatures, check_bodies_only, check_func_body_only,
    check_item_body_only, check_non_func_bodies_only, check_signatures_only, free_func_symbols,
    item_source_span, primary_def_key, translate_type, type_check,
};

pub use arandu_middle::{
    CodeReplacement, DiagCode, Diagnostic, Hint, Label, NodeKey, ScopeId, Severity, Span, SymbolId,
    SymbolKind, SymbolTable,
};
pub use arandu_resolve::{ResolutionResult, ResolvedNames};

pub mod passes {
    pub use crate::type_checker;
}
