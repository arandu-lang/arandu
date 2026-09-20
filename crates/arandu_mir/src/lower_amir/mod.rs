//! AMIR lowering pass.
//!
//! Transforms a [`HirProgram`] (High-level IR) into an [`AmirProgram`]
//! (Arandu Mid-level IR). Each HIR function is independently lowered into
//! SSA-like AMIR basic blocks. Aborts early if type-checking already failed.

use crate::amir::{
    AmirBasicBlock, AmirDebugBinding, AmirFunc, AmirLocal, AmirOperand, AmirProgram, AmirRvalue,
    AmirStmt, AmirStmtTable, AmirTemp, BlockId, LocalId, TempId,
};
use crate::diagnostics::{DiagCode, Diagnostic, Severity};
use crate::hir::{HirBlock, HirDecl, HirFunc, HirProgram};
use crate::literal_pool::AmirLiteralPool;
use crate::passes::type_checker::types::{ArType, Primitive};
use crate::{SymbolId, TypeCheckResult};
use arandu_lexer::Span;
use rustc_hash::{FxHashMap, FxHashSet};

mod arg_modes;
mod builder;
mod ctx;
mod expr;
mod flow;
mod func;
mod match_lower;
mod ops;
mod pattern;
mod place;
mod ssa;
mod stmt;

pub(crate) use arg_modes::CalleeArgModes;
pub(crate) use func::lower_func;

/// Lowers a [`HirProgram`] into an [`AmirProgram`].
///
/// Returns `Err` immediately if `tc` already contains any [`Severity::Error`]
/// diagnostics. Each function is lowered independently; partial errors are
/// collected and returned together so the caller sees all failures at once.
///
/// `pointer_width` (in bytes) comes from the target
/// [`arandu_middle::layout::DataLayout`]; it drives `mem.sizeOf`/`alignOf`
/// constant folding (no host `usize` in the hot path).
#[tracing::instrument(level = "trace", target = "arandu_mir::lower_amir", skip(tc, hir))]
pub fn lower_to_amir(
    tc: &TypeCheckResult,
    hir: &HirProgram,
    pointer_width: u64,
) -> Result<AmirProgram, Vec<Diagnostic>> {
    let mut owned = tc.clone();
    match lower_to_amir_with_interfaces(&mut owned, hir, pointer_width) {
        Ok((program, _)) => Ok(program),
        Err(diags) => Err(diags),
    }
}

