use arandu_semantics::amir::AmirTerminator;
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use cranelift_codegen::ir::{BlockArg, InstBuilder, TrapCode};
use cranelift_frontend::Switch;

use super::FunctionTranslator;
use super::operand::FatOperandKind;
use crate::types::{ClifType, clif_type};

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn translate_terminator(&mut self, terminator: &AmirTerminator) {
        match terminator {
            AmirTerminator::Return => {
                if self.symbol_table.get(self.current_func.symbol).name == "main"
                    && let Some(&shutdown_id) = self.func_ids.get("ar_gen_shutdown_raw")
                {
                    let shutdown = self
                        .module
                        .declare_func_in_func(shutdown_id, self.builder.func);
                    self.builder.ins().call(shutdown, &[]);
                }
                let return_ty = self.resolve_ty(self.current_func.return_type);
                if matches!(&return_ty, ArType::Primitive(Primitive::Str)) {
                    let ret_temp = arandu_semantics::amir::TempId::from_usize(0);
                    if let Some(&(var_ptr, var_len)) = self.str_temp_map.get(&ret_temp) {
                        let ptr_val = self.builder.use_var(var_ptr);
                        let len_val = self.builder.use_var(var_len);
                        self.builder.ins().return_(&[ptr_val, len_val]);
                    } else {
                        let p = self.poison_i32();
                        self.builder.ins().return_(&[p, p]);
                    }
                } else if matches!(self.fat_operand_kind(&return_ty), FatOperandKind::Slice) {
                    let ret_temp = arandu_semantics::amir::TempId::from_usize(0);
                    let operand = arandu_semantics::amir::AmirOperand::Copy(ret_temp);
                    let (data, len) = self.translate_slice_operand(&operand);
                    self.builder.ins().return_(&[data, len]);
                } else if matches!(&return_ty, ArType::Named(_, _) | ArType::Tuple(_)) {
                    let arg_abi = self.classify_arg_abi(&return_ty);
                    match arg_abi {
                        arandu_semantics::layout::ArgAbi::ZeroSized => {
                            self.builder.ins().return_(&[]);
                        }
                        arandu_semantics::layout::ArgAbi::Direct(direct) => {
                            let ret_temp = arandu_semantics::amir::TempId::from_usize(0);
                            let base_ptr = if let Some(&var) = self.temp_map.get(&ret_temp) {
                                self.builder.use_var(var)
                            } else {
                                self.poison_value(self.ptr_type)
                            };
                            let mut chunks = Vec::with_capacity(direct.slots.len());
                            for abi_slot in &direct.slots {
                                let chunk_ty = crate::abi::abi_scalar_to_clif(abi_slot.scalar);
                                let chunk_val = self.builder.ins().load(
                                    chunk_ty,
                                    cranelift_codegen::ir::MemFlagsData::new(),
                                    base_ptr,
                                    abi_slot.offset as i32,
                                );
                                chunks.push(chunk_val);
                            }
                            self.builder.ins().return_(&chunks);
                        }
                        arandu_semantics::layout::ArgAbi::Indirect => {
                            let ret_temp = arandu_semantics::amir::TempId::from_usize(0);
                            if let Some(&var) = self.temp_map.get(&ret_temp) {
                                let ret_val = self.builder.use_var(var);
                                self.builder.ins().return_(&[ret_val]);
                            } else {
                                let poison = self.poison_value(self.ptr_type);
                                self.builder.ins().return_(&[poison]);
                            }
                        }
                    }
                } else {
                    let ret_ty = return_ty;
                    let clif_ret = clif_type(&ret_ty, self.ptr_type);
                    match clif_ret {
                        ClifType::Concrete(clif_ty) => {
                            let ret_temp = arandu_semantics::amir::TempId::from_usize(0);
                            if let Some(&var) = self.temp_map.get(&ret_temp) {
                                let ret_val = self.builder.use_var(var);
                                self.builder.ins().return_(&[ret_val]);
                            } else {
                                let poison = self.poison_value(clif_ty);
                                self.builder.ins().return_(&[poison]);
                            }
                        }
                        ClifType::Void => {
                            self.builder.ins().return_(&[]);
                        }
                    }
                }
            }

            AmirTerminator::Goto { target, args } => {
                let target_block = &self.current_func.blocks[target.as_usize()];
                let target_params = self.current_func.block_params(target_block.params);
                let mut clif_args = Vec::new();
                for (j, arg) in args.iter().enumerate() {
                    let param_ty = self.resolve_ty(target_params[j].ty);
                    match self.fat_operand_kind(&param_ty) {
                        FatOperandKind::Str => {
                            let (ptr_val, len_val) = self.translate_str_operand(arg);
                            clif_args.push(BlockArg::Value(ptr_val));
                            clif_args.push(BlockArg::Value(len_val));
                        }
                        FatOperandKind::Slice => {
                            let (data, len) = self.translate_slice_operand(arg);
                            clif_args.push(BlockArg::Value(data));
                            clif_args.push(BlockArg::Value(len));
                        }
                        FatOperandKind::None => {
                            if let ClifType::Concrete(ty) = clif_type(&param_ty, self.ptr_type) {
                                let val = self.translate_operand(arg, Some(ty));
                                clif_args.push(BlockArg::Value(val));
                            }
                        }
                    }
                }
                let clif_target = self.block_map[target];
                self.builder.ins().jump(clif_target, &clif_args);
            }
            AmirTerminator::Branch {
                condition,
                if_true,
                true_args,
                if_false,
                false_args,
            } => {
                let cond_val = self.translate_operand(condition, None);

                let true_block_def = &self.current_func.blocks[if_true.as_usize()];
                let true_params = self.current_func.block_params(true_block_def.params);
                let mut true_clif_args = Vec::new();
                for (j, arg) in true_args.iter().enumerate() {
                    let param_ty = self.resolve_ty(true_params[j].ty);
                    match self.fat_operand_kind(&param_ty) {
                        FatOperandKind::Str => {
                            let (ptr_val, len_val) = self.translate_str_operand(arg);
                            true_clif_args.push(BlockArg::Value(ptr_val));
                            true_clif_args.push(BlockArg::Value(len_val));
                        }
                        FatOperandKind::Slice => {
                            let (data, len) = self.translate_slice_operand(arg);
                            true_clif_args.push(BlockArg::Value(data));
                            true_clif_args.push(BlockArg::Value(len));
                        }
                        FatOperandKind::None => {
                            if let ClifType::Concrete(ty) = clif_type(&param_ty, self.ptr_type) {
                                let val = self.translate_operand(arg, Some(ty));
                                true_clif_args.push(BlockArg::Value(val));
                            }
                        }
                    }
                }

                let false_block_def = &self.current_func.blocks[if_false.as_usize()];
                let false_params = self.current_func.block_params(false_block_def.params);
                let mut false_clif_args = Vec::new();
                for (j, arg) in false_args.iter().enumerate() {
                    let param_ty = self.resolve_ty(false_params[j].ty);
                    match self.fat_operand_kind(&param_ty) {
                        FatOperandKind::Str => {
                            let (ptr_val, len_val) = self.translate_str_operand(arg);
                            false_clif_args.push(BlockArg::Value(ptr_val));
                            false_clif_args.push(BlockArg::Value(len_val));
                        }
                        FatOperandKind::Slice => {
                            let (data, len) = self.translate_slice_operand(arg);
                            false_clif_args.push(BlockArg::Value(data));
                            false_clif_args.push(BlockArg::Value(len));
                        }
                        FatOperandKind::None => {
                            if let ClifType::Concrete(ty) = clif_type(&param_ty, self.ptr_type) {
                                let val = self.translate_operand(arg, Some(ty));
                                false_clif_args.push(BlockArg::Value(val));
                            }
                        }
                    }
                }

                let true_block = self.block_map[if_true];
                let false_block = self.block_map[if_false];
                self.builder.ins().brif(
                    cond_val,
                    true_block,
                    &true_clif_args,
                    false_block,
                    &false_clif_args,
                );
            }

            AmirTerminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => {
                let disc_val = self.translate_operand(discriminant, None);
                let otherwise_block = self.block_map[&otherwise.0];
                assert!(
                    otherwise.1.is_empty(),
                    "SwitchInt otherwise block cannot have arguments"
                );

                let mut switch = Switch::new();
                for &(val, ref target, ref args) in targets {
                    assert!(
                        args.is_empty(),
                        "SwitchInt target block cannot have arguments"
                    );
                    let target_block = self.block_map[target];
                    switch.set_entry(val as u128, target_block);
                }
                switch.emit(&mut self.builder, disc_val, otherwise_block);
            }
            // A3.1/A3.5: suspension edge — jump to resume with live state args.
            // Zip params/args so A3.5 capture length mismatches never OOB.
            AmirTerminator::Suspend {
                future: _,
                resume,
                args,
            } => {
                let target_block = &self.current_func.blocks[resume.as_usize()];
                let target_params = self.current_func.block_params(target_block.params);
                let mut clif_args = Vec::new();
                for (param, arg) in target_params.iter().zip(args.iter()) {
                    let param_ty = self.resolve_ty(param.ty);
                    match self.fat_operand_kind(&param_ty) {
                        FatOperandKind::Str => {
                            let (ptr_val, len_val) = self.translate_str_operand(arg);
                            clif_args.push(BlockArg::Value(ptr_val));
                            clif_args.push(BlockArg::Value(len_val));
                        }
                        FatOperandKind::Slice => {
                            let (data, len) = self.translate_slice_operand(arg);
                            clif_args.push(BlockArg::Value(data));
                            clif_args.push(BlockArg::Value(len));
                        }
                        FatOperandKind::None => {
                            if let ClifType::Concrete(ty) = clif_type(&param_ty, self.ptr_type) {
                                let val = self.translate_operand(arg, Some(ty));
                                clif_args.push(BlockArg::Value(val));
                            }
                        }
                    }
                }
                // Extra AMIR params without args: poison (should not happen after A3.5).
                for param in target_params.iter().skip(args.len()) {
                    let param_ty = self.resolve_ty(param.ty);
                    match self.fat_operand_kind(&param_ty) {
                        FatOperandKind::Str => {
                            let p = self.builder.ins().iconst(self.ptr_type, 0);
                            clif_args.push(BlockArg::Value(p));
                            clif_args.push(BlockArg::Value(p));
                        }
                        FatOperandKind::Slice => {
                            let p = self.builder.ins().iconst(self.ptr_type, 0);
                            clif_args.push(BlockArg::Value(p));
                            clif_args.push(BlockArg::Value(p));
                        }
                        FatOperandKind::None => {
                            if let ClifType::Concrete(ty) = clif_type(&param_ty, self.ptr_type) {
                                let p = self.builder.ins().iconst(ty, 0);
                                clif_args.push(BlockArg::Value(p));
                            }
                        }
                    }
                }
                let clif_target = self.block_map[resume];
                self.builder.ins().jump(clif_target, &clif_args);
            }
            AmirTerminator::Unreachable => {
                self.builder.ins().trap(TrapCode::unwrap_user(1));
            }
        }
    }
}
