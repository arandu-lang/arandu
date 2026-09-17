use arandu_middle::amir::value::{AmirConstant, AmirOperand};
use arandu_middle::literal_pool::AmirLiteralEntry;
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::{Instruction, ValType};

use super::FuncTranslator;
use crate::types::{self, NarrowInfo, Shape};

impl<'a> FuncTranslator<'a> {
    /// Load an operand's value onto the wasm stack.
    pub(super) fn emit_operand(&mut self, op: &AmirOperand, expected_ty: TypeId) {
        match op {
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                if let Some(&local) = self.temp_local.get(temp) {
                    let ty = self.func.temps[temp.as_usize()].ty;
                    let shape = types::shape(ty, self.interner, self.layout_engine.data_layout);
                    match shape {
                        Shape::Fat => {
                            self.code.push(Instruction::LocalGet(local));
                            self.code.push(Instruction::LocalGet(local + 1));
                        }
                        _ => self.code.push(Instruction::LocalGet(local)),
                    }
                } else {
                    self.emit_zero(expected_ty);
                }
            }
            AmirOperand::Constant(c) => self.emit_constant(c, expected_ty),
            AmirOperand::FunctionRef(sym) => {
                if let Some(&idx) = self.func_index_map.get(sym) {
                    self.code.push(Instruction::I32Const(idx as i32));
                } else {
                    self.code.push(Instruction::I32Const(0));
                }
            }
            AmirOperand::GlobalRef(_sym) => {
                // Global references not supported in MVP.
                self.code.push(Instruction::I32Const(0));
            }
        }
    }

    /// Store an operand into a wasm local.
    pub(super) fn emit_operand_to_local(&mut self, op: &AmirOperand, ty: TypeId, local: u32) {
        self.emit_operand(op, ty);
        let shape = types::shape(ty, self.interner, self.layout_engine.data_layout);
        match shape {
            Shape::Empty => {}
            Shape::Scalar => {
                self.code.push(Instruction::LocalSet(local));
            }
            Shape::Fat => {
                // Fat: stack has (ptr, len). Store len first, then ptr.
                self.code.push(Instruction::LocalSet(local + 1));
                self.code.push(Instruction::LocalSet(local));
            }
        }
    }

    /// Emit a constant value.
    fn emit_constant(&mut self, c: &AmirConstant, ty: TypeId) {
        match c {
            AmirConstant::Pool(literal_id) => {
                let entry = self.literal_pool.get(*literal_id);
                match entry {
                    AmirLiteralEntry::Int(lexeme) => {
                        let parsed = arandu_middle::literal_pool::parse_int_literal(lexeme);
                        match parsed {
                            Some(v) => {
                                if types::ar_is_64bit(
                                    ty,
                                    self.interner,
                                    self.layout_engine.data_layout,
                                ) {
                                    self.code.push(Instruction::I64Const(v as i64));
                                } else {
                                    self.code.push(Instruction::I32Const(v as i32));
                                }
                            }
                            None => self.emit_zero(ty),
                        }
                    }
                    AmirLiteralEntry::Float(lexeme) => {
                        let parsed = arandu_middle::literal_pool::parse_float_literal(lexeme);
                        match parsed {
                            Some(v) => {
                                if matches!(
                                    types::slot_valtype(
                                        ty,
                                        0,
                                        self.interner,
                                        self.layout_engine.data_layout
                                    ),
                                    Some(ValType::F32)
                                ) {
                                    self.code.push(Instruction::F32Const((v as f32).into()));
                                } else {
                                    self.code.push(Instruction::F64Const(v.into()));
                                }
                            }
                            None => self.emit_zero(ty),
                        }
                    }
                    AmirLiteralEntry::Str(lexeme) => {
                        let len = lexeme.len() as i32;
                        if let Some(&offset) = self.rodata_offsets.get(literal_id) {
                            self.code.push(Instruction::I32Const(offset as i32));
                            self.code.push(Instruction::I32Const(len));
                        } else {
                            // Fallback for dynamically generated or unregistered literals
                            self.alloc_cell(len);
                            self.code.push(Instruction::LocalGet(self.scratch));
                            self.code.push(Instruction::LocalSet(self.scratch_b));
                            for (i, byte) in lexeme.bytes().enumerate() {
                                self.code.push(Instruction::LocalGet(self.scratch_b));
                                self.code.push(Instruction::I32Const(i as i32));
                                self.code.push(Instruction::I32Add);
                                self.code.push(Instruction::I32Const(byte as i32));
                                self.code.push(Instruction::I32Store8(wasm_encoder::MemArg {
                                    offset: 0,
                                    align: 0,
                                    memory_index: 0,
                                }));
                            }
                            self.code.push(Instruction::LocalGet(self.scratch_b));
                            self.code.push(Instruction::I32Const(len));
                        }
                    }
                    AmirLiteralEntry::Char(lexeme) => {
                        let val = lexeme.chars().next().unwrap_or('\0') as i32;
                        self.code.push(Instruction::I32Const(val));
                    }
                }
            }
            AmirConstant::Bool(v) => {
                self.code
                    .push(Instruction::I32Const(if *v { 1 } else { 0 }));
            }
            AmirConstant::Nil => {
                self.emit_zero(ty);
            }
        }
    }

    /// Push a zero constant for the given type.
    pub(super) fn emit_zero(&mut self, ty: TypeId) {
        let vt = types::scalar_valtype_for(ty, self.interner, self.layout_engine.data_layout);
        match vt {
            Some(ValType::I32) => self.code.push(Instruction::I32Const(0)),
            Some(ValType::I64) => self.code.push(Instruction::I64Const(0)),
            Some(ValType::F32) => self.code.push(Instruction::F32Const(0.0.into())),
            Some(ValType::F64) => self.code.push(Instruction::F64Const(0.0.into())),
            Some(ValType::V128 | ValType::Ref(_)) | None => {}
        }
    }

    /// Emit narrowing (sign-extend or mask) for sub-byte/16-bit integers.
    pub(super) fn emit_narrow(&mut self, narrow: NarrowInfo) {
        match narrow {
            NarrowInfo::Mask(mask) => {
                self.code.push(Instruction::I32Const(mask as i32));
                self.code.push(Instruction::I32And);
            }
            NarrowInfo::SignExtend(8) => {
                self.code.push(Instruction::I32Extend8S);
            }
            NarrowInfo::SignExtend(16) => {
                self.code.push(Instruction::I32Extend16S);
            }
            NarrowInfo::SignExtend(_) => {}
        }
    }

    /// Resolve the (ptr_local, gen_local) slots for a GenRef operand.
    pub(super) fn resolve_gen_ref_slots(&mut self, gen_ref: &AmirOperand) -> (u32, u32) {
        match gen_ref {
            AmirOperand::Copy(t) | AmirOperand::Move(t) => {
                let base = self.temp_local.get(t).copied().unwrap_or(0);
                (base, base + 1)
            }
            _ => {
                let gen_ref_ty = self.interner.intern(ArType::GenRef);
                self.emit_operand(gen_ref, gen_ref_ty);
                self.code.push(Instruction::LocalSet(self.scratch_c));
                self.code.push(Instruction::LocalSet(self.scratch_b));
                (self.scratch_b, self.scratch_c)
            }
        }
    }

    /// Emit (0, 0) for a fat pointer local.
    pub(super) fn emit_zero_fat(&mut self, local: u32) {
        self.code.push(Instruction::I32Const(0));
        self.code.push(Instruction::LocalSet(local));
        self.code.push(Instruction::I32Const(0));
        self.code.push(Instruction::LocalSet(local + 1));
    }

    /// Strip one pointer/ref layer from a `TypeId`.
    pub(super) fn strip_ref(&self, ty: TypeId) -> Option<TypeId> {
        match self.interner.resolve(ty) {
            ArType::Ptr(inner)
            | ArType::Ref(inner)
            | ArType::RefMut(inner)
            | ArType::Nullable(inner) => Some(inner),
            _ => None,
        }
    }

    /// Infer the Arandu type of an operand from its own shape (mirrors the
    /// cranelift backend). Temp copies use the temp's declared type; constants
    /// derive from their literal kind.
    pub(super) fn operand_arity_ty(&self, op: &AmirOperand) -> TypeId {
        match op {
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                self.func.temps[temp.as_usize()].ty
            }
            AmirOperand::Constant(c) => match c {
                AmirConstant::Bool(_) => self.interner.intern(ArType::Primitive(Primitive::Bool)),
                AmirConstant::Nil => self.interner.intern(ArType::Void),
                AmirConstant::Pool(lit_id) => match self.literal_pool.get(*lit_id) {
                    AmirLiteralEntry::Int(_) => self.interner.intern(ArType::IntLiteral),
                    AmirLiteralEntry::Float(_) => self.interner.intern(ArType::FloatLiteral),
                    AmirLiteralEntry::Str(_) => {
                        self.interner.intern(ArType::Primitive(Primitive::Str))
                    }
                    AmirLiteralEntry::Char(_) => {
                        self.interner.intern(ArType::Primitive(Primitive::Char))
                    }
                },
            },
            AmirOperand::FunctionRef(_) | AmirOperand::GlobalRef(_) => {
                self.interner.intern(ArType::Primitive(Primitive::Int))
            }
        }
    }
}
