//! Checked invocation storage for aggregates without safe-borrow leaves.
use super::FuncTranslator;
use arandu_middle::amir::{AmirRvalue, AmirStmt};
use wasm_encoder::{BlockType, Instruction, ValType};

impl FuncTranslator<'_> {
    pub(super) fn plan_frame(&mut self, locals: &mut Vec<(u32, ValType)>) {
        use arandu_mir::aggregate_storage::{ScratchResultAbi, function_scratch_safe};
        self.plan_static_initializer_temps();
        self.transfer_owned_result = self.component_sig.is_none()
            && self.is_owned_aggregate(self.func.return_type)
            && arandu_mir::aggregate_storage::scratch_type_safe(
                self.func.return_type,
                self.interner,
                self.layout_provider,
            );
        let result_abi = if self.transfer_owned_result {
            ScratchResultAbi::TransferredAggregates
        } else {
            ScratchResultAbi::ScalarsOnly
        };
        if !function_scratch_safe(self.func, self.symbols, self.interner, result_abi) {
            return;
        }
        // A Wasm move into a whole local copies the backing pointer, so a
        // loop-carried local would keep naming a site's backing after the next
        // entry of that site reclaims (returned home) or overwrites it.
        if arandu_mir::aggregate_storage::cyclic_aggregate_local_store(self.func, self.interner) {
            return;
        }
        for id in self.func.stmts.iter_ids() {
            if self.static_initializer_temps.contains(&id) {
                continue;
            }
            if let Some(AmirStmt::Call {
                lhs: Some(temp),
                callee: arandu_middle::amir::AmirOperand::FunctionRef(symbol),
                ..
            }) = self.func.stmts.get(id)
            {
                let ty = self.func.temps.get(temp.as_usize()).map(|temp| temp.ty);
                if ty.is_some_and(|ty| {
                    self.is_owned_aggregate(ty)
                        && arandu_mir::aggregate_storage::scratch_type_safe(
                            ty,
                            self.interner,
                            self.layout_provider,
                        )
                }) && self.symbols.try_get(*symbol).is_some_and(|symbol| {
                    symbol.kind != arandu_middle::SymbolKind::ExternFunc
                        && arandu_middle::intrinsics::IntrinsicKind::from_declared_name(
                            &symbol.name,
                            symbol.lang_item,
                        )
                        .is_none()
                }) && let Some(next) = self.next_local.checked_add(1)
                {
                    self.returned_homes.insert(id, self.next_local);
                    self.next_local = next;
                    locals.push((1, ValType::I32));
                }
            }
            let ty = match self.func.stmts.get(id) {
                Some(AmirStmt::Assign {
                    lhs,
                    rhs:
                        AmirRvalue::Array { .. }
                        | AmirRvalue::Tuple { .. }
                        | AmirRvalue::StructLiteral { .. }
                        | AmirRvalue::Use(_)
                        | AmirRvalue::FieldAccess { .. }
                        | AmirRvalue::IndexAccess { .. }
                        | AmirRvalue::EnumPayload { .. }
                        | AmirRvalue::Load(_)
                        | AmirRvalue::EnumConstruct { .. },
                }) => self.func.temps.get(lhs.as_usize()).map(|t| t.ty),
                Some(AmirStmt::Store { lhs, .. }) if lhs.projections.is_empty() => {
                    self.func.locals.get(lhs.local.as_usize()).map(|l| l.ty)
                }
                _ => None,
            };
            let Some(ty) = ty else {
                continue;
            };
            if !self.is_owned_aggregate(ty)
                || !arandu_mir::aggregate_storage::scratch_type_safe(
                    ty,
                    self.interner,
                    self.layout_provider,
                )
            {
                continue;
            }
            let layout = self.layout_of_id(ty);
            let Ok(size) = u32::try_from(layout.size.max(1)) else {
                continue;
            };
            if i32::try_from(size).is_err() {
                continue;
            }
            let Ok(align) = u32::try_from(layout.align.max(1)) else {
                continue;
            };
            if align > 16 || !align.is_power_of_two() {
                continue;
            }
            let Some(offset) = self
                .frame_size
                .checked_add(align - 1)
                .map(|s| s & !(align - 1))
            else {
                continue;
            };
            let Some(end) = offset.checked_add(size) else {
                continue;
            };
            if end > 16384 {
                let Some(next_local) = self.next_local.checked_add(1) else {
                    continue;
                };
                self.heap_homes.insert(id, self.next_local);
                self.next_local = next_local;
                locals.push((1, ValType::I32));
                continue;
            }
            self.frame_homes.insert(id, offset);
            self.frame_size = end;
        }
        if self.frame_size == 0 {
            return;
        }
        self.frame_size = (self.frame_size + 15) & !15;
        let Some(next_local) = self.next_local.checked_add(2) else {
            self.frame_homes.clear();
            self.frame_size = 0;
            return;
        };
        self.frame_base = self.next_local;
        self.frame_saved = self.next_local + 1;
        self.next_local = next_local;
        locals.push((2, ValType::I32));
    }

    pub(super) fn enter_frame(&mut self) {
        use crate::memory::{GLOBAL_STACK_BASE, GLOBAL_STACK_POINTER, STACK_AREA};
        if self.frame_size == 0 {
            return;
        }
        let Ok(size) = i32::try_from(self.frame_size) else {
            self.code.push(Instruction::Unreachable);
            return;
        };
        self.code.extend([
            Instruction::GlobalGet(GLOBAL_STACK_POINTER),
            Instruction::LocalTee(self.frame_saved),
            Instruction::I32Const(size),
            Instruction::I32LtU,
            Instruction::If(BlockType::Empty),
            Instruction::Unreachable,
            Instruction::End,
            Instruction::LocalGet(self.frame_saved),
            Instruction::I32Const(size),
            Instruction::I32Sub,
            Instruction::LocalTee(self.frame_base),
            Instruction::GlobalGet(GLOBAL_STACK_BASE),
            Instruction::I32Const(STACK_AREA),
            Instruction::I32Sub,
            Instruction::I32LtU,
            Instruction::If(BlockType::Empty),
            Instruction::Unreachable,
            Instruction::End,
            Instruction::LocalGet(self.frame_base),
            Instruction::GlobalSet(GLOBAL_STACK_POINTER),
        ]);
    }

    pub(super) fn leave_frame(&mut self) {
        // Runtime move/drop cleanup ignores scratch headers. Only the owning
        // invocation restores their allocator magic and releases the backing;
        // payload destruction remains entirely in elaborated AMIR.
        let slots: Vec<_> = self
            .heap_homes
            .values()
            .chain(self.returned_homes.values())
            .copied()
            .collect();
        let mut slots = slots;
        slots.sort_unstable();
        for slot in slots {
            self.code.extend([
                Instruction::LocalGet(slot),
                Instruction::If(BlockType::Empty),
                Instruction::LocalGet(slot),
                Instruction::I32Const(crate::memory::CELL_HEADER_SIZE),
                Instruction::I32Sub,
                Instruction::I32Const(crate::memory::CELL_MAGIC),
                Instruction::I32Store(crate::memory::noffset_memarg()),
                Instruction::LocalGet(slot),
                Instruction::Call(self.free_func_idx),
                Instruction::End,
            ]);
        }
        if self.frame_size > 0 {
            self.code.push(Instruction::LocalGet(self.frame_saved));
            self.code
                .push(Instruction::GlobalSet(crate::memory::GLOBAL_STACK_POINTER));
        }
    }

    pub(super) fn alloc_heap_home(&mut self, slot: u32, size: i32) {
        self.code.extend([
            Instruction::LocalGet(slot),
            Instruction::I32Eqz,
            Instruction::If(BlockType::Empty),
            Instruction::I32Const(size),
            Instruction::Call(self.alloc_func_idx),
            Instruction::LocalTee(slot),
            Instruction::I32Const(crate::memory::CELL_HEADER_SIZE),
            Instruction::I32Sub,
            Instruction::I32Const(crate::memory::CELL_SCRATCH_MAGIC),
            Instruction::I32Store(crate::memory::noffset_memarg()),
            Instruction::End,
            Instruction::LocalGet(slot),
            Instruction::LocalSet(self.scratch),
        ]);
    }

    pub(super) fn adopt_returned_home(&mut self, slot: u32, result: u32) {
        // AMIR has moved/dropped affine payloads before re-entering this site.
        // The previous backing is no longer referenced by this call-site temp.
        // Reclaim after the call, not before evaluating its arguments.
        self.code.extend([
            Instruction::LocalGet(slot),
            Instruction::If(BlockType::Empty),
            Instruction::LocalGet(slot),
            Instruction::I32Const(crate::memory::CELL_HEADER_SIZE),
            Instruction::I32Sub,
            Instruction::I32Const(crate::memory::CELL_MAGIC),
            Instruction::I32Store(crate::memory::noffset_memarg()),
            Instruction::LocalGet(slot),
            Instruction::Call(self.free_func_idx),
            Instruction::End,
            Instruction::LocalGet(result),
            Instruction::LocalTee(slot),
            Instruction::I32Const(crate::memory::CELL_HEADER_SIZE),
            Instruction::I32Sub,
            Instruction::I32Const(crate::memory::CELL_SCRATCH_MAGIC),
            Instruction::I32Store(crate::memory::noffset_memarg()),
        ]);
    }
}
