use arandu_middle::Diagnostic;
use arandu_middle::amir::{
    AmirBasicBlock, AmirOperand, AmirPlace, AmirStmt, AmirTerminator, BlockId,
};
use arandu_middle::types::{ArType, Primitive};

use super::{FuncEmitter, TypedValue, block_label, ice, quote_name, temp_name};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(super) fn emit_block_phis(&mut self, block: &AmirBasicBlock) -> Result<(), Diagnostic> {
        let params = self.func.block_params(block.params).to_vec();
        if params.is_empty() {
            return Ok(());
        }
        let incoming = self.incoming(block.id);
        if incoming.is_empty() {
            return Err(ice(format!(
                "block {} has parameters but no predecessors",
                block_label(block.id)
            )));
        }
        for (index, param) in params.iter().enumerate() {
            let ty = self.module.interner.resolve(param.ty);
            let ty_text = self.module.llvm_ty(&ty)?;
            let mut entries = Vec::with_capacity(incoming.len());
            let mut seen = Vec::new();
            for (predecessor, args) in &incoming {
                if seen.contains(predecessor) {
                    return Err(ice(format!(
                        "block {} has multiple edges from {} with parameters",
                        block_label(block.id),
                        block_label(*predecessor)
                    )));
                }
                seen.push(*predecessor);
                if args.len() != params.len() {
                    return Err(ice(format!(
                        "block {} expects {} arguments, predecessor {} provides {}",
                        block_label(block.id),
                        params.len(),
                        block_label(*predecessor),
                        args.len()
                    )));
                }
                let Some(edge) = self.edge_args.get(&(*predecessor, block.id)) else {
                    return Err(ice(format!(
                        "missing materialized edge args from {} to {}",
                        block_label(*predecessor),
                        block_label(block.id)
                    )));
                };
                if edge.len() != params.len() {
                    return Err(ice(format!(
                        "edge {} -> {} has {} args, expected {}",
                        block_label(*predecessor),
                        block_label(block.id),
                        edge.len(),
                        params.len()
                    )));
                }
                let llvm_pred = self
                    .terminator_labels
                    .get(predecessor)
                    .cloned()
                    .unwrap_or_else(|| block_label(*predecessor));
                entries.push(format!("[ {}, %{llvm_pred} ]", edge[index].value));
            }
            self.writer.line(format!(
                "{} = phi {ty_text} {}",
                temp_name(param.id),
                entries.join(", ")
            ));
        }
        Ok(())
    }

    pub(super) fn enter_llvm_label(&mut self, name: &str) {
        self.writer.line(format!("{name}:"));
        self.current_llvm_label = name.to_owned();
    }

    pub(super) fn record_edge_args(
        &mut self,
        target: BlockId,
        args: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        let target_block = self
            .func
            .blocks
            .iter()
            .find(|block| block.id == target)
            .ok_or_else(|| ice(format!("unknown block {}", block_label(target))))?;
        let params = self.func.block_params(target_block.params).to_vec();
        if args.len() != params.len() {
            return Err(ice(format!(
                "block {} expects {} arguments, edge from {} provides {}",
                block_label(target),
                params.len(),
                block_label(self.current_block),
                args.len()
            )));
        }
        let mut values = Vec::with_capacity(args.len());
        for (arg, param) in args.iter().zip(&params) {
            let ty = self.module.interner.resolve(param.ty);
            values.push(self.materialize_block_arg(arg, &ty)?);
        }
        self.edge_args.insert((self.current_block, target), values);
        Ok(())
    }

    /// Evaluate a block-argument operand in the *predecessor*, emitting any
    /// loads/coercions before the branch so the successor can use a pure PHI.
    pub(super) fn materialize_block_arg(
        &mut self,
        operand: &AmirOperand,
        expected: &ArType,
    ) -> Result<TypedValue, Diagnostic> {
        self.operand(operand, expected)
    }

    pub(super) fn incoming(&self, target: BlockId) -> Vec<(BlockId, Vec<AmirOperand>)> {
        let mut incoming = Vec::new();
        for block in &self.func.blocks {
            match &block.terminator {
                AmirTerminator::Goto { target: edge, args } if *edge == target => {
                    incoming.push((block.id, args.clone()))
                }
                AmirTerminator::Branch {
                    if_true,
                    true_args,
                    if_false,
                    false_args,
                    ..
                } => {
                    if *if_true == target {
                        incoming.push((block.id, true_args.clone()));
                    }
                    if *if_false == target {
                        incoming.push((block.id, false_args.clone()));
                    }
                }
                AmirTerminator::SwitchInt {
                    targets, otherwise, ..
                } => {
                    for (_, edge, args) in targets {
                        if *edge == target {
                            incoming.push((block.id, args.clone()));
                        }
                    }
                    if otherwise.0 == target {
                        incoming.push((block.id, otherwise.1.clone()));
                    }
                }
                AmirTerminator::Suspend { resume, args, .. } if *resume == target => {
                    incoming.push((block.id, args.clone()));
                }
                _ => {}
            }
        }
        incoming
    }

    pub(super) fn emit_statement(&mut self, statement: &AmirStmt) -> Result<(), Diagnostic> {
        self.pending_assignment = None;
        self.slot_value = None;
        match statement {
            AmirStmt::Assign { lhs, rhs } => {
                let ty = self.temp_type(*lhs);
                let result = self.destination(*lhs);
                self.emit_rvalue(&result, &ty, rhs, Some(*lhs))?;
                if let Some(slot) = self.temp_slot(*lhs).map(str::to_owned) {
                    let ty_text = self.temp_llvm_ty[lhs.as_usize()]
                        .clone()
                        .map_or_else(|| self.module.llvm_ty(&ty), Ok)?;
                    let align = self.module.align_of(&ty);
                    let value = self.slot_value.take().unwrap_or(result);
                    self.writer.line(format!(
                        "store {ty_text} {value}, ptr {slot}, align {align}"
                    ));
                }
                Ok(())
            }
            AmirStmt::Store { lhs, rhs } => {
                let (address, ty) = self.place(lhs)?;
                let value = self.operand(rhs, &ty)?;
                let align = self.module.align_of(&ty);
                self.writer.line(format!(
                    "store {}, ptr {address}, align {align}",
                    value.render()
                ));
                Ok(())
            }
            AmirStmt::Call {
                lhs, callee, args, ..
            } => {
                let destination = lhs.map(|lhs| self.destination(lhs));
                self.emit_call(*lhs, destination.as_deref(), callee, args)?;
                if let (Some(lhs), Some(value)) = (lhs, destination)
                    && let Some(slot) = self.temp_slot(*lhs).map(str::to_owned)
                {
                    let ty = self.temp_type(*lhs);
                    let ty_text = self.module.llvm_ty(&ty)?;
                    let align = self.module.align_of(&ty);
                    let value = self.slot_value.take().unwrap_or(value);
                    self.writer.line(format!(
                        "store {ty_text} {value}, ptr {slot}, align {align}"
                    ));
                }
                Ok(())
            }
            AmirStmt::Free(operand) => {
                let operand_ty = self.module.operand_type(operand, self.func);
                let pointer = if matches!(
                    operand_ty,
                    ArType::Primitive(Primitive::Str) | ArType::Slice(_) | ArType::MutSlice(_)
                ) {
                    let value = self.operand(operand, &operand_ty)?;
                    let pointer = self.new_value();
                    let fat = self.module.fat_ptr_llvm_ty();
                    self.writer
                        .line(format!("{pointer} = extractvalue {fat} {}, 0", value.value));
                    TypedValue::new("ptr".to_owned(), pointer)
                } else {
                    let pointer_ty = self.module.void_ptr_ty();
                    self.operand(operand, &pointer_ty)?
                };
                self.module.require_raw_free();
                self.writer
                    .line(format!("call void @ar_rt_raw_free({})", pointer.render()));
                Ok(())
            }
            AmirStmt::Destroy(place) => self.emit_destroy(place),
            AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => Ok(()),
        }
    }

    pub(super) fn emit_destroy(&mut self, place: &AmirPlace) -> Result<(), Diagnostic> {
        let (address, ty) = self.place(place)?;
        if matches!(ty, ArType::Primitive(Primitive::Str)) {
            let value_ty = self.module.llvm_ty(&ty)?;
            let value = self.new_value();
            self.writer.line(format!(
                "{value} = load {value_ty}, ptr {address}, align {}",
                self.module.align_of(&ty)
            ));
            let pointer = self.new_value();
            self.writer
                .line(format!("{pointer} = extractvalue {value_ty} {value}, 0"));
            self.module.require_raw_free();
            self.writer
                .line(format!("call void @ar_rt_raw_free(ptr {pointer})"));
            self.writer.line(format!(
                "store {value_ty} zeroinitializer, ptr {address}, align {}",
                self.module.align_of(&ty)
            ));
            return Ok(());
        }

        let type_id = self.module.interner.intern(ty.clone());
        let Some(destructor) = self.module.info.destructor_for_type(type_id) else {
            return Ok(());
        };
        let destructor_func = self
            .module
            .program
            .funcs
            .iter()
            .find(|func| func.symbol == destructor)
            .ok_or_else(|| {
                ice(format!(
                    "destructor {} for {ty:?} is not present in the LLVM module",
                    self.module.function_name(destructor)
                ))
            })?;
        let Some(&receiver) = destructor_func.params.first() else {
            return Err(ice(format!(
                "destructor {} has no receiver parameter",
                self.module.function_name(destructor)
            )));
        };
        let receiver_ty = self
            .module
            .interner
            .resolve(destructor_func.temps[receiver.as_usize()].ty);
        let argument = if matches!(
            receiver_ty,
            ArType::Ref(_) | ArType::RefMut(_) | ArType::Ptr(_)
        ) {
            TypedValue::new("ptr".to_owned(), address)
        } else {
            let value_ty = self.module.llvm_ty(&receiver_ty)?;
            let value = self.new_value();
            self.writer.line(format!(
                "{value} = load {value_ty}, ptr {address}, align {}",
                self.module.align_of(&receiver_ty)
            ));
            TypedValue::new(value_ty, value)
        };
        self.writer.line(format!(
            "call void @{}({})",
            quote_name(&self.module.function_name(destructor)),
            argument.render()
        ));
        Ok(())
    }
}
