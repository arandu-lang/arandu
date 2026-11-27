//! Dual-Locus Semantic Memory Model (RFC 0043).
//!
//! Decouples a variable's **Storage Locus (`\lambda_store`)** from its
//! **Reference Locus (`\lambda_ref`)**:
//! - When OSSA escape analysis proves that a borrow of a local `L` escapes via
//!   an aggregate carrier (`StructLiteral`, `Tuple`, `Array`, `EnumConstruct` —
//!   including when that aggregate carrier is returned from the function) or via
//!   an indirect store (`set holder.value = &value`), `L`'s storage is promoted
//!   from the stack frame to the thread-local linear `TaskArena`
//!   ([`AmirRvalue::ArenaAlloc`]).
//! - `L`'s reference representation remains a direct native pointer (`*T`,
//!   `\lambda_ref = DirectPtr`), eliminating the representation mismatch
//!   between raw pointers and generational handles.
//! - When a promoted local `L` and all local carriers holding its address are
//!   strictly confined to a single loop basic block `B` (`B != bb0`) that ends
//!   `L`'s scope via `StorageDead(L)`, the pass brackets `L`'s allocation with
//!   [`AmirRvalue::ArenaCheckpoint`] and [`AmirStmt::ArenaRewind`] so the
//!   `TaskArena` bump cursor is rewound in $O(1)$ at the end of every iteration.

use crate::Span;
use crate::amir::{
    AmirFunc, AmirLocal, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt, AmirTemp,
    AmirTerminator, BlockId, Dominators, LocalId, TempId,
};
use crate::escape_analysis::{
    EscapeCheckOptions, EscapeEvent, EscapeKind, find_escapes, find_escapes_with_type_info,
    type_is_borrow_carrier,
};
use crate::layout::{
    DataLayout, DenseRange, EnumPayloadShape, LayoutEngine, StructFields, StructLayoutProvider,
};
use crate::types::{ArType, Primitive, TypeId, TypeInterner};
use arandu_middle::SymbolId;
use arandu_middle::type_context::AmirTypeContext;
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::smallvec;

/// Maximum payload size (in bytes) eligible for implicit `TaskArena` promotion (RFC 0043 §3.1).
pub const MAX_DUAL_LOCUS_PROMOTION_BYTES: u64 = 1024;

struct EmptyLayoutProvider;

impl StructLayoutProvider for EmptyLayoutProvider {
    fn get_struct_fields(&self, _struct_id: SymbolId) -> Option<&StructFields> {
        None
    }

    fn get_generic_params(&self, _struct_id: SymbolId) -> Option<&[SymbolId]> {
        None
    }

    fn get_enum_variants(&self, _enum_id: SymbolId) -> Option<Vec<EnumPayloadShape>> {
        None
    }
}

/// Returns `true` if `func`'s return type is an aggregate carrier that contains
/// an explicit reference (`ref T` / `mut ref T`), such as `struct Par { x: ref int, y: ref int }`,
/// `(ref int, ref int)`, or `[2]ref int`, as opposed to a bare reference or slice.
#[must_use]
pub fn return_is_dual_locus_carrier(
    func: &AmirFunc,
    interner: &TypeInterner,
    type_info: Option<&dyn AmirTypeContext>,
) -> bool {
    type_is_borrow_carrier(func.return_type, interner, type_info)
}

/// Returns `true` if `local` in `func` has already been promoted to `TaskArena`
/// by an earlier invocation of [`apply_dual_locus_promotion`].
#[must_use]
pub fn is_dual_locus_promoted(func: &AmirFunc, local: LocalId) -> bool {
    let mut arena_temps = FxHashSet::default();
    for block in &func.blocks {
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Assign {
                lhs,
                rhs: AmirRvalue::ArenaAlloc { .. },
            } = stmt
            {
                arena_temps.insert(*lhs);
            }
        }
    }
    if arena_temps.is_empty() {
        return false;
    }
    for block in &func.blocks {
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Store {
                lhs,
                rhs: AmirOperand::Copy(t) | AmirOperand::Move(t),
            } = stmt
                && lhs.local == local
                && lhs.projections.is_empty()
                && arena_temps.contains(t)
            {
                return true;
            }
        }
    }
    false
}

/// Checks whether `local` is eligible for Dual-Locus `TaskArena` storage promotion
/// (RFC 0043 §3.1) and returns `(size, align, payload_ty)` when eligible.
#[must_use]
pub fn dual_locus_layout(
    func: &AmirFunc,
    local: LocalId,
    interner: &TypeInterner,
    type_info: Option<&dyn AmirTypeContext>,
    data_layout: DataLayout,
) -> Option<(u32, u32, TypeId)> {
    // Formal parameters belong to the caller's frame and cannot be reallocated
    // inside the callee's TaskArena.
    if local.as_usize() < func.params.len() {
        return None;
    }
    // Coroutine state machines manage their own frame across suspension points.
    if func
        .blocks
        .iter()
        .any(|block| matches!(block.terminator, AmirTerminator::Suspend { .. }))
    {
        return None;
    }
    let local_info = func.locals.get(local.as_usize())?;
    let payload_ty = local_info.ty;
    match interner.resolve(payload_ty) {
        ArType::Void
        | ArType::Err
        | ArType::Error
        | ArType::Coroutine(_)
        | ArType::Primitive(Primitive::Str | Primitive::Any) => return None,
        _ => {}
    }

    // Never promote a local that has an explicit destructor or `Destroy` obligation.
    if let Some(ti) = type_info
        && crate::drop_elaborate::type_needs_drop(payload_ty, ti)
    {
        return None;
    }
    for block in &func.blocks {
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Destroy(place) = stmt
                && place.local == local
            {
                return None;
            }
        }
    }

    let engine = LayoutEngine::from_data_layout(data_layout);
    let empty_provider = EmptyLayoutProvider;
    let provider: &dyn StructLayoutProvider = match type_info {
        Some(ti) => ti,
        None => &empty_provider,
    };
    let layout = engine.layout_of(payload_ty, interner, provider).ok()?;
    if layout.size > MAX_DUAL_LOCUS_PROMOTION_BYTES
        || layout.align == 0
        || !layout.align.is_power_of_two()
    {
        return None;
    }
    let size = u32::try_from(layout.size.max(1)).ok()?;
    let align = u32::try_from(layout.align.max(1)).ok()?;
    Some((size, align, payload_ty))
}

