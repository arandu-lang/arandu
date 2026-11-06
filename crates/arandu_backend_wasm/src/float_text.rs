//! Text of an `f64` as the runtime's ToStr writes it (WASM.FLOAT): `nan`,
//! `inf`/`-inf`, an integral value below 1e15 as an integer (`-0.0` is `0`),
//! and otherwise the shortest decimal that rounds back to the value, in
//! positional notation, as Rust's `Display` writes it.
//!
//! The shortest digits come from the exact algorithm Rust's `flt2dec` uses as
//! its reference (Dragon4 in its free-format mode, Steele & White): the value
//! and its rounding interval as big integers, one decimal digit per step,
//! stopping at the first prefix inside the interval. It needs no power
//! tables, only 32-bit limb arithmetic, so a module pays for these helpers
//! only when it formats a float. The big integers (40 limbs, as Rust's
//! `Big32x40`) live in a frame on the shadow stack.

use wasm_encoder::{BlockType, Instruction, MemArg, ValType};

use crate::canonical::CabiFunc;
use crate::memory::{GLOBAL_STACK_BASE, GLOBAL_STACK_POINTER, STACK_AREA};

/// Index among [`functions`] of the one callers use:
/// `(value: f64, out: i32) -> i32`: writes the text at `out` followed by a
/// NUL and returns its length, at most [`MAX_TEXT`] bytes.
pub const ENTRY: u32 = 0;
const MUL_SMALL: u32 = 1;
const SHL: u32 = 2;
const CMP: u32 = 3;
const ADD: u32 = 4;
const SUB: u32 = 5;
const MUL_POW10: u32 = 6;

/// The longest text: `-` and `0.` before 323 zeros and 17 digits of the
/// smallest subnormal, whose shortest digits are fewer. The output cell must
/// hold it and the NUL.
pub const MAX_TEXT: i32 = 344;

const LIMBS: i32 = 40;
const BIG: i32 = LIMBS * 4;
const MANT: i32 = 0;
const MINUS: i32 = BIG;
const PLUS: i32 = 2 * BIG;
const SCALE: i32 = 3 * BIG;
const SCALE2: i32 = 4 * BIG;
const SCALE4: i32 = 5 * BIG;
const SCALE8: i32 = 6 * BIG;
const SUM: i32 = 7 * BIG;
const DIGITS: i32 = 8 * BIG;
const FRAME: i32 = DIGITS + 32;

const fn mem(align: u32) -> MemArg {
    MemArg {
        offset: 0,
        align,
        memory_index: 0,
    }
}

/// The helpers, for a module whose first one has Wasm index `first`.
#[must_use]
pub fn functions(first: u32) -> Vec<CabiFunc> {
    vec![
        entry(first),
        mul_small(),
        shl(),
        cmp(),
        add(),
        sub(),
        mul_pow10(first),
    ]
}

fn internal(
    params: Vec<ValType>,
    results: Vec<ValType>,
    locals: Vec<(u32, ValType)>,
    body: Vec<Instruction<'static>>,
) -> CabiFunc {
    CabiFunc {
        export_name: None,
        ty: (params, results),
        extra_locals: locals,
        body,
    }
}

/// `(p, m)`: `p *= m` for a 32-bit `m`.
fn mul_small() -> CabiFunc {
    use Instruction as I;
    let (p, m, i, carry) = (0, 1, 2, 3);
    let body = vec![
        I::Loop(BlockType::Empty),
        I::LocalGet(p),
        I::LocalGet(i),
        I::I32Add,
        I::LocalGet(p),
        I::LocalGet(i),
        I::I32Add,
        I::I64Load32U(mem(2)),
        I::LocalGet(m),
        I::I64ExtendI32U,
        I::I64Mul,
        I::LocalGet(carry),
        I::I64Add,
        I::LocalTee(carry),
        I::I64Store32(mem(2)),
        I::LocalGet(carry),
        I::I64Const(32),
        I::I64ShrU,
        I::LocalSet(carry),
        I::LocalGet(i),
        I::I32Const(4),
        I::I32Add,
        I::LocalTee(i),
        I::I32Const(BIG),
        I::I32LtU,
        I::BrIf(0),
        I::End,
    ];
    internal(
        vec![ValType::I32, ValType::I32],
        vec![],
        vec![(1, ValType::I32), (1, ValType::I64)],
        body,
    )
}

