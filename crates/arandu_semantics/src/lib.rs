#![allow(clippy::result_large_err)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::format_push_string,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::return_self_not_must_use,
    clippy::trivially_copy_pass_by_ref,
    clippy::unused_self,
    clippy::used_underscore_binding
)]

pub use arandu_codegen::{CodegenBackend, CompiledCode, JitError};
pub use arandu_middle::{
    BitMatrix, BitSet, CodeReplacement, DenseRange, DiagCode, Diagnostic, DocCommentMap, Hint,
    IntrinsicKind, Label, NodeKey, ResolutionResult, ResolvedNames, ScopeId, Severity, SmolStr,
    Symbol, SymbolId, SymbolKind, SymbolTable, amir, amir_validate, bitset, cfg, ctfe, diagnostics,
    hir, index_vec, intrinsics, layout, literal_pool, newtype_index, ops, resolved, symbol_table,
    types, validate_amir_program,
};
pub use arandu_mir::aggregate_storage;
pub use arandu_mir::static_data;

pub use arandu_middle::Visibility;
pub use arandu_middle::ops::{BinaryOp, SetOp, UnaryOp};

pub mod attributes;
pub mod passes;
pub mod testing;

pub use arandu_mir::{
    OptLevel, check_borrows, check_moves, lower_to_amir, lower_to_amir_with_interfaces,
    optimize_amir, optimize_amir_checked, optimize_amir_checked_with_level,
};
pub use arandu_resolve::{resolve_for_test, resolve_imports_and_bodies, resolve_local};
pub use arandu_typeck::type_checker::TargetInfo;
pub use arandu_typeck::{
    EnumPayloadShape, TypeCheckResult, TypeChecker, TypeInfo, body_item_symbols, check_bodies,
    check_bodies_only, check_func_body_only, check_item_body_only, check_non_func_bodies_only,
    check_signatures, check_signatures_only, free_func_symbols, item_source_span, primary_def_key,
    type_check,
};
pub use passes::lower_hir::{
    MaterializationError, link_hir_module, lower_block_to_hir, lower_ctfe_declarations_to_hir,
    lower_ctfe_function_to_hir, lower_declarations_to_hir, lower_expression_to_hir,
    lower_extern_to_hir, lower_function_to_hir, lower_to_hir, materialize_ctfe_scalar,
    materialize_ctfe_value,
};
pub use passes::monomorphize::monomorphize_program;
