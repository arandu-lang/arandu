//! Shared AMIR operand visitors (RC-ANALYSIS-LOAD).
//!
//! # Design (single source of truth)
//! Analyses (liveness, move, DCE, …) and backends **must** walk operands through
//! these helpers. Adding a new `AmirRvalue` / `AmirTerminator` field without
//! updating the matching visitor is a compile-time gap only if you use an
//! exhaustive `match` here — keep matches exhaustive (no `_ => {}` for new
//! payload-bearing variants).
//!
//! Historical bugs this pattern prevents:
//! - DCE ignoring `Goto.args` / `Suspend.args` → live values deleted, hang under `--opt`
//! - Analyses that only walked branch conditions but not block-param jump args

use super::stmt::AmirTerminator;
use super::value::{AmirOperand, AmirPlace, AmirProjection, AmirRvalue};

/// Remap the type/symbol domain of an independently produced function. Dense
/// locals, temps, blocks and instructions remain function-local and untouched.
/// Operand/projection traversal stays shared with liveness and literal remaps.
pub fn for_each_function_context_mut(
    function: &mut super::AmirFunc,
    mut ty: impl FnMut(&mut crate::types::TypeId),
    mut symbol: impl FnMut(&mut crate::SymbolId),
) {
    symbol(&mut function.symbol);
    ty(&mut function.return_type);
    for local in &mut function.locals {
        ty(&mut local.ty);
        if let Some(id) = &mut local.symbol {
            symbol(id);
        }
    }
    for temp in &mut function.temps {
        ty(&mut temp.ty);
    }
    for parameter in &mut function.block_params {
        ty(&mut parameter.ty);
    }
    for id in function.stmts.iter_ids().collect::<Vec<_>>() {
        let Some(statement) = function.stmts.get_mut(id) else {
            continue;
        };
        for_each_stmt_operand_mut(statement, |operand| {
            operand_symbol_mut(operand, &mut symbol)
        });
        match statement {
            super::AmirStmt::Assign { rhs, .. } => match rhs {
                AmirRvalue::StructLiteral { struct_symbol, .. } => symbol(struct_symbol),
                AmirRvalue::EnumPayload {
                    variant,
                    field_ty,
                    tuple_ty,
                    ..
                } => {
                    symbol(variant);
                    ty(field_ty);
                    if let Some(tuple) = tuple_ty {
                        ty(tuple);
                    }
                }
                AmirRvalue::Load(place)
                | AmirRvalue::Borrow(place)
                | AmirRvalue::BorrowMut(place) => place_context_mut(place, &mut ty, &mut symbol),
                AmirRvalue::ToStr { src_ty, .. } => ty(src_ty),
                AmirRvalue::BlackBox { value_ty, .. } => ty(value_ty),
                AmirRvalue::CoroutineReady { payload_ty, .. }
                | AmirRvalue::GenInsert { payload_ty, .. }
                | AmirRvalue::GenGet { payload_ty, .. }
                | AmirRvalue::GenSet { payload_ty, .. }
                | AmirRvalue::GenUpsert { payload_ty, .. }
                | AmirRvalue::GenRemove { payload_ty, .. } => ty(payload_ty),
                AmirRvalue::Use(_)
                | AmirRvalue::Binary { .. }
                | AmirRvalue::Unary { .. }
                | AmirRvalue::FieldAccess { .. }
                | AmirRvalue::IndexAccess { .. }
                | AmirRvalue::Array { .. }
                | AmirRvalue::Tuple { .. }
                | AmirRvalue::Discriminant { .. }
                | AmirRvalue::EnumConstruct { .. }
                | AmirRvalue::Len(_)
                | AmirRvalue::SliceData(_)
                | AmirRvalue::SliceView { .. }
                | AmirRvalue::SliceSubslice { .. }
                | AmirRvalue::StrBytes { .. }
                | AmirRvalue::StrView { .. }
                | AmirRvalue::Alloc(_)
                | AmirRvalue::RelativeBorrow { .. }
                | AmirRvalue::StringInterp { .. } => {}
            },
            super::AmirStmt::Store { lhs, .. } | super::AmirStmt::Destroy(lhs) => {
                place_context_mut(lhs, &mut ty, &mut symbol)
            }
            super::AmirStmt::Call { .. }
            | super::AmirStmt::Free(_)
            | super::AmirStmt::StorageLive(_)
            | super::AmirStmt::StorageDead(_)
            | super::AmirStmt::Nop => {}
        }
    }
    for block in &mut function.blocks {
        for_each_terminator_operand_mut(&mut block.terminator, |operand| {
            operand_symbol_mut(operand, &mut symbol)
        });
    }
}