/// `(p, n)`: `p <<= n` bits.
fn shl() -> CabiFunc {
    use Instruction as I;
    let (p, n, bytes, bits, i, value) = (0, 1, 2, 3, 4, 5);
    let body = vec![
        // Whole limbs move up; the vacated low limbs become zero.
        I::LocalGet(n),
        I::I32Const(5),
        I::I32ShrU,
        I::I32Const(2),
        I::I32Shl,
        I::LocalTee(bytes),
        I::If(BlockType::Empty),
        I::LocalGet(p),
        I::LocalGet(bytes),
        I::I32Add,
        I::LocalGet(p),
        I::I32Const(BIG),
        I::LocalGet(bytes),
        I::I32Sub,
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        },
        I::LocalGet(p),
        I::I32Const(0),
        I::LocalGet(bytes),
        I::MemoryFill(0),
        I::End,
        // Then the remaining bits, from the top limb down, so each limb
        // reads the one below it before that one changes.
        I::LocalGet(n),
        I::I32Const(31),
        I::I32And,
        I::LocalTee(bits),
        I::If(BlockType::Empty),
        I::I32Const(BIG - 4),
        I::LocalSet(i),
        I::Loop(BlockType::Empty),
        I::LocalGet(p),
        I::LocalGet(i),
        I::I32Add,
        I::I32Load(mem(2)),
        I::LocalGet(bits),
        I::I32Shl,
        I::LocalSet(value),
        I::LocalGet(i),
        I::If(BlockType::Empty),
        I::LocalGet(value),
        I::LocalGet(p),
        I::LocalGet(i),
        I::I32Add,
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(mem(2)),
        I::I32Const(32),
        I::LocalGet(bits),
        I::I32Sub,
        I::I32ShrU,
        I::I32Or,
        I::LocalSet(value),
        I::End,
        I::LocalGet(p),
        I::LocalGet(i),
        I::I32Add,
        I::LocalGet(value),
        I::I32Store(mem(2)),
        I::LocalGet(i),
        I::If(BlockType::Empty),
        I::LocalGet(i),
        I::I32Const(4),
        I::I32Sub,
        I::LocalSet(i),
        I::Br(1),
        I::End,
        I::End,
        I::End,
    ];
    internal(
        vec![ValType::I32, ValType::I32],
        vec![],
        vec![(4, ValType::I32)],
        body,
    )
}

/// `(a, b) -> -1 | 0 | 1`.
fn cmp() -> CabiFunc {
    use Instruction as I;
    let (a, b, i, x, y) = (0, 1, 2, 3, 4);
    let body = vec![
        I::I32Const(BIG - 4),
        I::LocalSet(i),
        I::Loop(BlockType::Empty),
        I::LocalGet(a),
        I::LocalGet(i),
        I::I32Add,
        I::I32Load(mem(2)),
        I::LocalTee(x),
        I::LocalGet(b),
        I::LocalGet(i),
        I::I32Add,
        I::I32Load(mem(2)),
        I::LocalTee(y),
        I::I32Ne,
        I::If(BlockType::Empty),
        I::I32Const(1),
        I::I32Const(-1),
        I::LocalGet(x),
        I::LocalGet(y),
        I::I32GtU,
        I::Select,
        I::Return,
        I::End,
        I::LocalGet(i),
        I::If(BlockType::Empty),
        I::LocalGet(i),
        I::I32Const(4),
        I::I32Sub,
        I::LocalSet(i),
        I::Br(1),
        I::End,
        I::End,
        I::I32Const(0),
    ];
    internal(
        vec![ValType::I32, ValType::I32],
        vec![ValType::I32],
        vec![(3, ValType::I32)],
        body,
    )
}

/// `(dst, a, b)`: `dst = a + b`; `dst` may be `a`.
fn add() -> CabiFunc {
    use Instruction as I;
    let (dst, a, b, i, carry) = (0, 1, 2, 3, 4);
    let body = vec![
        I::Loop(BlockType::Empty),
        I::LocalGet(dst),
        I::LocalGet(i),
        I::I32Add,
        I::LocalGet(a),
        I::LocalGet(i),
        I::I32Add,
        I::I64Load32U(mem(2)),
        I::LocalGet(b),
        I::LocalGet(i),
        I::I32Add,
        I::I64Load32U(mem(2)),
        I::I64Add,
        I::LocalGet(carry),
        I::I64Add,
        I::LocalTee(carry),
        I::I64Store32(mem(2)),
        I::LocalGet(carry),
        I::I64Const(32),
        I::I64ShrU,
        I::LocalSet(carry),
        I::LocalGet(i),
        I::I32Const(4),
        I::I32Add,
        I::LocalTee(i),
        I::I32Const(BIG),
        I::I32LtU,
        I::BrIf(0),
        I::End,
    ];
    internal(
        vec![ValType::I32, ValType::I32, ValType::I32],
        vec![],
        vec![(1, ValType::I32), (1, ValType::I64)],
        body,
    )
}

