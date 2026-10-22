use crate::amir::{AmirFunc, AmirPlace, AmirProjection, AmirStmt, AmirStmtTable, LocalId};
use crate::move_checker::MoveState;
use arandu_middle::SymbolId;
use arandu_middle::layout::instantiated_field_type;
use arandu_middle::types::{ArType, TypeId, build_subst_ids, substitute_type_id};
use arandu_typeck::TypeInfo;
use arandu_typeck::type_checker::EnumPayloadShape;

/// Checks recursively whether a type or any of its composite fields needs cleanup.
pub(crate) fn type_needs_drop(ty: TypeId, type_info: &TypeInfo) -> bool {
    let resolved = type_info.resolve_type_id(ty);
    match resolved {
        ArType::Named(sym, _) => {
            if type_info.destructor_instances.contains_key(&ty) {
                return true;
            }
            if let Some(fields) = type_info.struct_fields.get(&sym) {
                for f in fields.iter() {
                    let field_ty = instantiated_field_type(
                        &resolved,
                        &f.name,
                        &type_info.type_interner,
                        type_info,
                    )
                    .unwrap_or(f.ty);
                    if type_needs_drop(field_ty, type_info) {
                        return true;
                    }
                }
            }
            enum_variant_payload_types(&resolved, type_info).is_some_and(|variants| {
                variants.iter().any(|(_, fields)| {
                    fields
                        .iter()
                        .any(|field| type_needs_drop(*field, type_info))
                })
            })
        }
        ArType::Array(len, elem) => len > 0 && type_needs_drop(elem, type_info),
        ArType::ConstArray(_, elem) => type_needs_drop(elem, type_info),
        ArType::Option(elem) => type_needs_drop(elem, type_info),
        ArType::Result(ok, err) => {
            type_needs_drop(ok, type_info) || type_needs_drop(err, type_info)
        }
        ArType::Tuple(args) => {
            let arg_ids = type_info.type_interner.type_args(args);
            arg_ids
                .iter()
                .any(|&arg_ty| type_needs_drop(arg_ty, type_info))
        }
        _ => false,
    }
}

