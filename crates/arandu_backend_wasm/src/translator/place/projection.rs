use arandu_middle::amir::value::AmirProjection;
use arandu_middle::types::{ArType, Primitive, TypeId};
use wasm_encoder::{BlockType, Instruction};

use super::super::FuncTranslator;

impl<'a> FuncTranslator<'a> {
    /// Emit a single projection on the current address and return the element
    /// type after the projection.
    pub(super) fn emit_place_projection(
        &mut self,
        cur_ty: TypeId,
        proj: &AmirProjection,
        base_slot: Option<u32>,
        from_memory: &mut bool,
    ) -> TypeId {
        match proj {
            AmirProjection::Deref => {
                // The first dereference starts with the local's pointer value.
                // After a field/index/dereference projection the stack holds
                // an address containing the next pointer, which must be loaded.
                if *from_memory {
                    self.code
                        .push(Instruction::I32Load(crate::memory::noffset_memarg()));
                }
                *from_memory = true;
                self.strip_ref(cur_ty).unwrap_or(cur_ty)
            }
            AmirProjection::Field(symbol_id) => {
                *from_memory = true;
                let owner_ty = self.strip_ref(cur_ty).unwrap_or(cur_ty);
                let owner = self.interner.resolve(owner_ty);
                let ArType::Named(_, _) = owner else {
                    return cur_ty;
                };
                let Some(sym) = self.symbols.try_get(*symbol_id) else {
                    return cur_ty;
                };
                let name = &sym.name;
                let Some(fields) = self
                    .layout_provider
                    .get_struct_fields_for_type(&owner, self.interner)
                else {
                    return cur_ty;
                };
                let Some(field_info) = fields.get(name) else {
                    return cur_ty;
                };
                let struct_layout = self.layout_of(&owner);
                let offset = struct_layout
                    .field_offsets
                    .get(field_info.index)
                    .copied()
                    .unwrap_or(0);
                if offset > 0 {
                    self.code.push(Instruction::I32Const(offset as i32));
                    self.code.push(Instruction::I32Add);
                }
                match arandu_middle::layout::instantiated_field_type(
                    &owner,
                    name,
                    self.interner,
                    self.layout_provider,
                ) {
                    Some(field_ty) => field_ty,
                    None => cur_ty,
                }
            }
            AmirProjection::Variant(_) => cur_ty,
            AmirProjection::Payload {
                index,
                field_ty,
                tuple_ty,
                ..
            } => {
                let owner_ty = self.strip_ref(cur_ty).unwrap_or(cur_ty);
                let owner = self.interner.resolve(owner_ty);
                let owner_layout = self.layout_of(&owner);
                let mut offset = owner_layout.field_offsets.get(1).copied().unwrap_or(0);
                if let Some(tuple_ty) = tuple_ty {
                    let tuple = self.interner.resolve(*tuple_ty);
                    let tuple_layout = self.layout_of(&tuple);
                    offset = offset.saturating_add(
                        tuple_layout.field_offsets.get(*index).copied().unwrap_or(0),
                    );
                }
                if offset > 0 {
                    match i32::try_from(offset) {
                        Ok(offset) => {
                            self.code.push(Instruction::I32Const(offset));
                            self.code.push(Instruction::I32Add);
                        }
                        Err(_) => {
                            self.code.push(Instruction::Unreachable);
                            return *field_ty;
                        }
                    }
                }
                *field_ty
            }
            AmirProjection::TupleField(index) => {
                let tuple = self.interner.resolve(cur_ty);
                let tuple_layout = self.layout_of(&tuple);
                let offset = tuple_layout.field_offsets.get(*index).copied().unwrap_or(0);
                if offset > 0 {
                    match i32::try_from(offset) {
                        Ok(offset) => {
                            self.code.push(Instruction::I32Const(offset));
                            self.code.push(Instruction::I32Add);
                        }
                        Err(_) => {
                            self.code.push(Instruction::Unreachable);
                            return cur_ty;
                        }
                    }
                }
                match tuple {
                    ArType::Tuple(args) => self
                        .interner
                        .type_args(args)
                        .get(*index)
                        .copied()
                        .unwrap_or(cur_ty),
                    _ => cur_ty,
                }
            }
            AmirProjection::Index(index_op) => {
                let owner_ty = self.strip_ref(cur_ty).unwrap_or(cur_ty);
                let owner = self.interner.resolve(owner_ty);
                let elem_ty = match owner {
                    ArType::Slice(inner) | ArType::MutSlice(inner) | ArType::Array(_, inner) => {
                        inner
                    }
                    _ => return cur_ty,
                };
                let elem_size = self.layout_of(&self.interner.resolve(elem_ty)).size;
                let int_ty = self.interner.intern(ArType::Primitive(Primitive::Int));
                match owner {
                    ArType::Array(len, _) => {
                        // Bounds check: index < len.
                        self.emit_operand(index_op, int_ty);
                        self.code.push(Instruction::I32Const(len as i32));
                        self.code.push(Instruction::I32GeU);
                        self.code.push(Instruction::If(BlockType::Empty));
                        self.code.push(Instruction::Unreachable);
                        self.code.push(Instruction::End);
                        // address = base + index * elem_size
                        self.emit_operand(index_op, int_ty);
                        self.code.push(Instruction::I32Const(elem_size as i32));
                        self.code.push(Instruction::I32Mul);
                        self.code.push(Instruction::I32Add);
                    }
                    ArType::Slice(_) | ArType::MutSlice(_) => {
                        if !*from_memory && let Some(slot) = base_slot {
                            self.emit_operand(index_op, int_ty);
                            self.code.push(Instruction::LocalGet(slot + 1));
                            self.code.push(Instruction::I32GeU);
                            self.code.push(Instruction::If(BlockType::Empty));
                            self.code.push(Instruction::Unreachable);
                            self.code.push(Instruction::End);
                            self.emit_operand(index_op, int_ty);
                            self.code.push(Instruction::I32Const(elem_size as i32));
                            self.code.push(Instruction::I32Mul);
                            self.code.push(Instruction::I32Add);
                        } else {
                            self.code.push(Instruction::LocalSet(self.scratch));
                            self.emit_operand(index_op, int_ty);
                            self.code.push(Instruction::LocalGet(self.scratch));
                            self.code.push(Instruction::I32Load(wasm_encoder::MemArg {
                                offset: 4,
                                align: 2,
                                memory_index: 0,
                            }));
                            self.code.push(Instruction::I32GeU);
                            self.code.push(Instruction::If(BlockType::Empty));
                            self.code.push(Instruction::Unreachable);
                            self.code.push(Instruction::End);
                            self.code.push(Instruction::LocalGet(self.scratch));
                            self.code.push(Instruction::I32Load(wasm_encoder::MemArg {
                                offset: 0,
                                align: 2,
                                memory_index: 0,
                            }));
                            self.emit_operand(index_op, int_ty);
                            self.code.push(Instruction::I32Const(elem_size as i32));
                            self.code.push(Instruction::I32Mul);
                            self.code.push(Instruction::I32Add);
                        }
                    }
                    _ => {}
                }
                *from_memory = true;
                elem_ty
            }
            AmirProjection::IndexConstant(index) => {
                let owner_ty = self.strip_ref(cur_ty).unwrap_or(cur_ty);
                let owner = self.interner.resolve(owner_ty);
                let elem_ty = match owner {
                    ArType::Array(_, inner) => inner,
                    _ => return cur_ty,
                };
                let elem_size = self.layout_of(&self.interner.resolve(elem_ty)).size;
                let Some(offset) = elem_size
                    .checked_mul(*index as u64)
                    .and_then(|offset| i32::try_from(offset).ok())
                else {
                    self.code.push(Instruction::Unreachable);
                    return cur_ty;
                };
                if offset > 0 {
                    self.code.push(Instruction::I32Const(offset));
                    self.code.push(Instruction::I32Add);
                }
                *from_memory = true;
                elem_ty
            }
        }
    }
}
