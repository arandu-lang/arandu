//! AMIR CFG invariant validation (CFG-1 … CFG-5 per `docs/arandu-amir-v0.1.md`).

use crate::SymbolTable;
use crate::amir::{
    AmirConstant, AmirFunc, AmirOperand, AmirProgram, AmirRvalue, AmirStmt, AmirTerminator,
    BlockId, reachable_blocks_dense,
};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::types::{ArType, Primitive, TypeId, TypeInterner};

/// Validate all functions in an AMIR program.
#[must_use]
pub fn validate_amir_program(
    program: &AmirProgram,
    symbols: &SymbolTable,
    interner: &TypeInterner,
) -> Vec<Diagnostic> {
    program
        .funcs
        .iter()
        .flat_map(|f| validate_amir_func(f, symbols, interner))
        .collect()
}

#[must_use]
pub fn validate_amir_func(
    func: &AmirFunc,
    symbols: &SymbolTable,
    interner: &TypeInterner,
) -> Vec<Diagnostic> {
    let span = symbols.get(func.symbol).span;
    let mut diags = Vec::new();

    if func.blocks.is_empty() {
        diags.push(Diagnostic::ice(
            DiagCode::ICEGEN002,
            "function has no basic blocks (CFG-4)".to_string(),
            span,
        ));
        return diags;
    }

    // Edges may target blocks later in the table. Validate every parameter
    // range before following any edge or slicing the dense pool. Malformed
    // IR must produce a reportable ICE, not panic inside the validator.
    for (i, block) in func.blocks.iter().enumerate() {
        let start = block.params.start_usize();
        let len = block.params.len_usize();
        if start > func.block_params.len() || len > func.block_params.len() - start {
            diags.push(Diagnostic::ice(
                DiagCode::ICEGEN002,
                format!(
                    "bb{i} parameter range (start={start}, len={len}) exceeds parameter pool length {} (IR-RANGE)",
                    func.block_params.len()
                ),
                span,
            ));
        }
    }
    if !diags.is_empty() {
        return diags;
    }

    for (i, block) in func.blocks.iter().enumerate() {
        if block.id.as_usize() != i {
            diags.push(Diagnostic::ice(
                DiagCode::ICEGEN002,
                format!(
                    "block at index {i} has mismatched BlockId bb{} (expected bb{i}) (CFG-0)",
                    block.id.as_usize()
                ),
                span,
            ));
        }
        if !is_valid_terminator(&block.terminator) {
            diags.push(Diagnostic::ice(
                DiagCode::ICEGEN002,
                format!("bb{i}: invalid terminator (CFG-1)"),
                span,
            ));
        }

        for succ in terminator_targets(&block.terminator) {
            if succ.as_usize() >= func.blocks.len() {
                diags.push(Diagnostic::ice(
                    DiagCode::ICEGEN002,
                    format!(
                        "bb{i}: terminator targets non-existent bb{} (CFG-3)",
                        succ.as_usize()
                    ),
                    span,
                ));
            }
        }

        for_each_terminator_edge(&block.terminator, |target, args| {
            let Some(target_block) = func.blocks.get(target.as_usize()) else {
                return;
            };
            let target_params = func.block_params(target_block.params);
            let arg_count = args.len();
            if arg_count != target_params.len() {
                diags.push(Diagnostic::ice(
                    DiagCode::ICEGEN002,
                    format!(
                        "bb{i} passes {arg_count} argument(s) to bb{}, which expects {} block parameter(s) (SSA-EDGE)",
                        target.as_usize(),
                        target_params.len()
                    ),
                    span,
                ));
            }
            for (arg_index, (arg, param)) in args.iter().zip(target_params).enumerate() {
                let Some(arg_ty) = operand_type(func, arg, interner) else {
                    continue;
                };
                if !edge_types_compatible(interner, arg_ty, param.ty) {
                    diags.push(Diagnostic::ice(
                        DiagCode::ICEGEN002,
                        format!(
                            "bb{i} argument {arg_index} to bb{} has type {}, but block parameter expects {} (SSA-TYPE)",
                            target.as_usize(),
                            type_name(interner, arg_ty),
                            type_name(interner, param.ty)
                        ),
                        span,
                    ));
                }
            }
        });
    }

    let mut stmt_owner = vec![None; func.stmts.len()];
    for (i, block) in func.blocks.iter().enumerate() {
        let range = block.statements;
        if range.end_usize() > func.stmts.len() {
            diags.push(Diagnostic::ice(
                DiagCode::ICEGEN002,
                format!(
                    "bb{i} statement range {}..{} exceeds statement table length {} (IR-RANGE)",
                    range.start_usize(),
                    range.end_usize(),
                    func.stmts.len()
                ),
                span,
            ));
            continue;
        }
        for stmt_index in range.as_range() {
            if let Some(previous_block) = stmt_owner[stmt_index].replace(i) {
                diags.push(Diagnostic::ice(
                    DiagCode::ICEGEN002,
                    format!(
                        "statement {stmt_index} is owned by both bb{previous_block} and bb{i} (IR-RANGE)"
                    ),
                    span,
                ));
            }
        }
    }

    let reachable = reachable_blocks_dense(func);
    for (i, block) in func.blocks.iter().enumerate() {
        if i == 0 {
            continue;
        }
        if !reachable.contains(BlockId::from_usize(i))
            && !matches!(block.terminator, AmirTerminator::Unreachable)
        {
            diags.push(Diagnostic::ice(
                DiagCode::ICEGEN002,
                format!("bb{i}: not reachable from bb0 (CFG-5)"),
                span,
            ));
        }
    }

    for (i, local) in func.locals.iter().enumerate() {
        if local.id.as_usize() != i {
            diags.push(Diagnostic::ice(
                DiagCode::ICEGEN002,
                format!(
                    "local at index {i} has mismatched LocalId s{} (expected s{i}) (TYP-2)",
                    local.id.as_usize()
                ),
                span,
            ));
        }
        if interner.is_error(local.ty) {
            diags.push(Diagnostic::ice(
                DiagCode::ICEGEN002,
                format!(
                    "local s{} has poison type Error (TYP-1)",
                    local.id.as_usize()
                ),
                span,
            ));
        }
    }

    for (i, temp) in func.temps.iter().enumerate() {
        if temp.id.as_usize() != i {
            diags.push(Diagnostic::ice(
                DiagCode::ICEGEN002,
                format!(
                    "temp at index {i} has mismatched TempId _{} (expected _{i}) (TYP-2)",
                    temp.id.as_usize()
                ),
                span,
            ));
        }
        if interner.is_error(temp.ty) {
            diags.push(Diagnostic::ice(
                DiagCode::ICEGEN002,
                format!("temp _{} has poison type Error (TYP-1)", temp.id.as_usize()),
                span,
            ));
        }
    }

    for (block_index, block) in func.blocks.iter().enumerate() {
        for (param_index, param) in func.block_params(block.params).iter().enumerate() {
            let Some(temp) = func.temps.get(param.id.as_usize()) else {
                diags.push(Diagnostic::ice(
                    DiagCode::ICEGEN002,
                    format!(
                        "bb{block_index} parameter {param_index} references missing temp _{} (SSA-PARAM)",
                        param.id.as_usize()
                    ),
                    span,
                ));
                continue;
            };
            if temp.ty != param.ty {
                diags.push(Diagnostic::ice(
                    DiagCode::ICEGEN002,
                    format!(
                        "bb{block_index} parameter {param_index} type {} disagrees with temp _{} type {} (SSA-PARAM)",
                        type_name(interner, param.ty),
                        param.id.as_usize(),
                        type_name(interner, temp.ty)
                    ),
                    span,
                ));
            }
        }
    }

    validate_gen_operations(func, interner, &mut diags);
    validate_vector_operations(func, interner, span, &mut diags);

    diags
}

