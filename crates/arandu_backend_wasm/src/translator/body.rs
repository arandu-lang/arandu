//! Emission of a translated function body: statements, block arguments, and
//! the function epilogue.
//!
//! The stackifier has already resolved loops, blocks, and branch depths, so
//! this module only lowers the `Op` payloads and AMIR statements into
//! instructions. See `translator::mod` for the `Op` dispatch loop that drives
//! it.

use arandu_middle::amir::BlockId;
use arandu_middle::amir::local::TempId;
use arandu_middle::amir::stmt::AmirStmt;
use arandu_middle::amir::value::AmirOperand;
use wasm_encoder::{Instruction, ValType};

use super::FuncTranslator;
use crate::types::{self, Shape};

impl FuncTranslator<'_> {
    /// Emit the statements of a basic block.
    pub(super) fn emit_block_content(&mut self, block: BlockId) {
        // Block params were materialized by `emit_block_args` right before the
        // jump/branch that targets this block (see `ArgStore`/`Br`/`BrIf`).
        let block_data = &self.func.blocks[block.as_usize()];

        // Emit statements.
        for stmt_id in block_data
            .statements
            .iter_ids::<arandu_middle::amir::stmt::InstrId>()
        {
            if let Some(stmt) = self.func.stmts.get(stmt_id) {
                self.emit_stmt(stmt);
            }
        }
    }

    /// Emit a single statement.
    fn emit_stmt(&mut self, stmt: &AmirStmt) {
        match stmt {
            AmirStmt::Assign { lhs, rhs } => {
                self.emit_assign(*lhs, rhs);
            }
            AmirStmt::Store { lhs, rhs } => {
                self.emit_store(lhs, rhs);
            }
            AmirStmt::Call {
                lhs, callee, args, ..
            } => {
                self.emit_call(*lhs, callee, args);
            }
            AmirStmt::Free(op) => {
                self.emit_free(op);
            }
            AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) => {}
            AmirStmt::Destroy(place) => {
                self.emit_destroy(place);
            }
            AmirStmt::Nop => {}
        }
    }

    /// Materialize block arguments before a branch.
    pub(super) fn emit_block_args(&mut self, target: BlockId, args: &[AmirOperand]) {
        let target_block = &self.func.blocks[target.as_usize()];
        let block_params = self.func.block_params(target_block.params);
        for (j, arg) in args.iter().enumerate() {
            if j >= block_params.len() {
                break;
            }
            let param = &block_params[j];
            // Block params are ordinary temps; write the incoming value into the
            // parameter temp's wasm local, where the target block reads it.
            if let Some(&local) = self.temp_local.get(&param.id) {
                self.emit_operand_to_local(arg, param.ty, local);
            }
        }
    }

    /// Emit the return value and return instruction.
    pub(super) fn emit_return(&mut self) {
        let ret_ty = self.func.return_type;
        let shape = types::shape(ret_ty, self.interner, self.layout_engine.data_layout);
        let ret_temp = TempId::from_usize(0);
        match shape {
            Shape::Empty => {}
            Shape::Fat if self.retptr_return => {
                // Canonical ABI (MAX_FLAT_RESULTS = 1): a fat result is returned
                // as a single `i32` pointer to a 8-byte `(ptr, len)` pair stored
                // in guest memory. The host lifts by reading the pair at the
                // returned address, so only the pair buffer is materialized and
                // the payload stays wherever it lives (e.g. rodata).
                if let Some(&base) = self.temp_local.get(&ret_temp) {
                    self.alloc_cell(8);
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::LocalSet(self.scratch_b));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::LocalGet(base));
                    self.code
                        .push(Instruction::I32Store(crate::memory::noffset_memarg()));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::LocalGet(base + 1));
                    self.code.push(Instruction::I32Store(wasm_encoder::MemArg {
                        offset: 4,
                        align: crate::memory::MEM_ALIGN,
                        memory_index: 0,
                    }));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                } else {
                    // Degenerate fat return without a materialized temp: emit an
                    // empty `(0, 0)` pair so validation can never observe a
                    // garbage pointer.
                    self.alloc_cell(8);
                    self.code.push(Instruction::LocalGet(self.scratch));
                    self.code.push(Instruction::LocalSet(self.scratch_b));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::I32Const(0));
                    self.code
                        .push(Instruction::I32Store(crate::memory::noffset_memarg()));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                    self.code.push(Instruction::I32Const(0));
                    self.code.push(Instruction::I32Store(wasm_encoder::MemArg {
                        offset: 4,
                        align: crate::memory::MEM_ALIGN,
                        memory_index: 0,
                    }));
                    self.code.push(Instruction::LocalGet(self.scratch_b));
                }
            }
            Shape::Fat => {
                // Return (ptr, len) — two i32 values.
                if let Some(&base) = self.temp_local.get(&ret_temp) {
                    self.code.push(Instruction::LocalGet(base));
                    self.code.push(Instruction::LocalGet(base + 1));
                } else {
                    self.code.push(Instruction::I32Const(0));
                    self.code.push(Instruction::I32Const(0));
                }
            }
            Shape::Scalar => {
                if let Some(&local) = self.temp_local.get(&ret_temp) {
                    if let Some(sig) = self.component_sig
                        && !sig.retptr
                        && !sig.results.is_empty()
                        && matches!(
                            self.interner.resolve(ret_ty),
                            arandu_middle::types::ArType::Named(..)
                        )
                    {
                        // 1-field record returning unflattened scalar: load the field at offset 0
                        self.code.push(Instruction::LocalGet(local));
                        let vt = crate::emit::wasm_type_to_valtype(sig.results[0]);
                        match vt {
                            ValType::I32 => self
                                .code
                                .push(Instruction::I32Load(crate::memory::noffset_memarg())),
                            ValType::I64 => {
                                self.code.push(Instruction::I64Load(wasm_encoder::MemArg {
                                    offset: 0,
                                    align: 3,
                                    memory_index: 0,
                                }))
                            }
                            ValType::F32 => {
                                self.code.push(Instruction::F32Load(wasm_encoder::MemArg {
                                    offset: 0,
                                    align: 2,
                                    memory_index: 0,
                                }))
                            }
                            ValType::F64 => {
                                self.code.push(Instruction::F64Load(wasm_encoder::MemArg {
                                    offset: 0,
                                    align: 3,
                                    memory_index: 0,
                                }))
                            }
                            _ => self
                                .code
                                .push(Instruction::I32Load(crate::memory::noffset_memarg())),
                        }
                    } else {
                        self.code.push(Instruction::LocalGet(local));
                    }
                } else {
                    self.emit_zero(ret_ty);
                }
            }
        }
    }
}
