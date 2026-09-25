//! Type definitions, constants, and RNG for synthesized Arandu programs.

pub const MAX_DEPTH: u8 = 6;
pub const MAX_BASE: u64 = 31;
pub const MAX_LOOP_TRIPS: u64 = 4;
pub const SYNTH_FAILURE_CODE_BASE: i64 = -1_000_000;
pub const SYNTH_FAILURE_CODE_COUNT: i64 = 57;
pub const MAX_INT_EXPR_MAGNITUDE: i64 = {
    let mut magnitude = MAX_BASE as i64;
    let mut depth = 0;
    while depth < MAX_DEPTH {
        magnitude *= 3;
        depth += 1;
    }
    magnitude
};
pub const MAX_SYNTH_RESULT_MAGNITUDE: i64 = {
    let expression_result = MAX_INT_EXPR_MAGNITUDE * 3 + 8;
    let loop_result =
        MAX_INT_EXPR_MAGNITUDE + (MAX_LOOP_TRIPS as i64 * (MAX_LOOP_TRIPS as i64 + 1) / 2);
    if expression_result > loop_result {
        expression_result
    } else {
        loop_result
    }
};
const _: () = assert!(SYNTH_FAILURE_CODE_BASE < -MAX_SYNTH_RESULT_MAGNITUDE);
const _: () = assert!(SYNTH_FAILURE_CODE_BASE - SYNTH_FAILURE_CODE_COUNT + 1 >= i32::MIN as i64);
pub const SYNTHESIZED_PROGRAM_ARGS: [&str; 2] = ["", "argument-two"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ty {
    Int,
    UInt,
    Bool,
    Float,
    Char,
}

impl Ty {
    pub fn source_name(self) -> &'static str {
        match self {
            Self::Int => "int",
            Self::UInt => "uint",
            Self::Bool => "bool",
            Self::Float => "float",
            Self::Char => "char",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        let state = seed ^ 0x9e37_79b9_7f4a_7c15;
        // xorshift's all-zero state is absorbing. It is reachable from one
        // perfectly valid 64-bit fuzz seed, so map it to a fixed nonzero state.
        Self(if state == 0 {
            0xa076_1d64_78bd_642f
        } else {
            state
        })
    }

    pub fn next(&mut self) -> u64 {
        // xorshift64*: fixed algorithm and no platform-dependent state.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ConstantValue {
    Int(i64),
    UInt(u64),
    Bool(bool),
    Float(f64),
    Char(char),
}

pub fn char_literal(value: char) -> String {
    arandu_lexer::char_literal(value)
}

pub fn float_literal(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        value.to_string()
    }
}
