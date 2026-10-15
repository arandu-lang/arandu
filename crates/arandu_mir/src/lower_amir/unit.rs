//! Independently owned function/instance lowering, with a unit-local literal
//! pool. The owner of the typed HIR supplies the type/symbol context; neither
//! this producer nor its composer owns Salsa or final ownership validation.

use super::*;
use crate::amir::visit::{for_each_stmt_operand_mut, for_each_terminator_operand_mut};
use crate::literal_pool::LiteralId;

#[derive(Debug, Clone)]
pub struct FunctionUnit {
    pub function: std::sync::Arc<AmirFunc>,
    pub literals: AmirLiteralPool,
    pub debug_bindings: Vec<AmirDebugBinding>,
    pub debug_blocks: Vec<AmirDebugBlock>,
    pub diagnostics: Vec<Diagnostic>,
    pub no_fallback: bool,
}

/// Finalize one body against converged contracts in its own symbol/type
/// domain. This validates this caller, not its callees: an executable composer
/// must independently finalize every unit it publishes to a backend.
pub fn finalize_function_unit(
    unit: &mut FunctionUnit,
    tc: &TypeCheckResult,
    summaries: &FxHashMap<SymbolId, arandu_middle::types::ReturnBorrowSummary>,
) -> Result<arandu_middle::types::ReturnBorrowSummary, Vec<Diagnostic>> {
    let function = std::sync::Arc::make_mut(&mut unit.function);
    crate::borrow_interface::apply_call_interfaces(function, summaries);
    let (summary, missing) =
        crate::borrow_interface::infer_function_interface(function, &tc.type_info, summaries);
    let mut diagnostics = Vec::new();
    super::validate_borrowed_function(
        tc,
        function,
        (!summary.dependencies.is_empty()).then(|| summary.clone()),
        !missing.is_empty(),
        unit.no_fallback,
        &mut diagnostics,
    );
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return Err(diagnostics);
    }
    unit.diagnostics.extend(diagnostics);
    Ok(summary)
}

/// Lower just one concrete function, using the canonical function lowerer.
/// Call modes and constant values are read from the supplied typed HIR, not
/// inferred from machine representation or reconstructed by a backend.
pub fn lower_function_unit(
    tc: &TypeCheckResult,
    hir: &HirProgram,
    function: &HirFunc,
    layout: DataLayout,
) -> Result<FunctionUnit, Vec<Diagnostic>> {
    if tc.diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(tc.diagnostics.clone());
    }
    if tc.type_info.generic_params.contains_key(&function.symbol) {
        return Err(vec![Diagnostic::ice(
            DiagCode::ICEL001,
            "function unit requires a concrete instance, not a generic template",
            function.span,
        )]);
    }
    let const_values = hir
        .decls
        .iter()
        .filter_map(|&id| match hir.pool.decl(id) {
            HirDecl::Const(value) => Some((value.symbol, value.value)),
            _ => None,
        })
        .collect();
    let modes = CalleeArgModes::from_hir(hir, &tc.type_info.type_interner);
    lower_function_unit_with_context(tc, hir, function, &const_values, &modes, layout)
}

pub(super) fn lower_function_unit_with_context(
    tc: &TypeCheckResult,
    hir: &HirProgram,
    function: &HirFunc,
    const_values: &FxHashMap<SymbolId, crate::hir::HirExprId>,
    modes: &CalleeArgModes,
    layout: DataLayout,
) -> Result<FunctionUnit, Vec<Diagnostic>> {
    let Some(body) = function.body else {
        return Err(vec![Diagnostic::ice(
            DiagCode::ICEL001,
            "function unit has no body",
            function.span,
        )]);
    };
    lower_root_unit(
        tc,
        hir,
        function,
        const_values,
        modes,
        super::func::BodyRoot::Block(body),
        layout,
    )
}

/// Lower a typed expression as an isolated CTFE root without cloning HIR or
/// allocating a callable symbol. The owner supplies source identity only:
/// parameters and other runtime statements are not part of this root.
/// The evaluator must consume it with `evaluate_unit`, not as an owner callee.
pub fn lower_expression_unit(
    tc: &TypeCheckResult,
    hir: &HirProgram,
    owner: &HirFunc,
    expression: crate::hir::HirExprId,
    layout: DataLayout,
) -> Result<FunctionUnit, Vec<Diagnostic>> {
    if tc.diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(tc.diagnostics.clone());
    }
    // This is a small header, not an IR/pool clone. An expression may have a
    // different result type than its containing function.
    let root = HirFunc {
        symbol: owner.symbol,
        params: crate::hir::IndexRange::empty(),
        return_type: hir.pool.expr(expression).ty,
        body: None,
        span: hir.pool.expr(expression).span,
        is_async: false,
        no_fallback: owner.no_fallback,
    };
    let const_values = hir
        .decls
        .iter()
        .filter_map(|&id| match hir.pool.decl(id) {
            HirDecl::Const(value) => Some((value.symbol, value.value)),
            _ => None,
        })
        .collect();
    let modes = CalleeArgModes::from_hir(hir, &tc.type_info.type_interner);
    lower_root_unit(
        tc,
        hir,
        &root,
        &const_values,
        &modes,
        super::func::BodyRoot::Expression(expression),
        layout,
    )
}

