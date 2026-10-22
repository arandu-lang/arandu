//! Shared typed AMIR cases for native SIMD backend contract tests.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use arandu_base::Span;
use arandu_semantics::amir::{
    AmirBasicBlock, AmirConstant, AmirFunc, AmirLocal, AmirOperand, AmirPlace, AmirProgram,
    AmirRvalue, AmirStmt, AmirStmtTable, AmirTemp, AmirTerminator, BlockId, LocalId, TempId,
    VectorOp,
};
use arandu_semantics::cfg::compute_cfg_edges;
use arandu_semantics::layout::DenseRange;
use arandu_semantics::literal_pool::AmirLiteralPool;
use arandu_semantics::symbol_table::{ScopeId, Symbol, SymbolKind};
use arandu_semantics::types::{ArType, TypeId, VectorType};
use arandu_semantics::{SymbolId, SymbolTable, TypeInfo, Visibility};

pub struct Fixture {
    pub program: AmirProgram,
    pub symbols: SymbolTable,
    pub info: TypeInfo,
}

pub fn symbol(index: u32) -> SymbolId {
    SymbolId::new(999, index)
}

fn copy(index: usize) -> AmirOperand {
    AmirOperand::Copy(TempId::from_usize(index))
}

fn assign(stmts: &mut AmirStmtTable, index: usize, rhs: AmirRvalue) {
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(index),
        rhs,
    });
}

fn scalar(stmts: &mut AmirStmtTable, pool: &mut AmirLiteralPool, index: usize, value: &str) {
    assign(
        stmts,
        index,
        AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(
            pool.intern_int(value),
        ))),
    );
}

fn function(index: u32, result: TypeId, types: &[TypeId], stmts: AmirStmtTable) -> AmirFunc {
    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, stmts.payloads.len()),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    }];
    AmirFunc {
        symbol: symbol(index),
        return_type: result,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: types
            .iter()
            .enumerate()
            .map(|(index, ty)| AmirTemp {
                id: TempId::from_usize(index),
                ty: *ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            })
            .collect(),
        cfg: compute_cfg_edges(&blocks),
        blocks,
        block_params: vec![],
        stmts,
    }
}

fn fixture(funcs: Vec<AmirFunc>, pool: AmirLiteralPool, info: TypeInfo) -> Fixture {
    let mut symbols = SymbolTable::new(0);
    for function in &funcs {
        symbols.imported_symbols.insert(
            function.symbol,
            Symbol {
                id: function.symbol,
                name: if function.symbol == symbol(0) {
                    "main".into()
                } else {
                    "helper".into()
                },
                kind: SymbolKind::Func,
                span: Span::new(0, 0, 0),
                scope: ScopeId(0),
                visibility: if function.symbol == symbol(0) {
                    Visibility::Public
                } else {
                    Visibility::Private
                },
                lang_item: None,
            },
        );
    }
    Fixture {
        program: AmirProgram {
            funcs,
            literal_pool: pool,
            extern_funcs: Default::default(),
            debug_bindings: vec![],
            debug_blocks: vec![],
        },
        symbols,
        info,
    }
}

pub fn arithmetic(shape: VectorType, op: VectorOp) -> Fixture {
    arithmetic_values(shape, op, "7", "3")
}

pub fn arithmetic_values(shape: VectorType, op: VectorOp, left: &str, right: &str) -> Fixture {
    let info = TypeInfo::default();
    let vector = info.type_interner.intern(ArType::Vector(shape));
    let lane = info.type_interner.intern(ArType::Primitive(shape.lane()));
    let mut pool = AmirLiteralPool::default();
    let mut stmts = AmirStmtTable::new();
    scalar(&mut stmts, &mut pool, 4, left);
    scalar(&mut stmts, &mut pool, 5, right);
    assign(&mut stmts, 1, AmirRvalue::VectorSplat { value: copy(4) });
    assign(&mut stmts, 2, AmirRvalue::VectorSplat { value: copy(5) });
    assign(
        &mut stmts,
        3,
        AmirRvalue::VectorOp {
            op,
            left: copy(1),
            right: copy(2),
        },
    );
    assign(
        &mut stmts,
        0,
        AmirRvalue::VectorExtractLane {
            vector: copy(3),
            lane: shape.lanes() - 1,
        },
    );
    let main = function(0, lane, &[lane, vector, vector, vector, lane, lane], stmts);
    fixture(vec![main], pool, info)
}

