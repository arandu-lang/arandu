mod ar_type;
mod borrow;
pub mod lower;
mod primitive;
mod result_option;
mod shape;
pub mod subst;
pub mod type_interner;
mod unify;

/// Bounds eager repeat materialization in the shared AMIR producer. The parser
/// retains one initializer regardless of length; backends receive ordinary arrays.
pub const MAX_ARRAY_REPEAT_ELEMENTS: u64 = 65_536;

pub use ar_type::ArType;
pub use borrow::{
    BorrowKind, BorrowPath, BorrowPathSegment, BorrowSource, ReturnBorrowDependency,
    ReturnBorrowSummary,
};
pub use lower::{LowerCtx, lower_named_type, lower_result_type, lower_type_expr};
pub use primitive::Primitive;
pub use result_option::{
    index_elem_type, is_err_type, is_option_type, is_poll_type, is_result_type, is_tryable_type,
    is_vec_type, poll_ready_type, result_ok_err, result_ok_err_id, result_ok_err_id_fast,
    result_ok_err_ids, result_type_decl_span, try_ok_type, type_name_base,
};
pub use shape::{FunctionInstance, TypeShape, TypeShapeError};
pub use subst::{GenericSubst, build_subst, build_subst_ids, substitute_type, substitute_type_id};
pub use type_interner::{TypeId, TypeInterner};
pub use unify::{
    is_assignable, is_assignable_return_type, resolve_literal_pair, unify, unify_return_type,
};
