use super::super::LowerCtx;
use super::EnumPatternInput;
use crate::SymbolId;
use crate::amir::{AmirOperand, AmirRvalue};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::hir::HirDecl;
use crate::ops::BinaryOp;
use crate::passes::type_checker::types::{ArType, Primitive};

impl LowerCtx<'_> {
    pub(crate) fn lower_enum_pattern(
        &mut self,
        input: EnumPatternInput<'_>,
    ) -> Result<AmirOperand, Diagnostic> {
        let EnumPatternInput {
            scrutinee,
            span,
            type_symbol,
            variant,
            variant_symbol,
            payload,
            symbols,
            bind,
        } = input;
        let enum_name = &symbols.get(type_symbol).name;
        let variant_symbol_id = variant_symbol.or_else(|| {
            for (&var_id, &(parent_id, _)) in &self.tc.type_info.enum_variants {
                if parent_id == type_symbol {
                    let var_name = &symbols.get(var_id).name;
                    if var_name == variant || var_name.ends_with(&format!(".{}", variant)) {
                        return Some(var_id);
                    }
                }
            }
            None
        });

        let Some(variant_symbol_id) = variant_symbol_id else {
            return Err(Diagnostic::error(
                DiagCode::T018UndefinedField,
                format!("variant '{variant}' is not defined on enum '{enum_name}'"),
                span,
            ));
        };

        let scrutinee_ty = self.peel_ref_scrutinee_type(scrutinee).1;
        let enum_args = match scrutinee_ty {
            ArType::Named(id, args) if id == type_symbol => {
                self.tc.type_info.type_interner.type_args(args)
            }
            _ => Vec::new(),
        };

        let tag_value = self
            .tc
            .type_info
            .enum_variant_tags
            .get(&variant_symbol_id)
            .copied()
            .or_else(|| {
                for &decl_id in &self.hir.decls {
                    let decl = self.hir.pool.decl(decl_id);
                    if let HirDecl::Enum(hir_enum) = decl
                        && hir_enum.symbol == type_symbol
                    {
                        for (index, v) in self
                            .hir
                            .pool
                            .enum_variants_list(hir_enum.variants)
                            .iter()
                            .enumerate()
                        {
                            if v.symbol == variant_symbol_id
                                || symbols.get(v.symbol).name.rsplit('.').next() == Some(variant)
                            {
                                return Some(index);
                            }
                        }
                    }
                }
                None
            });

        let Some(tag_value) = tag_value else {
            return Err(Diagnostic::error(
                DiagCode::T018UndefinedField,
                format!("variant '{variant}' tag not found on enum '{enum_name}'"),
                span,
            ));
        };

        let tmp_tag = self.new_temp(ArType::Primitive(Primitive::Int));
        self.emit_assign_temp(tmp_tag, AmirRvalue::Discriminant { value: scrutinee });

        let tag_op = AmirOperand::Constant(self.intern_literal_int(tag_value.to_string()));
        let tag_matches = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            tag_matches,
            AmirRvalue::Binary {
                op: BinaryOp::Equal,
                left: AmirOperand::Copy(tmp_tag),
                right: tag_op,
            },
        );

        if payload.is_empty() {
            Ok(AmirOperand::Copy(tag_matches))
        } else {
            let variant_symbol_actual = variant_symbol_id;
            let shape_opt = self.tc.type_info.enum_variants.get(&variant_symbol_actual);
            let Some((_, crate::passes::type_checker::EnumPayloadShape::Tuple(tids))) = shape_opt
            else {
                return Err(Diagnostic::error(
                    DiagCode::T012WrongArgCount,
                    format!(
                        "enum variant '{}' expects 0 payload items, found {}",
                        variant,
                        payload.len()
                    ),
                    span,
                ));
            };

            let tids = self.instantiate_enum_payload_types(
                type_symbol,
                variant_symbol_actual,
                &enum_args,
                tids,
            );

            if tids.len() != payload.len() {
                return Err(Diagnostic::error(
                    DiagCode::T012WrongArgCount,
                    format!(
                        "enum variant '{}' expects {} payload items, found {}",
                        variant,
                        tids.len(),
                        payload.len()
                    ),
                    span,
                ));
            }

            let mut current_matches = AmirOperand::Copy(tag_matches);

            for (i, &pat_id) in payload.iter().enumerate() {
                let tmp_payload = self.new_temp_id(tids[i]);
                self.emit_assign_temp(
                    tmp_payload,
                    AmirRvalue::EnumPayload {
                        value: scrutinee,
                        variant: variant_symbol_actual,
                        variant_tag: tag_value,
                        index: i,
                        field_ty: tids[i],
                        tuple_ty: if tids.len() > 1 {
                            Some(
                                self.tc
                                    .type_info
                                    .type_interner
                                    .intern(ArType::tuple(&tids, &self.tc.type_info.type_interner)),
                            )
                        } else {
                            None
                        },
                    },
                );

                let pat = self.hir.pool.pattern(pat_id);
                let item_matches = self.lower_pattern_match_mode(
                    AmirOperand::Copy(tmp_payload),
                    pat,
                    symbols,
                    bind,
                )?;

                let and_dest = self.new_temp(ArType::Primitive(Primitive::Bool));
                self.emit_assign_temp(
                    and_dest,
                    AmirRvalue::Binary {
                        op: BinaryOp::And,
                        left: current_matches,
                        right: item_matches,
                    },
                );
                current_matches = AmirOperand::Copy(and_dest);
            }

            Ok(current_matches)
        }
    }

    pub(crate) fn instantiate_enum_payload_types(
        &self,
        enum_id: SymbolId,
        variant_id: SymbolId,
        enum_args: &[crate::types::TypeId],
        payload: &[crate::types::TypeId],
    ) -> Vec<crate::types::TypeId> {
        let payload = match self.tc.type_info.variant_payload_for(variant_id, enum_args) {
            Some(crate::passes::type_checker::EnumPayloadShape::Tuple(items)) => items.as_slice(),
            _ => payload,
        };
        let Some(params) = self.tc.type_info.generic_params.get(&enum_id) else {
            return payload.to_vec();
        };
        if enum_args.is_empty() || params.is_empty() {
            return payload.to_vec();
        }
        let concrete: Vec<_> = enum_args.iter().map(|&id| self.resolve_ty(id)).collect();
        let subst = crate::passes::type_checker::types::build_subst(
            params,
            &concrete[..params.len().min(concrete.len())],
        );
        payload
            .iter()
            .map(|&id| {
                let ty = self.resolve_ty(id);
                self.tc.type_info.type_interner.intern(
                    crate::passes::type_checker::types::substitute_type(
                        &ty,
                        &subst,
                        &self.tc.type_info.type_interner,
                    ),
                )
            })
            .collect()
    }
}
