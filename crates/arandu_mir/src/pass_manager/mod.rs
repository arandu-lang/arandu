//! Configurable AMIR optimization pass manager.
//!
//! Decomposes the former rigid `SCCP → DCE → SimplifyCFG` loop in
//! [`crate::optimize`] into an explicit [`FunctionPass`] pipeline selected per
//! [`OptLevel`]. The design follows two validated references:
//!
//! * **LLVM New Pass Manager**: pipelines are built per optimization level and
//!   the O0 pipeline performs no speculative transformation
//!   (<https://llvm.org/docs/NewPassManager.html>).
//! * **rustc `MirPass`**: passes are stateless unit structs with a stable
//!   `name()`, applied in place over a statically sequenced list
//!   (<https://rustc-dev-guide.rust-lang.org/mir/passes.html>).
//!
//! Determinism contract: a pass is a pure function of the IR plus
//! [`PassResources`]; the manager iterates the sequence to a bounded fixpoint
//! and reports `Diagnostic::ice` instead of looping forever.

mod fixpoint;
mod pipeline;
mod stats;

pub use pipeline::pipeline_for_level;
pub use stats::PassStats;

use crate::Diagnostic;
use crate::SymbolTable;
use crate::amir::{AmirFunc, AmirProgram};
use crate::literal_pool::AmirLiteralPool;
use crate::types::TypeInterner;
use bumpalo::Bump;

/// Optimization levels for the AMIR middle-end pipeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OptLevel {
    /// No transformations: minimize compile time, the IR is left untouched.
    O0,
    /// Baseline simplification loop: SCCP → mark-sweep DCE → CFG
    /// simplification (the historical behavior of `optimize_amir`).
    #[default]
    O1,
    /// Speed-oriented pipeline. Shares the O1 core simplification loop today;
    /// new speed passes join at named positions in [`pipeline_for_level`].
    O2,
    /// Size-oriented pipeline (`-Os`). Shares the O1 core today; size passes
    /// join at dedicated positions as they land.
    Os,
}

/// Shared per-function resources handed to every pass invocation.
pub struct PassResources<'a> {
    /// Canonical type domain. Without it numeric conversions fail closed.
    pub interner: Option<&'a TypeInterner>,
    /// Literal pool shared by the whole program; interning must stay stable.
    pub literal_pool: &'a mut AmirLiteralPool,
    /// Per-function scratch arena, reset between mutating fixpoint rounds.
    pub bump: &'a mut Bump,
    /// Callees whose call DCE may drop when the result is unused. Empty when
    /// no symbol table is available.
    pub removable_callees: &'a rustc_hash::FxHashSet<crate::SymbolId>,
}

/// Intrinsic callees of `func` that read no memory, cannot trap and return.
fn removable_callees(
    func: &AmirFunc,
    symbols: &SymbolTable,
) -> rustc_hash::FxHashSet<crate::SymbolId> {
    let mut callees = rustc_hash::FxHashSet::default();
    for stmt in func.stmts.payloads.iter() {
        let crate::amir::AmirStmt::Call {
            callee: crate::amir::AmirOperand::FunctionRef(symbol),
            ..
        } = stmt
        else {
            continue;
        };
        let removable = symbols.try_get(*symbol).is_some_and(|definition| {
            definition.kind == crate::SymbolKind::ExternFunc
                && arandu_middle::intrinsics::IntrinsicKind::from_declared_name(
                    &definition.name,
                    definition.lang_item,
                )
                .is_some_and(arandu_middle::intrinsics::IntrinsicKind::is_removable_when_unused)
        });
        if removable {
            callees.insert(*symbol);
        }
    }
    callees
}

/// A single AMIR function-to-function transformation step.
///
/// Implementations must:
///
/// * be deterministic and free of observable side effects (no I/O, no global
///   state),
/// * preserve AMIR invariants: SSA/OSSA dominance, block-parameter /
///   terminator-argument alignment and dense ids,
/// * converge: repeated application reaches a fixed point. The manager
///   enforces a hard iteration bound regardless.
pub trait FunctionPass {
    /// Stable identifier used by [`PassStats`] and pipeline debugging.
    fn name(&self) -> &'static str;

    /// Applies the pass in place, returning whether the function changed.
    ///
    /// An `Err` aborts the whole pipeline run with that diagnostic.
    fn run(
        &self,
        func: &mut AmirFunc,
        resources: &mut PassResources<'_>,
    ) -> Result<bool, Diagnostic>;
}

/// Sequences [`FunctionPass`] steps over functions until convergence.
pub struct PassManager {
    level: OptLevel,
    passes: Vec<Box<dyn FunctionPass>>,
}

impl PassManager {
    /// Builds the standard pipeline for `level`.
    pub fn for_level(level: OptLevel) -> Self {
        Self {
            level,
            passes: pipeline_for_level(level),
        }
    }

