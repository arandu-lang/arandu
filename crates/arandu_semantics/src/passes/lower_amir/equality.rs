//! Structural equality is a semantic operation, not an address or byte comparison.
//! Enum payloads are inspected only after equal discriminants select the active
//! variant. Keeping this CFG in AMIR gives every backend the same semantics.

use super::LowerCtx;
use crate::amir::{AmirConstant, AmirOperand, AmirRvalue};
use crate::diagnostics::Diagnostic;
use crate::ops::BinaryOp;
use crate::types::{ArType, Primitive, TypeId};

// One array root plus 65,536 scalar leaves, bounded independently of host size.
const MAX_EQUALITY_WORK: usize = 65_537;

impl LowerCtx<'_> {
    pub(super) fn lower_value_equality(
        &mut self,
        left: AmirOperand,
        right: AmirOperand,
        ty: TypeId,
    ) -> Result<AmirOperand, Diagnostic> {
        let mut remaining = MAX_EQUALITY_WORK;
        self.lower_value_equality_inner(left, right, ty, 0, &mut remaining)
    }

    fn lower_value_equality_inner(
        &mut self,
        left: AmirOperand,
        right: AmirOperand,
        ty: TypeId,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<AmirOperand, Diagnostic> {
        self.charge_equality_work(depth, remaining)?;
        let resolved = self.resolve_ty(ty);
        match resolved {
            ArType::Option(inner) => {
                self.lower_enum_equality(left, right, [None, Some(inner)], depth, remaining)
            }
            ArType::Result(ok, err) => {
                self.lower_enum_equality(left, right, [Some(ok), Some(err)], depth, remaining)
            }
            ArType::Tuple(ids) => {
                let fields = self.tc.type_info.type_interner.type_args(ids).to_vec();
                self.lower_fields_equality(left, right, fields, depth, remaining)
            }
            ArType::Named(symbol, _) if self.tc.type_info.struct_fields.contains_key(&symbol) => {
                let fields = self.tc.type_info.struct_fields[&symbol]
                    .iter()
                    .map(|field| {
                        (
                            field.index,
                            arandu_middle::layout::instantiated_field_type(
                                &resolved,
                                field.name.as_str(),
                                &self.tc.type_info.type_interner,
                                self.tc.type_info.as_ref(),
                            )
                            .unwrap_or(field.ty),
                        )
                    })
                    .collect::<Vec<_>>();
                let mut fields = fields;
                fields.sort_unstable_by_key(|(index, _)| *index);
                self.lower_fields_equality(
                    left,
                    right,
                    fields.into_iter().map(|(_, ty)| ty).collect(),
                    depth,
                    remaining,
                )
            }
            ArType::Named(symbol, args) => {
                let args = self.tc.type_info.type_interner.type_args(args);
                self.lower_nominal_enum_equality(left, right, symbol, &args, depth, remaining)
            }
            ArType::Array(count, element) => {
                let mut result = AmirOperand::Constant(AmirConstant::Bool(true));
                for index in 0..count {
                    let literal = self.intern_literal_int(index.to_string());
                    let l = self.new_temp_id(element);
                    let r = self.new_temp_id(element);
                    self.emit_assign_temp(
                        l,
                        AmirRvalue::IndexAccess {
                            base: left,
                            index: AmirOperand::Constant(literal),
                        },
                    );
                    self.emit_assign_temp(
                        r,
                        AmirRvalue::IndexAccess {
                            base: right,
                            index: AmirOperand::Constant(literal),
                        },
                    );
                    let equal = self.lower_value_equality_inner(
                        AmirOperand::Copy(l),
                        AmirOperand::Copy(r),
                        element,
                        depth + 1,
                        remaining,
                    )?;
                    result = self.equality_binary(BinaryOp::And, result, equal);
                }
                Ok(result)
            }
            ArType::Void => Ok(AmirOperand::Constant(AmirConstant::Bool(true))),
            _ => Ok(self.equality_binary(BinaryOp::Equal, left, right)),
        }
    }

    fn charge_equality_work(
        &mut self,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<(), Diagnostic> {
        if depth >= crate::types::TypeShape::MAX_DEPTH || *remaining == 0 {
            return Err(Diagnostic::error(
                crate::DiagCode::U001FeatureNotSupported,
                "structural equality exceeds the finite lowering budget",
                self.current_span,
            ));
        }
        *remaining -= 1;
        self.charge_static_expansion(self.current_span)
    }

    fn lower_fields_equality(
        &mut self,
        left: AmirOperand,
        right: AmirOperand,
        fields: Vec<TypeId>,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<AmirOperand, Diagnostic> {
        let mut result = AmirOperand::Constant(AmirConstant::Bool(true));
        for (field, ty) in fields.into_iter().enumerate() {
            let l = self.new_temp_id(ty);
            let r = self.new_temp_id(ty);
            self.emit_assign_temp(l, AmirRvalue::FieldAccess { base: left, field });
            self.emit_assign_temp(r, AmirRvalue::FieldAccess { base: right, field });
            let equal = self.lower_value_equality_inner(
                AmirOperand::Copy(l),
                AmirOperand::Copy(r),
                ty,
                depth + 1,
                remaining,
            )?;
            result = self.equality_binary(BinaryOp::And, result, equal);
        }
        Ok(result)
    }

    fn lower_nominal_enum_equality(
        &mut self,
        left: AmirOperand,
        right: AmirOperand,
        symbol: crate::SymbolId,
        args: &[TypeId],
        depth: usize,
        remaining: &mut usize,
    ) -> Result<AmirOperand, Diagnostic> {
        let mut variants = self
            .tc
            .type_info
            .enum_variants
            .iter()
            .filter(|(_, (owner, _))| *owner == symbol)
            .map(|(&variant, (_, shape))| {
                let tag = *self
                    .tc
                    .type_info
                    .enum_variant_tags
                    .get(&variant)
                    .ok_or_else(|| {
                        Diagnostic::ice(
                            crate::DiagCode::ICEGEN001,
                            "enum equality variant has no discriminant",
                            self.current_span,
                        )
                    })?;
                let fields = match shape {
                    crate::passes::type_checker::EnumPayloadShape::Unit => Vec::new(),
                    crate::passes::type_checker::EnumPayloadShape::Tuple(types) => {
                        self.instantiate_enum_payload_types(symbol, variant, args, types)
                    }
                };
                Ok((tag, variant, fields))
            })
            .collect::<Result<Vec<_>, Diagnostic>>()?;
        variants
            .sort_unstable_by_key(|(tag, variant, _)| (*tag, variant.file_id, variant.local_id.0));
        if variants.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(Diagnostic::ice(
                crate::DiagCode::ICEGEN001,
                "enum equality variants have duplicate discriminants",
                self.current_span,
            ));
        }
        if variants.is_empty() {
            return Err(Diagnostic::error(
                crate::DiagCode::U001FeatureNotSupported,
                "structural equality is not available for this payload type",
                self.current_span,
            ));
        }
        let ltag = self.equality_tag(left);
        let rtag = self.equality_tag(right);
        let same = self.equality_binary(BinaryOp::Equal, ltag, rtag);
        let different = self.new_block();
        let select = self.new_block();
        let join = self.new_block();
        let result = self.new_compiler_local(ArType::Primitive(Primitive::Bool));
        self.set_bool_branch(same, select, different);
        self.seal_block(select);
        self.builder.current_block = Some(select);
        for (tag, variant, fields) in variants {
            // Variant tests precede every payload projection. No inactive bytes
            // or struct padding participate in equality.
            self.charge_equality_work(depth, remaining)?;
            let literal = self.intern_literal_int(tag.to_string());
            let matches =
                self.equality_binary(BinaryOp::Equal, ltag, AmirOperand::Constant(literal));
            let matched = self.new_block();
            let next = self.new_block();
            self.set_bool_branch(matches, matched, next);
            self.seal_block(matched);
            self.seal_block(next);
            self.builder.current_block = Some(matched);
            let tuple_ty = (fields.len() > 1).then(|| {
                self.tc
                    .type_info
                    .type_interner
                    .intern(ArType::tuple(&fields, &self.tc.type_info.type_interner))
            });
            let mut equal = AmirOperand::Constant(AmirConstant::Bool(true));
            for (index, field_ty) in fields.into_iter().enumerate() {
                let l = self.new_temp_id(field_ty);
                let r = self.new_temp_id(field_ty);
                for (temp, value) in [(l, left), (r, right)] {
                    self.emit_assign_temp(
                        temp,
                        AmirRvalue::EnumPayload {
                            value,
                            variant,
                            variant_tag: tag,
                            index,
                            field_ty,
                            tuple_ty,
                        },
                    );
                }
                let field_equal = self.lower_value_equality_inner(
                    AmirOperand::Copy(l),
                    AmirOperand::Copy(r),
                    field_ty,
                    depth + 1,
                    remaining,
                )?;
                equal = self.equality_binary(BinaryOp::And, equal, field_equal);
            }
            let end = self.require_block()?;
            self.write_variable(end, result, equal);
            self.emit_goto(join);
            self.builder.current_block = Some(next);
        }
        self.emit_goto(different);
        self.seal_block(different);
        self.builder.current_block = Some(different);
        self.write_variable(
            different,
            result,
            AmirOperand::Constant(AmirConstant::Bool(false)),
        );
        self.emit_goto(join);
        self.seal_block(join);
        self.builder.current_block = Some(join);
        Ok(self.read_variable(join, result))
    }

    fn equality_binary(
        &mut self,
        op: BinaryOp,
        left: AmirOperand,
        right: AmirOperand,
    ) -> AmirOperand {
        let temp = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(temp, AmirRvalue::Binary { op, left, right });
        AmirOperand::Copy(temp)
    }

    fn equality_tag(&mut self, value: AmirOperand) -> AmirOperand {
        if matches!(value, AmirOperand::Constant(AmirConstant::Nil)) {
            return AmirOperand::Constant(self.intern_literal_int("0"));
        }
        let temp = self.new_temp(ArType::Primitive(Primitive::Int));
        self.emit_assign_temp(temp, AmirRvalue::Discriminant { value });
        AmirOperand::Copy(temp)
    }

    fn lower_enum_equality(
        &mut self,
        left: AmirOperand,
        right: AmirOperand,
        payloads: [Option<TypeId>; 2],
        depth: usize,
        remaining: &mut usize,
    ) -> Result<AmirOperand, Diagnostic> {
        let ltag = self.equality_tag(left);
        let rtag = self.equality_tag(right);
        let same = self.equality_binary(BinaryOp::Equal, ltag, rtag);
        let different = self.new_block();
        let select = self.new_block();
        let zero = self.new_block();
        let one = self.new_block();
        let join = self.new_block();
        let result = self.new_compiler_local(ArType::Primitive(Primitive::Bool));
        self.set_bool_branch(same, select, different);
        self.seal_block(select);
        self.seal_block(different);
        self.builder.current_block = Some(different);
        self.write_variable(
            different,
            result,
            AmirOperand::Constant(AmirConstant::Bool(false)),
        );
        self.emit_goto(join);
        self.builder.current_block = Some(select);
        let zero_tag = AmirOperand::Constant(self.intern_literal_int("0"));
        let is_zero = self.equality_binary(BinaryOp::Equal, ltag, zero_tag);
        self.set_bool_branch(is_zero, zero, one);
        self.seal_block(zero);
        self.seal_block(one);
        for (block, payload) in [zero, one].into_iter().zip(payloads) {
            self.builder.current_block = Some(block);
            let equal = if let Some(ty) = payload {
                let l = self.new_temp_id(ty);
                let r = self.new_temp_id(ty);
                self.lower_result_ok_field(left, l);
                self.lower_result_ok_field(right, r);
                self.lower_value_equality_inner(
                    AmirOperand::Copy(l),
                    AmirOperand::Copy(r),
                    ty,
                    depth + 1,
                    remaining,
                )?
            } else {
                AmirOperand::Constant(AmirConstant::Bool(true))
            };
            let end = self.require_block()?;
            self.write_variable(end, result, equal);
            self.emit_goto(join);
        }
        self.seal_block(join);
        self.builder.current_block = Some(join);
        Ok(self.read_variable(join, result))
    }
}
