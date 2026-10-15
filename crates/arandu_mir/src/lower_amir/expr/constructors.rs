//! Constructors for struct literals, enum variants, and result/option types.

use super::super::LowerCtx;
use crate::amir::{AmirOperand, AmirRvalue, TempId};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExpr, HirExprId, IndexRange, ResultCtorVariant};
use crate::{SymbolKind, SymbolTable};

impl LowerCtx<'_> {
    pub(super) fn lower_array_repeat(
        &mut self,
        value: HirExprId,
        expr: &HirExpr,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let array_ty = self.resolve_ty(expr.ty);
        let crate::types::ArType::Array(count, element) = array_ty else {
            return Err(Diagnostic::error(
                crate::DiagCode::T048InvalidArrayRepeat,
                "array repetition length must be concrete before lowering",
                expr.span,
            ));
        };
        let count = (count <= arandu_middle::types::MAX_ARRAY_REPEAT_ELEMENTS)
            .then(|| usize::try_from(count).ok())
            .flatten()
            .ok_or_else(|| {
                Diagnostic::error(
                    crate::DiagCode::T048InvalidArrayRepeat,
                    "array repetition exceeds the lowering limit of 65536 elements",
                    expr.span,
                )
            })?;
        // Constructors currently pass byte sizes through a u32 backend
        // allocation interface. Validate the full target layout before lowering
        // a nested initializer; checking the element count alone is insufficient.
        let layout = arandu_middle::layout::LayoutEngine::from_data_layout(self.layout)
            .layout_of_type(
                &array_ty,
                &self.tc.type_info.type_interner,
                self.tc.type_info.as_ref(),
            )
            .map_err(|error| {
                Diagnostic::error(
                    crate::DiagCode::T048InvalidArrayRepeat,
                    format!("cannot represent this repeated array: {error}"),
                    expr.span,
                )
            })?;
        if layout.size > u64::from(u32::MAX) {
            return Err(Diagnostic::error(
                crate::DiagCode::T048InvalidArrayRepeat,
                "repeated array exceeds the backend allocation size limit",
                expr.span,
            )
            .with_primary_label("the complete array requires more than 4294967295 bytes"));
        }
        if count > 1 && !self.tc.type_info.is_copy(element) {
            return Err(Diagnostic::error(
                crate::DiagCode::T048InvalidArrayRepeat,
                "repeating this value would duplicate ownership",
                expr.span,
            )
            .with_primary_label("this element is not Copy"));
        }
        // Evaluate once even for zero elements: effects and lexical cleanup
        // remain visible to the shared drop elaborator and optimizers.
        self.charge_static_expansion_work(expr.span, count)?;
        let operand = self.lower_expr(value, None, symbols)?;
        if count == 0 && !self.tc.type_info.is_copy(element) {
            self.begin_local_scope();
            let local = self.new_compiler_local_id(element, expr.span);
            if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = operand {
                self.owned_string_temps.remove(&temp);
            }
            let consumed = self.consume_operand(operand)?;
            self.write_variable_source(local, consumed)?;
            self.end_local_scope();
        }
        let operand = if count > 1 {
            match operand {
                AmirOperand::Move(temp) => AmirOperand::Copy(temp),
                other => other,
            }
        } else if count == 1 {
            if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = operand {
                self.owned_string_temps.remove(&temp);
            }
            self.consume_operand(operand)?
        } else {
            operand
        };
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
        self.emit_assign_temp(
            dest,
            AmirRvalue::Array {
                items: vec![operand; count],
            },
        );
        Ok(AmirOperand::Copy(dest))
    }

    pub(super) fn lower_type_path(
        &mut self,
        type_symbol: crate::SymbolId,
        member_symbol: crate::SymbolId,
        expr: &HirExpr,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let op: AmirOperand = if let Some(&local_id) = self.symbol_map.get(&member_symbol) {
            Ok::<AmirOperand, Diagnostic>(self.read_variable_source(local_id)?)
        } else if let Some(&tag) = self
            .tc
            .type_info
            .enum_variant_tags
            .get(&member_symbol)
            .or_else(|| {
                // Filter by the enum type that the parser already resolved (type_symbol).
                // This eliminates cross-enum collisions for identically-named variants.
                let lookup_bare = symbols
                    .get(member_symbol)
                    .name
                    .rsplit('.')
                    .next()
                    .unwrap_or("");
                self.tc
                    .type_info
                    .enum_variants
                    .iter()
                    .find(|&(v_sym, (parent_sym, _))| {
                        *parent_sym == type_symbol
                            && symbols.get(*v_sym).name.rsplit('.').next().unwrap_or("")
                                == lookup_bare
                            && self.tc.type_info.enum_variant_tags.contains_key(v_sym)
                    })
                    .and_then(|(v_sym, _)| self.tc.type_info.enum_variant_tags.get(v_sym))
            })
        {
            let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
            self.emit_assign_temp(
                dest,
                AmirRvalue::EnumConstruct {
                    variant_tag: tag,
                    payload: None,
                },
            );
            Ok(AmirOperand::Copy(dest))
        } else {
            let sym = symbols.get(member_symbol);
            Ok(match sym.kind {
                SymbolKind::Func
                | SymbolKind::ExternFunc
                | SymbolKind::AssociatedFunc
                | SymbolKind::NamespaceMember => AmirOperand::FunctionRef(member_symbol),
                _ => AmirOperand::GlobalRef(member_symbol),
            })
        }?;
        if let Some(dest) = target {
            let lookup_bare = self
                .tc
                .type_info
                .enum_variant_name(symbols, member_symbol)
                .unwrap_or_else(|| {
                    symbols
                        .try_get(member_symbol)
                        .map(|s| s.name.rsplit('.').next().unwrap_or("").to_string())
                        .unwrap_or_default()
                });
            let already_assigned = self
                .tc
                .type_info
                .enum_variant_tags
                .contains_key(&member_symbol)
                || (!lookup_bare.is_empty()
                    && self
                        .tc
                        .type_info
                        .enum_variant_by_name(symbols, type_symbol, &lookup_bare)
                        .is_some_and(|(canon_id, _, _)| {
                            self.tc.type_info.enum_variant_tags.contains_key(&canon_id)
                        }));
            if !already_assigned {
                let rhs = self.consume_operand(op)?;
                self.emit_assign_temp(dest, AmirRvalue::Use(rhs));
            }
        }
        Ok(op)
    }

    pub(super) fn lower_struct_literal(
        &mut self,
        struct_symbol: crate::SymbolId,
        fields: IndexRange,
        expr: &HirExpr,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let fields_slice = self.hir.pool.field_inits_list(fields);
        let mut field_ops = Vec::with_capacity(fields_slice.len());
        for f in fields_slice {
            let field_ty = self.hir.pool.expr(f.value).ty;
            let value = self.lower_expr(f.value, None, symbols)?;
            // A struct literal takes ownership of each non-Copy field value.
            // Record that move before drop elaboration so the source local is
            // not destroyed after its value has been installed in the result.
            let value = self.consume_operand(value)?;
            field_ops.push((f.name.clone(), value, field_ty));
        }
        if let Some(struct_fields) = self.tc.type_info.struct_fields.get(&struct_symbol) {
            field_ops.sort_by_key(|(name, _, _)| {
                struct_fields
                    .get(name.as_str())
                    .map(|f| f.index)
                    .unwrap_or(usize::MAX)
            });
        }
        if let Some(&tag) = self.tc.type_info.enum_variant_tags.get(&struct_symbol) {
            let payload_op = match field_ops.len() {
                0 => None,
                1 => field_ops.pop().map(|(_, op, _)| op),
                _ => {
                    let param_tys: Vec<_> = field_ops.iter().map(|(_, _, ty)| *ty).collect();
                    let item_ops: Vec<_> = field_ops.into_iter().map(|(_, op, _)| op).collect();
                    let tuple_ty =
                        crate::types::ArType::tuple(&param_tys, &self.tc.type_info.type_interner);
                    let dest_tuple = self.new_temp(tuple_ty);
                    self.emit_assign_temp(dest_tuple, AmirRvalue::Tuple { items: item_ops });
                    Some(self.consume_operand(AmirOperand::Copy(dest_tuple))?)
                }
            };
            let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
            self.emit_assign_temp(
                dest,
                AmirRvalue::EnumConstruct {
                    variant_tag: tag,
                    payload: payload_op,
                },
            );
            return Ok(AmirOperand::Copy(dest));
        }
        let field_ops = field_ops
            .into_iter()
            .map(|(name, op, _)| (name, op))
            .collect();
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
        self.emit_assign_temp(
            dest,
            AmirRvalue::StructLiteral {
                struct_symbol,
                fields: field_ops,
            },
        );
        Ok(AmirOperand::Copy(dest))
    }

    pub(super) fn lower_result_ctor(
        &mut self,
        variant: ResultCtorVariant,
        value: HirExprId,
        expr: &HirExpr,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let val_op = self.lower_expr(value, None, symbols)?;
        let val_op = if matches!(
            variant,
            ResultCtorVariant::Ok
                | ResultCtorVariant::Err
                | ResultCtorVariant::Some
                | ResultCtorVariant::PollReady
        ) {
            self.consume_operand(val_op)?
        } else {
            val_op
        };
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
        match variant {
            ResultCtorVariant::Ok => {
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::EnumConstruct {
                        variant_tag: 0,
                        payload: Some(val_op),
                    },
                );
            }
            ResultCtorVariant::Err => {
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::EnumConstruct {
                        variant_tag: 1,
                        payload: Some(val_op),
                    },
                );
            }
            ResultCtorVariant::Some => {
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::EnumConstruct {
                        variant_tag: 1,
                        payload: Some(val_op),
                    },
                );
            }
            // Option.None = tag 0, no payload (Some is tag 1).
            ResultCtorVariant::None => {
                let _ = val_op;
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::EnumConstruct {
                        variant_tag: 0,
                        payload: None,
                    },
                );
            }
            // A3.6: Poll.Ready = tag 0 + payload; Poll.Pending = tag 1, no payload.
            ResultCtorVariant::PollReady => {
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::EnumConstruct {
                        variant_tag: 0,
                        payload: Some(val_op),
                    },
                );
            }
            ResultCtorVariant::PollPending => {
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::EnumConstruct {
                        variant_tag: 1,
                        payload: None,
                    },
                );
            }
        }
        Ok(AmirOperand::Copy(dest))
    }
}