/// Lower AMIR and publish flow-derived borrow interfaces back into `tc`.
///
/// Query orchestration uses this entry point so the returned type-check bundle
/// and the AMIR consumed by codegen share exactly the same public contracts.
///
/// `pointer_width` (in bytes) comes from the target
/// [`arandu_middle::layout::DataLayout`]; it drives `mem.sizeOf`/`alignOf`
/// constant folding (no host `usize` in the hot path).
#[tracing::instrument(level = "trace", target = "arandu_mir::lower_amir", skip(tc, hir))]
pub fn lower_to_amir_with_interfaces(
    tc: &mut TypeCheckResult,
    hir: &HirProgram,
    pointer_width: u64,
) -> Result<(AmirProgram, Vec<Diagnostic>), Vec<Diagnostic>> {
    if tc.diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(tc.diagnostics.clone());
    }

    let mut funcs = Vec::new();
    let mut diagnostics = Vec::new();
    let mut literal_pool = AmirLiteralPool::default();
    let mut debug_bindings = Vec::new();
    let mut no_fallback = FxHashMap::default();
    // Single post-mono table: receiver Shared/Mut/Own → Copy vs Move at call sites.
    let arg_modes = CalleeArgModes::from_hir(hir, &tc.type_info.type_interner);

    for &decl_id in &hir.decls {
        if let HirDecl::Func(
            f @ HirFunc {
                body: Some(body), ..
            },
        ) = hir.pool.decl(decl_id)
        {
            no_fallback.insert(f.symbol, f.no_fallback);
            // Skip generic templates — only monomorphized specializations (and
            // non-generic functions) are lowered to AMIR.
            if tc.type_info.generic_params.contains_key(&f.symbol) {
                continue;
            }
            match lower_func(
                f,
                *body,
                tc,
                hir,
                &arg_modes,
                &mut literal_pool,
                &mut diagnostics,
                pointer_width,
            ) {
                Ok((amir_f, local_debug_bindings)) => {
                    debug_bindings.extend(local_debug_bindings.into_iter().map(|(temp, local)| {
                        AmirDebugBinding {
                            function: f.symbol,
                            temp,
                            local,
                        }
                    }));
                    funcs.push(amir_f);
                }
                Err(diag) => diagnostics.push(diag),
            }
        }
    }

    let has_errors = diagnostics.iter().any(|d| d.severity == Severity::Error);
    if !has_errors {
        let mut extern_funcs = rustc_hash::FxHashMap::default();
        for sym in tc.symbols.iter() {
            if sym.kind == arandu_middle::SymbolKind::ExternFunc
                && let Some(&ty_id) = tc.type_info.decl_types.get(&sym.id)
            {
                let ty = tc.type_info.type_interner.resolve(ty_id);
                if let arandu_middle::types::ArType::Func(params, ret) = ty {
                    let param_types: Vec<_> = tc
                        .type_info
                        .type_interner
                        .type_args(params)
                        .iter()
                        .map(|&p| tc.type_info.type_interner.resolve(p))
                        .collect();
                    let ret_type = tc.type_info.type_interner.resolve(ret);
                    extern_funcs.insert(sym.id, (param_types, ret_type));
                }
            }
        }
        let mut program = AmirProgram {
            funcs,
            literal_pool,
            extern_funcs,
            debug_bindings,
        };

        let solution =
            crate::borrow_interface::solve_borrow_interfaces(&mut program, &tc.type_info);
        {
            let info = tc.type_info_mut();
            info.return_borrow_summaries = solution.summaries.clone();
        }

        for function in &mut program.funcs {
            // M2 and escape validation intentionally run only after calls carry
            // the converged interprocedural interface.
            diagnostics.extend(crate::borrow_check::check_borrows(function, &tc.symbols));
            let options = crate::escape_analysis::EscapeCheckOptions {
                no_fallback: no_fallback.get(&function.symbol).copied().unwrap_or(false),
                return_borrow: solution.summaries.get(&function.symbol).cloned(),
            };
            let escape_diagnostics = crate::escape_analysis::check_escapes(
                function,
                &tc.symbols,
                &tc.type_info.type_interner,
                options.clone(),
            );
            let already_reports_return = escape_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagCode::O010EscapeOfBorrowedValue);
            diagnostics.extend(escape_diagnostics);
            if !already_reports_return
                && solution
                    .unproven
                    .iter()
                    .any(|failure| failure.function == function.symbol)
            {
                let span = function
                    .temps
                    .first()
                    .map_or(Span::new(0, 0, 0), |temp| temp.span);
                diagnostics.push(
                    Diagnostic::error(
                        DiagCode::O010EscapeOfBorrowedValue,
                        "borrowed return has no demonstrable formal origin".to_string(),
                        span,
                    )
                    .with_label(span, "this returned borrow cannot be tied to a caller-owned input")
                    .with_note(
                        "borrow interfaces are inferred from AMIR flow; compatible parameter types alone are not proof",
                    )
                    .with_hint("return owned data or forward a borrow derived from a formal `ref` input"),
                );
            }
            crate::gen_promote::apply_gen_promotion_with_type_info(
                function,
                &tc.type_info,
                options,
            );
        }

        let has_errors = diagnostics.iter().any(|d| d.severity == Severity::Error);
        if !has_errors {
            Ok((program, diagnostics))
        } else {
            Err(diagnostics)
        }
    } else {
        Err(diagnostics)
    }
}

