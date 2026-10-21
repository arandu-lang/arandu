//! AMIR node, dataflow, and IDE-fact hashes.

mod facts;
mod operand;
mod rvalue;
mod stmt;
mod term;
mod ty;

#[cfg(test)]
pub(super) use rvalue::hash_rvalue;
pub(super) use stmt::hash_stmt;
pub(super) use term::hash_terminator;
pub(super) use ty::{
    hash_ar_type, hash_id, hash_optional_span, hash_optional_symbol_id, hash_span,
};
