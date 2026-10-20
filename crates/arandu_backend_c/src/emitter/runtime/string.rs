//! C runtime helpers for string concatenation, formatting, and string equality.

use super::super::CEmitter;

impl CEmitter<'_> {
    pub(super) fn emit_str_eq_runtime(&mut self) {
        self.output.push_str(include_str!("string_eq.c"));
    }

    pub(super) fn emit_str_concat_runtime(&mut self) {
        self.output.push_str(include_str!("string_concat.c"));
    }

    pub(super) fn emit_str_format_runtime(&mut self) {
        self.output.push_str(include_str!("string_format.c"));
    }
}
