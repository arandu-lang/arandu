use arandu_middle::amir::local::TempId;
use arandu_middle::amir::value::AmirOperand;
use arandu_middle::intrinsics::IntrinsicKind;
use arandu_middle::types::{ArType, Primitive};
use wasm_encoder::{BlockType, Instruction};

use super::FuncTranslator;
use crate::types::{self, Shape};

impl<'a> FuncTranslator<'a> {
    /// Emit a call instruction.
    pub(super) fn emit_call(
        &mut self,
        lhs: Option<TempId>,
        callee: &AmirOperand,
        args: &[AmirOperand],
    ) {
        if let AmirOperand::FunctionRef(sym) = callee {
            if let Some(sym_def) = self.symbols.try_get(*sym)
                && let Some(kind) = IntrinsicKind::from_name(&sym_def.name)
                && self.emit_intrinsic_call(kind, lhs, args)
            {
                return;
            }
            if let Some(&func_idx) = self.func_index_map.get(sym) {
                for arg in args {
                    let arg_ty = self.operand_arity_ty(arg);
                    self.emit_operand(arg, arg_ty);
                }
                self.code.push(Instruction::Call(func_idx));
                // Store return value if needed.
                if let Some(temp) = lhs {
                    let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                    let shape = types::shape(
                        self.func.temps[temp.as_usize()].ty,
                        self.interner,
                        self.layout_engine.data_layout,
                    );
                    match shape {
                        Shape::Scalar => {
                            self.code.push(Instruction::LocalSet(local));
                        }
                        Shape::Fat => {
                            self.code.push(Instruction::LocalSet(local + 1));
                            self.code.push(Instruction::LocalSet(local));
                        }
                        Shape::Empty => {}
                    }
                }
                return;
            }
        }
        self.code.push(Instruction::Unreachable);
    }