/// Returns `true` if `local` can be (or already was) promoted to `TaskArena` under RFC 0043.
#[must_use]
pub fn is_dual_locus_promotable(
    func: &AmirFunc,
    local: LocalId,
    interner: &TypeInterner,
    type_info: Option<&dyn AmirTypeContext>,
    data_layout: DataLayout,
) -> bool {
    is_dual_locus_promoted(func, local)
        || dual_locus_layout(func, local, interner, type_info, data_layout).is_some()
}

/// Promotes all Dual-Locus-eligible escaping locals in `func` to `TaskArena`.
///
/// Returns the set of [`LocalId`]s that were promoted.
pub fn apply_dual_locus_promotion(
    func: &mut AmirFunc,
    type_info: &dyn AmirTypeContext,
    data_layout: DataLayout,
    opts: &EscapeCheckOptions,
) -> FxHashSet<LocalId> {
    apply_dual_locus_promotion_impl(
        func,
        type_info.type_interner(),
        Some(type_info),
        data_layout,
        opts,
    )
}

/// Standalone helper for unit tests that only have a [`TypeInterner`].
pub fn apply_dual_locus_promotion_with_interner(
    func: &mut AmirFunc,
    interner: &TypeInterner,
    data_layout: DataLayout,
    opts: &EscapeCheckOptions,
) -> FxHashSet<LocalId> {
    apply_dual_locus_promotion_impl(func, interner, None, data_layout, opts)
}