pub(crate) fn is_memory_type(ty: &ArType) -> bool {
    match ty {
        ArType::Primitive(p) => matches!(p, Primitive::Str | Primitive::Any),
        ArType::IntLiteral
        | ArType::FloatLiteral
        | ArType::Const(_)
        | ArType::ConstParam(_)
        | ArType::Void
        | ArType::Err
        | ArType::Error => false,
        // Pointers and safe refs are scalar values (fat/thin pointers), not memory objects.
        ArType::Ptr(_)
        | ArType::Ref(_)
        | ArType::RefMut(_)
        | ArType::GenRef
        | ArType::Nullable(_)
        | ArType::Func(_, _)
        | ArType::Slice(_) => false,
        ArType::Array(_, _)
        | ArType::ConstArray(_, _)
        | ArType::Named(_, _)
        | ArType::Tuple(_)
        | ArType::Option(_)
        | ArType::Result(_, _)
        | ArType::Coroutine(_)
        | ArType::Poll(_)
        | ArType::Range(_) => true,
    }
}

pub fn prune_dummy_loads_stores(func: &mut AmirFunc) {
    let mut new_stmts = AmirStmtTable::new();
    let mut new_blocks = Vec::with_capacity(func.blocks.len());

    for block in &func.blocks {
        let new_range_start = new_stmts.len();
        let mut new_range_len = 0;

        for stmt_id in func.block_stmt_ids(block.id) {
            // stmt_id comes from block ranges; missing id is a corrupt AMIR table — skip.
            let Some(stmt) = func.stmts.get(stmt_id) else {
                continue;
            };
            let keep = match stmt {
                AmirStmt::Store { lhs, .. } if lhs.projections.is_empty() => {
                    func.locals[lhs.local.as_usize()].is_memory
                }
                AmirStmt::Assign {
                    rhs: AmirRvalue::Load(place),
                    ..
                } if place.projections.is_empty() => func.locals[place.local.as_usize()].is_memory,
                _ => true,
            };

            if keep {
                new_stmts.push(stmt.clone());
                new_range_len += 1;
            }
        }

        new_blocks.push(AmirBasicBlock {
            id: block.id,
            params: block.params,
            statements: crate::layout::DenseRange::new(new_range_start, new_range_len),
            terminator: block.terminator.clone(),
        });
    }

    func.stmts = new_stmts;
    func.blocks = new_blocks;
    func.cfg = crate::cfg::compute_cfg_edges(&func.blocks);
}

