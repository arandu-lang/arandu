use super::LowerCtx;
use crate::amir::{AmirConstant, AmirOperand, AmirPlace, AmirRvalue, BlockId};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::hir::{HirCondition, HirDecl, HirPattern};
use crate::ops::BinaryOp;
use crate::passes::type_checker::types::{ArType, Primitive};
use crate::{SymbolId, SymbolTable};
use arandu_lexer::Span;

pub(crate) struct EnumPatternInput<'a> {
    scrutinee: AmirOperand,
    span: Span,
    type_symbol: SymbolId,
    variant: &'a str,
    variant_symbol: Option<SymbolId>,
    /// Payload pattern ids (looked up by ref — no HirPattern clone).
    payload: &'a [crate::hir::HirPatternId],
    symbols: &'a SymbolTable,
    bind: bool,
}

struct BuiltinResultPatternInput<'a> {
    scrutinee: AmirOperand,
    variant: &'a str,
    payload: &'a [crate::hir::HirPatternId],
    ok: crate::types::TypeId,
    err: crate::types::TypeId,
    span: Span,
    symbols: &'a SymbolTable,
}

struct BuiltinOptionPatternInput<'a> {
    scrutinee: AmirOperand,
    variant: &'a str,
    payload: &'a [crate::hir::HirPatternId],
    inner: crate::types::TypeId,
    span: Span,
    symbols: &'a SymbolTable,
    bind: bool,
}

struct BuiltinPollPatternInput<'a> {
    scrutinee: AmirOperand,
    variant: &'a str,
    payload: &'a [crate::hir::HirPatternId],
    inner: crate::types::TypeId,
    span: Span,
    symbols: &'a SymbolTable,
    bind: bool,
}
impl LowerCtx<'_> {
    /// Peel `&` / `&mut` / `*` **types** on a match scrutinee so enum/Result/Option
    /// patterns classify correctly for `shared self` methods.
    ///
    /// Keeps the operand as-is: Cranelift `Discriminant` / `FieldAccess` already
    /// treat a Ref/Ptr-typed base as a pointer and load through it. Materializing
    /// `*ref` into an aggregate temp breaks that ABI (JIT crash on enum match).
    pub(crate) fn peel_ref_scrutinee_type(&self, scrutinee: AmirOperand) -> (AmirOperand, ArType) {
        let mut ty = self.operand_type(&scrutinee);
        for _ in 0..4 {
            match ty {
                ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                    ty = self.resolve_ty(inner);
                }
                _ => break,
            }
        }
        (scrutinee, ty)
    }

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

    pub(crate) fn lower_condition(
        &mut self,
        cond: &HirCondition,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        match cond {
            HirCondition::Expr(expr_id) => self.lower_expr(*expr_id, None, symbols),
            HirCondition::Is { expr, pattern } => {
                let scrutinee = self.lower_expr(*expr, None, symbols)?;
                let pat = self.hir.pool.pattern(*pattern);
                self.lower_pattern_match(scrutinee, pat, symbols)
            }
            HirCondition::And(_) => Err(Diagnostic::ice(
                DiagCode::ICEGEN001,
                "compound condition reached scalar AMIR lowering",
                self.diag_span(self.current_span),
            )),
        }
    }

    /// Lowers a condition directly into short-circuit CFG edges.
    pub(crate) fn lower_condition_branch(
        &mut self,
        cond: &HirCondition,
        if_true: BlockId,
        if_false: BlockId,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        if let HirCondition::And(conditions) = cond {
            let Some((last, prefix)) = conditions.split_last() else {
                self.emit_goto(if_true);
                return Ok(());
            };
            for condition in prefix {
                let next = self.new_block();
                self.lower_condition_branch(condition, next, if_false, symbols)?;
                self.seal_block(next);
                self.builder.current_block = Some(next);
            }
            return self.lower_condition_branch(last, if_true, if_false, symbols);
        }

        let condition = self.lower_condition(cond, symbols)?;
        if self.builder.current_block.is_some() {
            self.set_bool_branch(condition, if_true, if_false);
        }
        Ok(())
    }

    /// SYN.3: `Some(v)` / `None` against `Option<T>` (tags: None=0, Some=1).
    fn lower_option_type_tuple_pattern(
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
    fn lower_result_type_tuple_pattern(
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
    fn lower_poll_type_tuple_pattern(
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

    pub(crate) fn lower_pattern_match(
        &mut self,
        scrutinee: AmirOperand,
        pattern: &HirPattern,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        self.lower_pattern_match_mode(scrutinee, pattern, symbols, true)
    }

    pub(crate) fn lower_pattern_test(
        &mut self,
        scrutinee: AmirOperand,
        pattern: &HirPattern,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        self.lower_pattern_match_mode(scrutinee, pattern, symbols, false)
    }

    fn lower_pattern_match_mode(
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