fn apply_dual_locus_promotion_impl(
    func: &mut AmirFunc,
    interner: &TypeInterner,
    type_info: Option<&dyn AmirTypeContext>,
    data_layout: DataLayout,
    opts: &EscapeCheckOptions,
) -> FxHashSet<LocalId> {
    if opts.effective_no_fallback() || func.blocks.is_empty() {
        return FxHashSet::default();
    }

    let events = type_info.map_or_else(
        || find_escapes(func, interner),
        |ti| find_escapes_with_type_info(func, ti),
    );
    if events.is_empty() {
        return FxHashSet::default();
    }

    let return_carrier = return_is_dual_locus_carrier(func, interner, type_info);

    // If any escape in `func` is a hard error (e.g. bare reference return `O010`),
    // do not rewrite the function.
    for ev in &events {
        if ev.kind == EscapeKind::Return && !return_carrier {
            return FxHashSet::default();
        }
    }

    let mut promoted_layouts: FxHashMap<LocalId, (u32, u32, TypeId)> = FxHashMap::default();
    for ev in &events {
        let eligible_kind = match ev.kind {
            EscapeKind::HeapStore => true,
            EscapeKind::Return => return_carrier,
        };
        if !eligible_kind || is_dual_locus_promoted(func, ev.place_local) {
            continue;
        }
        if let Some(layout) =
            dual_locus_layout(func, ev.place_local, interner, type_info, data_layout)
        {
            promoted_layouts.insert(ev.place_local, layout);
        }
    }

    if promoted_layouts.is_empty() {
        return FxHashSet::default();
    }

    let mut ordered_locals: Vec<LocalId> = promoted_layouts.keys().copied().collect();
    ordered_locals.sort_by_key(|l| l.as_usize());

    // Classify each promoted local's TaskArena placement:
    // - FunctionEntry: allocated once in bb0 (outside loops).
    // - SingleBlockRewind(B): fast path when local + carriers live strictly inside one block B.
    // - MultiBlockLoopRewind: allocated + checkpointed at StorageLive(L) in `live_block`,
    //   rewound before every `StorageDead(L)` across branched loop bodies.
    // - PerIterationAlloc(live_block): loop-scoped local whose borrow escapes the iteration;
    //   allocated per iteration at `StorageLive(L)` to prevent loop-carried aliasing.
    let doms = Dominators::new(func);
    let loops = discover_natural_loops(func, &doms);
    let mut placements: FxHashMap<LocalId, DualLocusPlacement> = FxHashMap::default();
    for &local in &ordered_locals {
        let placement = if let Some(block_id) = find_rewindable_block(func, local, &events) {
            DualLocusPlacement::SingleBlockRewind(block_id)
        } else if let Some((live_block, dead_blocks)) =
            find_rewindable_loop_region(func, local, &events, &doms, &loops, interner, type_info)
        {
            DualLocusPlacement::MultiBlockLoopRewind {
                live_block,
                dead_blocks,
            }
        } else if let Some(live_block) = find_loop_storage_live_block(func, local, &loops) {
            DualLocusPlacement::PerIterationAlloc(live_block)
        } else {
            DualLocusPlacement::FunctionEntry
        };
        placements.insert(local, placement);
    }

    // 1. Rewrite every existing AmirPlace referencing a promoted local `L` by
    //    prepending `AmirProjection::Deref` (`s0` -> `(*s0)`).
    let stmt_ids: Vec<_> = func.stmts.iter_ids().collect();
    for sid in stmt_ids {
        let Some(stmt) = func.stmts.get_mut(sid) else {
            continue;
        };
        match stmt {
            AmirStmt::Store { lhs, .. } | AmirStmt::Destroy(lhs) => {
                if promoted_layouts.contains_key(&lhs.local) {
                    lhs.projections.insert(0, AmirProjection::Deref);
                }
            }
            AmirStmt::Assign {
                rhs:
                    AmirRvalue::Load(place) | AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
                ..
            } if promoted_layouts.contains_key(&place.local) => {
                place.projections.insert(0, AmirProjection::Deref);
            }
            _ => {}
        }
    }

    // 2. Update the local's type to `ArType::Ptr(payload_ty)` while keeping
    //    `is_memory = true` because the promoted pointer slot is initialized
    //    and accessed via `Store`/`Load`/`Deref` places.
    for &local in &ordered_locals {
        let (_, _, payload_ty) = promoted_layouts[&local];
        let ptr_ty = interner.intern(ArType::Ptr(payload_ty));
        if let Some(local_info) = func.locals.get_mut(local.as_usize()) {
            local_info.ty = ptr_ty;
            local_info.is_memory = true;
        }
    }

    // 2.5 Allocate persistent checkpoint slots for multi-block loop rewinds.
    let i64_ty = interner.intern(ArType::Primitive(Primitive::I64));
    let mut mark_locals: FxHashMap<LocalId, LocalId> = FxHashMap::default();
    for &local in &ordered_locals {
        if matches!(
            placements.get(&local),
            Some(DualLocusPlacement::MultiBlockLoopRewind { .. })
        ) {
            let mark_id = LocalId::from_usize(func.locals.len());
            let span = local_span(func, local);
            func.locals.push(AmirLocal {
                id: mark_id,
                ty: i64_ty,
                is_memory: true,
                symbol: None,
                span,
                use_span: None,
            });
            mark_locals.insert(local, mark_id);
        }
    }

    // 3. Insert `ArenaAlloc` (and `ArenaCheckpoint`/`ArenaRewind` for loop-scoped
    //    locals) into the basic blocks.
    for bi in 0..func.blocks.len() {
        let bid = func.blocks[bi].id;
        let old_ids: Vec<_> = func.block_stmts_ids_or_empty(bid);
        let mut new_stmts = Vec::with_capacity(old_ids.len() + ordered_locals.len() * 3);

        if bi == 0 {
            for &local in &ordered_locals {
                if !matches!(
                    placements.get(&local),
                    Some(DualLocusPlacement::FunctionEntry)
                ) {
                    continue;
                }
                let (size, align, payload_ty) = promoted_layouts[&local];
                let ptr_ty = interner.intern(ArType::Ptr(payload_ty));
                let span = local_span(func, local);
                let t_alloc = alloc_temp(func, ptr_ty, span);
                new_stmts.push(AmirStmt::Assign {
                    lhs: t_alloc,
                    rhs: AmirRvalue::ArenaAlloc {
                        size,
                        align,
                        payload_ty,
                    },
                });
                new_stmts.push(AmirStmt::Store {
                    lhs: AmirPlace {
                        local,
                        projections: smallvec![],
                    },
                    rhs: AmirOperand::Copy(t_alloc),
                });
            }
        }

        // Single-block fast path locals assigned to this block `bid`.
        let block_single_locals: Vec<LocalId> = ordered_locals
            .iter()
            .copied()
            .filter(|l| matches!(placements.get(l), Some(DualLocusPlacement::SingleBlockRewind(b)) if *b == bid))
            .collect();
        let mut active_marks: FxHashMap<LocalId, TempId> = FxHashMap::default();

        for sid in old_ids {
            let Some(stmt) = func.stmts.get(sid).cloned() else {
                continue;
            };
            for &local in &block_single_locals {
                if !active_marks.contains_key(&local) && stmt_mentions_local(&stmt, local) {
                    let (size, align, payload_ty) = promoted_layouts[&local];
                    let ptr_ty = interner.intern(ArType::Ptr(payload_ty));
                    let span = local_span(func, local);
                    let t_mark = alloc_temp(func, i64_ty, span);
                    let t_alloc = alloc_temp(func, ptr_ty, span);
                    new_stmts.push(AmirStmt::Assign {
                        lhs: t_mark,
                        rhs: AmirRvalue::ArenaCheckpoint,
                    });
                    new_stmts.push(AmirStmt::Assign {
                        lhs: t_alloc,
                        rhs: AmirRvalue::ArenaAlloc {
                            size,
                            align,
                            payload_ty,
                        },
                    });
                    new_stmts.push(AmirStmt::Store {
                        lhs: AmirPlace {
                            local,
                            projections: smallvec![],
                        },
                        rhs: AmirOperand::Copy(t_alloc),
                    });
                    active_marks.insert(local, t_mark);
                }
            }
            if let AmirStmt::StorageDead(dead_local) = &stmt {
                if let Some(&t_mark) = active_marks.get(dead_local) {
                    new_stmts.push(AmirStmt::ArenaRewind(AmirOperand::Copy(t_mark)));
                } else if let Some(DualLocusPlacement::MultiBlockLoopRewind { dead_blocks, .. }) =
                    placements.get(dead_local)
                    && dead_blocks.contains(&bid)
                    && let Some(&mark_local) = mark_locals.get(dead_local)
                {
                    let span = local_span(func, *dead_local);
                    let t_load = alloc_temp(func, i64_ty, span);
                    new_stmts.push(AmirStmt::Assign {
                        lhs: t_load,
                        rhs: AmirRvalue::Load(AmirPlace {
                            local: mark_local,
                            projections: smallvec![],
                        }),
                    });
                    new_stmts.push(AmirStmt::ArenaRewind(AmirOperand::Copy(t_load)));
                }
            }
            let live_local_init = match &stmt {
                AmirStmt::StorageLive(live_local) => match placements.get(live_local) {
                    Some(DualLocusPlacement::MultiBlockLoopRewind { live_block, .. })
                        if *live_block == bid =>
                    {
                        Some((*live_local, true))
                    }
                    Some(DualLocusPlacement::PerIterationAlloc(live_block))
                        if *live_block == bid =>
                    {
                        Some((*live_local, false))
                    }
                    _ => None,
                },
                _ => None,
            };
            new_stmts.push(stmt);
            if let Some((live_local, with_checkpoint)) = live_local_init {
                let (size, align, payload_ty) = promoted_layouts[&live_local];
                let ptr_ty = interner.intern(ArType::Ptr(payload_ty));
                let span = local_span(func, live_local);
                if with_checkpoint && let Some(&mark_local) = mark_locals.get(&live_local) {
                    let t_mark = alloc_temp(func, i64_ty, span);
                    new_stmts.push(AmirStmt::Assign {
                        lhs: t_mark,
                        rhs: AmirRvalue::ArenaCheckpoint,
                    });
                    new_stmts.push(AmirStmt::Store {
                        lhs: AmirPlace {
                            local: mark_local,
                            projections: smallvec![],
                        },
                        rhs: AmirOperand::Copy(t_mark),
                    });
                }
                let t_alloc = alloc_temp(func, ptr_ty, span);
                new_stmts.push(AmirStmt::Assign {
                    lhs: t_alloc,
                    rhs: AmirRvalue::ArenaAlloc {
                        size,
                        align,
                        payload_ty,
                    },
                });
                new_stmts.push(AmirStmt::Store {
                    lhs: AmirPlace {
                        local: live_local,
                        projections: smallvec![],
                    },
                    rhs: AmirOperand::Copy(t_alloc),
                });
            }
        }

        let start = func.stmts.len();
        for s in new_stmts {
            func.stmts.push(s);
        }
        let len = func.stmts.len() - start;
        func.blocks[bi].statements = DenseRange::new(start, len);
    }

    promoted_layouts.into_keys().collect()
}

