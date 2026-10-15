pub mod aggregate_storage;
pub mod analysis_limits;
pub mod borrow_audit;
pub mod borrow_check;
pub mod borrow_facts;
pub mod borrow_interface;
pub mod ctfe;
pub(crate) mod dce;
pub mod definite_init;
pub mod drop_elaborate;
pub mod escape_analysis;
pub mod gen_promote;
pub(crate) mod gvn;
pub mod inlining;
pub mod licm;
pub mod liveness;
pub mod lower_amir;
pub mod move_checker;
pub mod optimize;
pub mod pass_manager;
pub mod pin_free;
pub mod reachability;
mod scalarize_locals;
pub(crate) mod sccp;
pub(crate) mod simplify_cfg;
pub(crate) mod sroa;
pub mod static_data;
pub mod suspend_check;
mod tail_calls;
mod tail_merge;

pub use borrow_check::check_borrows;
pub use inlining::inline_leaf_functions;
pub use licm::licm;
pub use lower_amir::{
    ComposedUnits, ContextualFunctionUnit, FunctionUnit, append_function_unit,
    compose_function_units, finalize_function_unit, lower_block_unit, lower_borrow_interfaces,
    lower_expression_unit, lower_function_unit, lower_to_amir, lower_to_amir_with_interfaces,
    lower_to_amir_with_layout,
};
pub use move_checker::check_moves;
pub use optimize::{
    optimize_amir, optimize_amir_checked, optimize_amir_checked_with_level,
    optimize_amir_with_level,
};
pub use pass_manager::{FunctionPass, OptLevel, PassManager, PassStats};
pub use pin_free::apply_pin_free_refs;
pub use suspend_check::check_borrow_across_suspend;

pub use arandu_middle::{
    BitMatrix, BitSet, CodeReplacement, DiagCode, Diagnostic, DocCommentMap, Label,
    NO_GENERATIONAL_FALLBACK, NodeKey, ResolvedNames, ScopeId, Severity, Span, SymbolId,
    SymbolKind, SymbolTable, amir, amir_validate, cfg, diagnostics, hir, layout, literal_pool, ops,
    types,
};

pub use arandu_typeck::TypeCheckResult;

pub mod passes {
    pub mod type_checker {
        pub use arandu_middle::types;
        pub use arandu_typeck::EnumPayloadShape;
    }
}
