use arandu_base::span::Span;
use arandu_middle::Diagnostic;
use arandu_middle::amir::{
    AmirBasicBlock, AmirOperand, AmirStmt, AmirTerminator, BlockId, InstrId,
};
use arandu_middle::types::{ArType, Primitive};
use cranelift_codegen::ir::{InstBuilder, SourceLoc};
use cranelift_module::Module;
use rustc_hash::FxHashMap;

use super::{AmirVisitor, FunctionTranslator};
use crate::types::{ClifType, clif_type, clif_types};

impl<'a, 'b, M: Module> FunctionTranslator<'a, 'b, M> {
    fn set_debug_span(&mut self, span: Span) {
        let Some(locations) = &mut self.debug_locations else {
            return;
        };
        let Ok(index) = u32::try_from(locations.len()) else {
            self.record_ice("DWARF source-location table exceeds u32", span);
            return;
        };
        locations.push(span);
        self.builder.set_srcloc(SourceLoc::new(index));
        self.current_debug_location = Some(index);
    }

    fn operand_span(&self, operand: &AmirOperand) -> Span {
        match operand {
            AmirOperand::Copy(temp) | AmirOperand::Move(temp) => self.temp_span(*temp),
            AmirOperand::FunctionRef(symbol) | AmirOperand::GlobalRef(symbol) => {
                self.symbol_table.get(*symbol).span
            }
            AmirOperand::Constant(_) => self.func_span(),
        }
    }

    fn stmt_span(&self, stmt: &AmirStmt) -> Span {
        match stmt {
            AmirStmt::Assign { lhs, .. } => self.temp_span(*lhs),
            AmirStmt::Store { lhs, rhs } => {
                let rhs_span = self.operand_span(rhs);
                if rhs_span.start != rhs_span.end {
                    rhs_span
                } else {
                    self.local_span(lhs.local)
                }
            }
            AmirStmt::Call { lhs, callee, .. } => lhs
                .map(|temp| self.temp_span(temp))
                .unwrap_or_else(|| self.operand_span(callee)),
            AmirStmt::Free(operand) => self.operand_span(operand),
            AmirStmt::StorageLive(local)
            | AmirStmt::StorageDead(local)
            | AmirStmt::Destroy(arandu_middle::amir::AmirPlace { local, .. }) => {
                self.local_span(*local)
            }
            AmirStmt::Nop => self.func_span(),
        }
    }

    #[tracing::instrument(level = "trace", target = "arandu_backend_cranelift", skip(self))]