/// Emits `Destroy` statements for `place`:
/// 1. Runs the type's custom `@Destructor` if declared (unless skipping top-level).
/// 2. Recursively destroys composite struct fields in reverse declaration order.
/// 3. Destroys container types (`Array`, `Option`, `Result`, `Tuple`).
pub(super) fn emit_recursive_drops(
    place: &AmirPlace,
    ty: TypeId,
    type_info: &TypeInfo,
    move_state: &MoveState,
    skip_top_level_destructor: bool,
    rebuilt: &mut AmirStmtTable,
) {
    if !move_state.place_itself_is_available(place) {
        return;
    }
    let resolved = type_info.resolve_type_id(ty);
    match resolved {
        ArType::Named(sym, _) => {
            let has_destructor = type_info.destructor_instances.contains_key(&ty);
            if has_destructor && !skip_top_level_destructor {
                rebuilt.push(AmirStmt::Destroy(place.clone()));
                return;
            }

            if let Some(variants) = enum_variant_payload_types(&resolved, type_info) {
                let selected_tag = move_state.moved_descendant_paths(place).find_map(|moved| {
                    match moved.projections.get(place.projections.len()) {
                        Some(AmirProjection::Variant(tag)) => Some(*tag),
                        _ => None,
                    }
                });
                if let Some(tag) = selected_tag {
                    if let Some((_, fields)) =
                        variants.iter().find(|(variant_tag, _)| *variant_tag == tag)
                    {
                        let tuple_ty = (fields.len() > 1).then(|| {
                            type_info
                                .type_interner
                                .intern(ArType::tuple(fields, &type_info.type_interner))
                        });
                        for (index, field_ty) in fields.iter().copied().enumerate().rev() {
                            if !type_needs_drop(field_ty, type_info) {
                                continue;
                            }
                            let mut payload_place = place.clone();
                            payload_place.projections.push(AmirProjection::Variant(tag));
                            payload_place.projections.push(AmirProjection::Payload {
                                variant_tag: tag,
                                index,
                                field_ty,
                                tuple_ty,
                            });
                            emit_recursive_drops(
                                &payload_place,
                                field_ty,
                                type_info,
                                move_state,
                                false,
                                rebuilt,
                            );
                        }
                    }
                } else {
                    // Variant selection is runtime-dependent. The canonical
                    // expansion pass turns this into discriminant CFG edges.
                    rebuilt.push(AmirStmt::Destroy(place.clone()));
                }
                return;
            }

            if let Some(fields) = type_info.struct_fields.get(&sym) {
                let mut indexed: Vec<(usize, Option<SymbolId>, TypeId)> = fields
                    .iter()
                    .map(|f| {
                        let ty = instantiated_field_type(
                            &resolved,
                            &f.name,
                            &type_info.type_interner,
                            type_info,
                        )
                        .unwrap_or(f.ty);
                        (f.index, f.symbol, ty)
                    })
                    .collect();
                indexed.sort_by_key(|(idx, _, _)| std::cmp::Reverse(*idx));

                for (_, fsym, fty) in indexed {
                    if type_needs_drop(fty, type_info) {
                        let mut sub_place = place.clone();
                        if let Some(fsym) = fsym {
                            sub_place.projections.push(AmirProjection::Field(fsym));
                        }
                        emit_recursive_drops(
                            &sub_place, fty, type_info, move_state, false, rebuilt,
                        );
                    }
                }

                // The composite has no explicit destructor, so its own storage is
                // still a cleanup obligation: the fields were walked above and the
                // root comes last. Backends whose aggregates live in frames treat
                // this `Destroy` as a no-op; a backend that owns heap storage (the
                // wasm cell model) reclaims the cell here.
                if !has_destructor {
                    rebuilt.push(AmirStmt::Destroy(place.clone()));
                }
            }
        }
        ArType::Option(inner) => {
            if let Some(payload_place) = moved_enum_payload_place(place, move_state, 1, inner, None)
            {
                emit_recursive_drops(&payload_place, inner, type_info, move_state, false, rebuilt);
            } else {
                rebuilt.push(AmirStmt::Destroy(place.clone()));
            }
        }
        ArType::Result(ok, err) => {
            let selected = move_state.moved_descendant_paths(place).find_map(|moved| {
                let variant = moved.projections.get(place.projections.len())?;
                let AmirProjection::Variant(tag) = variant else {
                    return None;
                };
                Some(*tag)
            });
            if let Some((tag, payload_ty)) = selected.and_then(|tag| match tag {
                0 => Some((0, ok)),
                1 => Some((1, err)),
                _ => None,
            }) {
                if let Some(payload_place) =
                    moved_enum_payload_place(place, move_state, tag, payload_ty, None)
                {
                    emit_recursive_drops(
                        &payload_place,
                        payload_ty,
                        type_info,
                        move_state,
                        false,
                        rebuilt,
                    );
                }
            } else {
                rebuilt.push(AmirStmt::Destroy(place.clone()));
            }
        }
        ArType::Tuple(args) => {
            let fields = type_info.type_interner.type_args(args);
            for (index, field_ty) in fields.iter().copied().enumerate().rev() {
                if !type_needs_drop(field_ty, type_info) {
                    continue;
                }
                let mut field_place = place.clone();
                field_place
                    .projections
                    .push(AmirProjection::TupleField(index));
                emit_recursive_drops(
                    &field_place,
                    field_ty,
                    type_info,
                    move_state,
                    false,
                    rebuilt,
                );
            }
        }
        ArType::Array(len, elem) => {
            for index in (0..len).rev() {
                if !type_needs_drop(elem, type_info) {
                    continue;
                }
                let Ok(index) = usize::try_from(index) else {
                    rebuilt.push(AmirStmt::Destroy(place.clone()));
                    return;
                };
                let mut element_place = place.clone();
                element_place
                    .projections
                    .push(AmirProjection::IndexConstant(index));
                emit_recursive_drops(&element_place, elem, type_info, move_state, false, rebuilt);
            }
        }
        ArType::ConstArray(..) => {
            rebuilt.push(AmirStmt::Destroy(place.clone()));
        }
        _ => {}
    }
}

/// Resolves the type at a projected place so an assignment destroys the old
/// field/element rather than the aggregate that contains it.
pub(super) fn projected_place_type(
    place: &AmirPlace,
    func: &AmirFunc,
    type_info: &TypeInfo,
) -> Option<TypeId> {
    let mut ty = func.locals.get(place.local.as_usize())?.ty;
    for projection in &place.projections {
        let resolved = type_info.resolve_type_id(ty);
        ty = match projection {
            AmirProjection::Deref => match resolved {
                ArType::Ref(inner)
                | ArType::RefMut(inner)
                | ArType::Ptr(inner)
                | ArType::Nullable(inner) => inner,
                _ => return None,
            },
            AmirProjection::Field(field_symbol) => {
                let ArType::Named(struct_symbol, _) = &resolved else {
                    return None;
                };
                let field = type_info
                    .struct_fields
                    .get(struct_symbol)?
                    .iter()
                    .find(|field| field.symbol == Some(*field_symbol))?;
                instantiated_field_type(&resolved, &field.name, &type_info.type_interner, type_info)
                    .unwrap_or(field.ty)
            }
            AmirProjection::Variant(_) => ty,
            AmirProjection::Payload { field_ty, .. } => *field_ty,
            AmirProjection::TupleField(index) => {
                let ArType::Tuple(args) = resolved else {
                    return None;
                };
                *type_info.type_interner.type_args(args).get(*index)?
            }
            AmirProjection::Index(_) | AmirProjection::IndexConstant(_) => match resolved {
                ArType::Array(_, inner)
                | ArType::ConstArray(_, inner)
                | ArType::Slice(inner)
                | ArType::MutSlice(inner) => inner,
                _ => return None,
            },
        };
    }
    Some(ty)
}