fn validate_gen_operations(func: &AmirFunc, interner: &TypeInterner, diags: &mut Vec<Diagnostic>) {
    let mut borrowed_results = Vec::new();
    for stmt_id in func.stmts.iter_ids() {
        let Some(stmt) = func.stmts.get(stmt_id) else {
            continue;
        };
        let AmirStmt::Assign { lhs, rhs } = stmt else {
            continue;
        };
        let Some(result_ty) = func.temps.get(lhs.as_usize()).map(|temp| temp.ty) else {
            continue;
        };

        match rhs {
            AmirRvalue::GenInsert {
                value,
                payload_ty,
                origin,
                ..
            } => {
                require_gen_ref_type(interner, result_ty, *origin, "GenInsert result", diags);
                require_payload_operand(
                    func,
                    interner,
                    value,
                    *payload_ty,
                    *origin,
                    "GenInsert payload",
                    diags,
                );
            }
            AmirRvalue::GenGet {
                gen_ref,
                payload_ty,
                origin,
                ..
            } => {
                if !func.temps[lhs.as_usize()].is_copy {
                    borrowed_results.push((*lhs, *origin));
                }
                require_gen_ref_operand(func, interner, gen_ref, *origin, diags);
                if result_ty != *payload_ty {
                    diags.push(Diagnostic::ice(
                        DiagCode::ICEGEN002,
                        format!(
                            "Gen operation result type {} differs from declared payload type {} (GEN-TYPE)",
                            type_name(interner, result_ty),
                            type_name(interner, *payload_ty)
                        ),
                        *origin,
                    ));
                }
            }
            AmirRvalue::GenRemove {
                gen_ref,
                payload_ty,
                origin,
                ..
            } => {
                require_gen_ref_operand(func, interner, gen_ref, *origin, diags);
                if result_ty != *payload_ty {
                    diags.push(Diagnostic::ice(
                        DiagCode::ICEGEN002,
                        format!(
                            "Gen operation result type {} differs from declared payload type {} (GEN-TYPE)",
                            type_name(interner, result_ty),
                            type_name(interner, *payload_ty)
                        ),
                        *origin,
                    ));
                }
            }
            AmirRvalue::GenSet {
                gen_ref,
                value,
                payload_ty,
                origin,
                ..
            } => {
                require_gen_ref_type(interner, result_ty, *origin, "GenSet result", diags);
                require_gen_ref_operand(func, interner, gen_ref, *origin, diags);
                require_payload_operand(
                    func,
                    interner,
                    value,
                    *payload_ty,
                    *origin,
                    "GenSet payload",
                    diags,
                );
            }
            AmirRvalue::GenUpsert {
                gen_ref,
                value,
                payload_ty,
                origin,
                ..
            } => {
                require_gen_ref_type(interner, result_ty, *origin, "GenUpsert result", diags);
                if !matches!(
                    gen_ref,
                    AmirOperand::Constant(crate::amir::AmirConstant::Nil)
                ) {
                    require_gen_ref_operand(func, interner, gen_ref, *origin, diags);
                }
                require_payload_operand(
                    func,
                    interner,
                    value,
                    *payload_ty,
                    *origin,
                    "GenUpsert payload",
                    diags,
                );
            }
            _ => {}
        }
    }

    for (borrowed, origin) in borrowed_results {
        validate_borrowed_gen_result(func, borrowed, origin, diags);
    }
}