fn operand_symbol_mut(operand: &mut AmirOperand, f: &mut impl FnMut(&mut crate::SymbolId)) {
    match operand {
        AmirOperand::FunctionRef(id) | AmirOperand::GlobalRef(id) => f(id),
        AmirOperand::Copy(_) | AmirOperand::Move(_) | AmirOperand::Constant(_) => {}
    }
}

fn place_context_mut(
    place: &mut AmirPlace,
    ty: &mut impl FnMut(&mut crate::types::TypeId),
    symbol: &mut impl FnMut(&mut crate::SymbolId),
) {
    for projection in &mut place.projections {
        match projection {
            AmirProjection::Field(id) => symbol(id),
            AmirProjection::Payload {
                field_ty, tuple_ty, ..
            } => {
                ty(field_ty);
                if let Some(tuple) = tuple_ty {
                    ty(tuple);
                }
            }
            AmirProjection::Index(_)
            | AmirProjection::Deref
            | AmirProjection::Variant(_)
            | AmirProjection::TupleField(_)
            | AmirProjection::IndexConstant(_) => {}
        }
    }
}

/// Mutable counterpart used when composing independently lowered literal pools.
/// Jump arguments are part of the same contract as ordinary operands.
pub fn for_each_terminator_operand_mut(
    term: &mut AmirTerminator,
    mut f: impl FnMut(&mut AmirOperand),
) {
    match term {
        AmirTerminator::Return | AmirTerminator::Unreachable => {}
        AmirTerminator::Goto { args, .. } => args.iter_mut().for_each(f),
        AmirTerminator::Branch {
            condition,
            true_args,
            false_args,
            ..
        } => {
            f(condition);
            true_args.iter_mut().for_each(&mut f);
            false_args.iter_mut().for_each(f);
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
        } => {
            f(discriminant);
            for (_, _, args) in targets {
                args.iter_mut().for_each(&mut f);
            }
            otherwise.1.iter_mut().for_each(f);
        }
        AmirTerminator::Suspend { future, args, .. } => {
            f(future);
            args.iter_mut().for_each(f);
        }
    }
}

pub fn for_each_place_operand_mut(place: &mut AmirPlace, mut f: impl FnMut(&mut AmirOperand)) {
    for projection in &mut place.projections {
        if let AmirProjection::Index(operand) = projection {
            f(operand);
        }
    }
}