/// Lower a separately typed block as an isolated evaluation root. Explicit
/// returns target its own register/frame. `value_tail` comes from initial AST
/// typing: HIR expression statements do not retain the source semicolon.
/// Like expression roots, this unit is not the owner's callable definition.
pub fn lower_block_unit(
    tc: &TypeCheckResult,
    hir: &HirProgram,
    owner: &HirFunc,
    block: crate::hir::HirBlockId,
    result_type: crate::types::TypeId,
    value_tail: bool,
    layout: DataLayout,
) -> Result<FunctionUnit, Vec<Diagnostic>> {
    if tc.diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(tc.diagnostics.clone());
    }
    let span = hir.pool.block(block).span;
    if value_tail
        && !hir
            .pool
            .stmt_list(hir.pool.block(block).statements)
            .last()
            .is_some_and(|&id| matches!(hir.pool.stmt(id).kind, crate::hir::HirStmtKind::Expr(_)))
    {
        return Err(vec![Diagnostic::ice(
            DiagCode::ICEL001,
            "isolated block value tail is not an expression statement",
            span,
        )]);
    }
    let root = HirFunc {
        symbol: owner.symbol,
        params: crate::hir::IndexRange::empty(),
        return_type: result_type,
        body: None,
        span,
        is_async: false,
        no_fallback: owner.no_fallback,
    };
    let const_values = hir
        .decls
        .iter()
        .filter_map(|&id| match hir.pool.decl(id) {
            HirDecl::Const(value) => Some((value.symbol, value.value)),
            _ => None,
        })
        .collect();
    let modes = CalleeArgModes::from_hir(hir, &tc.type_info.type_interner);
    let unit = lower_root_unit(
        tc,
        hir,
        &root,
        &const_values,
        &modes,
        super::func::BodyRoot::IsolatedBlock { block, value_tail },
        layout,
    )?;
    if !matches!(
        tc.type_info.type_interner.resolve(result_type),
        ArType::Void | ArType::Error
    ) && !crate::definite_init::uninitialized_return_exits(&unit.function).is_empty()
    {
        return Err(vec![
            Diagnostic::error(
                DiagCode::T004IncompatibleReturnType,
                "not every exit from this comptime block produces its required value",
                span,
            )
            .with_primary_label("a path reaches the end without a value")
            .with_hint("return a value on every path or provide a final expression"),
        ]);
    }
    Ok(unit)
}

#[allow(clippy::too_many_arguments)]
fn lower_root_unit(
    tc: &TypeCheckResult,
    hir: &HirProgram,
    function: &HirFunc,
    const_values: &FxHashMap<SymbolId, crate::hir::HirExprId>,
    modes: &CalleeArgModes,
    root: super::func::BodyRoot,
    layout: DataLayout,
) -> Result<FunctionUnit, Vec<Diagnostic>> {
    if let Err(error) = layout.validate() {
        return Err(vec![Diagnostic::ice(
            DiagCode::ICEL001,
            format!("invalid target layout for AMIR lowering: {error:?}"),
            function.span,
        )]);
    }
    let mut literals = AmirLiteralPool::default();
    let mut diagnostics = Vec::new();
    let (amir, bindings, spans) = lower_func(
        function,
        root,
        tc,
        hir,
        const_values,
        modes,
        &mut literals,
        &mut diagnostics,
        layout,
    )
    .map_err(|diagnostic| vec![diagnostic])?;
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(diagnostics);
    }
    Ok(FunctionUnit {
        function: std::sync::Arc::new(amir),
        literals,
        debug_bindings: bindings
            .into_iter()
            .map(|(temp, local)| AmirDebugBinding {
                function: function.symbol,
                temp,
                local,
            })
            .collect(),
        debug_blocks: spans
            .into_iter()
            .enumerate()
            .map(|(index, span)| AmirDebugBlock {
                function: function.symbol,
                block: BlockId::from_usize(index),
                span,
            })
            .collect(),
        diagnostics,
        no_fallback: function.no_fallback,
    })
}