enum DualLocusPlacement {
    FunctionEntry,
    SingleBlockRewind(BlockId),
    MultiBlockLoopRewind {
        live_block: BlockId,
        dead_blocks: FxHashSet<BlockId>,
    },
    PerIterationAlloc(BlockId),
}

struct NaturalLoop {
    header: BlockId,
    body: FxHashSet<BlockId>,
}

fn discover_natural_loops(func: &AmirFunc, doms: &Dominators) -> Vec<NaturalLoop> {
    let mut headers: Vec<BlockId> = Vec::new();
    let mut back_edges: Vec<(BlockId, BlockId)> = Vec::new();
    for block in &func.blocks {
        let bid = block.id;
        for &succ in func.successors(bid) {
            if doms.dominates(succ, bid) {
                back_edges.push((bid, succ));
                if !headers.contains(&succ) {
                    headers.push(succ);
                }
            }
        }
    }
    let mut loops = Vec::with_capacity(headers.len());
    for header in headers {
        let mut body = FxHashSet::default();
        body.insert(header);
        let mut worklist: Vec<BlockId> = back_edges
            .iter()
            .filter(|&&(_, target)| target == header)
            .map(|&(src, _)| src)
            .filter(|src| body.insert(*src))
            .collect();
        while let Some(block) = worklist.pop() {
            for &pred in func.predecessors(block) {
                if pred != header && body.insert(pred) {
                    worklist.push(pred);
                }
            }
        }
        loops.push(NaturalLoop { header, body });
    }
    loops
}

trait BlockStmtIdsExt {
    fn block_stmts_ids_or_empty(&self, block: BlockId) -> Vec<crate::amir::InstrId>;
}

impl BlockStmtIdsExt for AmirFunc {
    fn block_stmts_ids_or_empty(&self, block: BlockId) -> Vec<crate::amir::InstrId> {
        self.block_stmt_ids(block).collect()
    }
}

fn innermost_loop_containing(loops: &[NaturalLoop], block: BlockId) -> Option<&NaturalLoop> {
    loops
        .iter()
        .filter(|lp| lp.body.contains(&block))
        .min_by_key(|lp| lp.body.len())
}

fn find_loop_storage_live_block(
    func: &AmirFunc,
    local: LocalId,
    loops: &[NaturalLoop],
) -> Option<BlockId> {
    let mut live_block: Option<BlockId> = None;
    for block in &func.blocks {
        for stmt in func.block_stmts(block.id) {
            if matches!(stmt, AmirStmt::StorageLive(l) if *l == local) {
                match live_block {
                    None => live_block = Some(block.id),
                    Some(prev) if prev != block.id => return None,
                    Some(_) => {}
                }
            }
        }
    }
    let bid = live_block?;
    if bid == BlockId::from_usize(0) {
        return None;
    }
    innermost_loop_containing(loops, bid).map(|_| bid)
}

