use super::super::LowerCtx;
use crate::amir::{AmirOperand, AmirRvalue};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::ops::BinaryOp;
use crate::passes::type_checker::types::{ArType, Primitive};
use crate::{SymbolId, SymbolTable};
use arandu_lexer::Span;

pub(super) struct BuiltinResultPatternInput<'a> {
    pub(super) scrutinee: AmirOperand,
    pub(super) variant: &'a str,
    pub(super) payload: &'a [crate::hir::HirPatternId],
    pub(super) ok: crate::types::TypeId,
    pub(super) err: crate::types::TypeId,
    pub(super) span: Span,
    pub(super) symbols: &'a SymbolTable,
}

pub(super) struct BuiltinOptionPatternInput<'a> {
    pub(super) scrutinee: AmirOperand,
    pub(super) variant: &'a str,
    pub(super) payload: &'a [crate::hir::HirPatternId],
    pub(super) inner: crate::types::TypeId,
    pub(super) span: Span,
    pub(super) symbols: &'a SymbolTable,
    pub(super) bind: bool,
}

pub(super) struct BuiltinPollPatternInput<'a> {
    pub(super) scrutinee: AmirOperand,
    pub(super) variant: &'a str,
    pub(super) payload: &'a [crate::hir::HirPatternId],
    pub(super) inner: crate::types::TypeId,
    pub(super) span: Span,
    pub(super) symbols: &'a SymbolTable,
    pub(super) bind: bool,
}