    pub fn translate(&mut self) -> Result<(), Diagnostic> {
        self.set_debug_span(self.func_span());
        for (idx, _block) in self.current_func.blocks.iter().enumerate() {
            let block_id = BlockId::from_usize(idx);
            let clif_block = self.builder.create_block();
            self.block_map.insert(block_id, clif_block);
        }

        for (idx, block) in self.current_func.blocks.iter().enumerate() {
            let block_id = BlockId::from_usize(idx);
            let clif_block = self.block_map[&block_id];
            if block_id.as_usize() > 0 {
                for param in self.current_func.block_params(block.params) {
                    let pty = self.resolve_ty(param.ty);
                    match self.fat_operand_kind(&pty) {
                        super::operand::FatOperandKind::Str
                        | super::operand::FatOperandKind::Slice => {
                            self.builder.append_block_param(clif_block, self.ptr_type);
                            self.builder.append_block_param(clif_block, self.ptr_type);
                        }
                        super::operand::FatOperandKind::None => {
                            for &clif_ty in &clif_types(&pty, self.ptr_type) {
                                self.builder.append_block_param(clif_block, clif_ty);
                            }
                        }
                    }
                }
            }
        }

        let entry_clif = self.block_map[&BlockId::from_usize(0)];
        self.builder
            .append_block_params_for_function_params(entry_clif);

        for local in &self.current_func.locals {
            let lty = self.resolve_ty(local.ty);
            if matches!(lty, ArType::Primitive(Primitive::Str)) {
                let var_ptr = self.builder.declare_var(self.ptr_type);
                let var_len = self.builder.declare_var(self.ptr_type);
                self.str_local_map.insert(local.id, (var_ptr, var_len));
                if local.is_memory {
                    let size = 2 * self.ptr_type.bytes();
                    let align_shift = self.ptr_type.bytes().trailing_zeros() as u8;
                    let slot = self.builder.create_sized_stack_slot(
                        cranelift_codegen::ir::StackSlotData {
                            kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                            size,
                            align_shift,
                            key: None,
                        },
                    );
                    self.local_stack_slots.insert(local.id, slot);
                }
            } else if let ClifType::Concrete(clif_ty) = clif_type(&lty, self.ptr_type) {
                let var = self.builder.declare_var(clif_ty);
                self.local_map.insert(local.id, var);
                // F2.0: address-taken scalars get a real stack slot so `&x` is valid.
                if local.is_memory && Self::needs_scalar_stack_home(&lty) {
                    let layout = self.checked_layout(&lty);
                    let size = u32::try_from(layout.size.max(1)).unwrap_or_else(|_| {
                        self.record_ice(
                            "type layout exceeds Cranelift's u32 stack-slot limit",
                            local.span,
                        );
                        1
                    });
                    let align = layout.align.max(1);
                    let align_shift = align.trailing_zeros() as u8;
                    let slot = self.builder.create_sized_stack_slot(
                        cranelift_codegen::ir::StackSlotData {
                            kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                            size,
                            align_shift,
                            key: None,
                        },
                    );
                    self.local_stack_slots.insert(local.id, slot);
                }
            }
        }

        let temp_slots = arandu_codegen::temp_slots::block_local_temp_slots(
            self.current_func,
            |temp| {
                matches!(self.resolve_ty(self.current_func.temps[temp.as_usize()].ty),
                ArType::Primitive(primitive) if primitive.is_numeric() || matches!(primitive, Primitive::Bool | Primitive::Char))
            },
        );
        let mut slot_vars = FxHashMap::default();
        for temp in &self.current_func.temps {
            let tty = self.resolve_ty(temp.ty);
            if matches!(tty, ArType::Primitive(Primitive::Str)) {
                let var_ptr = self.builder.declare_var(self.ptr_type);
                let var_len = self.builder.declare_var(self.ptr_type);
                self.str_temp_map.insert(temp.id, (var_ptr, var_len));
            } else if let ClifType::Concrete(clif_ty) = clif_type(&tty, self.ptr_type) {
                // Reused values have one dominating definition and all uses
                // in that same block; SSABuilder never merges their states.
                let var = *slot_vars
                    .entry(temp_slots[temp.id.as_usize()])
                    .or_insert_with(|| self.builder.declare_var(clif_ty));
                self.temp_map.insert(temp.id, var);
            }
        }

        let rpo = arandu_middle::amir::reverse_post_order(self.current_func);
        for &block_id in &rpo {
            let block = self.current_func.block(block_id);
            self.visit_block(block);
        }

        self.finish_heap_scratch();
        self.builder.seal_all_blocks();

        if let Some(error) = self.error.take() {
            return Err(error);
        }
        Ok(())
    }
}