#[allow(clippy::too_many_arguments)]
fn find_rewindable_loop_region(
    func: &AmirFunc,
    local: LocalId,
    events: &[EscapeEvent],
    doms: &Dominators,
    loops: &[NaturalLoop],
    interner: &TypeInterner,
    type_info: Option<&dyn AmirTypeContext>,
) -> Option<(BlockId, FxHashSet<BlockId>)> {
    if events
        .iter()
        .any(|ev| ev.place_local == local && ev.kind == EscapeKind::Return)
    {
        return None;
    }

    let live_block = find_loop_storage_live_block(func, local, loops)?;
    let lp = innermost_loop_containing(loops, live_block)?;

    let mut dead_blocks = FxHashSet::default();
    for block in &func.blocks {
        for stmt in func.block_stmts(block.id) {
            if matches!(stmt, AmirStmt::StorageDead(l) if *l == local) {
                dead_blocks.insert(block.id);
            }
        }
    }
    if dead_blocks.is_empty() {
        return None;
    }
    if dead_blocks
        .iter()
        .any(|&b| !lp.body.contains(&b) || !doms.dominates(live_block, b))
    {
        return None;
    }

    // Reject if `local` is mentioned anywhere before `StorageLive(local)` in
    // `live_block`, after `StorageDead(local)` in a dead block, or outside
    // `(lp.body & dominated by live_block)`.
    for block in &func.blocks {
        let mut saw_live = false;
        let mut saw_dead = false;
        for stmt in func.block_stmts(block.id) {
            if matches!(stmt, AmirStmt::StorageLive(l) if *l == local) {
                saw_live = true;
                continue;
            }
            if matches!(stmt, AmirStmt::StorageDead(l) if *l == local) {
                saw_dead = true;
                continue;
            }
            if !stmt_mentions_local(stmt, local) {
                continue;
            }
            if !lp.body.contains(&block.id) || !doms.dominates(live_block, block.id) {
                return None;
            }
            if block.id == live_block && !saw_live {
                return None;
            }
            if saw_dead {
                return None;
            }
        }
    }

    // Trace all paths starting from `live_block` up to `dead_blocks`.
    // Every path must reach `StorageDead(local)` before exiting `lp.body`,
    // reaching `Return`/`Unreachable`, or taking a back-edge to `lp.header`.
    let mut region_blocks = FxHashSet::default();
    let mut worklist = vec![live_block];
    while let Some(bid) = worklist.pop() {
        if !region_blocks.insert(bid) {
            continue;
        }
        if !lp.body.contains(&bid) || !doms.dominates(live_block, bid) {
            return None;
        }
        if dead_blocks.contains(&bid) {
            continue;
        }
        let succs = func.successors(bid);
        if succs.is_empty() {
            return None;
        }
        for &succ in succs {
            if succ == lp.header || !lp.body.contains(&succ) || !doms.dominates(live_block, succ) {
                return None;
            }
            worklist.push(succ);
        }
    }

    // Check indirect stores and borrow-carrying local variables inside `region_blocks`.
    let mut carrier_locals = FxHashSet::default();
    for &bid in &region_blocks {
        for stmt in func.block_stmts(bid) {
            if let AmirStmt::Store { lhs, .. } = stmt {
                if lhs
                    .projections
                    .iter()
                    .any(|p| matches!(p, AmirProjection::Deref))
                {
                    return None;
                }
                if lhs.local != local
                    && let Some(local_info) = func.locals.get(lhs.local.as_usize())
                    && (crate::borrow_facts::type_may_carry_borrow(local_info.ty, interner)
                        || type_is_borrow_carrier(local_info.ty, interner, type_info))
                {
                    carrier_locals.insert(lhs.local);
                }
            }
        }
    }

    // Any borrow-carrying local written inside the lifetime region must stay
    // confined to `region_blocks` (and never read after `StorageDead(local)`).
    if !carrier_locals.is_empty() {
        for block in &func.blocks {
            let mut saw_dead = false;
            for stmt in func.block_stmts(block.id) {
                if matches!(stmt, AmirStmt::StorageDead(l) if *l == local) {
                    saw_dead = true;
                }
                for &carrier in &carrier_locals {
                    if !stmt_mentions_local(stmt, carrier) {
                        continue;
                    }
                    if !region_blocks.contains(&block.id) {
                        return None;
                    }
                    let is_carrier_dead = matches!(stmt, AmirStmt::StorageDead(c) if *c == carrier);
                    if saw_dead && !is_carrier_dead {
                        return None;
                    }
                }
            }
        }
    }

    Some((live_block, dead_blocks))
}