fn validate_borrowed_gen_result(
    func: &AmirFunc,
    borrowed: crate::amir::TempId,
    origin: crate::Span,
    diags: &mut Vec<Diagnostic>,
) {
    let references = |operand: &AmirOperand| matches!(operand, AmirOperand::Copy(temp) | AmirOperand::Move(temp) if *temp == borrowed);
    for stmt in func.stmts.payloads.iter() {
        let AmirStmt::Assign { lhs, rhs } = stmt else {
            let used = match stmt {
                AmirStmt::Store { rhs, .. } | AmirStmt::Free(rhs) => references(rhs),
                AmirStmt::Call { callee, args, .. } => {
                    references(callee) || args.iter().any(references)
                }
                AmirStmt::Destroy(_)
                | AmirStmt::StorageLive(_)
                | AmirStmt::StorageDead(_)
                | AmirStmt::Nop => false,
                AmirStmt::Assign { .. } => false,
            };
            if used {
                borrowed_gen_escape(origin, borrowed, diags);
            }
            continue;
        };
        if *lhs == borrowed && matches!(rhs, AmirRvalue::GenGet { .. }) {
            continue;
        }
        let mut used = false;
        crate::amir::for_each_rvalue_operand(rhs, |operand| used |= references(operand));
        if !used {
            continue;
        }
        let safe_projection = match rhs {
            AmirRvalue::FieldAccess { base, .. }
            | AmirRvalue::Len(base)
            | AmirRvalue::SliceData(base)
            | AmirRvalue::Discriminant { value: base }
            | AmirRvalue::EnumPayload { value: base, .. } => references(base),
            AmirRvalue::IndexAccess { base, index } => references(base) && !references(index),
            _ => false,
        };
        if !safe_projection {
            borrowed_gen_escape(origin, borrowed, diags);
        }
    }

    for block in &func.blocks {
        let used = match &block.terminator {
            AmirTerminator::Return | AmirTerminator::Unreachable => false,
            AmirTerminator::Goto { args, .. } => args.iter().any(references),
            AmirTerminator::Branch {
                condition,
                true_args,
                false_args,
                ..
            } => {
                references(condition)
                    || true_args.iter().any(references)
                    || false_args.iter().any(references)
            }
            AmirTerminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => {
                references(discriminant)
                    || targets
                        .iter()
                        .any(|(_, _, args)| args.iter().any(references))
                    || otherwise.1.iter().any(references)
            }
            AmirTerminator::Suspend { future, args, .. } => {
                references(future) || args.iter().any(references)
            }
        };
        if used {
            borrowed_gen_escape(origin, borrowed, diags);
        }
    }
}