impl<'a, 'b, M: Module> AmirVisitor for FunctionTranslator<'a, 'b, M> {
    fn visit_block(&mut self, block: &AmirBasicBlock) {
        if self.error.is_some() {
            return;
        }
        let clif_block = self.block_map[&block.id];
        self.builder.switch_to_block(clif_block);
        if let Some((coverage_func, function_index, session_id)) = self.block_coverage {
            let Ok(block_index) = i64::try_from(block.id.as_usize()) else {
                self.record_ice(
                    "AMIR block index exceeds the JIT coverage ABI",
                    self.func_span(),
                );
                return;
            };
            let func_ref = self
                .module
                .declare_func_in_func(coverage_func, self.builder.func);
            let session_id = self
                .builder
                .ins()
                .iconst(cranelift_codegen::ir::types::I64, session_id);
            let function_index = self
                .builder
                .ins()
                .iconst(cranelift_codegen::ir::types::I64, i64::from(function_index));
            let block_index = self
                .builder
                .ins()
                .iconst(cranelift_codegen::ir::types::I64, block_index);
            self.builder
                .ins()
                .call(func_ref, &[session_id, function_index, block_index]);
        }

        if block.id.as_usize() == 0 {
            for local in &self.current_func.locals {
                let lty = self.resolve_ty(local.ty);
                if matches!(lty, ArType::Primitive(Primitive::Str)) {
                    let &(var_ptr, var_len) = &self.str_local_map[&local.id];
                    let zero_ptr = self.builder.ins().iconst(self.ptr_type, 0);
                    let zero_len = self.builder.ins().iconst(self.ptr_type, 0);
                    self.builder.def_var(var_ptr, zero_ptr);
                    self.builder.def_var(var_len, zero_len);
                    if let Some(&slot) = self.local_stack_slots.get(&local.id) {
                        let addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            zero_ptr,
                            addr,
                            0,
                        );
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            zero_len,
                            addr,
                            self.ptr_type.bytes() as i32,
                        );
                    }
                } else if let Some(&var) = self.local_map.get(&local.id) {
                    let Some(clif_ty) = clif_type(&lty, self.ptr_type).concrete() else {
                        continue;
                    };
                    let zero = self.poison_value(clif_ty);
                    self.builder.def_var(var, zero);
                }
            }

