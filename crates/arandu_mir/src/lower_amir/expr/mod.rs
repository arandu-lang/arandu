//! AMIR expression lowering coordinator.

pub(super) mod calls;
pub(super) mod constructors;
pub(super) mod control;

use super::{LowerCtx, amir_unsupported};
use crate::amir::{AmirConstant, AmirOperand, AmirRvalue, AmirStmt, TempId};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExprId, HirExprKind};
use crate::passes::type_checker::types::{ArType, Primitive, is_option_type};
use crate::{SymbolKind, SymbolTable};

impl LowerCtx<'_> {
    pub(crate) fn maybe_to_str(
        &mut self,
        op: AmirOperand,
        src_ty: crate::types::TypeId,
    ) -> Result<AmirOperand, Diagnostic> {
        if self.tc.type_info.type_interner.is_error(src_ty) {
            return Ok(op);
        }
        let needs = self.tc.type_info.type_interner.with_type(src_ty, |t| {
            !matches!(t, ArType::Primitive(Primitive::Str)) && t.is_to_str_v01()
        });
        if !needs {
            return Ok(op);
        }
        let src_ty_id = src_ty;
        let dest = self.new_temp(ArType::Primitive(Primitive::Str));
        self.emit_assign_temp(
            dest,
            AmirRvalue::ToStr {
                value: op,
                src_ty: src_ty_id,
            },
        );
        self.owned_string_temps.insert(dest);
        Ok(AmirOperand::Copy(dest))
    }

    pub(crate) fn lower_expr(
        &mut self,
        expr_id: HirExprId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let expr = self.hir.pool.expr(expr_id).clone();
        match &expr.kind {
            HirExprKind::Int(v) => {
                // Move SmolStr into the pool when the expr is consumed by ref via clone of short str.
                let op = AmirOperand::Constant(self.intern_literal_int(v.clone()));
                if let Some(dest) = target {
                    self.emit_assign_temp(dest, AmirRvalue::Use(op));
                }
                Ok(op)
            }
            HirExprKind::Float(v) => {
                let op = AmirOperand::Constant(self.intern_literal_float(v.clone()));
                if let Some(dest) = target {
                    self.emit_assign_temp(dest, AmirRvalue::Use(op));
                }
                Ok(op)
            }
            HirExprKind::Bool(v) => {
                let op = AmirOperand::Constant(AmirConstant::Bool(*v));
                if let Some(dest) = target {
                    self.emit_assign_temp(dest, AmirRvalue::Use(op));
                }
                Ok(op)
            }
            HirExprKind::Str(v) => {
                let op = AmirOperand::Constant(self.intern_literal_str(v.clone()));
                if let Some(dest) = target {
                    self.emit_assign_temp(dest, AmirRvalue::Use(op));
                }
                Ok(op)
            }
            HirExprKind::StringInterp { parts } => {
                let mut part_ops = Vec::with_capacity(parts.len());
                let mut intermediate_to_free = Vec::new();
                for part in parts {
                    let op = match part {
                        arandu_middle::hir::HirStringPart::Text(t) => {
                            AmirOperand::Constant(self.intern_literal_str(t.clone()))
                        }
                        arandu_middle::hir::HirStringPart::Expr(e) => {
                            let part_expr = self.hir.pool.expr(*e);
                            let part_op = self.lower_expr(*e, None, symbols)?;
                            let str_op = self.maybe_to_str(part_op, part_expr.ty)?;
                            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = &str_op
                                && self.owned_string_temps.remove(t)
                            {
                                intermediate_to_free.push(str_op);
                            }
                            str_op
                        }
                    };
                    part_ops.push(op);
                }
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                self.emit_assign_temp(dest, AmirRvalue::StringInterp { parts: part_ops });
                self.owned_string_temps.insert(dest);
                for intermediate in intermediate_to_free {
                    self.push_stmt(AmirStmt::Free(intermediate));
                }
                Ok(AmirOperand::Copy(dest))
            }
            HirExprKind::ToStr { value } => {
                let value_expr = self.hir.pool.expr(*value);
                let op = self.lower_expr(*value, None, symbols)?;
                // Always materialize ToStr (even for `str` identity is a no-op in maybe_to_str).
                let str_op = self.maybe_to_str(op, value_expr.ty)?;
                if let Some(dest) = target {
                    self.emit_assign_temp(dest, AmirRvalue::Use(str_op));
                    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = &str_op
                        && self.owned_string_temps.contains(t)
                    {
                        self.owned_string_temps.insert(dest);
                    }
                    Ok(AmirOperand::Copy(dest))
                } else {
                    Ok(str_op)
                }
            }
            HirExprKind::Char(v) => {
                let op = AmirOperand::Constant(self.intern_literal_char(v.clone()));
                if let Some(dest) = target {
                    self.emit_assign_temp(dest, AmirRvalue::Use(op));
                }
                Ok(op)
            }
            HirExprKind::Nil => {
                let op = if self.with_ty(expr.ty, is_option_type) {
                    let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::EnumConstruct {
                            variant_tag: 0,
                            payload: None,
                        },
                    );
                    AmirOperand::Copy(dest)
                } else {
                    AmirOperand::Constant(AmirConstant::Nil)
                };
                if let (Some(dest), false) = (target, self.with_ty(expr.ty, is_option_type)) {
                    self.emit_assign_temp(dest, AmirRvalue::Use(op));
                }
                Ok(op)
            }
            HirExprKind::Path { symbol } => {
                // Derive the parent enum SymbolId from the expression's resolved type.
                // The type checker always resolves an enum-variant expression to
                // ArType::Named(enum_sym, []), so we can use that as a filter anchor
                // instead of doing a global name-based scan — which would silently
                // pick the wrong discriminant when two enums share a variant name.
                let enum_sym_from_ty = match self.resolve_ty(expr.ty) {
                    ArType::Named(id, _) => Some(id),
                    _ => None,
                };
                let op: AmirOperand = if let Some(&local_id) = self.symbol_map.get(symbol) {
                    Ok::<AmirOperand, Diagnostic>(self.read_variable_source(local_id)?)
                } else if let Some(&tag) =
                    self.tc.type_info.enum_variant_tags.get(symbol).or_else(|| {
                        // Fallback: find the canonical variant SymbolId whose parent enum
                        // matches the type we already know this expression has, then look up
                        // its tag. No string comparison needed — anchored by SymbolId.
                        let enum_id = enum_sym_from_ty?;
                        self.tc
                            .type_info
                            .enum_variants
                            .iter()
                            .find(|&(v_sym, (parent_sym, _))| {
                                *parent_sym == enum_id
                                    && self.tc.type_info.enum_variant_tags.contains_key(v_sym)
                                    && {
                                        // Name must match (bare suffix of the lookup symbol vs
                                        // bare suffix of the registered variant symbol).
                                        let lookup_bare = symbols
                                            .get(*symbol)
                                            .name
                                            .rsplit('.')
                                            .next()
                                            .unwrap_or("");
                                        let reg_bare = symbols
                                            .get(*v_sym)
                                            .name
                                            .rsplit('.')
                                            .next()
                                            .unwrap_or("");
                                        lookup_bare == reg_bare
                                    }
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
                    let sym = symbols.get(*symbol);
                    Ok(match sym.kind {
                        SymbolKind::Func
                        | SymbolKind::ExternFunc
                        | SymbolKind::AssociatedFunc
                        | SymbolKind::NamespaceMember => AmirOperand::FunctionRef(*symbol),
                        _ => AmirOperand::GlobalRef(*symbol),
                    })
                }?;
                if let Some(dest) = target {
                    let already_assigned = self.tc.type_info.enum_variant_tags.contains_key(symbol)
                        || enum_sym_from_ty.is_some_and(|enum_id| {
                            let lookup_bare =
                                symbols.get(*symbol).name.rsplit('.').next().unwrap_or("");
                            self.tc
                                .type_info
                                .enum_variants
                                .iter()
                                .any(|(v_sym, (parent, _))| {
                                    *parent == enum_id
                                        && symbols.get(*v_sym).name.rsplit('.').next().unwrap_or("")
                                            == lookup_bare
                                        && self.tc.type_info.enum_variant_tags.contains_key(v_sym)
                                })
                        });
                    if !already_assigned {
                        let rhs = self.consume_operand(op)?;
                        self.emit_assign_temp(dest, AmirRvalue::Use(rhs));
                    }
                }
                Ok(op)
            }
            HirExprKind::TypePath {
                type_symbol,
                member_symbol,
            } => self.lower_type_path(*type_symbol, *member_symbol, &expr, target, symbols),
            HirExprKind::Generic { callee, args } => {
                // mem.sizeOf<T>() / mem.alignOf<T>() — fold to target layout constants so
                // the JIT never needs a runtime `fn@sizeOf` symbol (L6.1 mem intrinsics).
                if let Some(op) = self
                    .try_lower_mem_size_align_intrinsic(*callee, args, expr.ty, target, symbols)?
                {
                    return Ok(op);
                }
                self.lower_expr(*callee, target, symbols)
            }
            HirExprKind::Alloc { expr: inner } => {
                let inner_op = self.lower_expr(*inner, None, symbols)?;
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                self.emit_assign_temp(dest, AmirRvalue::Alloc(inner_op));
                Ok(AmirOperand::Copy(dest))
            }
            HirExprKind::Binary { op, left, right } => {
                self.lower_binary(*op, *left, *right, expr.ty, target, symbols)
            }
            HirExprKind::Unary { op, expr: sub_expr } => {
                self.lower_unary(*op, *sub_expr, expr.ty, target, symbols)
            }
            HirExprKind::Field { base, field } => {
                self.lower_field(*base, field.as_str(), expr.ty, target, symbols)
            }
            HirExprKind::Index { base, index } => {
                self.lower_index(*base, *index, expr.ty, target, symbols)
            }
            HirExprKind::Array { items } => {
                let items_slice = self.hir.pool.expr_list(*items);
                let mut item_ops = Vec::with_capacity(items_slice.len());
                for &item in items_slice {
                    item_ops.push(self.lower_expr(item, None, symbols)?);
                }
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                self.emit_assign_temp(dest, AmirRvalue::Array { items: item_ops });
                Ok(AmirOperand::Copy(dest))
            }
            HirExprKind::Call { callee, args, .. } => {
                self.lower_call(*callee, *args, &expr, target, symbols)
            }
            HirExprKind::StructLiteral {
                struct_symbol,
                fields,
            } => self.lower_struct_literal(*struct_symbol, *fields, &expr, target, symbols),
            HirExprKind::If {
                condition,
                then_block,
                else_block,
            } => self.lower_if(condition, *then_block, *else_block, &expr, target, symbols),
            HirExprKind::Cast { expr: sub_expr, .. } => {
                let sub_op = self.lower_expr(*sub_expr, None, symbols)?;
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                self.emit_assign_temp(dest, AmirRvalue::Use(sub_op));
                Ok(AmirOperand::Copy(dest))
            }

            HirExprKind::Match { value, arms } => {
                self.lower_match(*value, arms, target, expr.ty, symbols)
            }
            HirExprKind::ResultCtor { variant, value } => {
                self.lower_result_ctor(*variant, *value, &expr, target, symbols)
            }
            HirExprKind::Try { expr: inner } => self.lower_try(*inner, &expr, target, symbols),
            HirExprKind::SafeField { base, field } => {
                self.lower_safe_field(*base, field.as_str(), &expr, target, symbols)
            }
            HirExprKind::SafeIndex { base, index } => {
                self.lower_safe_index(*base, *index, &expr, target, symbols)
            }
            HirExprKind::NullCoalesce { left, right } => {
                self.lower_null_coalesce(*left, *right, &expr, target, symbols)
            }
            HirExprKind::Catch {
                expr: inner,
                handler,
            } => self.lower_catch(*inner, handler, target, expr.ty, symbols),
            HirExprKind::Lambda { .. } => Err(amir_unsupported(
                expr.span,
                "lambda/closure",
                "v0.3 LAMBDA: closure lowering",
            )),
            // A3.0/A3.1/A3.3: evaluate block as coroutine body (Suspend on nested await),
            // wrap payload as Coroutine[T]. Stack-first when not the function return slot.
            HirExprKind::AsyncBlock { block } => {
                self.lower_async_block(*block, &expr, target, symbols)
            }
            HirExprKind::UnsafeBlock { block } => {
                let dest = match target {
                    Some(t) => t,
                    None => self.new_temp_id(expr.ty),
                };
                self.lower_block_as_expr(*block, Some(dest), symbols)?;
                Ok(AmirOperand::Copy(dest))
            }
            HirExprKind::Error => {
                let dest = self.new_temp(ArType::Error);
                Ok(AmirOperand::Copy(dest))
            }
        }
    }
}