/// If an enum payload was partially moved, its normal recursive drop glue
/// would visit that moved field again. The selected variant is encoded in the
/// move path, so continue elaborating only its payload; a fully moved payload
/// naturally produces no drop, while unmoved nested fields still do.
fn moved_enum_payload_place(
    place: &AmirPlace,
    move_state: &MoveState,
    tag: usize,
    field_ty: TypeId,
    tuple_ty: Option<TypeId>,
) -> Option<AmirPlace> {
    let moved = move_state.moved_descendant_paths(place).find(|moved| {
        matches!(
            moved.projections.get(place.projections.len()),
            Some(AmirProjection::Variant(found_tag)) if *found_tag == tag
        ) && matches!(
            moved.projections.get(place.projections.len() + 1),
            Some(AmirProjection::Payload { .. })
        )
    })?;
    let mut payload = place.clone();
    payload.projections.push(AmirProjection::Variant(tag));
    payload.projections.push(AmirProjection::Payload {
        variant_tag: tag,
        index: 0,
        field_ty,
        tuple_ty,
    });
    (moved.projections.len() >= payload.projections.len()).then_some(payload)
}

/// Identifies locals whose string ownership is unambiguous for the whole function.
///
/// A primitive `str` currently carries no runtime ownership bit. Consequently,
/// a local is safe to destroy only when it has exactly one root store and that
/// store receives a freshly allocated string. Treating a local as owned merely
/// because *one* of its stores is owned can free static string data after a
/// later reassignment. Multiple stores are deliberately left alone until AMIR
/// models string ownership explicitly or this pass grows path-sensitive drop
/// flags.
pub(super) fn find_owned_string_locals(func: &AmirFunc) -> rustc_hash::FxHashSet<LocalId> {
    let mut owned_temps = rustc_hash::FxHashSet::default();
    for stmt in func.stmts.payloads.iter() {
        if let AmirStmt::Assign { lhs, rhs } = stmt
            && matches!(
                rhs,
                crate::amir::AmirRvalue::ToStr { .. }
                    | crate::amir::AmirRvalue::StringInterp { .. }
            )
        {
            owned_temps.insert(lhs);
        }
    }
    let mut root_store_count = rustc_hash::FxHashMap::<LocalId, usize>::default();
    let mut owned_store_count = rustc_hash::FxHashMap::<LocalId, usize>::default();
    for stmt in func.stmts.payloads.iter() {
        if let AmirStmt::Store { lhs, rhs } = stmt
            && lhs.projections.is_empty()
        {
            *root_store_count.entry(lhs.local).or_default() += 1;
            if matches!(
                rhs,
                crate::amir::AmirOperand::Copy(t) | crate::amir::AmirOperand::Move(t)
                    if owned_temps.contains(t)
            ) {
                *owned_store_count.entry(lhs.local).or_default() += 1;
            }
        }
    }
    owned_store_count
        .into_iter()
        .filter_map(|(local, owned)| {
            (owned == 1 && root_store_count.get(&local) == Some(&1)).then_some(local)
        })
        .collect()
}

/// Returns enum variants in tag order with payload field types instantiated for
/// the concrete enum type. Built-in sum types use their language-defined tags.
pub(super) fn enum_variant_payload_types(
    enum_ty: &ArType,
    type_info: &TypeInfo,
) -> Option<Vec<(usize, Vec<TypeId>)>> {
    match enum_ty {
        ArType::Option(inner) => Some(vec![(0, Vec::new()), (1, vec![*inner])]),
        ArType::Result(ok, err) => Some(vec![(0, vec![*ok]), (1, vec![*err])]),
        ArType::Named(enum_symbol, args) => {
            if type_info
                .destructor_instances
                .contains_key(&type_info.type_interner.intern(enum_ty.clone()))
            {
                return None;
            }
            let generic_params = type_info
                .generic_params
                .get(enum_symbol)
                .map_or(&[][..], |params| params.as_slice());
            let subst = build_subst_ids(
                generic_params,
                &type_info.type_interner.type_args(*args),
                &type_info.type_interner,
            );
            let mut variants: Vec<_> = type_info
                .enum_variants
                .iter()
                .filter_map(|(variant, (parent, _))| {
                    let shape = type_info
                        .variant_payload_for(*variant, &type_info.type_interner.type_args(*args))?;
                    (*parent == *enum_symbol).then_some((
                        type_info
                            .enum_variant_tags
                            .get(variant)
                            .copied()
                            .unwrap_or(0),
                        shape,
                    ))
                })
                .collect();
            variants.sort_by_key(|(tag, _)| *tag);
            variants.dedup_by_key(|(tag, _)| *tag);
            if variants.is_empty() {
                return None;
            }
            Some(
                variants
                    .into_iter()
                    .map(|(tag, shape)| {
                        let fields = match shape {
                            EnumPayloadShape::Unit => Vec::new(),
                            EnumPayloadShape::Tuple(fields) => fields
                                .iter()
                                .map(|field| {
                                    substitute_type_id(*field, &subst, &type_info.type_interner)
                                })
                                .collect(),
                        };
                        (tag, fields)
                    })
                    .collect(),
            )
        }
        _ => None,
    }
}
