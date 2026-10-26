pub use arandu_middle::types::{
    ArType, GenericSubst, Primitive, TypeId, TypeInterner, build_subst, build_subst_ids,
    index_elem_type, is_assignable, is_assignable_return_type, is_err_type, is_option_type,
    is_result_type, is_tryable_type, is_vec_type, resolve_literal_pair, result_ok_err,
    result_ok_err_id, result_ok_err_id_fast, result_ok_err_ids, substitute_type,
    substitute_type_id, try_ok_type, type_interner, unify, unify_return_type,
};
pub use lower::{
    LowerCtx, lower_named_type, lower_result_type, lower_type_expr, result_type_decl_span,
    type_name_base,
};

pub mod generic_inst;
pub mod interfaces;
pub mod lower;
pub(crate) mod transfer;

pub use generic_inst::{
    expand_aliases, expand_named_with_defaults, expand_type_args_with_defaults,
    extract_generic_param_symbols, struct_field_instantiated, struct_fields_instantiated,
    synth_generic_instantiation,
};
pub use interfaces::{InterfaceInfo, InterfaceMethod, collect_interfaces_and_constraints};