pub fn memory_shuffle(shape: VectorType) -> Fixture {
    let info = TypeInfo::default();
    let vector = info.type_interner.intern(ArType::Vector(shape));
    let lane = info.type_interner.intern(ArType::Primitive(shape.lane()));
    let mut pool = AmirLiteralPool::default();
    let place = AmirPlace {
        local: LocalId::from_usize(0),
        projections: Default::default(),
    };
    let mut stmts = AmirStmtTable::new();
    scalar(&mut stmts, &mut pool, 8, "7");
    scalar(&mut stmts, &mut pool, 9, "42");
    assign(&mut stmts, 1, AmirRvalue::VectorSplat { value: copy(8) });
    stmts.push(AmirStmt::Store {
        lhs: place.clone(),
        rhs: copy(1),
    });
    assign(&mut stmts, 2, AmirRvalue::Load(place));
    assign(
        &mut stmts,
        3,
        AmirRvalue::VectorInsertLane {
            vector: copy(2),
            lane: shape.lanes() - 1,
            value: copy(9),
        },
    );
    let lane_bytes = 16 / shape.lanes();
    let mut mask = std::array::from_fn(|byte| 16 + u8::try_from(byte).unwrap());
    for byte in 0..lane_bytes {
        mask[usize::from(byte)] = 16 - lane_bytes + byte;
    }
    assign(
        &mut stmts,
        4,
        AmirRvalue::VectorShuffle {
            left: copy(3),
            right: copy(2),
            mask,
        },
    );
    assign(
        &mut stmts,
        5,
        AmirRvalue::BlackBox {
            value: copy(4),
            value_ty: vector,
        },
    );
    assign(
        &mut stmts,
        6,
        AmirRvalue::VectorExtractLane {
            vector: copy(5),
            lane: 0,
        },
    );
    assign(
        &mut stmts,
        7,
        AmirRvalue::VectorExtractLane {
            vector: copy(5),
            lane: shape.lanes() - 1,
        },
    );
    assign(
        &mut stmts,
        0,
        AmirRvalue::Binary {
            op: arandu_semantics::ops::BinaryOp::Add,
            left: copy(6),
            right: copy(7),
        },
    );
    let mut main = function(
        0,
        lane,
        &[
            lane, vector, vector, vector, vector, vector, lane, lane, lane, lane,
        ],
        stmts,
    );
    main.locals.push(AmirLocal {
        id: LocalId::from_usize(0),
        ty: vector,
        is_memory: true,
        symbol: None,
        span: Span::new(0, 0, 0),
        use_span: None,
    });
    fixture(vec![main], pool, info)
}

pub fn function_abi(shape: VectorType) -> Fixture {
    let info = TypeInfo::default();
    let vector = info.type_interner.intern(ArType::Vector(shape));
    let lane = info.type_interner.intern(ArType::Primitive(shape.lane()));
    let mut pool = AmirLiteralPool::default();
    let mut stmts = AmirStmtTable::new();
    scalar(&mut stmts, &mut pool, 3, "7");
    assign(&mut stmts, 1, AmirRvalue::VectorSplat { value: copy(3) });
    stmts.push(AmirStmt::Call {
        lhs: Some(TempId::from_usize(2)),
        callee: AmirOperand::FunctionRef(symbol(1)),
        args: vec![copy(1)].into(),
        return_borrow: None,
    });
    assign(
        &mut stmts,
        0,
        AmirRvalue::VectorExtractLane {
            vector: copy(2),
            lane: 0,
        },
    );
    let main = function(0, lane, &[lane, vector, vector, lane], stmts);
    let mut stmts = AmirStmtTable::new();
    assign(
        &mut stmts,
        0,
        AmirRvalue::VectorOp {
            op: VectorOp::Add,
            left: copy(1),
            right: copy(1),
        },
    );
    let mut helper = function(1, vector, &[vector, vector], stmts);
    helper.params.push(TempId::from_usize(1));
    fixture(vec![main, helper], pool, info)
}

pub fn vector_boundary(shape: VectorType) -> Fixture {
    let info = TypeInfo::default();
    let vector = info.type_interner.intern(ArType::Vector(shape));
    let mut stmts = AmirStmtTable::new();
    assign(
        &mut stmts,
        0,
        AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil)),
    );
    fixture(
        vec![function(0, vector, &[vector], stmts)],
        Default::default(),
        info,
    )
}

