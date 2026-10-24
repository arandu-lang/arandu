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
            if let Some(sym_def) = self.symbols.try_get(*sym) {
                if sym_def.name == "ar_rt_alloc_aligned" {
                    if let Some(size_arg) = args.first() {
                        let usize_ty = self.interner.intern(ArType::Primitive(Primitive::USize));
                        // Cells guarantee CELL_ALIGNMENT. A larger request
                        // reports allocation failure (null), like the C
                        // runtime's invalid-layout path, instead of returning
                        // a silently under-aligned pointer.
                        if let Some(align_arg) = args.get(1) {
                            self.emit_operand(align_arg, usize_ty);
                            self.code
                                .push(Instruction::I32Const(crate::memory::CELL_ALIGNMENT));
                            self.code.push(Instruction::I32GtU);
                            self.code.push(Instruction::If(BlockType::Result(
                                wasm_encoder::ValType::I32,
                            )));
                            self.code.push(Instruction::I32Const(0));
                            self.code.push(Instruction::Else);
                            self.emit_operand(size_arg, usize_ty);
                            self.code.push(Instruction::Call(self.alloc_func_idx));
                            self.code.push(Instruction::End);
                        } else {
                            self.emit_operand(size_arg, usize_ty);
                            self.code.push(Instruction::Call(self.alloc_func_idx));
                        }
                        if let Some(temp) = lhs {
                            let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                            self.code.push(Instruction::LocalSet(local));
                        } else {
                            self.code.push(Instruction::Drop);
                        }
                    }
                    return;
                }
                if sym_def.name == "ar_rt_free_aligned" {
                    if let Some(ptr_arg) = args.first() {
                        let ptr_ty = self.interner.intern(ArType::Primitive(Primitive::USize));
                        self.emit_operand(ptr_arg, ptr_ty);
                        self.code.push(Instruction::Call(self.free_func_idx));
                    }
                    return;
                }
                if let Some(kind) =
                    IntrinsicKind::from_declared_name(&sym_def.name, sym_def.lang_item)
                    && self.emit_intrinsic_call(kind, sym_def.name.ends_with("64"), lhs, args)
                {
                    return;
                }
            }
            if let Some(&func_idx) = self.func_index_map.get(sym) {
                let callee = self
                    .functions
                    .iter()
                    .find(|function| function.symbol == *sym);
                for (index, arg) in args.iter().enumerate() {
                    // Nil can stand for either unit or a nullable value. Its
                    // ABI shape comes from the declared parameter, not Nil.
                    let arg_ty = callee
                        .and_then(|function| {
                            function
                                .params
                                .get(index)
                                .map(|temp| function.temps[temp.as_usize()].ty)
                        })
                        .or_else(|| {
                            self.extern_functions
                                .get(sym)
                                .and_then(|(params, _)| params.get(index))
                                .map(|ty| self.interner.intern(ty.clone()))
                        })
                        .unwrap_or_else(|| self.operand_arity_ty(arg));
                    if types::shape(arg_ty, self.interner, self.layout_engine.data_layout)
                        == Shape::Empty
                    {
                        continue;
                    }
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
                            if let Some(slot) = self.current_returned_home {
                                self.adopt_returned_home(slot, local);
                            }
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
        wide: bool,
        lhs: Option<TempId>,
        args: &[AmirOperand],
    ) -> bool {
        match kind {
            IntrinsicKind::Atomic(..)
            | IntrinsicKind::AtomicFence
            | IntrinsicKind::AtomicCompilerFence => self.emit_atomic_intrinsic(kind, lhs, args),
            IntrinsicKind::CountOnes
            | IntrinsicKind::LeadingZeros
            | IntrinsicKind::TrailingZeros
            | IntrinsicKind::ReverseBits => {
                if let Some(argument) = args.first() {
                    let input_ty = self.interner.intern(ArType::Primitive(if wide {
                        Primitive::U64
                    } else {
                        Primitive::U32
                    }));
                    self.emit_operand(argument, input_ty);
                    if kind == IntrinsicKind::ReverseBits {
                        let scratch = if wide { self.scratch_i64 } else { self.scratch };
                        for (shift, mask) in [
                            (1i32, 0x5555_5555_5555_5555u64),
                            (2, 0x3333_3333_3333_3333),
                            (4, 0x0f0f_0f0f_0f0f_0f0f),
                            (8, 0x00ff_00ff_00ff_00ff),
                            (16, 0x0000_ffff_0000_ffff),
                            (32, 0x0000_0000_ffff_ffff),
                        ]
                        .into_iter()
                        .take(if wide { 6 } else { 5 })
                        {
                            self.code.push(Instruction::LocalSet(scratch));
                            self.code.push(Instruction::LocalGet(scratch));
                            if wide {
                                self.code.push(Instruction::I64Const(i64::from(shift)));
                                self.code.push(Instruction::I64ShrU);
                                self.code.push(Instruction::I64Const(i64::from_ne_bytes(
                                    mask.to_ne_bytes(),
                                )));
                                self.code.push(Instruction::I64And);
                            } else {
                                self.code.push(Instruction::I32Const(shift));
                                self.code.push(Instruction::I32ShrU);
                                let bytes = mask.to_le_bytes();
                                self.code.push(Instruction::I32Const(i32::from_le_bytes([
                                    bytes[0], bytes[1], bytes[2], bytes[3],
                                ])));
                                self.code.push(Instruction::I32And);
                            }
                            self.code.push(Instruction::LocalGet(scratch));
                            if wide {
                                self.code.push(Instruction::I64Const(i64::from_ne_bytes(
                                    mask.to_ne_bytes(),
                                )));
                                self.code.push(Instruction::I64And);
                                self.code.push(Instruction::I64Const(i64::from(shift)));
                                self.code.push(Instruction::I64Shl);
                                self.code.push(Instruction::I64Or);
                            } else {
                                let bytes = mask.to_le_bytes();
                                self.code.push(Instruction::I32Const(i32::from_le_bytes([
                                    bytes[0], bytes[1], bytes[2], bytes[3],
                                ])));
                                self.code.push(Instruction::I32And);
                                self.code.push(Instruction::I32Const(shift));
                                self.code.push(Instruction::I32Shl);
                                self.code.push(Instruction::I32Or);
                            }
                        }
                    } else {
                        self.code.push(match (wide, kind) {
                            (true, IntrinsicKind::CountOnes) => Instruction::I64Popcnt,
                            (true, IntrinsicKind::LeadingZeros) => Instruction::I64Clz,
                            (true, _) => Instruction::I64Ctz,
                            (false, IntrinsicKind::CountOnes) => Instruction::I32Popcnt,
                            (false, IntrinsicKind::LeadingZeros) => Instruction::I32Clz,
                            (false, _) => Instruction::I32Ctz,
                        });
                        if wide {
                            self.code.push(Instruction::I32WrapI64);
                        }
                    }
                    if let Some(destination) = lhs {
                        let local = self.temp_local.get(&destination).copied().unwrap_or(0);
                        self.code.push(Instruction::LocalSet(local));
                    } else {
                        self.code.push(Instruction::Drop);
                    }
                }
                true
            }
            IntrinsicKind::Abort => {
                self.code.push(Instruction::Unreachable);
                true
            }
            IntrinsicKind::BlackBox => {
                if let Some(arg) = args.first() {
                    let arg_ty = self.operand_arity_ty(arg);
                    self.emit_black_box_value(arg, arg_ty);
                    let shape = types::shape(arg_ty, self.interner, self.layout_engine.data_layout);
                    if let Some(temp) = lhs {
                        let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                        match shape {
                            Shape::Scalar => self.code.push(Instruction::LocalSet(local)),
                            Shape::Fat => {
                                self.code.push(Instruction::LocalSet(local + 1));
                                self.code.push(Instruction::LocalSet(local));
                            }
                            Shape::Empty => {}
                        }
                    } else {
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
            IntrinsicKind::AddressOf => {
                if let Some(arg) = args.first()
                    && let Some(temp) = lhs
                {
                    let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                    let arg_ty = self.operand_arity_ty(arg);
                    self.emit_operand_to_local(arg, arg_ty, local);
                }
                true
            }
            IntrinsicKind::PtrRead => {
                if let Some(arg) = args.first() {
                    let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
                    self.emit_operand(arg, int_ty);
                    if let Some(temp) = lhs {
                        let temp_ty = self.func.temps[temp.as_usize()].ty;
                        let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                        if self.is_owned_aggregate(temp_ty) {
                            let size = self.layout_of_id(temp_ty).size as i32;
                            self.code.push(Instruction::LocalSet(self.scratch_c));
                            self.alloc_cell(size);
                            self.code.push(Instruction::LocalGet(self.scratch));
                            self.code.push(Instruction::LocalSet(self.scratch_b));
                            self.code.push(Instruction::LocalGet(self.scratch_b));
                            self.code.push(Instruction::LocalGet(self.scratch_c));
                            self.code.push(Instruction::I32Const(size));
                            self.code.push(Instruction::MemoryCopy {
                                src_mem: 0,
                                dst_mem: 0,
                            });
                            self.code.push(Instruction::LocalGet(self.scratch_b));
                        } else {
                            self.emit_load_value_at(temp_ty, 0);
                        }
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
                    if self.is_owned_aggregate(val_ty) {
                        let size = self.layout_of_id(val_ty).size as i32;
                        self.code.push(Instruction::LocalSet(self.scratch_b));
                        self.emit_operand(&args[1], val_ty);
                        self.code.push(Instruction::LocalSet(self.scratch));
                        self.code.push(Instruction::LocalGet(self.scratch_b));
                        self.code.push(Instruction::LocalGet(self.scratch));
                        self.code.push(Instruction::I32Const(size));
                        self.code.push(Instruction::MemoryCopy {
                            src_mem: 0,
                            dst_mem: 0,
                        });
                    } else {
                        self.emit_operand(&args[1], val_ty);
                        self.emit_store_value_at(val_ty, 0);
                    }
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
                        let (data_arg, len_arg) = if args.len() >= 3 {
                            (&args[1], &args[2])
                        } else {
                            (&args[0], &args[1])
                        };
                        let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                        self.emit_operand(data_arg, int_ty);
                        self.code.push(Instruction::LocalSet(local));
                        self.emit_operand(len_arg, int_ty);
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
                        ArType::Slice(inner) | ArType::MutSlice(inner) => inner,
                        ArType::Ref(inner) | ArType::RefMut(inner) => {
                            match self.interner.resolve(inner) {
                                ArType::Slice(elem) | ArType::MutSlice(elem) => elem,
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
            IntrinsicKind::StrBytes => {
                if let Some(arg) = args.first() {
                    let str_ty = self.operand_arity_ty(arg);
                    if let Some(temp) = lhs {
                        let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                        self.emit_operand_to_local(arg, str_ty, local);
                    }
                }
                true
            }
            IntrinsicKind::StrView => {
                if let Some(arg) = args.first()
                    && let Some(temp) = lhs
                {
                    let local = self.temp_local.get(&temp).copied().unwrap_or(0);
                    let str_ty = self.operand_arity_ty(arg);
                    // Owned strings begin with their borrowed `(data, len)`
                    // descriptor, so a reference to the owner also points
                    // at the corresponding `str` view.
                    self.emit_operand_to_local(arg, str_ty, local);
                }
                true
            }
            IntrinsicKind::DropInPlace => false,
        }
    }
}