/// Visit every operand, including indices nested inside places. Exhaustive so
/// adding an rvalue cannot silently leave unit-local literal IDs unremapped.
pub fn for_each_rvalue_operand_mut(value: &mut AmirRvalue, mut f: impl FnMut(&mut AmirOperand)) {
    match value {
        AmirRvalue::Use(op)
        | AmirRvalue::Unary { operand: op, .. }
        | AmirRvalue::Len(op)
        | AmirRvalue::SliceData(op)
        | AmirRvalue::StrBytes { source: op }
        | AmirRvalue::StrView { owner: op }
        | AmirRvalue::Alloc(op)
        | AmirRvalue::Discriminant { value: op }
        | AmirRvalue::EnumPayload { value: op, .. }
        | AmirRvalue::FieldAccess { base: op, .. }
        | AmirRvalue::ToStr { value: op, .. }
        | AmirRvalue::BlackBox { value: op, .. }
        | AmirRvalue::CoroutineReady { value: op, .. }
        | AmirRvalue::GenInsert { value: op, .. }
        | AmirRvalue::GenGet { gen_ref: op, .. }
        | AmirRvalue::GenRemove { gen_ref: op, .. } => f(op),
        AmirRvalue::GenSet { gen_ref, value, .. }
        | AmirRvalue::GenUpsert { gen_ref, value, .. } => {
            f(gen_ref);
            f(value);
        }
        AmirRvalue::Binary { left, right, .. }
        | AmirRvalue::IndexAccess {
            base: left,
            index: right,
        } => {
            f(left);
            f(right);
        }
        AmirRvalue::SliceView { owner, data, len } => {
            f(owner);
            f(data);
            f(len);
        }
        AmirRvalue::SliceSubslice { slice, start, len } => {
            f(slice);
            f(start);
            f(len);
        }
        AmirRvalue::EnumConstruct { payload, .. } => {
            if let Some(payload) = payload {
                f(payload);
            }
        }
        AmirRvalue::StructLiteral { fields, .. } => {
            for (_, operand) in fields {
                f(operand);
            }
        }
        AmirRvalue::Array { items }
        | AmirRvalue::Tuple { items }
        | AmirRvalue::StringInterp { parts: items } => items.iter_mut().for_each(f),
        AmirRvalue::Load(place) | AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
            for_each_place_operand_mut(place, f);
        }
        AmirRvalue::RelativeBorrow { .. } => {}
    }
}

pub fn for_each_stmt_operand_mut(
    statement: &mut super::AmirStmt,
    mut f: impl FnMut(&mut AmirOperand),
) {
    match statement {
        super::AmirStmt::Assign { rhs, .. } => for_each_rvalue_operand_mut(rhs, f),
        super::AmirStmt::Store { lhs, rhs } => {
            for_each_place_operand_mut(lhs, &mut f);
            f(rhs);
        }
        super::AmirStmt::Call { callee, args, .. } => {
            f(callee);
            args.iter_mut().for_each(f);
        }
        super::AmirStmt::Free(op) => f(op),
        super::AmirStmt::Destroy(place) => for_each_place_operand_mut(place, f),
        super::AmirStmt::StorageLive(_)
        | super::AmirStmt::StorageDead(_)
        | super::AmirStmt::Nop => {}
    }
}

/// Visit statement operands, including operands nested in place projections.
pub fn for_each_stmt_operand(statement: &super::AmirStmt, mut f: impl FnMut(&AmirOperand)) {
    match statement {
        super::AmirStmt::Assign { rhs, .. } => for_each_rvalue_operand(rhs, f),
        super::AmirStmt::Store { lhs, rhs } => {
            for_each_place_operand(lhs, &mut f);
            f(rhs);
        }
        super::AmirStmt::Call { callee, args, .. } => {
            f(callee);
            args.iter().for_each(f);
        }
        super::AmirStmt::Free(op) => f(op),
        super::AmirStmt::Destroy(place) => for_each_place_operand(place, f),
        super::AmirStmt::StorageLive(_)
        | super::AmirStmt::StorageDead(_)
        | super::AmirStmt::Nop => {}
    }
}

/// Invoke `f` for every operand nested in `place` projections (e.g. index).
pub fn for_each_place_operand(place: &AmirPlace, mut f: impl FnMut(&AmirOperand)) {
    for proj in &place.projections {
        if let AmirProjection::Index(op) = proj {
            f(op);
        }
    }
}