/// Determines whether `local` and all local carriers receiving `&local` are
/// strictly confined to a single non-entry block `B` (`B != bb0`) that ends
/// `local`'s lifetime with `StorageDead(local)`.
fn find_rewindable_block(
    func: &AmirFunc,
    local: LocalId,
    events: &[EscapeEvent],
) -> Option<BlockId> {
    // Must not escape via function return.
    if events
        .iter()
        .any(|ev| ev.place_local == local && ev.kind == EscapeKind::Return)
    {
        return None;
    }

    let mut defining_block: Option<BlockId> = None;
    for block in &func.blocks {
        for stmt in func.block_stmts(block.id) {
            let is_storage_boundary = matches!(
                stmt,
                AmirStmt::StorageLive(l) | AmirStmt::StorageDead(l) if *l == local
            );
            if stmt_mentions_local(stmt, local) && !is_storage_boundary {
                match defining_block {
                    None => defining_block = Some(block.id),
                    Some(prev) if prev == block.id => {}
                    Some(_) => return None,
                }
            }
        }
    }
    let block_id = defining_block?;
    if block_id == BlockId::from_usize(0) {
        return None;
    }

    // Must have `StorageDead(local)` in `block_id`, and no mention of `local` after `StorageDead(local)`.
    let mut saw_storage_dead = false;
    for stmt in func.block_stmts(block_id) {
        if matches!(stmt, AmirStmt::StorageDead(l) if *l == local) {
            saw_storage_dead = true;
        } else if saw_storage_dead && stmt_mentions_local(stmt, local) {
            return None;
        }
    }
    if !saw_storage_dead {
        return None;
    }

    // Every store in `block_id` must not store through an indirect `Deref`
    // (e.g., `set holder_param.value = &local`), and any local aggregate carrier
    // written in `block_id` must itself only be used within `block_id`.
    let mut local_carriers_in_block = FxHashSet::default();
    for stmt in func.block_stmts(block_id) {
        if let AmirStmt::Store { lhs, .. } = stmt {
            if lhs
                .projections
                .iter()
                .any(|p| matches!(p, AmirProjection::Deref))
            {
                return None;
            }
            if lhs.local != local {
                local_carriers_in_block.insert(lhs.local);
            }
        }
    }
    for other_block in &func.blocks {
        if other_block.id == block_id {
            continue;
        }
        for stmt in func.block_stmts(other_block.id) {
            for &carrier in &local_carriers_in_block {
                if stmt_mentions_local(stmt, carrier) {
                    return None;
                }
            }
        }
    }

    Some(block_id)
}

fn stmt_mentions_local(stmt: &AmirStmt, local: LocalId) -> bool {
    match stmt {
        AmirStmt::Store { lhs, .. } | AmirStmt::Destroy(lhs) => lhs.local == local,
        AmirStmt::StorageLive(l) | AmirStmt::StorageDead(l) => *l == local,
        AmirStmt::Assign { rhs, .. } => match rhs {
            AmirRvalue::Load(place) | AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
                place.local == local
            }
            AmirRvalue::RelativeBorrow { local: l, .. } => *l == local,
            _ => false,
        },
        _ => false,
    }
}

fn local_span(func: &AmirFunc, local: LocalId) -> Span {
    func.locals
        .get(local.as_usize())
        .map_or_else(|| Span::new(0, 0, 0), |l| l.span)
}

