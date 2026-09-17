//! Wasm local allocation for a translated function.
//!
//! Locals are laid out in three separate index spaces that must not alias:
//! parameters (implicit from the signature, or flattened by the Canonical ABI),
//! AMIR temps, and AMIR locals. A final block of scratch locals backs the bump
//! allocator, fat-cell stores, logical i64 normalization, and float ops.

use arandu_middle::amir::local::TempId;
use arandu_middle::layout::DataLayout;
use arandu_middle::types::{TypeId, TypeInterner};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use wasm_encoder::ValType;

use super::{FuncTranslator, shape_slots};
use crate::types;

/// Describes one flattened field of an aggregate parameter stored into guest memory.
#[derive(Clone, Copy, Debug)]
pub struct FlatFieldStore {
    pub wasm_param_slot: u32,
    pub offset: u32,
    pub val_type: ValType,
}

/// Plan for unpacking flattened wasm parameter fields into a memory-backed aggregate cell.
#[derive(Clone, Debug)]
pub struct ParamUnpack {
    pub target_temp: TempId,
    pub target_local: u32,
    pub cell_size: u32,
    pub fields: SmallVec<[FlatFieldStore; 8]>,
}

impl<'a> FuncTranslator<'a> {
    /// Allocate wasm locals for all params, temps, and scratch.
    /// Returns the extra-local groupings (excluding function params, which are
    /// implicit locals declared by the signature) for `Function::new`, with
    /// consecutive slots of the same `ValType` compressed into single groups.
    pub(super) fn allocate_locals(&mut self) -> Vec<(u32, ValType)> {
        let mut slot_types: Vec<ValType> = Vec::new();

        if let Some(sig) = self.component_sig {
            let num_sig_params = sig.params.len() as u32;
            let mut next = num_sig_params;
            let mut wasm_param_cursor = 0u32;

            if let Some(recv) = self.func.receiver
                && !self.func.params.contains(&recv.temp)
            {
                let ty = self.func.temps[recv.temp.as_usize()].ty;
                self.assign_component_param(
                    recv.temp,
                    ty,
                    &mut wasm_param_cursor,
                    &mut next,
                    &mut slot_types,
                );
            }

            for &param in &self.func.params {
                let ty = self.func.temps[param.as_usize()].ty;
                self.assign_component_param(
                    param,
                    ty,
                    &mut wasm_param_cursor,
                    &mut next,
                    &mut slot_types,
                );
            }

            // Non-param temps
            for temp in &self.func.temps {
                if self.temp_local.contains_key(&temp.id) {
                    continue;
                }
                Self::assign_slots(
                    self.interner,
                    self.layout_engine.data_layout,
                    &mut self.temp_local,
                    &mut next,
                    &mut slot_types,
                    temp.id,
                    temp.ty,
                );
            }

            // AMIR locals
            for local in &self.func.locals {
                let base = next;
                self.local_wasm.insert(local.id, base);
                let slots = shape_slots(types::shape(
                    local.ty,
                    self.interner,
                    self.layout_engine.data_layout,
                ));
                for k in 0..slots {
                    slot_types.push(
                        types::slot_valtype(
                            local.ty,
                            k as usize,
                            self.interner,
                            self.layout_engine.data_layout,
                        )
                        .unwrap_or(ValType::I32),
                    );
                }
                next += slots;
            }

            self.next_local = next;

            self.scratch = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::I32);
            self.scratch_b = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::I32);
            self.scratch_c = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::I32);
            self.scratch_d = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::I32);
            self.scratch_i64 = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::I64);
            self.scratch_i64b = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::I64);
            self.scratch_f64 = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::F64);

            Self::compress_locals(&slot_types)
        } else {
            let mut next = 0u32;

            // Receiver (self) parameter. It is already `func.params[0]` in the
            // AMIR contract; allocate a slot here only if it is missing from the
            // parameter list (defensive), so it is never counted twice.
            if let Some(recv) = self.func.receiver
                && !self.func.params.contains(&recv.temp)
            {
                let ty = self.func.temps[recv.temp.as_usize()].ty;
                Self::assign_slots(
                    self.interner,
                    self.layout_engine.data_layout,
                    &mut self.temp_local,
                    &mut next,
                    &mut slot_types,
                    recv.temp,
                    ty,
                );
            }

            // Explicit function parameters.
            for &param in &self.func.params {
                let ty = self.func.temps[param.as_usize()].ty;
                Self::assign_slots(
                    self.interner,
                    self.layout_engine.data_layout,
                    &mut self.temp_local,
                    &mut next,
                    &mut slot_types,
                    param,
                    ty,
                );
            }
            let param_slots = next as usize;

            // Non-param temps (including block-parameter temps, which are ordinary
            // temp slots in this scheme — the block arguments are written into them
            // before the jump by `emit_block_args`).
            for temp in &self.func.temps {
                if self.temp_local.contains_key(&temp.id) {
                    continue;
                }
                Self::assign_slots(
                    self.interner,
                    self.layout_engine.data_layout,
                    &mut self.temp_local,
                    &mut next,
                    &mut slot_types,
                    temp.id,
                    temp.ty,
                );
            }

            // AMIR locals. They live in a separate index space from temps, so they
            // get their own wasm slots; sharing them (the previous behaviour) made a
            // live local alias the temp whose index happened to match.
            for local in &self.func.locals {
                let base = next;
                self.local_wasm.insert(local.id, base);
                let slots = shape_slots(types::shape(
                    local.ty,
                    self.interner,
                    self.layout_engine.data_layout,
                ));
                for k in 0..slots {
                    slot_types.push(
                        types::slot_valtype(
                            local.ty,
                            k as usize,
                            self.interner,
                            self.layout_engine.data_layout,
                        )
                        .unwrap_or(ValType::I32),
                    );
                }
                next += slots;
            }

            self.next_local = next;

            // Reserve scratch locals: the bump address (i32), three i32 words for
            // fat (ptr, len) cell stores, and two i64 slots for 64-bit logical
            // `And`/`Or` normalization.
            self.scratch = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::I32);
            self.scratch_b = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::I32);
            self.scratch_c = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::I32);
            self.scratch_d = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::I32);
            self.scratch_i64 = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::I64);
            self.scratch_i64b = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::I64);
            self.scratch_f64 = self.next_local;
            self.next_local += 1;
            slot_types.push(ValType::F64);

            // Params occupy the first locals; body locals are declared after them.
            Self::compress_locals(&slot_types[param_slots..])
        }
    }

    fn assign_component_param(
        &mut self,
        param: TempId,
        ty: TypeId,
        cursor: &mut u32,
        next: &mut u32,
        extra_slot_types: &mut Vec<ValType>,
    ) {
        let resolved = self.interner.resolve(ty);
        let shape = types::shape(ty, self.interner, self.layout_engine.data_layout);
        match shape {
            crate::types::Shape::Empty => {}
            crate::types::Shape::Fat => {
                self.temp_local.insert(param, *cursor);
                *cursor += 2;
            }
            crate::types::Shape::Scalar => {
                if let arandu_middle::types::ArType::Named(struct_sym, _) = resolved
                    && let Some(fields_def) = self.layout_provider.get_struct_fields(struct_sym)
                    && !fields_def.is_empty()
                {
                    let layout = self.layout_of(&resolved);
                    let target_local = *next;
                    *next += 1;
                    extra_slot_types.push(ValType::I32);

                    let mut flat_fields = SmallVec::new();
                    let mut sorted_fields: Vec<_> = fields_def.iter().collect();
                    sorted_fields.sort_by_key(|f| f.index);

                    for f in sorted_fields {
                        let f_shape =
                            types::shape(f.ty, self.interner, self.layout_engine.data_layout);
                        let f_offset =
                            layout.field_offsets.get(f.index).copied().unwrap_or(0) as u32;
                        match f_shape {
                            crate::types::Shape::Empty => {}
                            crate::types::Shape::Scalar => {
                                let vt = types::scalar_valtype_for(
                                    f.ty,
                                    self.interner,
                                    self.layout_engine.data_layout,
                                )
                                .unwrap_or(ValType::I32);
                                flat_fields.push(FlatFieldStore {
                                    wasm_param_slot: *cursor,
                                    offset: f_offset,
                                    val_type: vt,
                                });
                                *cursor += 1;
                            }
                            crate::types::Shape::Fat => {
                                flat_fields.push(FlatFieldStore {
                                    wasm_param_slot: *cursor,
                                    offset: f_offset,
                                    val_type: ValType::I32,
                                });
                                flat_fields.push(FlatFieldStore {
                                    wasm_param_slot: *cursor + 1,
                                    offset: f_offset + 4,
                                    val_type: ValType::I32,
                                });
                                *cursor += 2;
                            }
                        }
                    }

                    self.temp_local.insert(param, target_local);
                    self.param_unpacks.push(ParamUnpack {
                        target_temp: param,
                        target_local,
                        cell_size: layout.size as u32,
                        fields: flat_fields,
                    });
                } else {
                    self.temp_local.insert(param, *cursor);
                    *cursor += 1;
                }
            }
        }
    }

    fn compress_locals(types: &[ValType]) -> Vec<(u32, ValType)> {
        let mut groups: Vec<(u32, ValType)> = Vec::new();
        for &vt in types {
            match groups.last_mut() {
                Some((count, last)) if *last == vt => {
                    *count += 1;
                }
                _ => groups.push((1, vt)),
            }
        }
        groups
    }

    /// Allocate `shape_slots` consecutive wasm locals for `temp` and record each
    /// slot's `ValType` in `slot_types` (for the body local grouping).
    fn assign_slots(
        interner: &TypeInterner,
        layout: DataLayout,
        temp_local: &mut FxHashMap<TempId, u32>,
        next: &mut u32,
        slot_types: &mut Vec<ValType>,
        temp: TempId,
        ty: TypeId,
    ) {
        let base = *next;
        temp_local.insert(temp, base);
        let slots = shape_slots(types::shape(ty, interner, layout));
        for k in 0..slots {
            slot_types.push(
                types::slot_valtype(ty, k as usize, interner, layout).unwrap_or(ValType::I32),
            );
        }
        *next += slots;
    }
}