/// Compose an owned unit into an executable program. All operand positions,
/// including projection indices and successor arguments, are remapped through
/// the shared visitors. Type/symbol IDs must already share the supplied HIR's
/// context; this operation does not pretend they are process-wide identities.
pub fn append_function_unit(
    program: &mut AmirProgram,
    unit: FunctionUnit,
) -> Result<Vec<Diagnostic>, Diagnostic> {
    let span = unit
        .function
        .temps
        .first()
        .map_or(Span::new(0, 0, 0), |t| t.span);
    let mapping = unit
        .literals
        .entries
        .into_iter()
        .map(|literal| program.literal_pool.intern(literal))
        .collect::<Vec<_>>();
    let mut function = std::sync::Arc::unwrap_or_clone(unit.function);
    let mut invalid = false;
    let mut remap = |operand: &mut AmirOperand| {
        if let AmirOperand::Constant(crate::amir::AmirConstant::Pool(LiteralId(index))) = operand {
            if let Some(&mapped) = mapping.get(*index as usize) {
                *operand = AmirOperand::Constant(crate::amir::AmirConstant::Pool(mapped));
            } else {
                invalid = true;
            }
        }
    };
    for id in function.stmts.iter_ids().collect::<Vec<_>>() {
        if let Some(statement) = function.stmts.get_mut(id) {
            for_each_stmt_operand_mut(statement, &mut remap);
        }
    }
    for block in &mut function.blocks {
        for_each_terminator_operand_mut(&mut block.terminator, &mut remap);
    }
    if invalid {
        return Err(Diagnostic::ice(
            DiagCode::ICEL001,
            "function unit contains an invalid literal pool ID",
            span,
        ));
    }
    program.funcs.push(function);
    program.debug_bindings.extend(unit.debug_bindings);
    program.debug_blocks.extend(unit.debug_blocks);
    Ok(unit.diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amir::{AmirConstant, AmirTerminator};
    use crate::literal_pool::AmirLiteralEntry;

    fn unit(symbol: SymbolId, literal: &str) -> FunctionUnit {
        let mut literals = AmirLiteralPool::default();
        let literal = literals.intern(AmirLiteralEntry::Int(literal.into()));
        let mut statements = AmirStmtTable::new();
        statements.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(literal))),
        });
        FunctionUnit {
            function: std::sync::Arc::new(AmirFunc {
                symbol,
                return_type: crate::types::TypeInterner::new().intern(ArType::Void),
                receiver: None,
                params: Vec::new(),
                locals: Vec::new(),
                temps: Vec::new(),
                block_params: Vec::new(),
                blocks: vec![AmirBasicBlock {
                    id: BlockId::from_usize(0),
                    params: arandu_middle::DenseRange::empty(),
                    statements: arandu_middle::DenseRange::new(0, 1),
                    terminator: AmirTerminator::Goto {
                        target: BlockId::from_usize(0),
                        args: vec![AmirOperand::Constant(AmirConstant::Pool(literal))],
                    },
                }],
                stmts: statements,
                cfg: crate::cfg::ControlFlowGraph::default(),
            }),
            literals,
            debug_bindings: Vec::new(),
            debug_blocks: Vec::new(),
            diagnostics: Vec::new(),
            no_fallback: false,
        }
    }

    fn empty_program() -> AmirProgram {
        AmirProgram {
            funcs: Vec::new(),
            literal_pool: AmirLiteralPool::default(),
            extern_funcs: FxHashMap::default(),
            debug_bindings: Vec::new(),
            debug_blocks: Vec::new(),
        }
    }

    #[test]
    fn composer_distinguishes_equal_local_ids_with_different_literal_values() {
        let mut program = empty_program();
        append_function_unit(&mut program, unit(SymbolId::new(1, 1), "41")).expect("first unit");
        append_function_unit(&mut program, unit(SymbolId::new(1, 2), "42")).expect("second unit");
        append_function_unit(&mut program, unit(SymbolId::new(1, 3), "41"))
            .expect("deduplicated unit");
        assert_eq!(program.literal_pool.entries.len(), 2);
        for (function, expected) in program.funcs.iter().zip([0, 1, 0]) {
            let mut operands = Vec::new();
            crate::amir::visit::for_each_rvalue_operand(
                match function
                    .stmts
                    .get(crate::amir::InstrId::from_usize(0))
                    .expect("assignment")
                {
                    AmirStmt::Assign { rhs, .. } => rhs,
                    _ => panic!("assignment"),
                },
                |op| operands.push(*op),
            );
            crate::amir::visit::for_each_terminator_operand(&function.blocks[0].terminator, |op| {
                operands.push(*op)
            });
            assert_eq!(
                operands,
                vec![AmirOperand::Constant(AmirConstant::Pool(LiteralId(expected))); 2]
            );
        }
    }

    #[test]
    fn composer_rejects_an_invalid_literal_id_without_publishing_the_function() {
        let mut program = empty_program();
        let mut invalid = unit(SymbolId::new(1, 1), "41");
        invalid.literals.entries.clear();
        let diagnostic =
            append_function_unit(&mut program, invalid).expect_err("invalid pool reference");
        assert_eq!(diagnostic.code, DiagCode::ICEL001);
        assert!(program.funcs.is_empty());
    }
}