/// `(dst, b)`: `dst -= b`, for `dst >= b`.
fn sub() -> CabiFunc {
    use Instruction as I;
    let (dst, b, i, difference) = (0, 1, 2, 3);
    let body = vec![
        I::Loop(BlockType::Empty),
        I::LocalGet(dst),
        I::LocalGet(i),
        I::I32Add,
        I::LocalGet(dst),
        I::LocalGet(i),
        I::I32Add,
        I::I64Load32U(mem(2)),
        I::LocalGet(b),
        I::LocalGet(i),
        I::I32Add,
        I::I64Load32U(mem(2)),
        I::I64Sub,
        // The previous borrow, kept as 0 or 1 in the low bit.
        I::LocalGet(difference),
        I::I64Const(63),
        I::I64ShrU,
        I::I64Sub,
        I::LocalTee(difference),
        I::I64Store32(mem(2)),
        I::LocalGet(i),
        I::I32Const(4),
        I::I32Add,
        I::LocalTee(i),
        I::I32Const(BIG),
        I::I32LtU,
        I::BrIf(0),
        I::End,
    ];
    internal(
        vec![ValType::I32, ValType::I32],
        vec![],
        vec![(1, ValType::I32), (1, ValType::I64)],
        body,
    )
}

/// `(p, n)`: `p *= 10^n`.
fn mul_pow10(first: u32) -> CabiFunc {
    use Instruction as I;
    let (p, n, power) = (0, 1, 2);
    let body = vec![
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty),
        I::LocalGet(n),
        I::I32Const(9),
        I::I32LtU,
        I::BrIf(1),
        I::LocalGet(p),
        I::I32Const(1_000_000_000),
        I::Call(first + MUL_SMALL),
        I::LocalGet(n),
        I::I32Const(9),
        I::I32Sub,
        I::LocalSet(n),
        I::Br(0),
        I::End,
        I::End,
        I::I32Const(1),
        I::LocalSet(power),
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty),
        I::LocalGet(n),
        I::I32Eqz,
        I::BrIf(1),
        I::LocalGet(power),
        I::I32Const(10),
        I::I32Mul,
        I::LocalSet(power),
        I::LocalGet(n),
        I::I32Const(1),
        I::I32Sub,
        I::LocalSet(n),
        I::Br(0),
        I::End,
        I::End,
        I::LocalGet(p),
        I::LocalGet(power),
        I::Call(first + MUL_SMALL),
    ];
    internal(
        vec![ValType::I32, ValType::I32],
        vec![],
        vec![(1, ValType::I32)],
        body,
    )
}

/// Locals of [`entry`] beyond its parameters `value` (0) and `out` (1).
struct EntryLocals {
    len: u32,
    frame: u32,
    saved: u32,
    exp: u32,
    k: u32,
    inclusive: u32,
    digits: u32,
    digit: u32,
    down: u32,
    up: u32,
    index: u32,
    bits: u32,
    mant: u32,
    plus: u32,
}

const LOCALS: EntryLocals = EntryLocals {
    len: 2,
    frame: 3,
    saved: 4,
    exp: 5,
    k: 6,
    inclusive: 7,
    digits: 8,
    digit: 9,
    down: 10,
    up: 11,
    index: 12,
    bits: 13,
    mant: 14,
    plus: 15,
};
const I32_LOCALS: u32 = 11;
const I64_LOCALS: u32 = 3;