/// Invoke `f` for every operand **used** by a terminator.
///
/// Includes:
/// - control conditions (`Branch.condition`, `SwitchInt.discriminant`, `Suspend.future`)
/// - **all jump args** that feed successor block parameters (`Goto.args`,
///   `Branch.true_args` / `false_args`, `SwitchInt` arm args, `Suspend.args`)
///
/// Omitting jump args is incorrect for SSA/block-param form: those values are
/// live uses even when no statement in the block references them.
pub fn for_each_terminator_operand(term: &AmirTerminator, mut f: impl FnMut(&AmirOperand)) {
    match term {
        AmirTerminator::Return | AmirTerminator::Unreachable => {}
        AmirTerminator::Goto { args, .. } => {
            for a in args {
                f(a);
            }
        }
        AmirTerminator::Branch {
            condition,
            true_args,
            false_args,
            ..
        } => {
            f(condition);
            for a in true_args {
                f(a);
            }
            for a in false_args {
                f(a);
            }
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
        } => {
            f(discriminant);
            for (_, _, args) in targets {
                for a in args {
                    f(a);
                }
            }
            for a in &otherwise.1 {
                f(a);
            }
        }
        AmirTerminator::Suspend { future, args, .. } => {
            f(future);
            for a in args {
                f(a);
            }
        }
    }
}

/// Invoke `f` for every operand used by `rvalue` (not places themselves).
pub fn for_each_rvalue_operand(rvalue: &AmirRvalue, mut f: impl FnMut(&AmirOperand)) {
    match rvalue {
        AmirRvalue::Use(op)
        | AmirRvalue::Unary { operand: op, .. }
        | AmirRvalue::Len(op)
        | AmirRvalue::SliceData(op)
        | AmirRvalue::StrBytes { source: op }
        | AmirRvalue::StrView { owner: op }
        | AmirRvalue::Alloc(op)
        | AmirRvalue::Discriminant { value: op }
        | AmirRvalue::EnumPayload { value: op, .. }
        | AmirRvalue::FieldAccess { base: op, .. }
        | AmirRvalue::ToStr { value: op, .. }
        | AmirRvalue::BlackBox { value: op, .. }
        | AmirRvalue::CoroutineReady { value: op, .. }
        | AmirRvalue::GenInsert { value: op, .. }
        | AmirRvalue::GenGet { gen_ref: op, .. }
        | AmirRvalue::GenRemove { gen_ref: op, .. } => f(op),
        AmirRvalue::GenSet { gen_ref, value, .. }
        | AmirRvalue::GenUpsert { gen_ref, value, .. } => {
            f(gen_ref);
            f(value);
        }

        AmirRvalue::Binary { left, right, .. }
        | AmirRvalue::IndexAccess {
            base: left,
            index: right,
        } => {
            f(left);
            f(right);
        }

        AmirRvalue::SliceView { owner, data, len } => {
            f(owner);
            f(data);
            f(len);
        }
        AmirRvalue::SliceSubslice { slice, start, len } => {
            f(slice);
            f(start);
            f(len);
        }

        AmirRvalue::EnumConstruct { payload, .. } => {
            if let Some(op) = payload {
                f(op);
            }
        }

        AmirRvalue::StructLiteral { fields, .. } => {
            for (_, op) in fields {
                f(op);
            }
        }

        AmirRvalue::Array { items }
        | AmirRvalue::Tuple { items }
        | AmirRvalue::StringInterp { parts: items } => {
            for op in items {
                f(op);
            }
        }

        AmirRvalue::Load(place) | AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
            for_each_place_operand(place, &mut f);
        }

        // A3.4: index only — no nested operands.
        AmirRvalue::RelativeBorrow { .. } => {}
    }
}