fn borrowed_gen_escape(
    origin: crate::Span,
    borrowed: crate::amir::TempId,
    diags: &mut Vec<Diagnostic>,
) {
    diags.push(Diagnostic::ice(
        DiagCode::ICEGEN002,
        format!(
            "borrowed non-Copy GenGet result _{} escapes a projection-only use (GEN-BORROW)",
            borrowed.as_usize()
        ),
        origin,
    ));
}

fn require_payload_operand(
    func: &AmirFunc,
    interner: &TypeInterner,
    operand: &AmirOperand,
    expected: TypeId,
    origin: crate::Span,
    label: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(actual) = operand_type(func, operand, interner) else {
        return;
    };
    if actual != expected {
        diags.push(Diagnostic::ice(
            DiagCode::ICEGEN002,
            format!(
                "{label} has type {}, but operation declares payload type {} (GEN-TYPE)",
                type_name(interner, actual),
                type_name(interner, expected)
            ),
            origin,
        ));
    }
}

fn require_gen_ref_operand(
    func: &AmirFunc,
    interner: &TypeInterner,
    operand: &AmirOperand,
    origin: crate::Span,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(actual) = operand_type(func, operand, interner) else {
        return;
    };
    require_gen_ref_type(interner, actual, origin, "Gen operation handle", diags);
}

fn require_gen_ref_type(
    interner: &TypeInterner,
    actual: TypeId,
    origin: crate::Span,
    label: &str,
    diags: &mut Vec<Diagnostic>,
) {
    if !matches!(interner.resolve(actual), ArType::GenRef) {
        diags.push(Diagnostic::ice(
            DiagCode::ICEGEN002,
            format!(
                "{label} has type {}, expected GenRef (GEN-TYPE)",
                type_name(interner, actual)
            ),
            origin,
        ));
    }
}

fn is_valid_terminator(term: &AmirTerminator) -> bool {
    matches!(
        term,
        AmirTerminator::Return
            | AmirTerminator::Goto { .. }
            | AmirTerminator::Branch { .. }
            | AmirTerminator::SwitchInt { .. }
            | AmirTerminator::Suspend { .. }
            | AmirTerminator::Unreachable
    )
}

fn terminator_targets(term: &AmirTerminator) -> Vec<BlockId> {
    match term {
        AmirTerminator::Return | AmirTerminator::Unreachable => Vec::new(),
        AmirTerminator::Goto { target, .. } => vec![*target],
        AmirTerminator::Branch {
            if_true, if_false, ..
        } => vec![*if_true, *if_false],
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let mut v: Vec<BlockId> = targets.iter().map(|(_, b, _)| *b).collect();
            v.push(otherwise.0);
            v
        }
        AmirTerminator::Suspend { resume, .. } => vec![*resume],
    }
}