fn entry(first: u32) -> CabiFunc {
    use Instruction as I;
    let l = LOCALS;
    let (value, out) = (0, 1);
    let call = |helper: u32| I::Call(first + helper);
    let big = |offset: i32| [I::LocalGet(l.frame), I::I32Const(offset), I::I32Add];
    // `out[len] = byte; len += 1`.
    let put = |byte: u8| {
        [
            I::LocalGet(out),
            I::LocalGet(l.len),
            I::I32Add,
            I::I32Const(i32::from(byte)),
            I::I32Store8(mem(0)),
            I::LocalGet(l.len),
            I::I32Const(1),
            I::I32Add,
            I::LocalSet(l.len),
        ]
    };
    let mut code: Vec<I<'static>> = Vec::new();

    // The whole text, specials included, leaves this block with its length
    // in `len`.
    code.push(I::Block(BlockType::Empty));

    // NaN and infinities.
    code.extend([
        I::LocalGet(value),
        I::LocalGet(value),
        I::F64Ne,
        I::If(BlockType::Empty),
    ]);
    code.extend(put(b'n'));
    code.extend(put(b'a'));
    code.extend(put(b'n'));
    code.extend([I::Br(1), I::End]);
    code.extend([
        I::LocalGet(value),
        I::F64Abs,
        I::F64Const(f64::INFINITY.into()),
        I::F64Eq,
        I::If(BlockType::Empty),
        I::LocalGet(value),
        I::F64Const(0.0.into()),
        I::F64Lt,
        I::If(BlockType::Empty),
    ]);
    code.extend(put(b'-'));
    code.push(I::End);
    code.extend(put(b'i'));
    code.extend(put(b'n'));
    code.extend(put(b'f'));
    code.extend([I::Br(1), I::End]);

    // An integral value below 1e15 is written as an integer.
    code.extend([
        I::LocalGet(value),
        I::F64Trunc,
        I::LocalGet(value),
        I::F64Eq,
        I::LocalGet(value),
        I::F64Abs,
        I::F64Const(1e15.into()),
        I::F64Lt,
        I::I32And,
        I::If(BlockType::Empty),
        I::LocalGet(value),
        I::I64TruncF64S,
        I::LocalTee(l.mant),
        I::I64Const(0),
        I::I64LtS,
        I::If(BlockType::Empty),
    ]);
    code.extend(put(b'-'));
    code.extend([
        I::I64Const(0),
        I::LocalGet(l.mant),
        I::I64Sub,
        I::LocalSet(l.mant),
        I::End,
        // Count the digits, then write them from the last one.
        I::I32Const(1),
        I::LocalSet(l.digits),
        I::LocalGet(l.mant),
        I::LocalSet(l.plus),
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty),
        I::LocalGet(l.plus),
        I::I64Const(10),
        I::I64LtU,
        I::BrIf(1),
        I::LocalGet(l.plus),
        I::I64Const(10),
        I::I64DivU,
        I::LocalSet(l.plus),
        I::LocalGet(l.digits),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(l.digits),
        I::Br(0),
        I::End,
        I::End,
        I::LocalGet(l.len),
        I::LocalGet(l.digits),
        I::I32Add,
        I::LocalTee(l.len),
        I::LocalSet(l.index),
        I::Loop(BlockType::Empty),
        I::LocalGet(l.index),
        I::I32Const(1),
        I::I32Sub,
        I::LocalSet(l.index),
        I::LocalGet(out),
        I::LocalGet(l.index),
        I::I32Add,
        I::LocalGet(l.mant),
        I::I64Const(10),
        I::I64RemU,
        I::I32WrapI64,
        I::I32Const(i32::from(b'0')),
        I::I32Add,
        I::I32Store8(mem(0)),
        I::LocalGet(l.mant),
        I::I64Const(10),
        I::I64DivU,
        I::LocalTee(l.mant),
        I::I64Const(0),
        I::I64Ne,
        I::BrIf(0),
        I::End,
        I::Br(1),
        I::End,
    ]);

    // The shortest digits. Frame on the shadow stack, as functions with
    // frames take it, trapping instead of overrunning the stack area.
    code.extend([
        I::GlobalGet(GLOBAL_STACK_POINTER),
        I::LocalTee(l.saved),
        I::I32Const(FRAME),
        I::I32Sub,
        I::LocalTee(l.frame),
        I::GlobalGet(GLOBAL_STACK_BASE),
        I::I32Const(STACK_AREA),
        I::I32Sub,
        I::I32LtU,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End,
        I::LocalGet(l.frame),
        I::GlobalSet(GLOBAL_STACK_POINTER),
        I::LocalGet(l.frame),
        I::I32Const(0),
        I::I32Const(FRAME),
        I::MemoryFill(0),
    ]);
    code.extend([
        I::LocalGet(value),
        I::F64Const(0.0.into()),
        I::F64Lt,
        I::If(BlockType::Empty),
    ]);
    code.extend(put(b'-'));
    code.push(I::End);

    // Decode as `flt2dec::decoder::decode`: the value is `mant * 2^exp` and
    // its rounding interval is `(mant - minus, mant + plus) * 2^exp`, closed
    // when the significand is even. `minus` is always 1.
    code.extend([
        I::LocalGet(value),
        I::I64ReinterpretF64,
        I::LocalTee(l.bits),
        I::I64Const(52),
        I::I64ShrU,
        I::I32WrapI64,
        I::I32Const(0x7ff),
        I::I32And,
        I::LocalTee(l.exp),
        I::I32Eqz,
        I::If(BlockType::Empty),
        // Subnormal: the significand is shifted once, so it is even.
        I::LocalGet(l.bits),
        I::I64Const(0x000f_ffff_ffff_ffff),
        I::I64And,
        I::I64Const(1),
        I::I64Shl,
        I::LocalSet(l.mant),
        I::I64Const(1),
        I::LocalSet(l.plus),
        I::I32Const(-1075),
        I::LocalSet(l.exp),
        I::I32Const(1),
        I::LocalSet(l.inclusive),
        I::Else,
        I::LocalGet(l.bits),
        I::I64Const(0x000f_ffff_ffff_ffff),
        I::I64And,
        I::I64Const(1 << 52),
        I::I64Or,
        I::LocalTee(l.mant),
        I::I32WrapI64,
        I::I32Const(1),
        I::I32And,
        I::I32Eqz,
        I::LocalSet(l.inclusive),
        I::LocalGet(l.exp),
        I::I32Const(1075),
        I::I32Sub,
        I::LocalSet(l.exp),
        I::LocalGet(l.mant),
        I::I64Const(1 << 52),
        I::I64Eq,
        I::If(BlockType::Empty),
        // A power of two: the gap below is half the gap above.
        I::LocalGet(l.mant),
        I::I64Const(2),
        I::I64Shl,
        I::LocalSet(l.mant),
        I::I64Const(2),
        I::LocalSet(l.plus),
        I::LocalGet(l.exp),
        I::I32Const(2),
        I::I32Sub,
        I::LocalSet(l.exp),
        I::Else,
        I::LocalGet(l.mant),
        I::I64Const(1),
        I::I64Shl,
        I::LocalSet(l.mant),
        I::I64Const(1),
        I::LocalSet(l.plus),
        I::LocalGet(l.exp),
        I::I32Const(1),
        I::I32Sub,
        I::LocalSet(l.exp),
        I::End,
        I::End,
    ]);
    // The big integers; the frame is zeroed, so a low 64-bit store sets one.
    let store_u64 = |offset: i32, local: u32| {
        [
            I::LocalGet(l.frame),
            I::LocalGet(local),
            I::I64Store(MemArg {
                offset: u64::from(offset.unsigned_abs()),
                align: 3,
                memory_index: 0,
            }),
        ]
    };
    code.extend(store_u64(MANT, l.mant));
    code.extend(store_u64(PLUS, l.plus));
    code.extend([
        I::LocalGet(l.frame),
        I::I64Const(1),
        I::I64Store(MemArg {
            offset: MINUS as u64,
            align: 3,
            memory_index: 0,
        }),
        I::LocalGet(l.frame),
        I::I64Const(1),
        I::I64Store(MemArg {
            offset: SCALE as u64,
            align: 3,
            memory_index: 0,
        }),
    ]);
    // `scale = 2^-exp`, or the value and its interval times `2^exp`.
    code.extend([
        I::LocalGet(l.exp),
        I::I32Const(0),
        I::I32LtS,
        I::If(BlockType::Empty),
    ]);
    code.extend(big(SCALE));
    code.extend([
        I::I32Const(0),
        I::LocalGet(l.exp),
        I::I32Sub,
        call(SHL),
        I::Else,
    ]);
    for offset in [MANT, MINUS, PLUS] {
        code.extend(big(offset));
        code.extend([I::LocalGet(l.exp), call(SHL)]);
    }
    code.push(I::End);

    // The decimal exponent `k`, possibly one too small:
    // `estimate_scaling_factor(mant + plus, exp)`.
    code.extend([
        I::I64Const(64),
        I::LocalGet(l.mant),
        I::LocalGet(l.plus),
        I::I64Add,
        I::I64Const(1),
        I::I64Sub,
        I::I64Clz,
        I::I64Sub,
        I::LocalGet(l.exp),
        I::I64ExtendI32S,
        I::I64Add,
        I::I64Const(1_292_913_986),
        I::I64Mul,
        I::I64Const(32),
        I::I64ShrS,
        I::I32WrapI64,
        I::LocalTee(l.k),
        I::I32Const(0),
        I::I32GeS,
        I::If(BlockType::Empty),
    ]);
    code.extend(big(SCALE));
    code.extend([I::LocalGet(l.k), call(MUL_POW10), I::Else]);
    for offset in [MANT, MINUS, PLUS] {
        code.extend(big(offset));
        code.extend([I::I32Const(0), I::LocalGet(l.k), I::I32Sub, call(MUL_POW10)]);
    }
    code.push(I::End);

    // `rounding`: with a closed interval a bound counts as inside it, so
    // `x.cmp(y) < rounding` is `x <= y`; otherwise `x < y`.
    let below = |code: &mut Vec<I<'static>>| {
        // Leaves `cmp < rounding` for the comparison on the stack.
        code.extend([
            I::I32Const(1),
            I::I32Const(0),
            I::LocalGet(l.inclusive),
            I::Select,
            I::I32LtS,
        ]);
    };
    let sum = |code: &mut Vec<I<'static>>| {
        code.extend(big(SUM));
        code.extend(big(MANT));
        code.extend(big(PLUS));
        code.push(call(ADD));
    };

    // Fixup: when the value's upper bound reaches `scale`, `k` was right;
    // otherwise move the digits one place.
    sum(&mut code);
    code.extend(big(SCALE));
    code.extend(big(SUM));
    code.push(call(CMP));
    below(&mut code);
    code.extend([
        I::If(BlockType::Empty),
        I::LocalGet(l.k),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(l.k),
        I::Else,
    ]);
    for offset in [MANT, MINUS, PLUS] {
        code.extend(big(offset));
        code.extend([I::I32Const(10), call(MUL_SMALL)]);
    }
    code.push(I::End);

    // `scale * 2`, `* 4`, `* 8` to extract each digit by subtraction.
    for (to, from) in [(SCALE2, SCALE), (SCALE4, SCALE2), (SCALE8, SCALE4)] {
        code.extend(big(to));
        code.extend(big(from));
        code.extend([
            I::I32Const(BIG),
            I::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            },
        ]);
        code.extend(big(to));
        code.extend([I::I32Const(1), call(SHL)]);
    }

    // One digit per step until a prefix lies inside the interval.
    code.extend([
        I::I32Const(0),
        I::LocalSet(l.digits),
        I::Loop(BlockType::Empty),
    ]);
    code.extend([I::I32Const(0), I::LocalSet(l.digit)]);
    for (scale, weight) in [(SCALE8, 8), (SCALE4, 4), (SCALE2, 2), (SCALE, 1)] {
        code.extend(big(MANT));
        code.extend(big(scale));
        code.extend([
            call(CMP),
            I::I32Const(0),
            I::I32GeS,
            I::If(BlockType::Empty),
        ]);
        code.extend(big(MANT));
        code.extend(big(scale));
        code.extend([
            call(SUB),
            I::LocalGet(l.digit),
            I::I32Const(weight),
            I::I32Add,
            I::LocalSet(l.digit),
            I::End,
        ]);
    }
    code.extend([
        I::LocalGet(l.frame),
        I::LocalGet(l.digits),
        I::I32Add,
        I::LocalGet(l.digit),
        I::I32Const(i32::from(b'0')),
        I::I32Add,
        I::I32Store8(MemArg {
            offset: DIGITS as u64,
            align: 0,
            memory_index: 0,
        }),
        I::LocalGet(l.digits),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(l.digits),
    ]);
    // down: `mant < minus` (or `<=`); up: `scale < mant + plus` (or `<=`).
    code.extend(big(MANT));
    code.extend(big(MINUS));
    code.push(call(CMP));
    below(&mut code);
    code.push(I::LocalSet(l.down));
    sum(&mut code);
    code.extend(big(SCALE));
    code.extend(big(SUM));
    code.push(call(CMP));
    below(&mut code);
    code.extend([
        I::LocalTee(l.up),
        I::LocalGet(l.down),
        I::I32Or,
        I::I32Eqz,
        I::If(BlockType::Empty),
    ]);
    for offset in [MANT, MINUS, PLUS] {
        code.extend(big(offset));
        code.extend([I::I32Const(10), call(MUL_SMALL)]);
    }
    code.extend([I::Br(1), I::End, I::End]);

    // Round the last digit up when only the upper prefix is inside, or both
    // are and the remainder is at least half a unit.
    code.extend([
        I::LocalGet(l.up),
        I::If(BlockType::Empty),
        I::LocalGet(l.down),
    ]);
    code.push(I::If(BlockType::Result(ValType::I32)));
    code.extend(big(MANT));
    code.extend([I::I32Const(1), call(SHL)]);
    code.extend(big(MANT));
    code.extend(big(SCALE));
    code.extend([
        call(CMP),
        I::I32Const(0),
        I::I32GeS,
        I::Else,
        I::I32Const(1),
        I::End,
        I::If(BlockType::Empty),
        // `round_up`: the last digit that is not 9 grows by one and the
        // nines after it become zeros; all nines become `1` and zeros, one
        // digit longer, and `k` grows.
        I::LocalGet(l.digits),
        I::LocalSet(l.index),
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty),
        I::LocalGet(l.index),
        I::I32Eqz,
        I::BrIf(1),
        I::LocalGet(l.frame),
        I::LocalGet(l.index),
        I::I32Add,
        I::I32Const(1),
        I::I32Sub,
        I::I32Load8U(MemArg {
            offset: DIGITS as u64,
            align: 0,
            memory_index: 0,
        }),
        I::I32Const(i32::from(b'9')),
        I::I32Ne,
        I::BrIf(1),
        I::LocalGet(l.frame),
        I::LocalGet(l.index),
        I::I32Add,
        I::I32Const(1),
        I::I32Sub,
        I::I32Const(i32::from(b'0')),
        I::I32Store8(MemArg {
            offset: DIGITS as u64,
            align: 0,
            memory_index: 0,
        }),
        I::LocalGet(l.index),
        I::I32Const(1),
        I::I32Sub,
        I::LocalSet(l.index),
        I::Br(0),
        I::End,
        I::End,
        I::LocalGet(l.index),
        I::If(BlockType::Empty),
        I::LocalGet(l.frame),
        I::LocalGet(l.index),
        I::I32Add,
        I::I32Const(1),
        I::I32Sub,
        I::LocalTee(l.index),
        I::LocalGet(l.index),
        I::I32Load8U(MemArg {
            offset: DIGITS as u64,
            align: 0,
            memory_index: 0,
        }),
        I::I32Const(1),
        I::I32Add,
        I::I32Store8(MemArg {
            offset: DIGITS as u64,
            align: 0,
            memory_index: 0,
        }),
        I::Else,
        I::LocalGet(l.frame),
        I::I32Const(i32::from(b'1')),
        I::I32Store8(MemArg {
            offset: DIGITS as u64,
            align: 0,
            memory_index: 0,
        }),
        I::LocalGet(l.frame),
        I::LocalGet(l.digits),
        I::I32Add,
        I::I32Const(i32::from(b'0')),
        I::I32Store8(MemArg {
            offset: DIGITS as u64,
            align: 0,
            memory_index: 0,
        }),
        I::LocalGet(l.digits),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(l.digits),
        I::LocalGet(l.k),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(l.k),
        I::End,
        I::End,
        I::End,
    ]);

    // Positional notation, as `digits_to_dec_str` without fraction padding:
    // `0.` and `-k` zeros before the digits, a point after `k` of them, or
    // `k - digits` zeros after them.
    let copy_digits = |code: &mut Vec<I<'static>>, from: u32, count: Option<u32>| {
        // `out[len..] = DIGITS[from..from + count]`; `count` defaults to the rest.
        code.extend([
            I::LocalGet(out),
            I::LocalGet(l.len),
            I::I32Add,
            I::LocalGet(l.frame),
            I::I32Const(DIGITS),
            I::I32Add,
            I::LocalGet(from),
            I::I32Add,
        ]);
        match count {
            Some(count) => code.push(I::LocalGet(count)),
            None => code.extend([I::LocalGet(l.digits), I::LocalGet(from), I::I32Sub]),
        }
        code.extend([
            I::LocalTee(l.index),
            I::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            },
            I::LocalGet(l.len),
            I::LocalGet(l.index),
            I::I32Add,
            I::LocalSet(l.len),
        ]);
    };
    // `out[len..len + n] = '0'`, `n` on the stack.
    let zeros = |code: &mut Vec<I<'static>>| {
        code.extend([
            I::LocalSet(l.index),
            I::LocalGet(out),
            I::LocalGet(l.len),
            I::I32Add,
            I::I32Const(i32::from(b'0')),
            I::LocalGet(l.index),
            I::MemoryFill(0),
            I::LocalGet(l.len),
            I::LocalGet(l.index),
            I::I32Add,
            I::LocalSet(l.len),
        ]);
    };
    code.extend([
        I::I32Const(0),
        I::LocalSet(l.digit),
        I::LocalGet(l.k),
        I::I32Const(0),
        I::I32LeS,
        I::If(BlockType::Empty),
    ]);
    code.extend(put(b'0'));
    code.extend(put(b'.'));
    code.extend([I::I32Const(0), I::LocalGet(l.k), I::I32Sub]);
    zeros(&mut code);
    copy_digits(&mut code, l.digit, None);
    code.extend([
        I::Else,
        I::LocalGet(l.k),
        I::LocalGet(l.digits),
        I::I32LtS,
        I::If(BlockType::Empty),
    ]);
    copy_digits(&mut code, l.digit, Some(l.k));
    code.extend(put(b'.'));
    copy_digits(&mut code, l.k, None);
    code.push(I::Else);
    copy_digits(&mut code, l.digit, None);
    code.extend([I::LocalGet(l.k), I::LocalGet(l.digits), I::I32Sub]);
    zeros(&mut code);
    code.extend([I::End, I::End]);

    code.extend([I::LocalGet(l.saved), I::GlobalSet(GLOBAL_STACK_POINTER)]);
    code.push(I::End);

    // NUL after the text, as the other ToStr cells end.
    code.extend([
        I::LocalGet(out),
        I::LocalGet(l.len),
        I::I32Add,
        I::I32Const(0),
        I::I32Store8(mem(0)),
        I::LocalGet(l.len),
    ]);
    internal(
        vec![ValType::F64, ValType::I32],
        vec![ValType::I32],
        vec![(I32_LOCALS, ValType::I32), (I64_LOCALS, ValType::I64)],
        code,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use wasm_encoder::{
        CodeSection, ConstExpr, ExportKind, ExportSection, Function, FunctionSection,
        GlobalSection, GlobalType, MemorySection, MemoryType, Module, TypeSection,
    };

    const OUT: i32 = 0x3_0000;

    /// The runtime's ToStr policy (`arandu_runtime::to_str_runtime`).
    fn reference(value: f64) -> String {
        if value.is_nan() {
            "nan".to_owned()
        } else if value.is_infinite() {
            if value < 0.0 { "-inf" } else { "inf" }.to_owned()
        } else if value.fract() == 0.0 && value.abs() < 1e15 {
            (value as i64).to_string()
        } else {
            format!("{value}")
        }
    }

    /// A module holding only the helpers, with the globals and memory the
    /// emitted modules give them, exporting the entry as `text`.
    fn module() -> Vec<u8> {
        let helpers = functions(0);
        let mut types = TypeSection::new();
        let mut functions = FunctionSection::new();
        let mut code = CodeSection::new();
        for (index, helper) in helpers.iter().enumerate() {
            types
                .ty()
                .function(helper.ty.0.clone(), helper.ty.1.clone());
            functions.function(u32::try_from(index).unwrap());
            let mut body = Function::new(helper.extra_locals.clone());
            for instruction in &helper.body {
                body.instruction(instruction);
            }
            body.instruction(&Instruction::End);
            code.function(&body);
        }
        let mut memory = MemorySection::new();
        memory.memory(MemoryType {
            minimum: 4,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        let mut globals = GlobalSection::new();
        for mutable in [true, false, true, false] {
            globals.global(
                GlobalType {
                    val_type: ValType::I32,
                    mutable,
                    shared: false,
                },
                &ConstExpr::i32_const(crate::memory::STACK_BASE),
            );
        }
        let mut exports = ExportSection::new();
        exports.export("text", ExportKind::Func, ENTRY);
        exports.export("memory", ExportKind::Memory, 0);
        exports.export("stack", ExportKind::Global, GLOBAL_STACK_POINTER);
        let mut module = Module::new();
        module
            .section(&types)
            .section(&functions)
            .section(&memory)
            .section(&globals)
            .section(&exports)
            .section(&code);
        module.finish()
    }

    struct Runner {
        store: wasmtime::Store<()>,
        text: wasmtime::TypedFunc<(f64, i32), i32>,
        memory: wasmtime::Memory,
        stack: wasmtime::Global,
    }

    impl Runner {
        fn new() -> Self {
            let engine = wasmtime::Engine::default();
            let bytes = module();
            wasmparser::validate(&bytes).expect("valid helpers");
            let module = wasmtime::Module::new(&engine, &bytes).unwrap();
            let mut store = wasmtime::Store::new(&engine, ());
            let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
            let text = instance.get_typed_func(&mut store, "text").unwrap();
            let memory = instance.get_memory(&mut store, "memory").unwrap();
            let stack = instance.get_global(&mut store, "stack").unwrap();
            Self {
                store,
                text,
                memory,
                stack,
            }
        }

        fn text(&mut self, value: f64) -> String {
            let len = self.text.call(&mut self.store, (value, OUT)).unwrap();
            assert_eq!(
                self.stack.get(&mut self.store).i32(),
                Some(crate::memory::STACK_BASE),
                "{value:?}: the frame is released"
            );
            let len = usize::try_from(len).unwrap();
            assert!(
                len < usize::try_from(MAX_TEXT).unwrap(),
                "{value:?}: {len} bytes"
            );
            let start = usize::try_from(OUT).unwrap();
            let bytes = &self.memory.data(&self.store)[start..start + len + 1];
            assert_eq!(bytes[len], 0, "{value:?}: NUL after the text");
            String::from_utf8(bytes[..len].to_vec()).unwrap()
        }
    }

    fn check(runner: &mut Runner, value: f64) {
        assert_eq!(
            runner.text(value),
            reference(value),
            "bits {:#018x}",
            value.to_bits()
        );
    }

    #[test]
    fn edge_values_match_the_runtime() {
        let mut runner = Runner::new();
        for value in [
            0.0,
            -0.0,
            1.0,
            -3.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.1,
            0.1 + 0.2,
            1.5,
            -2.75,
            123.456,
            1e15,
            1e16,
            1e21,
            1e22,
            1e23,
            9_007_199_254_740_993.0,
            999_999_999_999_999.9,
            0.000_000_1,
            1e-7,
            5e-324,
            -5e-324,
            f64::MIN_POSITIVE,
            f64::MIN_POSITIVE / 2.0,
            f64::MAX,
            f64::MIN,
            f64::EPSILON,
            std::f64::consts::PI,
            std::f64::consts::E,
            2f64.powi(-1022),
            2f64.powi(1023),
            2f64.powi(-1074),
            2f64.powi(60),
            0.3,
            2.5,
            1.0 / 3.0,
        ] {
            check(&mut runner, value);
        }
        for exponent in -1074..=1023 {
            check(&mut runner, 2f64.powi(exponent));
            check(&mut runner, -(2f64.powi(exponent)) * 1.5);
        }
        for power in -323..=308 {
            check(&mut runner, format!("1e{power}").parse().unwrap());
            check(
                &mut runner,
                format!("9.999999999999999e{power}").parse().unwrap(),
            );
        }
    }

    /// Bit patterns from a fixed xorshift sequence, over every exponent.
    #[test]
    fn random_bit_patterns_match_the_runtime() {
        let mut runner = Runner::new();
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..200_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            check(&mut runner, f64::from_bits(state));
        }
    }

    /// Decimal literals with few digits, whose shortest text is the literal.
    #[test]
    fn short_decimals_round_trip_as_written() {
        let mut runner = Runner::new();
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        for _ in 0..50_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let digits = state % 1_000_000;
            let exponent = i32::try_from((state >> 40) % 40).unwrap() - 20;
            check(&mut runner, format!("{digits}e{exponent}").parse().unwrap());
        }
    }
}