/// Invoke `f` for every place nested in `rvalue` (Load/Borrow/BorrowMut).
pub fn for_each_rvalue_place(rvalue: &AmirRvalue, mut f: impl FnMut(&AmirPlace)) {
    match rvalue {
        AmirRvalue::Load(place) | AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
            f(place);
        }
        AmirRvalue::Use(_)
        | AmirRvalue::Binary { .. }
        | AmirRvalue::Unary { .. }
        | AmirRvalue::FieldAccess { .. }
        | AmirRvalue::IndexAccess { .. }
        | AmirRvalue::StructLiteral { .. }
        | AmirRvalue::Array { .. }
        | AmirRvalue::Tuple { .. }
        | AmirRvalue::Discriminant { .. }
        | AmirRvalue::EnumPayload { .. }
        | AmirRvalue::EnumConstruct { .. }
        | AmirRvalue::Len(_)
        | AmirRvalue::SliceData(_)
        | AmirRvalue::SliceView { .. }
        | AmirRvalue::SliceSubslice { .. }
        | AmirRvalue::StrBytes { .. }
        | AmirRvalue::StrView { .. }
        | AmirRvalue::Alloc(_)
        | AmirRvalue::ToStr { .. }
        | AmirRvalue::BlackBox { .. }
        | AmirRvalue::StringInterp { .. }
        | AmirRvalue::RelativeBorrow { .. }
        | AmirRvalue::CoroutineReady { .. }
        | AmirRvalue::GenInsert { .. }
        | AmirRvalue::GenGet { .. }
        | AmirRvalue::GenSet { .. }
        | AmirRvalue::GenUpsert { .. }
        | AmirRvalue::GenRemove { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amir::local::TempId;
    use crate::ops::BinaryOp;

    #[test]
    fn mutable_visitor_rewrites_nested_indices_and_every_successor_operand() {
        use crate::amir::{AmirConstant, AmirStmt, BlockId, LocalId};
        let literal = AmirOperand::Constant(AmirConstant::Pool(crate::literal_pool::LiteralId(0)));
        let mut store = AmirStmt::Store {
            lhs: AmirPlace {
                local: LocalId::from_usize(0),
                projections: vec![AmirProjection::Index(literal)].into(),
            },
            rhs: literal,
        };
        let mut rewritten = 0;
        for_each_stmt_operand_mut(&mut store, |operand| {
            assert_eq!(*operand, literal);
            *operand = AmirOperand::Constant(AmirConstant::Bool(true));
            rewritten += 1;
        });
        assert_eq!(rewritten, 2);
        let block = BlockId::from_usize(1);
        let mut terms = vec![
            AmirTerminator::Goto {
                target: block,
                args: vec![literal],
            },
            AmirTerminator::Branch {
                condition: literal,
                if_true: block,
                true_args: vec![literal],
                if_false: block,
                false_args: vec![literal],
            },
            AmirTerminator::SwitchInt {
                discriminant: literal,
                targets: vec![(1, block, vec![literal])],
                otherwise: (block, vec![literal]),
            },
            AmirTerminator::Suspend {
                future: literal,
                resume: block,
                args: vec![literal],
            },
        ];
        for term in &mut terms {
            let mut before = Vec::new();
            for_each_terminator_operand(term, |op| before.push(*op));
            let mut after = Vec::new();
            for_each_terminator_operand_mut(term, |op| {
                after.push(*op);
                *op = AmirOperand::Constant(AmirConstant::Bool(false));
            });
            assert_eq!(before, after);
            for_each_terminator_operand(term, |op| {
                assert_eq!(*op, AmirOperand::Constant(AmirConstant::Bool(false)))
            });
        }
    }

    #[test]
    fn function_context_visitor_includes_payload_layout_and_jump_symbols() {
        use crate::amir::{AmirBasicBlock, AmirFunc, AmirStmt, AmirStmtTable, BlockId, LocalId};
        use crate::types::TypeId;
        let type_id = TypeId::from_usize(77);
        let symbol_id = crate::SymbolId::new(5, 9);
        let field_id = crate::SymbolId::new(5, 10);
        let mut statements = AmirStmtTable::new();
        statements.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::EnumPayload {
                value: AmirOperand::FunctionRef(symbol_id),
                variant: symbol_id,
                variant_tag: 1,
                index: 0,
                field_ty: type_id,
                tuple_ty: Some(type_id),
            },
        });
        statements.push(AmirStmt::Destroy(AmirPlace {
            local: LocalId::from_usize(0),
            projections: vec![
                AmirProjection::Field(field_id),
                AmirProjection::Payload {
                    variant_tag: 1,
                    index: 0,
                    field_ty: type_id,
                    tuple_ty: Some(type_id),
                },
                AmirProjection::Index(AmirOperand::GlobalRef(symbol_id)),
            ]
            .into(),
        }));
        let mut function = AmirFunc {
            symbol: symbol_id,
            return_type: type_id,
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: Vec::new(),
            block_params: Vec::new(),
            blocks: vec![AmirBasicBlock {
                id: BlockId::from_usize(0),
                params: crate::DenseRange::empty(),
                statements: crate::DenseRange::new(0, 2),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(0),
                    args: vec![AmirOperand::FunctionRef(symbol_id)],
                },
            }],
            stmts: statements,
            cfg: crate::cfg::ControlFlowGraph::default(),
        };
        let mut types = Vec::new();
        let mut symbols = Vec::new();
        for_each_function_context_mut(
            &mut function,
            |ty| {
                types.push(*ty);
                *ty = TypeId::from_usize(88);
            },
            |symbol| {
                symbols.push(*symbol);
                symbol.file_id = 6;
            },
        );
        assert_eq!(types, vec![type_id; 5]);
        assert_eq!(
            symbols,
            vec![
                symbol_id, symbol_id, symbol_id, symbol_id, field_id, symbol_id
            ]
        );
        assert_eq!(function.return_type, TypeId::from_usize(88));
    }

    #[test]
    fn visits_binary_operands() {
        let rv = AmirRvalue::Binary {
            op: BinaryOp::Add,
            left: AmirOperand::Copy(TempId::from_usize(1)),
            right: AmirOperand::Copy(TempId::from_usize(2)),
        };
        let mut temps = Vec::new();
        for_each_rvalue_operand(&rv, |op| {
            if let AmirOperand::Copy(t) = op {
                temps.push(t.as_usize());
            }
        });
        assert_eq!(temps, vec![1, 2]);
    }

    #[test]
    fn visits_string_interp_parts() {
        let rv = AmirRvalue::StringInterp {
            parts: vec![
                AmirOperand::Copy(TempId::from_usize(0)),
                AmirOperand::Copy(TempId::from_usize(3)),
            ],
        };
        let mut n = 0;
        for_each_rvalue_operand(&rv, |_| n += 1);
        assert_eq!(n, 2);
    }

    #[test]
    fn terminator_visits_goto_jump_args() {
        use crate::amir::block::BlockId;
        use crate::amir::stmt::AmirTerminator;
        let term = AmirTerminator::Goto {
            target: BlockId::from_usize(1),
            args: vec![
                AmirOperand::Copy(TempId::from_usize(4)),
                AmirOperand::Copy(TempId::from_usize(7)),
            ],
        };
        let mut temps = Vec::new();
        for_each_terminator_operand(&term, |op| {
            if let AmirOperand::Copy(t) = op {
                temps.push(t.as_usize());
            }
        });
        assert_eq!(temps, vec![4, 7]);
    }

    #[test]
    fn terminator_visits_branch_condition_and_args() {
        use crate::amir::block::BlockId;
        use crate::amir::stmt::AmirTerminator;
        let term = AmirTerminator::Branch {
            condition: AmirOperand::Copy(TempId::from_usize(0)),
            if_true: BlockId::from_usize(1),
            true_args: vec![AmirOperand::Copy(TempId::from_usize(1))],
            if_false: BlockId::from_usize(2),
            false_args: vec![AmirOperand::Copy(TempId::from_usize(2))],
        };
        let mut temps = Vec::new();
        for_each_terminator_operand(&term, |op| {
            if let AmirOperand::Copy(t) = op {
                temps.push(t.as_usize());
            }
        });
        assert_eq!(temps, vec![0, 1, 2]);
    }
}
