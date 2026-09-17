//! Per-function translator from AMIR + stackifier ops → wasm instructions.
//!
//! The translator consumes the flat [`crate::stackify::Op`] stream produced by
//! [`crate::stackify::stackify`] and emits actual wasm instructions into
//! a `wasm_encoder::Function` buffer. No CFG analysis is performed here — all
//! structural decisions (loops, blocks, depths) were made by the stackifier.
//!
//! Responsibilities are split by concern:
//! * `locals` — wasm local allocation and Canonical ABI parameter flattening.
//! * `body` — statement, block-argument and epilogue emission.
//! * the operator submodules (`operand`, `place`, `arith`, `aggregate`,
//!   `call`, `string`, `expr`) — one AMIR construct family each.

mod aggregate;
mod arith;
mod body;
mod call;
pub mod expr;
mod locals;
mod operand;
mod place;
mod string;

pub use locals::{FlatFieldStore, ParamUnpack};

use arandu_middle::SymbolId;
use arandu_middle::amir::AmirFunc;
use arandu_middle::amir::local::{LocalId, TempId};
use arandu_middle::layout::{DataLayout, LayoutEngine, StructLayoutProvider, TypeLayout};
use arandu_middle::literal_pool::AmirLiteralPool;
use arandu_middle::types::{TypeId, TypeInterner};
use rustc_hash::FxHashMap;
use wasm_encoder::{BlockType, Function, Instruction, ValType};

use arandu_semantics::SymbolTable;

use crate::types::{self, Shape};

/// Per-function translation context.
pub(super) struct FuncTranslator<'a> {
    pub func: &'a AmirFunc,
    pub symbols: &'a SymbolTable,
    pub interner: &'a TypeInterner,
    pub layout_provider: &'a dyn StructLayoutProvider,
    pub layout_engine: LayoutEngine,
    pub literal_pool: &'a AmirLiteralPool,
    pub rodata_offsets: &'a FxHashMap<arandu_middle::literal_pool::LiteralId, u32>,
    pub func_index_map: &'a FxHashMap<SymbolId, u32>,

    /// Maps each AMIR temp to its first wasm local index.
    pub temp_local: FxHashMap<TempId, u32>,

    /// Maps each AMIR local to its first wasm local index. Locals and temps are
    /// distinct index spaces in the AMIR, so they must not share wasm slots.
    pub local_wasm: FxHashMap<LocalId, u32>,

    /// Next available wasm local index (starts after all param + temp + block_param locals).
    pub next_local: u32,

    /// Scratch local index (i32, for bump allocator temporaries).
    pub scratch: u32,

    /// Whether this function is exported through the Component Model Canonical
    /// ABI with a *flattened* (single `i32` return-pointer) result, per
    /// `MAX_FLAT_RESULTS = 1`.
    pub retptr_return: bool,

    /// If this function is exported through the Component Model Canonical ABI,
    /// its exact Canonical ABI signature as computed by `Resolve::wasm_signature`.
    pub component_sig: Option<&'a wit_parser::abi::WasmSignature>,

    /// Aggregate parameters that need to be unpacked in the prologue.
    pub param_unpacks: Vec<ParamUnpack>,

    /// Scratch locals for fat (ptr, len) cell stores: address / ptr / len are
    /// all i32 words. They let the store helper reorder its input stack
    /// `[addr, ptr, len]` without disturbing the cell pointer kept in `scratch`.
    pub scratch_b: u32,
    pub scratch_c: u32,
    pub scratch_d: u32,

    /// Scratch locals (i64) for 64-bit logical `And`/`Or` normalization.
    pub scratch_i64: u32,
    pub scratch_i64b: u32,

    /// Scratch local (f64) for floating-point operations and ToStr formatting.
    pub scratch_f64: u32,

    /// The wasm instruction buffer.
    pub code: Vec<Instruction<'static>>,

    /// Wasm function index of the internal `__arandu_alloc(size) -> i32`.
    pub alloc_func_idx: u32,
    /// Wasm function index of the internal `__arandu_free(ptr)`.
    pub free_func_idx: u32,
}