fn for_each_terminator_edge(term: &AmirTerminator, mut f: impl FnMut(BlockId, &[AmirOperand])) {
    match term {
        AmirTerminator::Return | AmirTerminator::Unreachable => {}
        AmirTerminator::Goto { target, args } => f(*target, args),
        AmirTerminator::Branch {
            if_true,
            true_args,
            if_false,
            false_args,
            ..
        } => {
            f(*if_true, true_args);
            f(*if_false, false_args);
        }
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            for (_, target, args) in targets {
                f(*target, args);
            }
            f(otherwise.0, &otherwise.1);
        }
        AmirTerminator::Suspend { resume, args, .. } => f(*resume, args),
    }
}

fn operand_type(func: &AmirFunc, operand: &AmirOperand, interner: &TypeInterner) -> Option<TypeId> {
    match operand {
        AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
            func.temps.get(temp.as_usize()).map(|temp| temp.ty)
        }
        AmirOperand::Constant(AmirConstant::Bool(_)) => {
            Some(interner.intern(ArType::Primitive(Primitive::Bool)))
        }
        AmirOperand::Constant(AmirConstant::Nil | AmirConstant::Pool(_))
        | AmirOperand::FunctionRef(_)
        | AmirOperand::GlobalRef(_) => None,
    }
}

fn type_name(interner: &TypeInterner, ty: TypeId) -> String {
    format!("{:?}", interner.resolve(ty))
}

fn edge_types_compatible(interner: &TypeInterner, actual: TypeId, expected: TypeId) -> bool {
    if actual == expected {
        return true;
    }
    let actual = interner.resolve(actual);
    let expected = interner.resolve(expected);
    actual.literal_absorbs(&expected) || expected.literal_absorbs(&actual)
}

/// Native vector IR has a closed type/operation matrix. Reject malformed IR
/// before any backend invokes a verifier or indexes an immediate lane/mask.
fn validate_vector_operations(
    func: &AmirFunc,
    interner: &TypeInterner,
    span: crate::Span,
    diags: &mut Vec<Diagnostic>,
) {
    for stmt in func.stmts.payloads.iter() {
        let AmirStmt::Assign { lhs, rhs } = stmt else {
            continue;
        };
        let Some(result) = func
            .temps
            .get(lhs.as_usize())
            .map(|temp| interner.resolve(temp.ty))
        else {
            continue;
        };
        let operand = |value: &AmirOperand| {
            operand_type(func, value, interner).map(|id| interner.resolve(id))
        };
        let same = |value: &AmirOperand| operand(value).as_ref() == Some(&result);
        let valid = match rhs {
            AmirRvalue::VectorOp { op, left, right } => {
                matches!(result, ArType::Vector(shape) if op.supports(shape))
                    && same(left)
                    && same(right)
            }
            AmirRvalue::VectorCompare { left, right, .. } => {
                result == ArType::Vector(crate::types::VectorType::U8x16)
                    && matches!(operand(left), Some(ArType::Vector(_)))
                    && operand(left) == operand(right)
            }
            AmirRvalue::VectorSelect {
                mask,
                if_true,
                if_false,
            } => {
                matches!(result, ArType::Vector(_))
                    && operand(mask) == Some(ArType::Vector(crate::types::VectorType::U8x16))
                    && same(if_true)
                    && same(if_false)
            }
            AmirRvalue::VectorSplat { value } => {
                matches!(result, ArType::Vector(shape) if operand(value) == Some(ArType::Primitive(shape.lane())))
            }
            AmirRvalue::VectorShuffle { left, right, mask } => {
                matches!(result, ArType::Vector(_))
                    && same(left)
                    && same(right)
                    && mask.iter().all(|index| *index < 32)
            }
            AmirRvalue::VectorExtractLane { vector, lane } => {
                matches!(operand(vector), Some(ArType::Vector(shape)) if *lane < shape.lanes() && result == ArType::Primitive(shape.lane()))
            }
            AmirRvalue::VectorInsertLane {
                vector,
                lane,
                value,
            } => {
                matches!(result, ArType::Vector(shape) if *lane < shape.lanes() && operand(value) == Some(ArType::Primitive(shape.lane())))
                    && same(vector)
            }
            _ => continue,
        };
        if !valid {
            diags.push(Diagnostic::ice(
                DiagCode::ICEGEN002,
                format!(
                    "invalid vector operand, operation or immediate for temp _{} (SIMD-TYPE)",
                    lhs.as_usize()
                ),
                span,
            ));
        }
    }
}
