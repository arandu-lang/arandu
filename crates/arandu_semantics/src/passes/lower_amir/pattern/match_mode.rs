use super::super::LowerCtx;
use super::EnumPatternInput;
use super::builtin::{
    BuiltinOptionPatternInput, BuiltinPollPatternInput, BuiltinResultPatternInput,
};
use crate::SymbolTable;
use crate::amir::{AmirConstant, AmirOperand, AmirPlace, AmirRvalue};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::hir::HirPattern;
use crate::ops::BinaryOp;
use crate::passes::type_checker::types::{ArType, Primitive};

impl LowerCtx<'_> {
    pub(super) fn lower_pattern_match_mode(
        &mut self,
        scrutinee: AmirOperand,
        pattern: &HirPattern,
        symbols: &SymbolTable,
        bind: bool,
    ) -> Result<AmirOperand, Diagnostic> {
        match pattern {
            HirPattern::Wildcard { .. } => Ok(AmirOperand::Constant(AmirConstant::Bool(true))),
            HirPattern::Bind { symbol, .. } => {
                if !bind {
                    return Ok(AmirOperand::Constant(AmirConstant::Bool(true)));
                }
                let ty = self
                    .tc
                    .type_info
                    .decl_type(*symbol)
                    .unwrap_or(ArType::Error);
                let local_id = self.new_local(ty, *symbol, pattern.span());
                let binding_value = match scrutinee {
                    AmirOperand::Copy(temp) if !self.temps[temp.as_usize()].is_copy => {
                        AmirOperand::Move(temp)
                    }
                    other => other,
                };
                self.emit_store_place(
                    AmirPlace {
                        local: local_id,
                        projections: smallvec::SmallVec::new(),
                    },
                    binding_value,
                )?;
                Ok(AmirOperand::Constant(AmirConstant::Bool(true)))
            }
            HirPattern::Literal {
                expr: lit_expr_id, ..
            } => {
                let lit_op = self.lower_expr(*lit_expr_id, None, symbols)?;
                let dest = self.new_temp(ArType::Primitive(Primitive::Bool));
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::Binary {
                        op: BinaryOp::Equal,
                        left: scrutinee,
                        right: lit_op,
                    },
                );
                Ok(AmirOperand::Copy(dest))
            }
            HirPattern::Enum {
                span,
                type_symbol,
                variant,
                variant_symbol,
                payload,
            } => {
                let payload_ids = self.hir.pool.pattern_list(*payload);
                self.lower_enum_pattern(EnumPatternInput {
                    scrutinee,
                    span: *span,
                    type_symbol: *type_symbol,
                    variant: variant.as_str(),
                    variant_symbol: *variant_symbol,
                    payload: payload_ids,
                    symbols,
                    bind,
                })
            }
            HirPattern::Struct {
                struct_symbol,
                fields,
                ..
            } => {
                let fields_map = self.tc.type_info.struct_fields.get(struct_symbol);
                let mut current_matches = AmirOperand::Constant(AmirConstant::Bool(true));

                let field_ids = self.hir.pool.field_pattern_list(*fields);
                for &fid in field_ids {
                    let field = self.hir.pool.field_pattern(fid);
                    let field_tid = fields_map
                        .and_then(|m| m.get(field.name.as_str()))
                        .map(|f| f.ty);
                    let tmp_field = match field_tid {
                        Some(tid) => self.new_temp_id(tid),
                        None => self.new_temp(ArType::Error),
                    };
                    let field_idx = fields_map
                        .and_then(|m| m.get(field.name.as_str()))
                        .map(|f| f.index)
                        .unwrap_or(0);
                    self.emit_assign_temp(
                        tmp_field,
                        AmirRvalue::FieldAccess {
                            base: scrutinee,
                            field: field_idx,
                        },
                    );

                    let item_matches = if let Some(pat_id) = field.pattern {
                        let pat = self.hir.pool.pattern(pat_id);
                        self.lower_pattern_match_mode(
                            AmirOperand::Copy(tmp_field),
                            pat,
                            symbols,
                            bind,
                        )?
                    } else {
                        if !bind {
                            continue;
                        }
                        let key = crate::NodeKey::from(field.span);
                        let Some(symbol_id) = self.tc.resolved.definitions.get(&key).copied()
                        else {
                            return Err(Diagnostic::error(
                                DiagCode::T018UndefinedField,
                                format!(
                                    "field '{}' symbol not found during struct lowering",
                                    field.name
                                ),
                                field.span,
                            ));
                        };
                        let local_id = match field_tid {
                            Some(tid) => self.new_local_id(tid, symbol_id, field.span),
                            None => self.new_local(ArType::Error, symbol_id, field.span),
                        };
                        self.emit_store_place(
                            AmirPlace {
                                local: local_id,
                                projections: smallvec::SmallVec::new(),
                            },
                            AmirOperand::Copy(tmp_field),
                        )?;
                        AmirOperand::Constant(AmirConstant::Bool(true))
                    };

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
            HirPattern::Tuple { items, .. } => {
                let scrutinee_ty = self.operand_type(&scrutinee);
                let pat_ids = self.hir.pool.pattern_list(*items);
                let item_tys: Vec<ArType> = if let ArType::Tuple(tys) = scrutinee_ty {
                    let interner = &self.tc.type_info.type_interner;
                    interner
                        .type_args(tys)
                        .iter()
                        .map(|&tid| interner.resolve(tid))
                        .collect()
                } else {
                    vec![ArType::Error; pat_ids.len()]
                };

                let mut current_matches = AmirOperand::Constant(AmirConstant::Bool(true));
                for (i, &pid) in pat_ids.iter().enumerate() {
                    let pat = self.hir.pool.pattern(pid);
                    let item_ty = item_tys.get(i).unwrap_or(&ArType::Error);
                    let tmp_item = self.new_temp_ref(item_ty);
                    self.emit_assign_temp(
                        tmp_item,
                        AmirRvalue::FieldAccess {
                            base: scrutinee,
                            field: i,
                        },
                    );

                    let item_matches = self.lower_pattern_match_mode(
                        AmirOperand::Copy(tmp_item),
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
            HirPattern::TypeTuple {
                span,
                name,
                payload,
            } => {
                // Peel & / &mut / ptr **types** so `match self` on `shared self`
                // sees Result/Option/Named (same family as typeck check_pattern).
                let (scrutinee, scrutinee_ty) = self.peel_ref_scrutinee_type(scrutinee);
                let payload_ids = self.hir.pool.pattern_list(*payload);
                // SYN.3: builtin `Option` / `Result` are not `Named` enums — match by tag.
                match scrutinee_ty {
                    ArType::Option(inner) => {
                        self.lower_option_type_tuple_pattern(BuiltinOptionPatternInput {
                            scrutinee,
                            variant: name.as_str(),
                            payload: payload_ids,
                            inner,
                            span: *span,
                            symbols,
                            bind,
                        })
                    }
                    ArType::Result(ok, err) => self.lower_result_type_tuple_pattern(
                        BuiltinResultPatternInput {
                            scrutinee,
                            variant: name.as_str(),
                            payload: payload_ids,
                            ok,
                            err,
                            span: *span,
                            symbols,
                        },
                        bind,
                    ),
                    ArType::Poll(inner) => {
                        self.lower_poll_type_tuple_pattern(BuiltinPollPatternInput {
                            scrutinee,
                            variant: name.as_str(),
                            payload: payload_ids,
                            inner,
                            span: *span,
                            symbols,
                            bind,
                        })
                    }
                    ArType::Named(type_symbol, _) => self.lower_enum_pattern(EnumPatternInput {
                        scrutinee,
                        span: *span,
                        type_symbol,
                        variant: name.as_str(),
                        variant_symbol: None,
                        payload: payload_ids,
                        symbols,
                        bind,
                    }),
                    other => Err(Diagnostic::error(
                        DiagCode::T002IncompatibleAssignment,
                        format!(
                            "cannot match type tuple pattern against non-enum type `{}`",
                            other.display(symbols, &self.tc.type_info.type_interner)
                        ),
                        *span,
                    )),
                }
            }
            HirPattern::Range {
                span: _,
                start,
                inclusive,
                end,
            } => {
                let start_op = self.lower_expr(*start, None, symbols)?;
                let end_op = self.lower_expr(*end, None, symbols)?;

                let ge_dest = self.new_temp(ArType::Primitive(Primitive::Bool));
                self.emit_assign_temp(
                    ge_dest,
                    AmirRvalue::Binary {
                        op: BinaryOp::GtEqual,
                        left: scrutinee,
                        right: start_op,
                    },
                );

                let limit_op = if *inclusive {
                    BinaryOp::LtEqual
                } else {
                    BinaryOp::Lt
                };
                let limit_dest = self.new_temp(ArType::Primitive(Primitive::Bool));
                self.emit_assign_temp(
                    limit_dest,
                    AmirRvalue::Binary {
                        op: limit_op,
                        left: scrutinee,
                        right: end_op,
                    },
                );

                let range_dest = self.new_temp(ArType::Primitive(Primitive::Bool));
                self.emit_assign_temp(
                    range_dest,
                    AmirRvalue::Binary {
                        op: BinaryOp::And,
                        left: AmirOperand::Copy(ge_dest),
                        right: AmirOperand::Copy(limit_dest),
                    },
                );
                Ok(AmirOperand::Copy(range_dest))
            }
            // SYN.4: `p1 | p2 | …` → boolean OR of alternative matches.
            HirPattern::Or { alts, .. } => {
                let alt_ids = self.hir.pool.pattern_list(*alts);
                if alt_ids.is_empty() {
                    return Ok(AmirOperand::Constant(AmirConstant::Bool(false)));
                }
                let mut acc = self.lower_pattern_match_mode(
                    scrutinee,
                    self.hir.pool.pattern(alt_ids[0]),
                    symbols,
                    bind,
                )?;
                for &aid in &alt_ids[1..] {
                    let next = self.lower_pattern_match_mode(
                        scrutinee,
                        self.hir.pool.pattern(aid),
                        symbols,
                        bind,
                    )?;
                    let or_dest = self.new_temp(ArType::Primitive(Primitive::Bool));
                    self.emit_assign_temp(
                        or_dest,
                        AmirRvalue::Binary {
                            op: BinaryOp::Or,
                            left: acc,
                            right: next,
                        },
                    );
                    acc = AmirOperand::Copy(or_dest);
                }
                Ok(acc)
            }
        }
    }
}