            let clif_params = self.builder.block_params(clif_block).to_vec();
            let mut clif_slot_idx = 0;
            if self
                .builder
                .func
                .signature
                .params
                .first()
                .is_some_and(|param| {
                    param.purpose == cranelift_codegen::ir::ArgumentPurpose::StructReturn
                })
            {
                self.indirect_return_destination = clif_params.first().copied();
                clif_slot_idx = 1;
            }
            for &param_temp_id in &self.current_func.params {
                let param_ty = self.temp_ar_ty(param_temp_id);
                if matches!(&param_ty, ArType::Primitive(Primitive::Str)) {
                    let ptr_val = clif_params[clif_slot_idx];
                    let len_val = clif_params[clif_slot_idx + 1];
                    clif_slot_idx += 2;
                    if let Some(&(var_ptr, var_len)) = self.str_temp_map.get(&param_temp_id) {
                        self.builder.def_var(var_ptr, ptr_val);
                        self.builder.def_var(var_len, len_val);
                    }
                } else if matches!(
                    self.fat_operand_kind(&param_ty),
                    super::operand::FatOperandKind::Slice
                ) {
                    let data = clif_params[clif_slot_idx];
                    let len = clif_params[clif_slot_idx + 1];
                    clif_slot_idx += 2;
                    let descriptor = self.materialize_slice_descriptor(data, len);
                    if let Some(&var) = self.temp_map.get(&param_temp_id) {
                        self.builder.def_var(var, descriptor);
                    }
                } else if matches!(
                    param_ty,
                    ArType::Option(_) | ArType::Result(..) | ArType::Poll(_)
                ) {
                    let source = clif_params[clif_slot_idx];
                    clif_slot_idx += 1;
                    let destination = self.allocate_independent_aggregate(&param_ty);
                    let layout = self.checked_layout(&param_ty);
                    self.copy_aggregate_bytes(destination, source, layout.size);
                    if let Some(&var) = self.temp_map.get(&param_temp_id) {
                        self.builder.def_var(var, destination);
                    }
                } else if matches!(
                    &param_ty,
                    ArType::Named(_, _) | ArType::Tuple(_) | ArType::Array(_, _)
                ) {
                    let arg_abi = self.classify_arg_abi(&param_ty);
                    match arg_abi {
                        arandu_middle::layout::ArgAbi::ZeroSized => {
                            let null_ptr = self.builder.ins().iconst(self.ptr_type, 0);
                            if let Some(&var) = self.temp_map.get(&param_temp_id) {
                                self.builder.def_var(var, null_ptr);
                            }
                        }
                        arandu_middle::layout::ArgAbi::Direct(direct) => {
                            let layout = self.checked_layout(&param_ty);
                            let addr = self.allocate_independent_aggregate(&param_ty);
                            for abi_slot in &direct.slots {
                                let chunk_val = clif_params[clif_slot_idx];
                                clif_slot_idx += 1;
                                self.store_abi_slot(chunk_val, addr, abi_slot, layout.size);
                            }
                            if let Some(&var) = self.temp_map.get(&param_temp_id) {
                                self.builder.def_var(var, addr);
                            }
                        }
                        arandu_middle::layout::ArgAbi::Indirect => {
                            let ptr_val = clif_params[clif_slot_idx];
                            clif_slot_idx += 1;
                            // A by-value parameter is not a borrow of the
                            // caller's backing object. In particular a callee
                            // constructing an escaping coroutine needs its own
                            // storage, not the caller's transient frame slot.
                            let destination = self.allocate_independent_aggregate(&param_ty);
                            let layout = self.checked_layout(&param_ty);
                            self.copy_aggregate_bytes(destination, ptr_val, layout.size);
                            if let Some(&var) = self.temp_map.get(&param_temp_id) {
                                self.builder.def_var(var, destination);
                            }
                        }
                    }
                } else if let ClifType::Concrete(_) = clif_type(&param_ty, self.ptr_type) {
                    let val = clif_params[clif_slot_idx];
                    clif_slot_idx += 1;
                    if let Some(&var) = self.temp_map.get(&param_temp_id) {
                        self.builder.def_var(var, val);
                    }
                }
                if let Some(var) = self.temp_map.get(&param_temp_id).copied() {
                    let value = self.builder.use_var(var);
                    self.label_temp_value(param_temp_id, value);
                }
            }
        } else {
            let clif_params = self.builder.block_params(clif_block).to_vec();
            let mut clif_slot_idx = 0;
            for param in self.current_func.block_params(block.params) {
                let pty = self.resolve_ty(param.ty);
                if matches!(pty, ArType::Primitive(Primitive::Str)) {
                    let ptr_val = clif_params[clif_slot_idx];
                    let len_val = clif_params[clif_slot_idx + 1];
                    clif_slot_idx += 2;
                    if let Some(&(var_ptr, var_len)) = self.str_temp_map.get(&param.id) {
                        self.builder.def_var(var_ptr, ptr_val);
                        self.builder.def_var(var_len, len_val);
                    }
                } else if matches!(
                    self.fat_operand_kind(&pty),
                    super::operand::FatOperandKind::Slice
                ) {
                    let data = clif_params[clif_slot_idx];
                    let len = clif_params[clif_slot_idx + 1];
                    clif_slot_idx += 2;
                    let descriptor = self.materialize_slice_descriptor(data, len);
                    if let Some(&var) = self.temp_map.get(&param.id) {
                        self.builder.def_var(var, descriptor);
                    }
                } else if let ClifType::Concrete(_) = clif_type(&pty, self.ptr_type) {
                    let val = clif_params[clif_slot_idx];
                    clif_slot_idx += 1;
                    if let Some(&var) = self.temp_map.get(&param.id) {
                        self.builder.def_var(var, val);
                    }
                }
                if let Some(var) = self.temp_map.get(&param.id).copied() {
                    let value = self.builder.use_var(var);
                    self.label_temp_value(param.id, value);
                }
            }
        }

        for stmt_id in block.statements.iter_ids::<InstrId>() {
            let stmt = self.current_func.stmt(stmt_id);
            self.current_initializer = Some(stmt_id);
            self.visit_stmt(stmt);
            self.current_initializer = None;
            if self.is_current_block_terminated() {
                break;
            }
        }

        if !self.is_current_block_terminated() {
            self.visit_terminator(&block.terminator);
        }
    }

    fn visit_stmt(&mut self, stmt: &AmirStmt) {
        self.set_debug_span(self.stmt_span(stmt));
        self.translate_stmt(stmt);
    }

    fn visit_terminator(&mut self, term: &AmirTerminator) {
        self.translate_terminator(term);
    }
}
