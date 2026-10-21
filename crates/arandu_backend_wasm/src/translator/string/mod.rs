use arandu_middle::amir::value::AmirOperand;
use arandu_middle::types::{ArType, Primitive, TypeId};

use super::FuncTranslator;
use crate::types;

mod interp;
mod to_str;
mod to_str_float;
mod to_str_int;

impl<'a> FuncTranslator<'a> {
    /// Format a primitive value as a `str` fat pointer (ptr, len).
    pub(super) fn emit_to_str(&mut self, value: &AmirOperand, src_ty: TypeId) {
        let ar_ty = self.interner.resolve(src_ty);
        if matches!(ar_ty, ArType::Primitive(Primitive::Str)) {
            self.emit_to_str_str(value, src_ty);
            return;
        }

        if matches!(ar_ty, ArType::Primitive(Primitive::Bool)) {
            self.emit_to_str_bool(value);
            return;
        }

        if matches!(ar_ty, ArType::Primitive(Primitive::Char)) {
            self.emit_to_str_char(value);
            return;
        }

        if types::ar_is_float(src_ty, self.interner) {
            self.emit_to_str_float(value, src_ty);
            return;
        }

        self.emit_to_str_int(value, src_ty);
    }
}