    /// Extension point for tests and experimental pipelines: explicit pass list.
    pub fn from_passes(level: OptLevel, passes: Vec<Box<dyn FunctionPass>>) -> Self {
        Self { level, passes }
    }

    /// The optimization level this manager was built for.
    pub fn level(&self) -> OptLevel {
        self.level
    }

    /// Names of the passes in this pipeline, in execution order.
    pub fn pass_names(&self) -> Vec<&'static str> {
        self.passes.iter().map(|pass| pass.name()).collect()
    }

    /// Optimizes one function to a fixpoint.
    ///
    /// Empty pipelines (O0) return zeroed stats without touching the IR.
    pub fn run_function(
        &self,
        func: &mut AmirFunc,
        literal_pool: &mut AmirLiteralPool,
    ) -> Result<PassStats, Diagnostic> {
        if self.passes.is_empty() {
            return Ok(PassStats::default());
        }
        fixpoint::run_pipeline_to_fixpoint(
            &self.passes,
            func,
            literal_pool,
            None,
            &rustc_hash::FxHashSet::default(),
            fixpoint::DEFAULT_MAX_FIXPOINT_ITERATIONS,
            &mut |_, _| Ok(()),
        )
    }

    /// Optimizes one function and validates AMIR immediately after each pass.
    ///
    /// This catches the pass that introduced malformed IR even when a later
    /// pass would erase or rewrite the damaged structure.
    pub fn run_function_checked(
        &self,
        func: &mut AmirFunc,
        literal_pool: &mut AmirLiteralPool,
        symbols: &SymbolTable,
        interner: &TypeInterner,
    ) -> Result<PassStats, Diagnostic> {
        if self.passes.is_empty() {
            return Ok(PassStats::default());
        }
        let removable = removable_callees(func, symbols);
        fixpoint::run_pipeline_to_fixpoint(
            &self.passes,
            func,
            literal_pool,
            Some(interner),
            &removable,
            fixpoint::DEFAULT_MAX_FIXPOINT_ITERATIONS,
            &mut |func, pass| match crate::amir_validate::validate_amir_func(
                func, symbols, interner,
            )
            .into_iter()
            .next()
            {
                Some(issue) => Err(issue.with_note(format!(
                    "AMIR invariant failed immediately after optimization pass `{pass}`"
                ))),
                None => Ok(()),
            },
        )
    }

    /// Optimizes every function in `program`, merging per-function stats.
    /// Numeric folding requires canonical types; use `run_program_with_types`
    /// or `run_program_checked` when that domain is available.
    pub fn run_program(&self, program: &mut AmirProgram) -> Result<PassStats, Diagnostic> {
        let mut total = PassStats::default();
        for func in &mut program.funcs {
            let stats = self.run_function(func, &mut program.literal_pool)?;
            total.merge(stats);
        }
        Ok(total)
    }

    /// Optimizes valid AMIR using its canonical type domain, without running
    /// the validator. Compiler entrypoints should prefer `run_program_checked`.
    pub fn run_program_with_types(
        &self,
        program: &mut AmirProgram,
        interner: &TypeInterner,
    ) -> Result<PassStats, Diagnostic> {
        let mut total = PassStats::default();
        for func in &mut program.funcs {
            if self.passes.is_empty() {
                continue;
            }
            let stats = fixpoint::run_pipeline_to_fixpoint(
                &self.passes,
                func,
                &mut program.literal_pool,
                Some(interner),
                &rustc_hash::FxHashSet::default(),
                fixpoint::DEFAULT_MAX_FIXPOINT_ITERATIONS,
                &mut |_, _| Ok(()),
            )?;
            total.merge(stats);
        }
        Ok(total)
    }

    /// Optimizes a program while checking invariants after every pass.
    pub fn run_program_checked(
        &self,
        program: &mut AmirProgram,
        symbols: &SymbolTable,
        interner: &TypeInterner,
    ) -> Result<PassStats, Diagnostic> {
        let mut total = PassStats::default();
        for func in &mut program.funcs {
            let stats =
                self.run_function_checked(func, &mut program.literal_pool, symbols, interner)?;
            total.merge(stats);
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amir::{
        AmirBasicBlock, AmirConstant, AmirOperand, AmirRvalue, AmirStmt, AmirStmtTable, AmirTemp,
        AmirTerminator, BlockId, TempId,
    };
    use crate::cfg::compute_cfg_edges;
    use crate::layout::DenseRange;
    use crate::types::{ArType, Primitive};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn intern_ty(ty: ArType) -> crate::types::TypeId {
        // Fresh interner per call is OK in unit tests (pre-interns primitives).
        crate::types::TypeInterner::new().intern(ty)
    }

    fn int_temp(id: usize) -> AmirTemp {
        AmirTemp {
            id: TempId::from_usize(id),
            ty: intern_ty(ArType::Primitive(Primitive::Int)),
            is_copy: true,
            is_nullable: false,
            span: crate::Span::new(0, 0, 0),
        }
    }

    fn func(statements: Vec<AmirStmt>, temps: Vec<AmirTemp>) -> AmirFunc {
        let mut stmts = AmirStmtTable::new();
        let mut range = DenseRange::empty();
        for stmt in statements {
            let instr = stmts.push(stmt);
            crate::amir::program::extend_block_range(&mut range, instr);
        }
        let blocks = vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: range,
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }];
        let cfg = compute_cfg_edges(&blocks);
        AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps,
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        }
    }

    struct AlwaysChangesPass;

    impl FunctionPass for AlwaysChangesPass {
        fn name(&self) -> &'static str {
            "always_changes"
        }

        fn run(
            &self,
            _func: &mut AmirFunc,
            _resources: &mut PassResources<'_>,
        ) -> Result<bool, Diagnostic> {
            Ok(true)
        }
    }

    struct CorruptBlockIdPass;

    impl FunctionPass for CorruptBlockIdPass {
        fn name(&self) -> &'static str {
            "corrupt_block_id"
        }

        fn run(
            &self,
            func: &mut AmirFunc,
            _resources: &mut PassResources<'_>,
        ) -> Result<bool, Diagnostic> {
            func.blocks[0].id = BlockId::from_usize(1);
            Ok(true)
        }
    }

    struct RepairBlockIdPass(Arc<AtomicBool>);

    impl FunctionPass for RepairBlockIdPass {
        fn name(&self) -> &'static str {
            "repair_block_id"
        }

        fn run(
            &self,
            func: &mut AmirFunc,
            _resources: &mut PassResources<'_>,
        ) -> Result<bool, Diagnostic> {
            self.0.store(true, Ordering::Relaxed);
            func.blocks[0].id = BlockId::from_usize(0);
            Ok(true)
        }
    }

    #[test]
    fn o0_pipeline_is_empty_and_leaves_ir_untouched() {
        let manager = PassManager::for_level(OptLevel::O0);
        assert!(manager.pass_names().is_empty());

        let mut f = func(
            vec![AmirStmt::Assign {
                lhs: TempId::from_usize(1),
                rhs: AmirRvalue::Binary {
                    op: crate::ops::BinaryOp::Add,
                    left: AmirOperand::Constant(AmirConstant::Bool(true)),
                    right: AmirOperand::Constant(AmirConstant::Bool(true)),
                },
            }],
            vec![int_temp(0), int_temp(1)],
        );
        let before = f.blocks[0].statements.len;

        let stats = manager
            .run_function(&mut f, &mut AmirLiteralPool::default())
            .unwrap();
        assert_eq!(stats.iterations, 0);
        assert_eq!(f.blocks[0].statements.len, before);
    }

    #[test]
    fn o1_removes_dead_pure_assign_and_reports_stats() {
        let mut pool = AmirLiteralPool::default();
        let mut f = func(
            vec![AmirStmt::Assign {
                lhs: TempId::from_usize(1),
                rhs: AmirRvalue::Binary {
                    op: crate::ops::BinaryOp::Add,
                    left: AmirOperand::Constant(AmirConstant::Bool(true)),
                    right: AmirOperand::Constant(AmirConstant::Bool(true)),
                },
            }],
            vec![int_temp(0), int_temp(1)],
        );

        let stats = PassManager::for_level(OptLevel::O1)
            .run_function(&mut f, &mut pool)
            .unwrap();

        assert_eq!(f.blocks[0].statements.len, 0);
        // Only DCE has work here: the assign is dead, and SCCP/CFG find no
        // opportunities in a single-block function with no constants to fold.
        assert_eq!(stats.changes(), &[("mark_sweep_dce", 1)]);
        // One mutating round + one confirming round before the early return.
        assert_eq!(stats.iterations, 2);
        assert_eq!(stats.total_changes(), 1);
    }

    #[test]
    fn stats_are_deterministic_across_runs() {
        let build = || {
            func(
                vec![AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Binary {
                        op: crate::ops::BinaryOp::Add,
                        left: AmirOperand::Constant(AmirConstant::Bool(true)),
                        right: AmirOperand::Constant(AmirConstant::Bool(true)),
                    },
                }],
                vec![int_temp(0), int_temp(1)],
            )
        };
        let mut first = build();
        let mut second = build();
        let stats_a = PassManager::for_level(OptLevel::O2)
            .run_function(&mut first, &mut AmirLiteralPool::default())
            .unwrap();
        let stats_b = PassManager::for_level(OptLevel::O2)
            .run_function(&mut second, &mut AmirLiteralPool::default())
            .unwrap();
        assert_eq!(stats_a, stats_b);
    }

    #[test]
    fn non_convergence_reports_ice_guardrail() {
        let manager = PassManager::from_passes(OptLevel::O1, vec![Box::new(AlwaysChangesPass)]);
        let mut f = func(Vec::new(), Vec::new());
        let err = manager
            .run_function(&mut f, &mut AmirLiteralPool::default())
            .unwrap_err();
        assert_eq!(err.code, crate::DiagCode::ICEO001);
        assert_eq!(
            err.kind,
            crate::diagnostics::DiagnosticKind::InternalCompilerError
        );
    }

    #[test]
    fn checked_pipeline_attributes_invalid_ir_to_the_first_faulty_pass() {
        let repair_ran = Arc::new(AtomicBool::new(false));
        let manager = PassManager::from_passes(
            OptLevel::O1,
            vec![
                Box::new(CorruptBlockIdPass),
                Box::new(RepairBlockIdPass(Arc::clone(&repair_ran))),
            ],
        );
        let interner = TypeInterner::new();
        let mut symbols = SymbolTable::new(0);
        let symbol = symbols
            .define(
                symbols.global_scope(),
                "probe",
                crate::SymbolKind::Func,
                crate::Span::new(0, 0, 0),
            )
            .unwrap();
        let mut f = func(Vec::new(), Vec::new());
        f.symbol = symbol;

        let err = manager
            .run_function_checked(&mut f, &mut AmirLiteralPool::default(), &symbols, &interner)
            .unwrap_err();

        assert_eq!(err.code, crate::DiagCode::ICEGEN002);
        assert!(
            err.notes
                .iter()
                .any(|note| note.contains("`corrupt_block_id`"))
        );
        assert!(!repair_ran.load(Ordering::Relaxed));
    }

    #[test]
    fn pipelines_share_core_simplification_above_o0() {
        assert!(PassManager::for_level(OptLevel::O0).pass_names().is_empty());
        assert_eq!(
            PassManager::for_level(OptLevel::O1).pass_names(),
            vec![
                "sccp",
                "bounds_check_elim",
                "mark_sweep_dce",
                "simplify_cfg"
            ]
        );
        assert_eq!(
            PassManager::for_level(OptLevel::Os).pass_names(),
            vec![
                "sccp",
                "bounds_check_elim",
                "mark_sweep_dce",
                "tail_merge",
                "simplify_cfg"
            ]
        );
        assert_eq!(
            PassManager::for_level(OptLevel::O2).pass_names(),
            vec![
                "sroa",
                "gvn",
                "sccp",
                "licm",
                "bounds_check_elim",
                "mark_sweep_dce",
                "tail_merge",
                "simplify_cfg"
            ]
        );
    }

    /// Regression (roadmap 1.3): the O1 pipeline must converge on pass
    /// *interaction* — SCCP folds a constant branch, which makes DCE remove
    /// the dead assign, which lets CFG simplification merge/join blocks.
    /// Final shape: one block, `Return`, no statements.
    #[test]
    fn o1_converges_when_passes_enable_each_other() {
        let bool_temp = AmirTemp {
            id: TempId::from_usize(1),
            ty: intern_ty(ArType::Primitive(Primitive::Bool)),
            is_copy: true,
            is_nullable: false,
            span: crate::Span::new(0, 0, 0),
        };
        let mut stmts = AmirStmtTable::new();
        let instr = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
        });
        let mut range = DenseRange::empty();
        crate::amir::program::extend_block_range(&mut range, instr);

        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                params: DenseRange::empty(),
                statements: range,
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Copy(TempId::from_usize(1)),
                    if_true: BlockId::from_usize(1),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(2),
                    false_args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = compute_cfg_edges(&blocks);
        let mut f = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            // TempId doubles as the SCCP lattice index — keep ids dense.
            temps: vec![int_temp(0), bool_temp],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };

        let stats = PassManager::for_level(OptLevel::O1)
            .run_function(&mut f, &mut AmirLiteralPool::default())
            .unwrap();

        // All three passes contributed at least once.
        for name in ["sccp", "mark_sweep_dce", "simplify_cfg"] {
            assert!(
                stats.changes().iter().any(|(n, _)| *n == name),
                "expected {name} to report changes"
            );
        }
        // Converged IR: single entry block, no statements, direct Return.
        assert_eq!(f.blocks.len(), 1);
        assert_eq!(f.blocks[0].id, BlockId::from_usize(0));
        assert_eq!(f.blocks[0].statements.len, 0);
        assert!(matches!(f.blocks[0].terminator, AmirTerminator::Return));
        // Terminator targets stay dense/in-range after every rewrite.
        for block in &f.blocks {
            if let AmirTerminator::Goto { target, .. } = &block.terminator {
                assert!(target.as_usize() < f.blocks.len());
            }
        }
    }
}
