mod ctor;
mod expr;
mod field;
mod match_exhaust;
pub(crate) mod method;
mod pattern;

pub(crate) use ctor::{synth_option_ctor, synth_poll_ctor, synth_result_ctor};
pub(crate) use field::{
    resolve_field, resolve_field_type, resolve_index, resolve_namespace_field,
    resolve_namespace_member_type,
};
pub(crate) use method::synth_method_call;

pub use expr::{synth_expr, synth_expr_expected};
pub use pattern::check_pattern;