pub const OPERATORS: [(VectorOp, f64); 7] = [
    (VectorOp::Add, 10.0),
    (VectorOp::Sub, 4.0),
    (VectorOp::Mul, 21.0),
    (VectorOp::Div, 7.0 / 3.0),
    (VectorOp::BitAnd, 3.0),
    (VectorOp::BitOr, 7.0),
    (VectorOp::BitXor, 4.0),
];

pub const INTEGER_BOUNDARIES: [(VectorType, VectorOp, &str, &str, f64); 8] = [
    (VectorType::U8x16, VectorOp::Add, "255", "1", 0.0),
    (VectorType::U8x16, VectorOp::Sub, "0", "1", 255.0),
    (VectorType::I16x8, VectorOp::Add, "32767", "1", -32768.0),
    (VectorType::I16x8, VectorOp::Mul, "30000", "3", 24464.0),
    (
        VectorType::I32x4,
        VectorOp::Add,
        "2147483647",
        "1",
        -2147483648.0,
    ),
    (
        VectorType::I32x4,
        VectorOp::Mul,
        "100000",
        "100000",
        1410065408.0,
    ),
    (
        VectorType::I64x2,
        VectorOp::Add,
        "9223372036854775807",
        "1",
        -9223372036854775808.0,
    ),
    (
        VectorType::I64x2,
        VectorOp::Mul,
        "4611686018427387904",
        "4",
        0.0,
    ),
];

/// Compare mixed unequal/equal lanes, then select distinct values bit-for-bit.
pub fn comparison_select(
    shape: VectorType,
    op: arandu_semantics::amir::VectorCompareOp,
    left: &str,
    right: &str,
) -> Fixture {
    let info = TypeInfo::default();
    let vector = info.type_interner.intern(ArType::Vector(shape));
    let mask = info.type_interner.intern(ArType::Vector(VectorType::U8x16));
    let lane = info.type_interner.intern(ArType::Primitive(shape.lane()));
    let mut pool = AmirLiteralPool::default();
    let mut stmts = AmirStmtTable::new();
    scalar(&mut stmts, &mut pool, 1, left);
    scalar(&mut stmts, &mut pool, 2, right);
    scalar(&mut stmts, &mut pool, 3, "11");
    scalar(&mut stmts, &mut pool, 4, "19");
    assign(&mut stmts, 5, AmirRvalue::VectorSplat { value: copy(1) });
    assign(&mut stmts, 6, AmirRvalue::VectorSplat { value: copy(2) });
    assign(
        &mut stmts,
        7,
        AmirRvalue::VectorInsertLane {
            vector: copy(5),
            lane: shape.lanes() - 1,
            value: copy(2),
        },
    );
    assign(
        &mut stmts,
        8,
        AmirRvalue::VectorCompare {
            op,
            left: copy(7),
            right: copy(6),
        },
    );
    assign(&mut stmts, 9, AmirRvalue::VectorSplat { value: copy(3) });
    assign(&mut stmts, 10, AmirRvalue::VectorSplat { value: copy(4) });
    assign(
        &mut stmts,
        11,
        AmirRvalue::VectorSelect {
            mask: copy(8),
            if_true: copy(9),
            if_false: copy(10),
        },
    );
    assign(
        &mut stmts,
        12,
        AmirRvalue::VectorExtractLane {
            vector: copy(11),
            lane: 0,
        },
    );
    assign(
        &mut stmts,
        13,
        AmirRvalue::VectorExtractLane {
            vector: copy(11),
            lane: shape.lanes() - 1,
        },
    );
    assign(
        &mut stmts,
        0,
        AmirRvalue::Binary {
            op: arandu_semantics::ops::BinaryOp::Add,
            left: copy(12),
            right: copy(13),
        },
    );
    let main = function(
        0,
        lane,
        &[
            lane, lane, lane, lane, lane, vector, vector, vector, mask, vector, vector, vector,
            lane, lane,
        ],
        stmts,
    );
    fixture(vec![main], pool, info)
}

pub const COMPARISONS: [(arandu_semantics::amir::VectorCompareOp, f64); 6] = {
    use arandu_semantics::amir::VectorCompareOp::*;
    [
        (Eq, 30.0),
        (Ne, 30.0),
        (Lt, 38.0),
        (Le, 30.0),
        (Gt, 30.0),
        (Ge, 22.0),
    ]
};