fn alloc_temp(func: &mut AmirFunc, ty: TypeId, span: Span) -> TempId {
    let id = TempId::from_usize(func.temps.len());
    func.temps.push(AmirTemp {
        id,
        ty,
        is_copy: true,
        is_nullable: false,
        span,
    });
    id
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::amir::{AmirBasicBlock, AmirLocal, AmirStmtTable};
    use crate::cfg::compute_cfg_edges;

    #[test]
    fn dual_locus_promotes_multiple_locals_in_returned_struct_carrier() {
        let interner = TypeInterner::new();
        let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int_ty = interner.intern(ArType::Ref(int_ty));
        let par_sym = SymbolId::new(0, 1);
        let par_ty = interner.intern(ArType::named(par_sym, &[], &interner));

        let mut stmts = AmirStmtTable::new();
        // _1 = &s0 (a)
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(AmirPlace {
                local: LocalId::from_usize(0),
                projections: smallvec![],
            }),
        });
        // _2 = &s1 (b)
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::Borrow(AmirPlace {
                local: LocalId::from_usize(1),
                projections: smallvec![],
            }),
        });
        // _0 = Par { x: _1, y: _2 }
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::StructLiteral {
                struct_symbol: par_sym,
                fields: vec![
                    ("x".into(), AmirOperand::Copy(TempId::from_usize(1))),
                    ("y".into(), AmirOperand::Copy(TempId::from_usize(2))),
                ],
            },
        });

        let blocks = vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::new(0, 3),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }];
        let cfg = compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: par_ty,
            receiver: None,
            params: vec![],
            locals: vec![
                AmirLocal {
                    id: LocalId::from_usize(0),
                    ty: int_ty,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 0, 1),
                    use_span: None,
                },
                AmirLocal {
                    id: LocalId::from_usize(1),
                    ty: int_ty,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 2, 3),
                    use_span: None,
                },
            ],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: par_ty,
                    is_copy: false,
                    is_nullable: false,
                    span: Span::new(0, 0, 1),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: ref_int_ty,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 0, 1),
                },
                AmirTemp {
                    id: TempId::from_usize(2),
                    ty: ref_int_ty,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 2, 3),
                },
            ],
            blocks,
            block_params: Vec::new(),
            stmts,
            cfg,
        };

        let promoted = apply_dual_locus_promotion_with_interner(
            &mut func,
            &interner,
            DataLayout::ptr_width(8),
            &EscapeCheckOptions::default(),
        );
        assert!(promoted.contains(&LocalId::from_usize(0)));
        assert!(promoted.contains(&LocalId::from_usize(1)));
        assert!(is_dual_locus_promoted(&func, LocalId::from_usize(0)));
        assert!(is_dual_locus_promoted(&func, LocalId::from_usize(1)));
    }

    #[test]
    fn dual_locus_rewinds_loop_scoped_local_with_storage_dead() {
        let interner = TypeInterner::new();
        let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int_ty = interner.intern(ArType::Ref(int_ty));
        let par_sym = SymbolId::new(0, 1);
        let par_ty = interner.intern(ArType::named(par_sym, &[], &interner));

        let mut stmts = AmirStmtTable::new();
        // bb0: goto bb1
        // bb1:
        // StorageLive(s0)
        // _1 = &s0
        // _0 = Par { x: _1, y: _1 }
        // s1 = _0
        // StorageDead(s0)
        // goto bb1
        stmts.push(AmirStmt::StorageLive(LocalId::from_usize(0)));
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(AmirPlace {
                local: LocalId::from_usize(0),
                projections: smallvec![],
            }),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::StructLiteral {
                struct_symbol: par_sym,
                fields: vec![
                    ("x".into(), AmirOperand::Copy(TempId::from_usize(1))),
                    ("y".into(), AmirOperand::Copy(TempId::from_usize(1))),
                ],
            },
        });
        stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: LocalId::from_usize(1),
                projections: smallvec![],
            },
            rhs: AmirOperand::Copy(TempId::from_usize(0)),
        });
        stmts.push(AmirStmt::StorageDead(LocalId::from_usize(0)));

        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                statements: DenseRange::new(0, 5),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: Vec::new(),
                },
            },
        ];
        let cfg = compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: int_ty,
            receiver: None,
            params: vec![],
            locals: vec![
                AmirLocal {
                    id: LocalId::from_usize(0),
                    ty: int_ty,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 0, 1),
                    use_span: None,
                },
                AmirLocal {
                    id: LocalId::from_usize(1),
                    ty: par_ty,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 2, 3),
                    use_span: None,
                },
            ],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: par_ty,
                    is_copy: false,
                    is_nullable: false,
                    span: Span::new(0, 0, 1),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: ref_int_ty,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 0, 1),
                },
            ],
            blocks,
            block_params: Vec::new(),
            stmts,
            cfg,
        };

        let promoted = apply_dual_locus_promotion_with_interner(
            &mut func,
            &interner,
            DataLayout::ptr_width(8),
            &EscapeCheckOptions::default(),
        );
        assert!(promoted.contains(&LocalId::from_usize(0)));
        assert!(is_dual_locus_promoted(&func, LocalId::from_usize(0)));

        // Verify that block 1 contains ArenaCheckpoint and ArenaRewind
        let has_checkpoint = func.block_stmts(BlockId::from_usize(1)).any(|s| {
            matches!(
                s,
                AmirStmt::Assign {
                    rhs: AmirRvalue::ArenaCheckpoint,
                    ..
                }
            )
        });
        let has_rewind = func
            .block_stmts(BlockId::from_usize(1))
            .any(|s| matches!(s, AmirStmt::ArenaRewind(_)));
        assert!(has_checkpoint, "Block 1 must contain ArenaCheckpoint");
        assert!(has_rewind, "Block 1 must contain ArenaRewind");
    }

    #[test]
    fn dual_locus_rewinds_multi_block_branched_loop_local() {
        let interner = TypeInterner::new();
        let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int_ty = interner.intern(ArType::Ref(int_ty));
        let holder_sym = SymbolId::new(0, 1);
        let holder_ty = interner.intern(ArType::named(holder_sym, &[], &interner));

        let mut stmts = AmirStmtTable::new();
        // bb0: entry -> bb1
        // bb1: loop header -> SwitchInt -> bb2 (body entry) / bb6 (exit)
        // bb2: StorageLive(s0), StorageLive(s1) -> SwitchInt -> bb3 / bb4
        // bb3: _1 = &s0; _2 = Holder { value: _1 }; s1 = _2 -> Goto bb5
        // bb4: _1 = &s0; _2 = Holder { value: _1 }; s1 = _2 -> Goto bb5
        // bb5: StorageDead(s1); StorageDead(s0) -> Goto bb1
        // bb6: Return
        let bb2_start = stmts.len();
        stmts.push(AmirStmt::StorageLive(LocalId::from_usize(0)));
        stmts.push(AmirStmt::StorageLive(LocalId::from_usize(1)));
        let bb2_len = stmts.len() - bb2_start;

        let bb3_start = stmts.len();
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(AmirPlace {
                local: LocalId::from_usize(0),
                projections: smallvec![],
            }),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::StructLiteral {
                struct_symbol: holder_sym,
                fields: vec![("value".into(), AmirOperand::Copy(TempId::from_usize(1)))],
            },
        });
        stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: LocalId::from_usize(1),
                projections: smallvec![],
            },
            rhs: AmirOperand::Copy(TempId::from_usize(2)),
        });
        let bb3_len = stmts.len() - bb3_start;

        let bb4_start = stmts.len();
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(AmirPlace {
                local: LocalId::from_usize(0),
                projections: smallvec![],
            }),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::StructLiteral {
                struct_symbol: holder_sym,
                fields: vec![("value".into(), AmirOperand::Copy(TempId::from_usize(1)))],
            },
        });
        stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: LocalId::from_usize(1),
                projections: smallvec![],
            },
            rhs: AmirOperand::Copy(TempId::from_usize(2)),
        });
        let bb4_len = stmts.len() - bb4_start;

        let bb5_start = stmts.len();
        stmts.push(AmirStmt::StorageDead(LocalId::from_usize(1)));
        stmts.push(AmirStmt::StorageDead(LocalId::from_usize(0)));
        let bb5_len = stmts.len() - bb5_start;

        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Copy(TempId::from_usize(0)),
                    if_true: BlockId::from_usize(2),
                    if_false: BlockId::from_usize(6),
                    true_args: vec![],
                    false_args: vec![],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                statements: DenseRange::new(bb2_start, bb2_len),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Copy(TempId::from_usize(0)),
                    if_true: BlockId::from_usize(3),
                    if_false: BlockId::from_usize(4),
                    true_args: vec![],
                    false_args: vec![],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                statements: DenseRange::new(bb3_start, bb3_len),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(5),
                    args: vec![],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(4),
                statements: DenseRange::new(bb4_start, bb4_len),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(5),
                    args: vec![],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(5),
                statements: DenseRange::new(bb5_start, bb5_len),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(6),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: int_ty,
            receiver: None,
            params: vec![],
            locals: vec![
                AmirLocal {
                    id: LocalId::from_usize(0),
                    ty: int_ty,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 0, 1),
                    use_span: None,
                },
                AmirLocal {
                    id: LocalId::from_usize(1),
                    ty: holder_ty,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 2, 3),
                    use_span: None,
                },
            ],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: int_ty,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 0, 1),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: ref_int_ty,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 0, 1),
                },
                AmirTemp {
                    id: TempId::from_usize(2),
                    ty: holder_ty,
                    is_copy: false,
                    is_nullable: false,
                    span: Span::new(0, 2, 3),
                },
            ],
            blocks,
            block_params: Vec::new(),
            stmts,
            cfg,
        };

        let promoted = apply_dual_locus_promotion_with_interner(
            &mut func,
            &interner,
            DataLayout::ptr_width(8),
            &EscapeCheckOptions::default(),
        );
        assert!(promoted.contains(&LocalId::from_usize(0)));
        assert!(
            !func.block_stmts(BlockId::from_usize(0)).any(|s| matches!(
                s,
                AmirStmt::Assign {
                    rhs: AmirRvalue::ArenaAlloc { .. },
                    ..
                }
            )),
            "bb0 must not allocate loop-scoped local"
        );
        assert!(func.block_stmts(BlockId::from_usize(2)).any(|s| matches!(
            s,
            AmirStmt::Assign {
                rhs: AmirRvalue::ArenaCheckpoint,
                ..
            }
        )));
        assert!(func.block_stmts(BlockId::from_usize(2)).any(|s| matches!(
            s,
            AmirStmt::Assign {
                rhs: AmirRvalue::ArenaAlloc { .. },
                ..
            }
        )));
        assert!(
            func.block_stmts(BlockId::from_usize(5))
                .any(|s| matches!(s, AmirStmt::ArenaRewind(_))),
            "merge block bb5 must contain ArenaRewind"
        );
    }

    #[test]
    fn dual_locus_allocates_per_iteration_when_loop_borrow_escapes_outer_carrier() {
        let interner = TypeInterner::new();
        let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int_ty = interner.intern(ArType::Ref(int_ty));
        let holder_sym = SymbolId::new(0, 1);
        let holder_ty = interner.intern(ArType::named(holder_sym, &[], &interner));

        let mut stmts = AmirStmtTable::new();
        // s1 (outer carrier) declared in bb0; loop body bb2 allocates s0 via
        // StorageLive(s0) and overwrites s1 outside bb2's StorageDead scope.
        let bb2_start = stmts.len();
        stmts.push(AmirStmt::StorageLive(LocalId::from_usize(0)));
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(AmirPlace {
                local: LocalId::from_usize(0),
                projections: smallvec![],
            }),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::StructLiteral {
                struct_symbol: holder_sym,
                fields: vec![("value".into(), AmirOperand::Copy(TempId::from_usize(1)))],
            },
        });
        stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: LocalId::from_usize(1),
                projections: smallvec![],
            },
            rhs: AmirOperand::Copy(TempId::from_usize(2)),
        });
        stmts.push(AmirStmt::StorageDead(LocalId::from_usize(0)));
        let bb2_len = stmts.len() - bb2_start;

        let bb3_start = stmts.len();
        stmts.push(AmirStmt::StorageDead(LocalId::from_usize(1)));
        let bb3_len = stmts.len() - bb3_start;

        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Copy(TempId::from_usize(0)),
                    if_true: BlockId::from_usize(2),
                    if_false: BlockId::from_usize(3),
                    true_args: vec![],
                    false_args: vec![],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                statements: DenseRange::new(bb2_start, bb2_len),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                statements: DenseRange::new(bb3_start, bb3_len),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: int_ty,
            receiver: None,
            params: vec![],
            locals: vec![
                AmirLocal {
                    id: LocalId::from_usize(0),
                    ty: int_ty,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 0, 1),
                    use_span: None,
                },
                AmirLocal {
                    id: LocalId::from_usize(1),
                    ty: holder_ty,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 2, 3),
                    use_span: None,
                },
            ],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: int_ty,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 0, 1),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: ref_int_ty,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, 0, 1),
                },
                AmirTemp {
                    id: TempId::from_usize(2),
                    ty: holder_ty,
                    is_copy: false,
                    is_nullable: false,
                    span: Span::new(0, 2, 3),
                },
            ],
            blocks,
            block_params: Vec::new(),
            stmts,
            cfg,
        };

        let promoted = apply_dual_locus_promotion_with_interner(
            &mut func,
            &interner,
            DataLayout::ptr_width(8),
            &EscapeCheckOptions::default(),
        );
        assert!(promoted.contains(&LocalId::from_usize(0)));
        assert!(
            !func.block_stmts(BlockId::from_usize(0)).any(|s| matches!(
                s,
                AmirStmt::Assign {
                    rhs: AmirRvalue::ArenaAlloc { .. },
                    ..
                }
            )),
            "bb0 must not allocate per-iteration loop local"
        );
        assert!(
            func.block_stmts(BlockId::from_usize(2)).any(|s| matches!(
                s,
                AmirStmt::Assign {
                    rhs: AmirRvalue::ArenaAlloc { .. },
                    ..
                }
            )),
            "loop body bb2 must perform per-iteration ArenaAlloc"
        );
        assert!(
            !func
                .block_stmts(BlockId::from_usize(2))
                .any(|s| matches!(s, AmirStmt::ArenaRewind(_))),
            "outer-escaping borrow must NOT be rewound per iteration"
        );
    }
}
