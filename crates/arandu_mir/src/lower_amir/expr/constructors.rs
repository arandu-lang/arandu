//! Constructors for struct literals, enum variants, and result/option types.

use super::super::LowerCtx;
use crate::amir::{AmirOperand, AmirRvalue, TempId};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExpr, HirExprId, IndexRange, ResultCtorVariant};
use crate::{SymbolKind, SymbolTable};

impl LowerCtx<'_> {
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
            let value = self.lower_expr(f.value, None, symbols)?;
            // A struct literal takes ownership of each non-Copy field value.
            // Record that move before drop elaboration so the source local is
            // not destroyed after its value has been installed in the result.
            let value = self.consume_operand(value)?;
            field_ops.push((f.name.clone(), value));
        }
        if let Some(struct_fields) = self.tc.type_info.struct_fields.get(&struct_symbol) {
            field_ops.sort_by_key(|(name, _)| {
                struct_fields
                    .get(name.as_str())
                    .map(|f| f.index)
                    .unwrap_or(usize::MAX)
            });
        }
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
