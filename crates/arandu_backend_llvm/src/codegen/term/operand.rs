use arandu_middle::Diagnostic;
use arandu_middle::amir::{AmirConstant, AmirOperand};
use arandu_middle::literal_pool::LiteralId;
use arandu_middle::types::ArType;

use super::super::{
    FuncEmitter, TypedValue, ensure_float_decimal, ice, parse_char_literal, quote_name, temp_name,
};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(in crate::codegen) fn operand(
        &mut self,
        operand: &AmirOperand,
        expected: &ArType,
    ) -> Result<TypedValue, Diagnostic> {
        match operand {
            AmirOperand::Constant(constant) => self.constant(constant, expected),
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                let actual_ty = self.temp_type(*temp);
                let override_ty = self.temp_llvm_ty[temp.as_usize()].clone();
                if let Some(slot) = self.temp_slot(*temp).map(str::to_owned) {
                    let ty_text = override_ty
                        .clone()
                        .map_or_else(|| self.module.llvm_ty(&actual_ty), Ok)?;
                    let align = self.module.align_of(&actual_ty);
                    let loaded = self.new_value();
                    self.writer.line(format!(
                        "{loaded} = load {ty_text}, ptr {slot}, align {align}"
                    ));
                    let mut loaded_value = TypedValue::new(ty_text, loaded);
                    if self.module.integer_width(&actual_ty).is_some() {
                        loaded_value =
                            loaded_value.with_source_signed(self.module.is_signed(&actual_ty));
                    }
                    return self.coerce(loaded_value, expected);
                }
                let value = if temp.as_usize() == 0 {
                    self.return_value()
                        .ok_or_else(|| ice("return register is read before it is assigned"))?
                } else {
                    self.ensure_defined(*temp)?;
                    temp_name(*temp)
                };
                let llvm_ty = override_ty.map_or_else(|| self.module.llvm_ty(&actual_ty), Ok)?;
                let mut actual = TypedValue::new(llvm_ty, value);
                // Prefer AMIR temp signedness: `u8 as u32` must zext, not sext.
                if self.module.integer_width(&actual_ty).is_some() {
                    actual = actual.with_source_signed(self.module.is_signed(&actual_ty));
                }
                self.coerce(actual, expected)
            }
            AmirOperand::FunctionRef(symbol) | AmirOperand::GlobalRef(symbol) => {
                let value = TypedValue::new(
                    "ptr".to_owned(),
                    format!("@{}", quote_name(&self.module.function_name(*symbol))),
                );
                self.coerce(value, expected)
            }
        }
    }

    pub(super) fn constant(
        &self,
        constant: &AmirConstant,
        expected: &ArType,
    ) -> Result<TypedValue, Diagnostic> {
        let expected_text = self.module.llvm_ty(expected)?;
        match constant {
            AmirConstant::Bool(value) => {
                // `true`/`false` are i1 tokens; integer projections (ToStr →
                // `ar_jit_bool_to_str(i8, ...)`) need numeric 0/1 literals.
                let literal = if expected_text == "i1" {
                    if *value { "true" } else { "false" }
                } else if *value {
                    "1"
                } else {
                    "0"
                };
                Ok(TypedValue::new(expected_text, literal.to_owned()))
            }
            AmirConstant::Nil => {
                if expected_text == "ptr" {
                    Ok(TypedValue::new("ptr".to_owned(), "null".to_owned()))
                } else {
                    Ok(TypedValue::new(expected_text, "0".to_owned()))
                }
            }
            AmirConstant::Pool(id) => self.literal(*id, expected, &expected_text),
        }
    }

    pub(super) fn literal(
        &self,
        id: LiteralId,
        expected: &ArType,
        expected_text: &str,
    ) -> Result<TypedValue, Diagnostic> {
        use arandu_middle::literal_pool::AmirLiteralEntry;
        match self.module.program.literal_pool.get(id) {
            AmirLiteralEntry::Int(text) => {
                let value = arandu_middle::literal_pool::parse_int_literal(text)
                    .ok_or_else(|| ice(format!("invalid integer literal {text:?}")))?;
                let width = self
                    .module
                    .integer_width(expected)
                    .or_else(|| {
                        if expected_text == "ptr" {
                            Some(self.module.layout.pointer_width())
                        } else {
                            None
                        }
                    })
                    .unwrap_or(32);
                let masked = if width >= 128 {
                    value as u128
                } else {
                    (value as u128) & ((1u128 << width) - 1)
                };
                let rendered = if expected_text == "float" || expected_text == "double" {
                    format!("{masked}.0")
                } else {
                    masked.to_string()
                };
                Ok(TypedValue::new(expected_text.to_owned(), rendered))
            }
            AmirLiteralEntry::Float(text) => {
                let value = arandu_middle::literal_pool::parse_float_literal(text)
                    .ok_or_else(|| ice(format!("invalid float literal {text:?}")))?;
                let rendered = if expected_text == "float" {
                    format!("{:?}", value as f32)
                } else {
                    format!("{value:?}")
                };
                Ok(TypedValue::new(
                    expected_text.to_owned(),
                    ensure_float_decimal(&rendered),
                ))
            }
            AmirLiteralEntry::FloatBits(value) => {
                let rendered = if value.ty().bit_width() == 32 {
                    format!("{:?}", f32::from_bits(value.bits() as u32))
                } else {
                    format!("{:?}", f64::from_bits(value.bits()))
                };
                Ok(TypedValue::new(
                    expected_text.to_owned(),
                    ensure_float_decimal(&rendered),
                ))
            }
            AmirLiteralEntry::Char(text) => {
                let ch = parse_char_literal(text)
                    .ok_or_else(|| ice(format!("invalid char literal {text:?}")))?;
                Ok(TypedValue::new(
                    expected_text.to_owned(),
                    u32::from(ch).to_string(),
                ))
            }
            AmirLiteralEntry::Str(_) => {
                let index = id.0;
                let (name, length) = self
                    .module
                    .string_globals
                    .get(&index)
                    .ok_or_else(|| ice("string literal has no LLVM global"))?;
                if expected_text == "ptr" {
                    Ok(TypedValue::new(
                        "ptr",
                        format!(
                            "getelementptr inbounds ([{} x i8], ptr {name}, i64 0, i64 0)",
                            length + 1
                        ),
                    ))
                } else if expected_text == self.module.fat_ptr_llvm_ty() {
                    let len_ty = self.module.ptr_int_ty();
                    Ok(TypedValue::new(
                        expected_text.to_owned(),
                        format!("{{ ptr {name}, {len_ty} {length} }}"),
                    ))
                } else if self.module.is_pointer(expected) {
                    Ok(TypedValue::new(
                        "ptr",
                        format!(
                            "getelementptr inbounds ([{} x i8], ptr {name}, i64 0, i64 0)",
                            length + 1
                        ),
                    ))
                } else {
                    Err(ice(format!(
                        "LLVM backend cannot use a string literal as {expected:?}"
                    )))
                }
            }
        }
    }
}
