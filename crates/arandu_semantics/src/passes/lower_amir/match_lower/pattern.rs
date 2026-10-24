use super::LowerCtx;
use crate::SymbolTable;
use crate::amir::{AmirOperand, AmirPlace, AmirProjection, AmirRvalue};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::hir::HirPattern;
use crate::passes::type_checker::types::{ArType, Primitive};

impl LowerCtx<'_> {
    pub(super) fn pattern_has_bindings(&self, pattern: &HirPattern) -> bool {
        match pattern {
            HirPattern::Bind { .. } => true,
            HirPattern::Enum { payload, .. } | HirPattern::TypeTuple { payload, .. } => self
                .hir
                .pool
                .pattern_list(*payload)
                .iter()
                .any(|id| self.pattern_has_bindings(self.hir.pool.pattern(*id))),
            HirPattern::Struct { fields, .. } => {
                self.hir.pool.field_pattern_list(*fields).iter().any(|id| {
                    let field = self.hir.pool.field_pattern(*id);
                    field.pattern.is_none_or(|pattern| {
                        self.pattern_has_bindings(self.hir.pool.pattern(pattern))
                    })
                })
            }
            HirPattern::Tuple { items, .. } | HirPattern::Or { alts: items, .. } => self
                .hir
                .pool
                .pattern_list(*items)
                .iter()
                .any(|id| self.pattern_has_bindings(self.hir.pool.pattern(*id))),
            HirPattern::Wildcard { .. } | HirPattern::Literal { .. } | HirPattern::Range { .. } => {
                false
            }
        }
    }

    pub(super) fn scrutinee_source_place(&self, scrutinee: AmirOperand) -> Option<AmirPlace> {
        let (AmirOperand::Copy(temp) | AmirOperand::Move(temp)) = scrutinee else {
            return None;
        };
        self.temp_place_origins
            .get(temp.as_usize())
            .cloned()
            .flatten()
    }

    fn pattern_bindings_are_copy(&self, pattern: &HirPattern) -> bool {
        match pattern {
            HirPattern::Bind { symbol, .. } => self
                .tc
                .type_info
                .decl_type_id(*symbol)
                .is_some_and(|ty| self.tc.type_info.is_copy(ty)),
            HirPattern::Enum { payload, .. } | HirPattern::TypeTuple { payload, .. } => self
                .hir
                .pool
                .pattern_list(*payload)
                .iter()
                .all(|id| self.pattern_bindings_are_copy(self.hir.pool.pattern(*id))),
            HirPattern::Struct { fields, .. } => {
                self.hir.pool.field_pattern_list(*fields).iter().all(|id| {
                    let field = self.hir.pool.field_pattern(*id);
                    if let Some(pattern) = field.pattern {
                        self.pattern_bindings_are_copy(self.hir.pool.pattern(pattern))
                    } else {
                        self.tc
                            .resolved
                            .definitions
                            .get(&crate::NodeKey::from(field.span))
                            .and_then(|symbol| self.tc.type_info.decl_type_id(*symbol))
                            .is_some_and(|ty| self.tc.type_info.is_copy(ty))
                    }
                })
            }
            HirPattern::Tuple { items, .. } | HirPattern::Or { alts: items, .. } => self
                .hir
                .pool
                .pattern_list(*items)
                .iter()
                .all(|id| self.pattern_bindings_are_copy(self.hir.pool.pattern(*id))),
            HirPattern::Wildcard { .. } | HirPattern::Literal { .. } | HirPattern::Range { .. } => {
                true
            }
        }
    }

    pub(super) fn commit_match_bindings(
        &mut self,
        scrutinee: AmirOperand,
        pattern: &HirPattern,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        let scrutinee_ty = self.operand_type(&scrutinee);
        if self.pattern_bindings_are_copy(pattern)
            && !matches!(
                scrutinee_ty,
                ArType::Ref(_) | ArType::RefMut(_) | ArType::Ptr(_)
            )
        {
            // Copy payload extraction must use the enum-aware rvalue. Besides
            // handling ordinary aggregate payloads, this decodes pointer-tag
            // enums whose payload is carried in the tag word itself.
            let _ = self.lower_pattern_match(scrutinee, pattern, symbols)?;
            return Ok(());
        }

        let root = self.scrutinee_source_place(scrutinee).ok_or_else(|| {
            Diagnostic::ice(
                DiagCode::ICEGEN001,
                "selected match scrutinee was not materialized as a place",
                self.diag_span(pattern.span()),
            )
        })?;
        self.lower_pattern_bindings_from_place(root, pattern, scrutinee_ty, symbols)
    }

    pub(super) fn lower_guard_pattern_borrows(
        &mut self,
        root: AmirPlace,
        pattern: &HirPattern,
        value_ty: ArType,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        self.lower_pattern_places_at(root, pattern, value_ty, symbols, false)
    }

    fn lower_pattern_bindings_from_place(
        &mut self,
        root: AmirPlace,
        pattern: &HirPattern,
        value_ty: ArType,
        symbols: &SymbolTable,
    ) -> Result<(), Diagnostic> {
        self.lower_pattern_places_at(root, pattern, value_ty, symbols, true)
    }

    /// Traverse the pattern against a stable source place. Guard mode creates
    /// shared aliases for tests; commit mode moves each binding from its exact
    /// projected place after the whole pattern and guard have succeeded.
    fn lower_pattern_places_at(
        &mut self,
        mut place: AmirPlace,
        pattern: &HirPattern,
        mut value_ty: ArType,
        symbols: &SymbolTable,
        commit: bool,
    ) -> Result<(), Diagnostic> {
        match pattern {
            HirPattern::Bind { symbol, span, .. } => {
                let bound_ty = self
                    .tc
                    .type_info
                    .decl_type_id(*symbol)
                    .unwrap_or_else(|| self.intern_ty_ref(&value_ty));
                if commit {
                    let local = self.new_local_id(bound_ty, *symbol, *span);
                    let bound_type = self.tc.type_info.resolve_type_id(bound_ty);
                    let source_ty_id = self.tc.type_info.type_interner.intern(value_ty.clone());
                    let source_is_already_reference = bound_ty == source_ty_id;
                    let value = if matches!(bound_type, ArType::Ref(_) | ArType::RefMut(_))
                        && !source_is_already_reference
                    {
                        let borrowed = self.new_temp_id(bound_ty);
                        let borrow = if matches!(
                            self.tc.type_info.resolve_type_id(bound_ty),
                            ArType::RefMut(_)
                        ) {
                            AmirRvalue::BorrowMut(place.clone())
                        } else {
                            AmirRvalue::Borrow(place.clone())
                        };
                        self.emit_assign_temp(borrowed, borrow);
                        AmirOperand::Copy(borrowed)
                    } else {
                        self.load_place(&place, bound_ty)?
                    };
                    self.emit_store_place(
                        AmirPlace {
                            local,
                            projections: smallvec::SmallVec::new(),
                        },
                        value,
                    )?;
                } else {
                    let borrowed_ty = match self.tc.type_info.resolve_type_id(bound_ty) {
                        ArType::Ref(_) | ArType::RefMut(_) => bound_ty,
                        _ => self.intern_ty(ArType::Ref(bound_ty)),
                    };
                    let borrowed = self.new_temp_id(borrowed_ty);
                    self.emit_assign_temp(borrowed, AmirRvalue::Borrow(place));
                    let local = self.new_compiler_local_id(borrowed_ty, *span);
                    self.emit_store_place(
                        AmirPlace {
                            local,
                            projections: smallvec::SmallVec::new(),
                        },
                        AmirOperand::Copy(borrowed),
                    )?;
                    self.guard_borrows.insert(*symbol, (local, bound_ty));
                }
            }
            HirPattern::Struct {
                struct_symbol,
                fields,
                ..
            } => {
                self.peel_guard_reference(&mut value_ty, &mut place);
                let struct_fields = self.tc.type_info.struct_fields.get(struct_symbol);
                let field_ids = self.hir.pool.field_pattern_list(*fields);
                for &field_id in field_ids {
                    let field = self.hir.pool.field_pattern(field_id);
                    let Some(info) =
                        struct_fields.and_then(|fields| fields.get(field.name.as_str()))
                    else {
                        continue;
                    };
                    let Some(field_symbol) = info.symbol else {
                        continue;
                    };
                    let field_ty = self.tc.type_info.resolve_type_id(info.ty);
                    let mut field_place = place.clone();
                    self.peel_guard_reference(&mut value_ty, &mut field_place);
                    field_place
                        .projections
                        .push(AmirProjection::Field(field_symbol));
                    if let Some(pattern_id) = field.pattern {
                        let nested = self.hir.pool.pattern(pattern_id);
                        self.lower_pattern_places_at(
                            field_place,
                            nested,
                            field_ty,
                            symbols,
                            commit,
                        )?;
                    } else if let Some(symbol) = self
                        .tc
                        .resolved
                        .definitions
                        .get(&crate::NodeKey::from(field.span))
                        .copied()
                    {
                        self.lower_pattern_places_at(
                            field_place,
                            &HirPattern::Bind {
                                span: field.span,
                                name: field.name.clone(),
                                symbol,
                            },
                            field_ty,
                            symbols,
                            commit,
                        )?;
                    }
                }
            }
            HirPattern::Tuple { items, .. } => {
                self.peel_guard_reference(&mut value_ty, &mut place);
                let ArType::Tuple(tuple_args) = &value_ty else {
                    return Ok(());
                };
                let item_types = self.tc.type_info.type_interner.type_args(*tuple_args);
                for (index, &pattern_id) in self.hir.pool.pattern_list(*items).iter().enumerate() {
                    let Some(&field_ty) = item_types.get(index) else {
                        continue;
                    };
                    let mut field_place = place.clone();
                    self.peel_guard_reference(&mut value_ty, &mut field_place);
                    field_place
                        .projections
                        .push(AmirProjection::TupleField(index));
                    let nested = self.hir.pool.pattern(pattern_id);
                    self.lower_pattern_places_at(
                        field_place,
                        nested,
                        self.tc.type_info.resolve_type_id(field_ty),
                        symbols,
                        commit,
                    )?;
                }
            }
            HirPattern::TypeTuple {
                span,
                name,
                payload,
            } => {
                self.peel_guard_reference(&mut value_ty, &mut place);
                let payload_patterns = self.hir.pool.pattern_list(*payload);
                match (&value_ty, name.as_str()) {
                    (ArType::Option(inner), "Some") if payload_patterns.len() == 1 => {
                        let field_ty = *inner;
                        let mut payload_place = place;
                        payload_place.projections.push(AmirProjection::Variant(1));
                        payload_place.projections.push(AmirProjection::Payload {
                            variant_tag: 1,
                            index: 0,
                            field_ty,
                            tuple_ty: None,
                        });
                        let nested = self.hir.pool.pattern(payload_patterns[0]);
                        self.lower_pattern_places_at(
                            payload_place,
                            nested,
                            self.tc.type_info.resolve_type_id(field_ty),
                            symbols,
                            commit,
                        )?;
                    }
                    (ArType::Result(ok, err), "Ok" | "Err") if payload_patterns.len() == 1 => {
                        let (tag, field_ty) = if name.as_str() == "Ok" {
                            (0, *ok)
                        } else {
                            (1, *err)
                        };
                        let mut payload_place = place;
                        payload_place.projections.push(AmirProjection::Variant(tag));
                        payload_place.projections.push(AmirProjection::Payload {
                            variant_tag: tag,
                            index: 0,
                            field_ty,
                            tuple_ty: None,
                        });
                        let nested = self.hir.pool.pattern(payload_patterns[0]);
                        self.lower_pattern_places_at(
                            payload_place,
                            nested,
                            self.tc.type_info.resolve_type_id(field_ty),
                            symbols,
                            commit,
                        )?;
                    }
                    (ArType::Poll(inner), "Ready") if payload_patterns.len() == 1 => {
                        let field_ty = *inner;
                        let mut payload_place = place;
                        payload_place.projections.push(AmirProjection::Variant(0));
                        payload_place.projections.push(AmirProjection::Payload {
                            variant_tag: 0,
                            index: 0,
                            field_ty,
                            tuple_ty: None,
                        });
                        let nested = self.hir.pool.pattern(payload_patterns[0]);
                        self.lower_pattern_places_at(
                            payload_place,
                            nested,
                            self.tc.type_info.resolve_type_id(field_ty),
                            symbols,
                            commit,
                        )?;
                    }
                    _ => {
                        let _ = span;
                    }
                }
            }
            HirPattern::Enum {
                type_symbol,
                variant,
                variant_symbol,
                payload,
                ..
            } => {
                self.peel_guard_reference(&mut value_ty, &mut place);
                let payload_patterns = self.hir.pool.pattern_list(*payload);
                let Some(variant_id) =
                    variant_symbol.or_else(|| {
                        self.tc.type_info.enum_variants.iter().find_map(
                            |(candidate, (owner, _))| {
                                (*owner == *type_symbol
                                    && symbols.get(*candidate).name.rsplit('.').next()
                                        == Some(variant.as_str()))
                                .then_some(*candidate)
                            },
                        )
                    })
                else {
                    return Ok(());
                };
                let Some(&tag) = self.tc.type_info.enum_variant_tags.get(&variant_id) else {
                    return Ok(());
                };
                let Some((_, crate::passes::type_checker::EnumPayloadShape::Tuple(payload_tys))) =
                    self.tc.type_info.enum_variants.get(&variant_id)
                else {
                    return Ok(());
                };
                let enum_args = match &value_ty {
                    ArType::Named(id, args) if id == type_symbol => {
                        self.tc.type_info.type_interner.type_args(*args)
                    }
                    _ => Vec::new(),
                };
                let payload_tys = self.instantiate_enum_payload_types(
                    *type_symbol,
                    variant_id,
                    &enum_args,
                    payload_tys,
                );
                let tuple_ty = if payload_tys.len() > 1 {
                    let tuple = ArType::tuple(&payload_tys, &self.tc.type_info.type_interner);
                    Some(self.tc.type_info.type_interner.intern(tuple))
                } else {
                    None
                };
                for (index, (&pattern_id, &field_ty)) in
                    payload_patterns.iter().zip(&payload_tys).enumerate()
                {
                    let mut payload_place = place.clone();
                    payload_place.projections.push(AmirProjection::Variant(tag));
                    payload_place.projections.push(AmirProjection::Payload {
                        variant_tag: tag,
                        index,
                        field_ty,
                        tuple_ty,
                    });
                    let nested = self.hir.pool.pattern(pattern_id);
                    self.lower_pattern_places_at(
                        payload_place,
                        nested,
                        self.tc.type_info.resolve_type_id(field_ty),
                        symbols,
                        commit,
                    )?;
                }
            }
            HirPattern::Wildcard { .. } | HirPattern::Literal { .. } | HirPattern::Range { .. } => {
            }
            HirPattern::Or { alts, .. } => {
                let alternatives = self.hir.pool.pattern_list(*alts);
                if alternatives
                    .iter()
                    .any(|id| self.pattern_has_bindings(self.hir.pool.pattern(*id)))
                {
                    return Err(Diagnostic::error(
                        DiagCode::U001FeatureNotSupported,
                        "guarded or-pattern bindings cannot yet be borrowed transactionally",
                        pattern.span(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn peel_guard_reference(&mut self, ty: &mut ArType, place: &mut AmirPlace) {
        for _ in 0..4 {
            match ty {
                ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                    *ty = self.resolve_ty(*inner);
                    place.projections.push(AmirProjection::Deref);
                    // A projected place is addressed through this scalar
                    // reference local after dummy SSA loads/stores are pruned.
                    self.mark_local_materialized(place.local);
                }
                _ => break,
            }
        }
    }

    pub(super) fn lower_option_variant_test(
        &mut self,
        scrutinee: AmirOperand,
        tag: i128,
    ) -> AmirOperand {
        let tag_temp = self.new_temp(ArType::Primitive(Primitive::Int));
        self.emit_assign_temp(tag_temp, AmirRvalue::Discriminant { value: scrutinee });
        let expected = AmirOperand::Constant(self.intern_literal_int(tag.to_string()));
        let matches = self.new_temp(ArType::Primitive(Primitive::Bool));
        self.emit_assign_temp(
            matches,
            AmirRvalue::Binary {
                op: crate::ops::BinaryOp::Equal,
                left: AmirOperand::Copy(tag_temp),
                right: expected,
            },
        );
        AmirOperand::Copy(matches)
    }
}