impl LowerCtx<'_> {
    /// SYN.3: `Some(v)` / `None` against `Option<T>` (tags: None=0, Some=1).
    pub(super) fn lower_option_type_tuple_pattern(
        &mut self,
        input: BuiltinOptionPatternInput<'_>,
    ) -> Result<AmirOperand, Diagnostic> {
        let BuiltinOptionPatternInput {
            scrutinee,
            variant,
            payload,
            inner,
            span,
            symbols,
            bind,
        } = input;
        let (want_tag, expect_payload) = match variant {
            "None" => (0usize, false),
            "Some" => (1usize, true),
            _ => {
                return Err(Diagnostic::error(
                    DiagCode::T018UndefinedField,
                    format!("variant '{variant}' is not defined on Option"),
                    span,
                ));
            }
        };
        if expect_payload {
            if payload.len() != 1 {
                return Err(Diagnostic::error(
                    DiagCode::T012WrongArgCount,
                    format!(
                        "variant 'Some' expects 1 payload item, found {}",
                        payload.len()
                    ),
                    span,
                ));
            }
        } else if !payload.is_empty() {
            return Err(Diagnostic::error(
                DiagCode::T012WrongArgCount,
                format!(
                    "variant 'None' expects 0 payload items, found {}",
                    payload.len()
                ),
                span,
            ));
        }

        let tmp_tag = self.new_temp(ArType::Primitive(Primitive::Int));
        self.emit_assign_temp(tmp_tag, AmirRvalue::Discriminant { value: scrutinee });
        let tag_op = AmirOperand::Constant(self.intern_literal_int(want_tag.to_string()));
        let tag_matches = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            tag_matches,
            AmirRvalue::Binary {
                op: BinaryOp::Equal,
                left: AmirOperand::Copy(tmp_tag),
                right: tag_op,
            },
        );

        if !expect_payload {
            return Ok(AmirOperand::Copy(tag_matches));
        }

        let payload_tmp = self.new_temp_id(inner);
        // Result/Option layout: field 0 = discriminant, field 1 = payload.
        self.emit_assign_temp(
            payload_tmp,
            AmirRvalue::EnumPayload {
                value: scrutinee,
                variant: SymbolId::DUMMY,
                variant_tag: want_tag,
                index: 0,
                field_ty: inner,
                tuple_ty: None,
            },
        );
        let pat = self.hir.pool.pattern(payload[0]);
        let item_matches =
            self.lower_pattern_match_mode(AmirOperand::Copy(payload_tmp), pat, symbols, bind)?;
        let and_dest = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            and_dest,
            AmirRvalue::Binary {
                op: BinaryOp::And,
                left: AmirOperand::Copy(tag_matches),
                right: item_matches,
            },
        );
        Ok(AmirOperand::Copy(and_dest))
    }

    /// `Ok(v)` / `Err(e)` against builtin `Result<T, E>` (tags: Ok=0, Err=1).
    pub(super) fn lower_result_type_tuple_pattern(
        &mut self,
        input: BuiltinResultPatternInput<'_>,
        bind: bool,
    ) -> Result<AmirOperand, Diagnostic> {
        let BuiltinResultPatternInput {
            scrutinee,
            variant,
            payload,
            ok,
            err,
            span,
            symbols,
        } = input;
        let (want_tag, payload_ty) = match variant {
            "Ok" => (0usize, ok),
            "Err" => (1usize, err),
            _ => {
                return Err(Diagnostic::error(
                    DiagCode::T018UndefinedField,
                    format!("variant '{variant}' is not defined on Result"),
                    span,
                ));
            }
        };
        if payload.len() != 1 {
            return Err(Diagnostic::error(
                DiagCode::T012WrongArgCount,
                format!(
                    "variant '{variant}' expects 1 payload item, found {}",
                    payload.len()
                ),
                span,
            ));
        }

        let tmp_tag = self.new_temp(ArType::Primitive(Primitive::Int));
        self.emit_assign_temp(tmp_tag, AmirRvalue::Discriminant { value: scrutinee });
        let tag_op = AmirOperand::Constant(self.intern_literal_int(want_tag.to_string()));
        let tag_matches = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            tag_matches,
            AmirRvalue::Binary {
                op: BinaryOp::Equal,
                left: AmirOperand::Copy(tmp_tag),
                right: tag_op,
            },
        );

        let payload_tmp = self.new_temp_id(payload_ty);
        self.emit_assign_temp(
            payload_tmp,
            AmirRvalue::EnumPayload {
                value: scrutinee,
                variant: SymbolId::DUMMY,
                variant_tag: want_tag,
                index: 0,
                field_ty: payload_ty,
                tuple_ty: None,
            },
        );
        let pat = self.hir.pool.pattern(payload[0]);
        let item_matches =
            self.lower_pattern_match_mode(AmirOperand::Copy(payload_tmp), pat, symbols, bind)?;
        let and_dest = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            and_dest,
            AmirRvalue::Binary {
                op: BinaryOp::And,
                left: AmirOperand::Copy(tag_matches),
                right: item_matches,
            },
        );
        Ok(AmirOperand::Copy(and_dest))
    }

    /// `Ready(v)` / `Pending` against builtin `Poll<T>` (tags: Ready=0, Pending=1).
    pub(super) fn lower_poll_type_tuple_pattern(
        &mut self,
        input: BuiltinPollPatternInput<'_>,
    ) -> Result<AmirOperand, Diagnostic> {
        let BuiltinPollPatternInput {
            scrutinee,
            variant,
            payload,
            inner,
            span,
            symbols,
            bind,
        } = input;
        let (tag, has_payload) = match variant {
            "Ready" => (0usize, true),
            "Pending" => (1usize, false),
            _ => {
                return Err(Diagnostic::error(
                    DiagCode::T018UndefinedField,
                    format!("variant '{variant}' is not defined on Poll"),
                    span,
                ));
            }
        };
        if payload.len() != usize::from(has_payload) {
            return Err(Diagnostic::error(
                DiagCode::T012WrongArgCount,
                format!(
                    "variant '{variant}' expects {} payload item(s), found {}",
                    usize::from(has_payload),
                    payload.len()
                ),
                span,
            ));
        }

        let tag_temp = self.new_temp(ArType::Primitive(Primitive::Int));
        self.emit_assign_temp(tag_temp, AmirRvalue::Discriminant { value: scrutinee });
        let expected_tag = AmirOperand::Constant(self.intern_literal_int(tag.to_string()));
        let matches_tag = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            matches_tag,
            AmirRvalue::Binary {
                op: BinaryOp::Equal,
                left: AmirOperand::Copy(tag_temp),
                right: expected_tag,
            },
        );
        if !has_payload {
            return Ok(AmirOperand::Copy(matches_tag));
        }

        let payload_temp = self.new_temp_id(inner);
        self.emit_assign_temp(
            payload_temp,
            AmirRvalue::FieldAccess {
                base: scrutinee,
                field: 1,
            },
        );
        let pattern = self.hir.pool.pattern(payload[0]);
        let matches_payload =
            self.lower_pattern_match_mode(AmirOperand::Copy(payload_temp), pattern, symbols, bind)?;
        let result = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            result,
            AmirRvalue::Binary {
                op: BinaryOp::And,
                left: AmirOperand::Copy(matches_tag),
                right: matches_payload,
            },
        );
        Ok(AmirOperand::Copy(result))
    }
}