    /// Emit an intrinsic function call inline.
    fn emit_intrinsic_call(
        &mut self,
        kind: IntrinsicKind,
        lhs: Option<TempId>,
        args: &[AmirOperand],
    ) -> bool {
        match kind {
            IntrinsicKind::Abort => {
                self.code.push(Instruction::Unreachable);
                true
            }
            IntrinsicKind::BlackBox => {
                if let Some(arg) = args.first() {
                    let arg_ty = self.operand_arity_ty(arg);
                    if let Some(temp) = lhs {
                        let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                        self.emit_operand_to_local(arg, arg_ty, local);
                    } else {
                        self.emit_operand(arg, arg_ty);
                        let shape =
                            types::shape(arg_ty, self.interner, self.layout_engine.data_layout);
                        match shape {
                            Shape::Scalar => self.code.push(Instruction::Drop),
                            Shape::Fat => {
                                self.code.push(Instruction::Drop);
                                self.code.push(Instruction::Drop);
                            }
                            Shape::Empty => {}
                        }
                    }
                }
                true
            }
            IntrinsicKind::PtrRead => {
                if let Some(arg) = args.first() {
                    let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
                    self.emit_operand(arg, int_ty);
                    if let Some(temp) = lhs {
                        let temp_ty = self.func.temps[temp.as_usize()].ty;
                        self.emit_load_value_at(temp_ty, 0);
                        let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                        let shape =
                            types::shape(temp_ty, self.interner, self.layout_engine.data_layout);
                        match shape {
                            Shape::Scalar => {
                                self.code.push(Instruction::LocalSet(local));
                            }
                            Shape::Fat => {
                                self.code.push(Instruction::LocalSet(local + 1));
                                self.code.push(Instruction::LocalSet(local));
                            }
                            Shape::Empty => {}
                        }
                    } else {
                        self.code.push(Instruction::Drop);
                    }
                }
                true
            }
            IntrinsicKind::PtrWrite => {
                if args.len() >= 2 {
                    let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
                    let val_ty = self.operand_arity_ty(&args[1]);
                    self.emit_operand(&args[0], int_ty);
                    self.emit_operand(&args[1], val_ty);
                    self.emit_store_value_at(val_ty, 0);
                }
                true
            }
            IntrinsicKind::PtrOffset => {
                if args.len() >= 2 {
                    let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
                    self.emit_operand(&args[0], int_ty);
                    self.emit_operand(&args[1], int_ty);

                    let base_ty = self.operand_arity_ty(&args[0]);
                    let pointee_ty = self.strip_ref(base_ty).unwrap_or(int_ty);
                    let elem_size = self.layout_of_id(pointee_ty).size.max(1) as i32;

                    self.code.push(Instruction::I32Const(elem_size));
                    self.code.push(Instruction::I32Mul);
                    self.code.push(Instruction::I32Add);

                    if let Some(temp) = lhs {
                        let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                        self.code.push(Instruction::LocalSet(local));
                    } else {
                        self.code.push(Instruction::Drop);
                    }
                }
                true
            }
            IntrinsicKind::SizeOf | IntrinsicKind::AlignOf => {
                // Residual only — the AMIR fold normally resolves this. The
                // intrinsic operates on the host `int`, so its size and
                // alignment are the target pointer width (wasm32: 4).
                let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
                let layout = self.layout_of_id(int_ty);
                let val = if kind == IntrinsicKind::SizeOf {
                    layout.size
                } else {
                    layout.align
                } as i32;
                if let Some(temp) = lhs {
                    let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                    self.code.push(Instruction::I32Const(val));
                    self.code.push(Instruction::LocalSet(local));
                }
                true
            }
            IntrinsicKind::SliceFromRaw => {
                if args.len() >= 2 {
                    let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
                    if let Some(temp) = lhs {
                        let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                        self.emit_operand(&args[0], int_ty);
                        self.code.push(Instruction::LocalSet(local));
                        self.emit_operand(&args[1], int_ty);
                        self.code.push(Instruction::LocalSet(local + 1));
                    }
                }
                true
            }
            IntrinsicKind::SliceLen => {
                if let Some(arg) = args.first() {
                    match arg {
                        AmirOperand::Copy(t) | AmirOperand::Move(t) => {
                            let base = self.temp_local.get(t).copied().unwrap_or(0);
                            self.code.push(Instruction::LocalGet(base + 1));
                        }
                        _ => {
                            self.code.push(Instruction::I32Const(0));
                        }
                    }
                    if let Some(temp) = lhs {
                        let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                        self.code.push(Instruction::LocalSet(local));
                    } else {
                        self.code.push(Instruction::Drop);
                    }
                }
                true
            }
            IntrinsicKind::SliceData => {
                if let Some(arg) = args.first() {
                    match arg {
                        AmirOperand::Copy(t) | AmirOperand::Move(t) => {
                            let base = self.temp_local.get(t).copied().unwrap_or(0);
                            self.code.push(Instruction::LocalGet(base));
                        }
                        _ => {
                            self.code.push(Instruction::I32Const(0));
                        }
                    }
                    if let Some(temp) = lhs {
                        let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                        self.code.push(Instruction::LocalSet(local));
                    } else {
                        self.code.push(Instruction::Drop);
                    }
                }
                true
            }
            IntrinsicKind::SliceSubslice => {
                if args.len() >= 3 {
                    let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
                    let slice_ty = self.operand_arity_ty(&args[0]);
                    let elem_ty = match self.interner.resolve(slice_ty) {
                        ArType::Slice(inner) => inner,
                        ArType::Ref(inner) | ArType::RefMut(inner) => {
                            match self.interner.resolve(inner) {
                                ArType::Slice(elem) => elem,
                                _ => slice_ty,
                            }
                        }
                        _ => slice_ty,
                    };
                    let elem_size =
                        self.layout_of(&self.interner.resolve(elem_ty)).size.max(1) as i32;

                    let (slice_ptr_local, slice_len_local) = match &args[0] {
                        AmirOperand::Copy(t) | AmirOperand::Move(t) => {
                            let base = self.temp_local.get(t).copied().unwrap_or(0);
                            (base, base + 1)
                        }
                        _ => {
                            self.emit_operand(&args[0], slice_ty);
                            self.code.push(Instruction::LocalSet(self.scratch_c));
                            self.code.push(Instruction::LocalSet(self.scratch_b));
                            (self.scratch_b, self.scratch_c)
                        }
                    };

                    // Evaluate start into scratch
                    self.emit_operand(&args[1], int_ty);
                    self.code.push(Instruction::LocalSet(self.scratch));

                    // Evaluate len into scratch_d
                    self.emit_operand(&args[2], int_ty);
                    self.code.push(Instruction::LocalSet(self.scratch_d));

                    // Bounds check: (start + len) > slice_len ? trap
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::LocalGet(self.scratch_d));
                    self.code.push(Instruction::I32Add);
                    self.code.push(Instruction::LocalGet(slice_len_local));
                    self.code.push(Instruction::I32GtU);
                    self.code.push(Instruction::If(BlockType::Empty));
                    self.code.push(Instruction::Unreachable);
                    self.code.push(Instruction::End);

                    if let Some(temp) = lhs {
                        let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                        // new_ptr = slice_ptr + start * elem_size
                        self.code.push(Instruction::LocalGet(slice_ptr_local));
                        self.code.push(Instruction::LocalGet(self.scratch));
                        self.code.push(Instruction::I32Const(elem_size));
                        self.code.push(Instruction::I32Mul);
                        self.code.push(Instruction::I32Add);
                        self.code.push(Instruction::LocalSet(local));

                        // new_len = len
                        self.code.push(Instruction::LocalGet(self.scratch_d));
                        self.code.push(Instruction::LocalSet(local + 1));
                    }
                }
                true
            }
            IntrinsicKind::StrView => {
                if let Some(arg) = args.first() {
                    let str_ty = self.operand_arity_ty(arg);
                    if let Some(temp) = lhs {
                        let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                        self.emit_operand_to_local(arg, str_ty, local);
                    }
                }
                true
            }
        }
    }
}