/// Number of wasm locals a shape occupies.
#[must_use]
pub fn shape_slots(shape: Shape) -> u32 {
    match shape {
        Shape::Empty => 0,
        Shape::Scalar => 1,
        Shape::Fat => 2,
    }
}

/// Global module-level translation context passed to each function translator.
pub struct TranslationContext<'a> {
    pub symbols: &'a SymbolTable,
    pub interner: &'a TypeInterner,
    pub layout_provider: &'a dyn StructLayoutProvider,
    pub data_layout: DataLayout,
    pub literal_pool: &'a AmirLiteralPool,
    pub rodata_offsets: &'a FxHashMap<arandu_middle::literal_pool::LiteralId, u32>,
    pub func_index_map: &'a FxHashMap<SymbolId, u32>,
    /// Wasm function index of the internal `__arandu_alloc(size) -> i32`.
    pub alloc_func_idx: u32,
    /// Wasm function index of the internal `__arandu_free(ptr)`.
    pub free_func_idx: u32,
}

impl<'a> FuncTranslator<'a> {
    pub fn new(
        func: &'a AmirFunc,
        ctx: &'a TranslationContext<'a>,
        component_sig: Option<&'a wit_parser::abi::WasmSignature>,
    ) -> Self {
        let retptr_return = component_sig.is_some_and(|s| s.retptr);
        Self {
            func,
            symbols: ctx.symbols,
            interner: ctx.interner,
            layout_provider: ctx.layout_provider,
            layout_engine: LayoutEngine::from_data_layout(ctx.data_layout),
            literal_pool: ctx.literal_pool,
            rodata_offsets: ctx.rodata_offsets,
            func_index_map: ctx.func_index_map,
            temp_local: FxHashMap::default(),
            local_wasm: FxHashMap::default(),
            next_local: 0,
            scratch: 0,
            scratch_b: 0,
            scratch_c: 0,
            scratch_d: 0,
            scratch_i64: 0,
            scratch_i64b: 0,
            scratch_f64: 0,
            retptr_return,
            component_sig,
            param_unpacks: Vec::new(),
            code: Vec::new(),
            alloc_func_idx: ctx.alloc_func_idx,
            free_func_idx: ctx.free_func_idx,
        }
    }

    /// Compute the checked physical layout of a type, falling back to a
    /// zero-size layout for shapes the wasm MVP does not materialize.
    #[must_use]
    fn layout_of(&self, ty: &arandu_middle::types::ArType) -> TypeLayout {
        self.layout_engine
            .layout_of_type(ty, self.interner, self.layout_provider)
            .unwrap_or_else(|_| TypeLayout {
                size: 0,
                align: 1,
                field_offsets: Vec::new(),
            })
    }

    /// Resolve a `TypeId` and compute its checked layout.
    #[must_use]
    pub(super) fn layout_of_id(&self, ty: TypeId) -> TypeLayout {
        self.layout_of(&self.interner.resolve(ty))
    }

    /// First wasm local index backing an AMIR local, if it was allocated.
    #[must_use]
    pub(super) fn local_slot(&self, local: LocalId) -> Option<u32> {
        self.local_wasm.get(&local).copied()
    }

