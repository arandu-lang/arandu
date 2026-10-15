//! Elaborate semantic cleanup obligations into explicit AMIR `Destroy`.
//!
//! Arandu currently rejects inconsistent moves between CFG branches, so a
//! valid return boundary never needs a runtime drop flag: each initialized
//! local is either available on every path or moved on every path. This pass
//! reuses the move and definite-init facts that enforce that rule.

use crate::amir::{
    AmirBasicBlock, AmirFunc, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt,
    AmirStmtTable, AmirTemp, AmirTerminator, BlockId, LocalId, TempId,
};
use crate::move_checker::{MoveState, move_states_at_block_exit};
use arandu_lexer::Span;
use arandu_middle::SymbolId;
use arandu_middle::layout::{DenseRange, instantiated_field_type};
use arandu_middle::types::{ArType, TypeId, build_subst_ids, substitute_type_id};
use arandu_typeck::TypeInfo;
use arandu_typeck::type_checker::EnumPayloadShape;
use smallvec::SmallVec;

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
fn emit_recursive_drops(
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
fn projected_place_type(
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
                ArType::Array(_, inner) | ArType::ConstArray(_, inner) | ArType::Slice(inner) => {
                    inner
                }
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
fn find_owned_string_locals(func: &AmirFunc) -> rustc_hash::FxHashSet<LocalId> {
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

/// Insert exactly-once root-local and nested cascade destruction before normal function returns.
pub fn elaborate_drops(func: &mut AmirFunc, type_info: &TypeInfo) {
    let owned_string_locals = find_owned_string_locals(func);
    let is_destructor_func = type_info
        .destructor_instances
        .values()
        .any(|symbol| *symbol == func.symbol);

    let initialized = crate::definite_init::initialized_at_block_exit(func);
    let initialized_before = crate::definite_init::initialized_before_statements(func);
    let moved = move_states_at_block_exit(func);
    let moved_before = crate::move_checker::move_states_before_statements(func);
    // A merge can make a payload move look conditional even when the incoming
    // paths are disjoint enum variants (for example `Ok(value)` vs `Err(_)`).
    // A root `Destroy` after that merge would recursively drop the already-moved
    // payload. When the merge is immediately followed by `StorageDead`, move
    // cleanup back onto its unconditional incoming goto edges, where each path's
    // ownership state is precise.
    let mut edge_drops = rustc_hash::FxHashMap::<BlockId, Vec<LocalId>>::default();
    let mut edge_cleaned = rustc_hash::FxHashSet::<(BlockId, LocalId)>::default();
    for block in &func.blocks {
        for (stmt_index, stmt) in func.block_stmts(block.id).enumerate() {
            let AmirStmt::StorageDead(local_id) = stmt else {
                continue;
            };
            let root = AmirPlace {
                local: *local_id,
                projections: SmallVec::new(),
            };
            let local = &func.locals[local_id.as_usize()];
            if !type_needs_drop(local.ty, type_info)
                || !moved_before[block.id.as_usize()][stmt_index].has_maybe_moved_descendant(&root)
            {
                continue;
            }
            let prefix_is_lifecycle_only =
                func.block_stmts(block.id).take(stmt_index).all(|stmt| {
                    matches!(
                        stmt,
                        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop
                    )
                });
            if !prefix_is_lifecycle_only {
                continue;
            }
            let predecessors = func.predecessors(block.id);
            if predecessors.is_empty()
                || predecessors.iter().any(|pred| {
                    !matches!(
                        func.block(*pred).terminator,
                        AmirTerminator::Goto { target, .. } if target == block.id
                    )
                })
            {
                continue;
            }
            for &pred in predecessors {
                edge_drops.entry(pred).or_default().push(*local_id);
                edge_cleaned.insert((block.id, *local_id));
            }
        }
    }
    let old = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    let mut rebuilt = AmirStmtTable::new();
    let mut ranges = Vec::with_capacity(func.blocks.len());

    for block in &func.blocks {
        let start = rebuilt.len();
        for (stmt_index, id) in block
            .statements
            .iter_ids::<crate::amir::InstrId>()
            .enumerate()
        {
            let stmt = &old.payloads[id];
            if let AmirStmt::Store { lhs, .. } = stmt
                && initialized_before[block.id.as_usize()][stmt_index].contains(lhs.local)
                && !(is_destructor_func && lhs.local.as_usize() == 0)
                && let Some(place_ty) = projected_place_type(lhs, func, type_info)
                && (type_needs_drop(place_ty, type_info)
                    || (lhs.projections.is_empty() && owned_string_locals.contains(&lhs.local)))
                && moved_before[block.id.as_usize()][stmt_index].place_itself_is_available(lhs)
            {
                if lhs.projections.is_empty() && owned_string_locals.contains(&lhs.local) {
                    rebuilt.push(AmirStmt::Destroy(lhs.clone()));
                } else {
                    emit_recursive_drops(
                        lhs,
                        place_ty,
                        type_info,
                        &moved_before[block.id.as_usize()][stmt_index],
                        false,
                        &mut rebuilt,
                    );
                }
            }
            if let AmirStmt::StorageDead(local_id) = stmt {
                let local = &func.locals[local_id.as_usize()];
                let root = AmirPlace {
                    local: *local_id,
                    projections: SmallVec::new(),
                };
                let is_owned_string = owned_string_locals.contains(local_id);
                if !edge_cleaned.contains(&(block.id, *local_id))
                    && !(is_destructor_func && local_id.as_usize() == 0)
                    && (type_needs_drop(local.ty, type_info) || is_owned_string)
                    && initialized_before[block.id.as_usize()][stmt_index].contains(*local_id)
                {
                    let move_state = &moved_before[block.id.as_usize()][stmt_index];
                    if is_owned_string {
                        if move_state.place_itself_is_available(&root) {
                            rebuilt.push(AmirStmt::Destroy(root));
                        }
                    } else {
                        emit_recursive_drops(
                            &root,
                            local.ty,
                            type_info,
                            move_state,
                            false,
                            &mut rebuilt,
                        );
                    }
                }
            }
            rebuilt.push(stmt.clone());
        }
        if let Some(locals) = edge_drops.get(&block.id) {
            for local_id in locals {
                let local = &func.locals[local_id.as_usize()];
                let root = AmirPlace {
                    local: *local_id,
                    projections: SmallVec::new(),
                };
                emit_recursive_drops(
                    &root,
                    local.ty,
                    type_info,
                    &moved[block.id.as_usize()],
                    false,
                    &mut rebuilt,
                );
            }
        }
        if matches!(block.terminator, AmirTerminator::Return) {
            let move_state = &moved[block.id.as_usize()];
            for local in func.locals.iter().rev() {
                let skip_self = is_destructor_func && local.id.as_usize() == 0;
                let is_owned_string = owned_string_locals.contains(&local.id);
                if (type_needs_drop(local.ty, type_info) || is_owned_string)
                    && initialized[block.id.as_usize()].contains(local.id)
                {
                    let root_place = AmirPlace {
                        local: LocalId::from_usize(local.id.as_usize()),
                        projections: SmallVec::new(),
                    };
                    if is_owned_string {
                        if move_state.place_itself_is_available(&root_place) {
                            rebuilt.push(AmirStmt::Destroy(root_place));
                        }
                    } else {
                        emit_recursive_drops(
                            &root_place,
                            local.ty,
                            type_info,
                            move_state,
                            skip_self,
                            &mut rebuilt,
                        );
                    }
                }
            }
        }
        ranges.push(DenseRange::new(start, rebuilt.len() - start));
    }

    func.stmts = rebuilt;
    for (block, range) in func.blocks.iter_mut().zip(ranges) {
        block.statements = range;
    }

    expand_indirect_drops(func, type_info);
    expand_dynamic_enum_drops(func, type_info);
}

/// Canonicalize unsafe `dropInPlace` operations before backends see the AMIR.
/// The intrinsic is lowered to `Destroy(*pointer)`; this pass recursively
/// decomposes that place just like lexical cleanup, so emitters only need to
/// implement atomic leaf destruction and nominal destructor calls.
fn expand_indirect_drops(func: &mut AmirFunc, type_info: &TypeInfo) {
    let moved_before = crate::move_checker::move_states_before_statements(func);
    let old = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    let mut rebuilt = AmirStmtTable::new();
    let mut ranges = Vec::with_capacity(func.blocks.len());

    for block in &func.blocks {
        let start = rebuilt.len();
        for (stmt_index, id) in block
            .statements
            .iter_ids::<crate::amir::InstrId>()
            .enumerate()
        {
            let stmt = &old.payloads[id];
            if let AmirStmt::Destroy(place) = stmt
                && place
                    .projections
                    .iter()
                    .any(|projection| matches!(projection, AmirProjection::Deref))
                && let Some(ty) = projected_place_type(place, func, type_info)
            {
                let move_state = &moved_before[block.id.as_usize()][stmt_index];
                if type_needs_drop(ty, type_info) {
                    emit_recursive_drops(place, ty, type_info, move_state, false, &mut rebuilt);
                }
            } else {
                rebuilt.push(stmt.clone());
            }
        }
        ranges.push(DenseRange::new(start, rebuilt.len() - start));
    }

    func.stmts = rebuilt;
    for (block, range) in func.blocks.iter_mut().zip(ranges) {
        block.statements = range;
    }
}

/// Returns enum variants in tag order with payload field types instantiated for
/// the concrete enum type. Built-in sum types use their language-defined tags.
fn enum_variant_payload_types(
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

#[derive(Clone)]
struct DropBlockWork {
    block: BlockId,
    params: DenseRange,
    statements: Vec<AmirStmt>,
    move_states: Vec<MoveState>,
    terminator: AmirTerminator,
}

/// Turn dynamic enum cleanup into ordinary discriminant branches plus atomic
/// `Destroy` places. Backends only read the discriminant and run named
/// destructors; they do not recurse through sum-type payloads.
fn expand_dynamic_enum_drops(func: &mut AmirFunc, type_info: &TypeInfo) {
    let moved_before = crate::move_checker::move_states_before_statements(func);
    let old_blocks = std::mem::take(&mut func.blocks);
    let old_stmts = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    if old_blocks.is_empty() {
        func.blocks = old_blocks;
        func.stmts = old_stmts;
        return;
    }

    let mut pending = std::collections::VecDeque::new();
    for block in &old_blocks {
        pending.push_back(DropBlockWork {
            block: block.id,
            params: block.params,
            statements: old_stmts
                .payloads
                .raw
                .get(block.statements.as_range())
                .unwrap_or(&[])
                .to_vec(),
            move_states: moved_before[block.id.as_usize()].clone(),
            terminator: block.terminator.clone(),
        });
    }
    let mut blocks: Vec<Option<AmirBasicBlock>> = vec![None; old_blocks.len()];
    let mut statements = AmirStmtTable::new();

    while let Some(work) = pending.pop_front() {
        let current_id = work.block;
        let current_params = work.params;
        let mut remaining = work.statements;
        let mut remaining_move_states = work.move_states;
        let current_term = work.terminator;
        loop {
            let split_at = remaining.iter().position(|stmt| {
                let AmirStmt::Destroy(place) = stmt else {
                    return false;
                };
                let Some(ty) = projected_place_type(place, func, type_info) else {
                    return false;
                };
                enum_variant_payload_types(&type_info.resolve_type_id(ty), type_info).is_some_and(
                    |variants| {
                        variants.iter().any(|(_, fields)| {
                            fields
                                .iter()
                                .any(|field| type_needs_drop(*field, type_info))
                        })
                    },
                )
            });
            let Some(split_at) = split_at else {
                let start = statements.len();
                for stmt in remaining {
                    statements.push(stmt);
                }
                blocks[current_id.as_usize()] = Some(AmirBasicBlock {
                    id: current_id,
                    params: current_params,
                    statements: DenseRange::new(start, statements.len() - start),
                    terminator: current_term,
                });
                break;
            };

            let Some(AmirStmt::Destroy(enum_place)) = remaining.get(split_at).cloned() else {
                continue;
            };
            let tail = remaining.split_off(split_at + 1);
            let tail_move_states = remaining_move_states.split_off(split_at + 1);
            let enum_move_state = remaining_move_states[split_at].clone();
            remaining.truncate(split_at);
            remaining_move_states.truncate(split_at);
            let enum_ty = projected_place_type(&enum_place, func, type_info)
                .map(|ty| type_info.resolve_type_id(ty))
                .unwrap_or(ArType::Error);
            let Some(variants) = enum_variant_payload_types(&enum_ty, type_info) else {
                remaining.push(AmirStmt::Destroy(enum_place));
                remaining.extend(tail);
                remaining_move_states.push(enum_move_state);
                remaining_move_states.extend(tail_move_states);
                continue;
            };

            let start = statements.len();
            for stmt in remaining.drain(..) {
                statements.push(stmt);
            }
            let int_ty = type_info.type_interner.intern(ArType::Primitive(
                crate::passes::type_checker::types::Primitive::Int,
            ));
            let enum_ty_id = type_info.type_interner.intern(enum_ty.clone());
            let discriminant_ty = type_info.type_interner.intern(ArType::Ref(enum_ty_id));
            let borrowed_temp = TempId::from_usize(func.temps.len());
            func.temps.push(AmirTemp {
                id: borrowed_temp,
                ty: discriminant_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            });
            statements.push(AmirStmt::Assign {
                lhs: borrowed_temp,
                rhs: AmirRvalue::Borrow(enum_place.clone()),
            });
            let tag_temp = TempId::from_usize(func.temps.len());
            func.temps.push(AmirTemp {
                id: tag_temp,
                ty: int_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            });
            statements.push(AmirStmt::Assign {
                lhs: tag_temp,
                rhs: AmirRvalue::Discriminant {
                    value: AmirOperand::Copy(borrowed_temp),
                },
            });

            let continuation = BlockId::from_usize(blocks.len());
            blocks.push(None);
            pending.push_back(DropBlockWork {
                block: continuation,
                params: DenseRange::empty(),
                statements: tail,
                move_states: tail_move_states,
                terminator: current_term,
            });
            let mut targets = Vec::with_capacity(variants.len());
            for (tag, fields) in variants {
                let branch = BlockId::from_usize(blocks.len());
                blocks.push(None);
                let tuple_ty = if fields.len() > 1 {
                    let field_ids: Vec<_> = fields.clone();
                    Some(
                        type_info
                            .type_interner
                            .intern(ArType::tuple(&field_ids, &type_info.type_interner)),
                    )
                } else {
                    None
                };
                let mut payload_drops = AmirStmtTable::new();
                for (index, field_ty) in fields.into_iter().enumerate().rev() {
                    if !type_needs_drop(field_ty, type_info) {
                        continue;
                    }
                    let mut payload_place = enum_place.clone();
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
                        &enum_move_state,
                        false,
                        &mut payload_drops,
                    );
                }
                let branch_move_states = vec![enum_move_state.clone(); payload_drops.len()];
                pending.push_back(DropBlockWork {
                    block: branch,
                    params: DenseRange::empty(),
                    statements: payload_drops.payloads.raw,
                    move_states: branch_move_states,
                    terminator: AmirTerminator::Goto {
                        target: continuation,
                        args: Vec::new(),
                    },
                });
                targets.push((tag as i128, branch, Vec::new()));
            }
            blocks[current_id.as_usize()] = Some(AmirBasicBlock {
                id: current_id,
                params: current_params,
                statements: DenseRange::new(start, statements.len() - start),
                terminator: AmirTerminator::SwitchInt {
                    discriminant: AmirOperand::Copy(tag_temp),
                    targets,
                    otherwise: (continuation, Vec::new()),
                },
            });
            // Continuation statements are owned by the pending queue.
            break;
        }
    }

    func.blocks = blocks
        .into_iter()
        .enumerate()
        .map(|(index, block)| {
            block.unwrap_or(AmirBasicBlock {
                id: BlockId::from_usize(index),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Unreachable,
            })
        })
        .collect();
    func.stmts = statements;
    func.cfg = crate::cfg::compute_cfg_edges(&func.blocks);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SymbolId;
    use crate::passes::type_checker::types::Primitive;

    #[test]
    fn test_type_needs_drop_containers() {
        let mut type_info = TypeInfo::new();
        let sym_destructible = SymbolId::new(0, 42);
        let destructible_ty_id = type_info.type_interner.intern(ArType::named(
            sym_destructible,
            &[],
            &type_info.type_interner,
        ));
        let destructor_fn = SymbolId::new(0, 99);
        type_info
            .destructor_instances
            .insert(destructible_ty_id, destructor_fn);

        let int_ty_id = type_info
            .type_interner
            .intern(ArType::Primitive(Primitive::Int));

        assert!(type_needs_drop(destructible_ty_id, &type_info));
        assert!(!type_needs_drop(int_ty_id, &type_info));

        // Array
        let arr_empty = type_info
            .type_interner
            .intern(ArType::Array(0, destructible_ty_id));
        assert!(!type_needs_drop(arr_empty, &type_info));

        let arr_active = type_info
            .type_interner
            .intern(ArType::Array(3, destructible_ty_id));
        assert!(type_needs_drop(arr_active, &type_info));

        let arr_int = type_info.type_interner.intern(ArType::Array(3, int_ty_id));
        assert!(!type_needs_drop(arr_int, &type_info));

        // ConstArray
        let const_arr = type_info
            .type_interner
            .intern(ArType::ConstArray(SymbolId::new(0, 10), destructible_ty_id));
        assert!(type_needs_drop(const_arr, &type_info));

        // Option
        let opt_res = type_info
            .type_interner
            .intern(ArType::Option(destructible_ty_id));
        assert!(type_needs_drop(opt_res, &type_info));
        let opt_int = type_info.type_interner.intern(ArType::Option(int_ty_id));
        assert!(!type_needs_drop(opt_int, &type_info));

        // Result
        let res_ok = type_info
            .type_interner
            .intern(ArType::Result(destructible_ty_id, int_ty_id));
        assert!(type_needs_drop(res_ok, &type_info));
        let res_err = type_info
            .type_interner
            .intern(ArType::Result(int_ty_id, destructible_ty_id));
        assert!(type_needs_drop(res_err, &type_info));
        let res_int = type_info
            .type_interner
            .intern(ArType::Result(int_ty_id, int_ty_id));
        assert!(!type_needs_drop(res_int, &type_info));

        // Tuple
        let tup_res = type_info.type_interner.intern(ArType::tuple(
            &[int_ty_id, destructible_ty_id],
            &type_info.type_interner,
        ));
        assert!(type_needs_drop(tup_res, &type_info));

        let tup_int = type_info.type_interner.intern(ArType::tuple(
            &[int_ty_id, int_ty_id],
            &type_info.type_interner,
        ));
        assert!(!type_needs_drop(tup_int, &type_info));
    }

    #[test]
    fn test_emit_recursive_drops_for_containers() {
        let mut type_info = TypeInfo::new();
        let sym_destructible = SymbolId::new(0, 42);
        let destructible_ty_id = type_info.type_interner.intern(ArType::named(
            sym_destructible,
            &[],
            &type_info.type_interner,
        ));
        let destructor_fn = SymbolId::new(0, 99);
        type_info
            .destructor_instances
            .insert(destructible_ty_id, destructor_fn);

        let arr_ty = type_info
            .type_interner
            .intern(ArType::Array(3, destructible_ty_id));

        let place = AmirPlace {
            local: LocalId::from_usize(0),
            projections: SmallVec::new(),
        };
        let move_state = MoveState::default();
        let mut rebuilt = AmirStmtTable::new();

        emit_recursive_drops(&place, arr_ty, &type_info, &move_state, false, &mut rebuilt);

        let destroyed_indices: Vec<_> = rebuilt
            .payloads
            .iter()
            .filter_map(|stmt| match stmt {
                AmirStmt::Destroy(place) => match place.projections.last() {
                    Some(AmirProjection::IndexConstant(index)) => Some(*index),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(destroyed_indices, [2, 1, 0]);
    }

    #[test]
    fn tuple_drop_glue_is_expanded_in_reverse_field_order() {
        let mut type_info = TypeInfo::new();
        let destructible = SymbolId::new(0, 42);
        let destructible_ty = type_info.type_interner.intern(ArType::named(
            destructible,
            &[],
            &type_info.type_interner,
        ));
        type_info
            .destructor_instances
            .insert(destructible_ty, SymbolId::new(0, 99));
        let int_ty = type_info
            .type_interner
            .intern(ArType::Primitive(Primitive::Int));
        let tuple_ty = type_info.type_interner.intern(ArType::tuple(
            &[destructible_ty, int_ty, destructible_ty],
            &type_info.type_interner,
        ));
        let place = AmirPlace {
            local: LocalId::from_usize(0),
            projections: SmallVec::new(),
        };
        let mut rebuilt = AmirStmtTable::new();

        emit_recursive_drops(
            &place,
            tuple_ty,
            &type_info,
            &MoveState::default(),
            false,
            &mut rebuilt,
        );

        let destroyed_fields = rebuilt
            .payloads
            .iter()
            .filter_map(|stmt| match stmt {
                AmirStmt::Destroy(place) => place.projections.last().and_then(|projection| {
                    if let AmirProjection::TupleField(index) = projection {
                        Some(*index)
                    } else {
                        None
                    }
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(destroyed_fields, [2, 0]);
    }
}