pub(crate) fn amir_unsupported(span: Span, feature: &str, roadmap: &str) -> Diagnostic {
    Diagnostic::error(
        DiagCode::U001FeatureNotSupported,
        format!("AMIR v0.1: {feature} is not supported yet ({roadmap})"),
        span,
    )
    .with_hint("see docs/arandu-compiler-roadmap-v0.1.md for the planned milestone")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeferKind {
    Defer,
    ErrDefer,
}

#[derive(Clone)]
pub(crate) struct DeferFrame {
    entries: Vec<(HirBlock, DeferKind)>,
}

pub(crate) struct LowerCtx<'a> {
    tc: &'a TypeCheckResult,
    hir: &'a HirProgram,
    /// Shared/mut/own modes for every callable (incl. mono specializations).
    arg_modes: &'a CalleeArgModes,
    func_return_type: crate::types::TypeId,
    /// A3: function was declared `async` — returns wrap bare `T` as `Coroutine[T]`.
    func_is_async: bool,
    /// A3: nesting depth of `async { … }` bodies being lowered (enables Suspend split
    /// inside blocks even when the enclosing function is sync).
    coroutine_depth: u32,
    locals: Vec<AmirLocal>,
    temps: Vec<AmirTemp>,
    /// Structural construction state (blocks, stmts, cursor, predecessors).
    builder: builder::AmirBuilder,
    symbol_map: FxHashMap<SymbolId, LocalId>,
    /// (`continue_block`, `exit_block`, `defer_frame_depth_at_loop_entry`)
    loop_stack: Vec<(BlockId, BlockId, usize)>,
    literal_pool: &'a mut AmirLiteralPool,
    defer_frames: Vec<DeferFrame>,
    temp_states: Vec<MoveState>,
    temp_origins: Vec<Option<LocalId>>,
    /// Cold typed mapping consumed only by native debug-info emission.
    debug_bindings: Vec<(TempId, LocalId)>,
    local_states: Vec<MoveState>,

    // SSA builder fields (OSSA Braun et al.)
    sealed_blocks: FxHashSet<BlockId>,
    current_def: FxHashMap<(BlockId, LocalId), AmirOperand>,
    incomplete_phis: FxHashMap<BlockId, Vec<(LocalId, TempId)>>,
    redirected_temps: FxHashMap<TempId, AmirOperand>,
    /// Span of the HIR construct currently being lowered (for `use_span` / diags).
    current_span: Span,
    /// Target pointer width in bytes (drives `mem.sizeOf`/`alignOf` folding).
    pointer_width: u64,
    /// Temporaries that hold freshly allocated heap string buffers (`ToStr` or `StringInterp`).
    owned_string_temps: FxHashSet<TempId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MoveState {
    Available,
    Moved,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amir::{
        AmirBasicBlock, AmirFunc, AmirLocal, AmirOperand, AmirPlace, AmirRvalue, AmirStmt,
        AmirStmtTable, AmirTerminator, BlockId, InstrId, LocalId, TempId,
    };
    use crate::cfg::ControlFlowGraph;
    use crate::layout::DenseRange;
    use crate::types::TypeId;
    use crate::{Span, SymbolId};
    use smallvec::SmallVec;

    #[test]
    fn test_prune_dummy_loads_stores() {
        let span = Span::new(0, 0, 0);

        let local_non_mem = AmirLocal {
            id: LocalId::from_usize(0),
            ty: TypeId::from_usize(0),
            is_memory: false,
            symbol: None,
            span,
            use_span: None,
        };

        let local_mem = AmirLocal {
            id: LocalId::from_usize(1),
            ty: TypeId::from_usize(1),
            is_memory: true,
            symbol: None,
            span,
            use_span: None,
        };

        let mut stmts = AmirStmtTable::new();
        // 0: Store to non-memory local with empty projections (redundant) -> PRUNE
        let _ = stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: LocalId::from_usize(0),
                projections: SmallVec::new(),
            },
            rhs: AmirOperand::Copy(TempId::from_usize(0)),
        });

        // 1: Store to memory local with empty projections (needed) -> KEEP
        let _ = stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: LocalId::from_usize(1),
                projections: SmallVec::new(),
            },
            rhs: AmirOperand::Copy(TempId::from_usize(0)),
        });

        // 2: Load from non-memory local with empty projections (redundant) -> PRUNE
        let _ = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Load(AmirPlace {
                local: LocalId::from_usize(0),
                projections: SmallVec::new(),
            }),
        });

        // 3: Load from memory local with empty projections (needed) -> KEEP
        let _ = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Load(AmirPlace {
                local: LocalId::from_usize(1),
                projections: SmallVec::new(),
            }),
        });

        let block = AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::new(0, 4),
            terminator: AmirTerminator::Return,
        };

        let mut func = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: TypeId::from_usize(0),
            receiver: None,
            params: Vec::new(),
            locals: vec![local_non_mem, local_mem],
            temps: Vec::new(),
            blocks: vec![block],
            block_params: Vec::new(),
            stmts,
            cfg: ControlFlowGraph::default(),
        };

        prune_dummy_loads_stores(&mut func);

        let new_block = &func.blocks[0];
        assert_eq!(new_block.statements.len, 2);

        let new_stmt_0 = func
            .stmts
            .get(InstrId::from_usize(new_block.statements.start as usize))
            .unwrap();
        let new_stmt_1 = func
            .stmts
            .get(InstrId::from_usize(
                (new_block.statements.start + 1) as usize,
            ))
            .unwrap();

        if let AmirStmt::Store { lhs, .. } = new_stmt_0 {
            assert_eq!(lhs.local.as_usize(), 1);
        } else {
            panic!("Expected Store statement, got {:?}", new_stmt_0);
        }

        if let AmirStmt::Assign {
            rhs: AmirRvalue::Load(place),
            ..
        } = new_stmt_1
        {
            assert_eq!(place.local.as_usize(), 1);
        } else {
            panic!("Expected Assign/Load statement, got {:?}", new_stmt_1);
        }
    }
}