    /// Translate the function body from stackifier ops into a wasm [`Function`].
    pub fn translate(&mut self, ops: Vec<crate::stackify::Op>) -> Function {
        use crate::stackify::Op;

        // Pre-pass: assign locals.
        let extra_locals = self.allocate_locals();

        // Emit prologue: unpack flattened parameters into memory cells if needed
        let param_unpacks = std::mem::take(&mut self.param_unpacks);
        for unpack in &param_unpacks {
            self.alloc_cell(unpack.cell_size as i32);
            self.code.push(Instruction::LocalGet(self.scratch));
            self.code.push(Instruction::LocalSet(self.scratch_b));

            for field in &unpack.fields {
                self.code.push(Instruction::LocalGet(self.scratch_b));
                self.code.push(Instruction::LocalGet(field.wasm_param_slot));
                match field.val_type {
                    ValType::I32 => self.code.push(Instruction::I32Store(wasm_encoder::MemArg {
                        offset: field.offset as u64,
                        align: crate::memory::MEM_ALIGN,
                        memory_index: 0,
                    })),
                    ValType::I64 => self.code.push(Instruction::I64Store(wasm_encoder::MemArg {
                        offset: field.offset as u64,
                        align: crate::memory::MEM_ALIGN,
                        memory_index: 0,
                    })),
                    ValType::F32 => self.code.push(Instruction::F32Store(wasm_encoder::MemArg {
                        offset: field.offset as u64,
                        align: crate::memory::MEM_ALIGN,
                        memory_index: 0,
                    })),
                    ValType::F64 => self.code.push(Instruction::F64Store(wasm_encoder::MemArg {
                        offset: field.offset as u64,
                        align: crate::memory::MEM_ALIGN,
                        memory_index: 0,
                    })),
                    _ => self.code.push(Instruction::I32Store(wasm_encoder::MemArg {
                        offset: field.offset as u64,
                        align: crate::memory::MEM_ALIGN,
                        memory_index: 0,
                    })),
                }
            }

            self.code.push(Instruction::LocalGet(self.scratch_b));
            self.code.push(Instruction::LocalSet(unpack.target_local));
        }

        // Walk the op stream.
        for op in ops {
            match op {
                Op::LoopBegin { .. } => {
                    self.code.push(Instruction::Loop(BlockType::Empty));
                }
                Op::LoopEnd => {
                    self.code.push(Instruction::End);
                }
                Op::BlockBegin { .. } => {
                    self.code.push(Instruction::Block(BlockType::Empty));
                }
                Op::BlockEnd { .. } => {
                    self.code.push(Instruction::End);
                }
                Op::BlockContent { block } => {
                    self.emit_block_content(block);
                }
                Op::ArgStore { target, args } => {
                    self.emit_block_args(target, &args);
                }
                Op::Br {
                    depth,
                    target,
                    args,
                } => {
                    self.emit_block_args(target, &args);
                    self.code.push(Instruction::Br(depth));
                }
                Op::BrIf {
                    depth,
                    target,
                    args,
                    condition,
                    invert,
                } => {
                    let bool_ty = self
                        .interner
                        .intern(arandu_middle::types::ArType::Primitive(
                            arandu_middle::types::Primitive::Bool,
                        ));
                    self.emit_operand(&condition, bool_ty);
                    if invert {
                        self.code.push(Instruction::I32Eqz);
                    }
                    self.emit_block_args(target, &args);
                    self.code.push(Instruction::BrIf(depth));
                }
                Op::BrIfEq {
                    depth,
                    target,
                    args,
                    discriminant,
                    value,
                } => {
                    let disc_ty = self.operand_arity_ty(&discriminant);
                    if types::ar_is_64bit(disc_ty, self.interner, self.layout_engine.data_layout) {
                        self.emit_operand(&discriminant, disc_ty);
                        self.code.push(Instruction::I64Const(value as i64));
                        self.code.push(Instruction::I64Eq);
                    } else {
                        self.emit_operand(&discriminant, disc_ty);
                        self.code.push(Instruction::I32Const(value as i32));
                        self.code.push(Instruction::I32Eq);
                    }
                    self.emit_block_args(target, &args);
                    self.code.push(Instruction::BrIf(depth));
                }
                Op::BrTable {
                    depths,
                    default_depth,
                    discriminant,
                    otherwise,
                    args,
                } => {
                    let int_ty = self
                        .interner
                        .intern(arandu_middle::types::ArType::Primitive(
                            arandu_middle::types::Primitive::Int,
                        ));
                    self.emit_operand(&discriminant, int_ty);
                    self.emit_block_args(otherwise, &args);
                    self.code.push(Instruction::BrTable(
                        std::borrow::Cow::Owned(depths),
                        default_depth,
                    ));
                }
                Op::Return => {
                    self.emit_return();
                    self.code.push(Instruction::Return);
                }
                Op::Trap => {
                    self.code.push(Instruction::Unreachable);
                }
            }
        }

        // Build the wasm Function.
        let mut wasm_func = Function::new(extra_locals);
        for insn in &self.code {
            wasm_func.instruction(insn);
        }
        wasm_func.instruction(&Instruction::End);
        wasm_func
    }
}
