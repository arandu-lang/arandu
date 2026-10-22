use std::collections::HashMap;

use arandu_middle::Diagnostic;
use arandu_middle::amir::{AmirFunc, AmirStmt, BlockId, TempId};
use arandu_middle::types::ArType;

use super::{
    FuncEmitter, ModuleEmitter, TypedValue, block_label, ice, local_name, quote_name, temp_name,
};
use crate::writer::IrWriter;

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(super) fn new(
        module: &'m ModuleEmitter<'a>,
        func: &'a AmirFunc,
    ) -> Result<Self, Diagnostic> {
        let mut this = Self {
            module,
            func,
            writer: IrWriter::default(),
            defined: vec![None; func.temps.len()],
            temp_slots: vec![None; func.temps.len()],
            temp_values: vec![None; func.temps.len()],
            pending_assignment: None,
            temp_llvm_ty: vec![None; func.temps.len()],
            next_value: 0,
            current_return: None,
            return_counter: 0,
            current_block: BlockId::from_usize(0),
            current_llvm_label: String::new(),
            terminator_labels: HashMap::new(),
            edge_args: HashMap::new(),
        };
        for temp in &func.params {
            this.define(*temp, "function parameter")?;
        }
        let block_params = func
            .blocks
            .iter()
            .flat_map(|block| func.block_params(block.params).iter().cloned())
            .collect::<Vec<_>>();
        let assignments = func
            .blocks
            .iter()
            .flat_map(|block| func.block_stmts(block.id))
            .filter_map(|statement| match statement {
                AmirStmt::Assign { lhs, .. } => Some(*lhs),
                AmirStmt::Call { lhs, .. } => *lhs,
                _ => None,
            })
            .collect::<Vec<_>>();
        for param in &block_params {
            if assignments.contains(&param.id) {
                return Err(ice(format!(
                    "AMIR block parameter %t{} is also assigned, which the LLVM backend cannot represent as SSA",
                    param.id.as_usize()
                )));
            }
            this.define(param.id, "block parameter")?;
        }
        let mut counts = vec![0usize; func.temps.len()];
        for temp in &assignments {
            counts[temp.as_usize()] += 1;
        }
        for temp in assignments {
            if counts[temp.as_usize()] > 1 {
                this.temp_slots[temp.as_usize()] = Some(String::new());
            } else if temp.as_usize() != 0 {
                this.define(temp, "assignment")?;
            }
        }
        Ok(this)
    }

    pub(super) fn temp_slot(&self, temp: TempId) -> Option<&str> {
        self.temp_slots[temp.as_usize()].as_deref()
    }

    pub(super) fn bind_temp(&mut self, lhs: TempId, value: TypedValue) -> Result<(), Diagnostic> {
        if self.temp_slots[lhs.as_usize()].is_some() {
            return Err(ice(format!(
                "AMIR temp %t{} has a stack slot but was bound as an SSA alias",
                lhs.as_usize()
            )));
        }
        if lhs.as_usize() == 0 {
            self.current_return = Some(value.value.clone());
        }
        self.temp_values[lhs.as_usize()] = Some(value);
        Ok(())
    }

    pub(super) fn destination(&mut self, lhs: TempId) -> String {
        self.pending_assignment = Some(lhs);
        if self.temp_slots[lhs.as_usize()].is_some() {
            let name = self.new_value();
            if lhs.as_usize() == 0 {
                self.current_return = Some(name.clone());
            }
            return name;
        }
        if lhs.as_usize() == 0 {
            let name = format!("%ret{}", self.return_counter);
            self.return_counter += 1;
            self.current_return = Some(name.clone());
            name
        } else {
            temp_name(lhs)
        }
    }

    pub(super) fn return_value(&self) -> Option<String> {
        self.current_return.clone()
    }

    fn define(&mut self, temp: TempId, source: &'static str) -> Result<(), Diagnostic> {
        if let Some(previous) = self.defined[temp.as_usize()] {
            return Err(ice(format!(
                "AMIR temp %t{} is defined twice ({previous} and {source})",
                temp.as_usize()
            )));
        }
        self.defined[temp.as_usize()] = Some(source);
        Ok(())
    }

    pub(super) fn ensure_defined(&self, temp: TempId) -> Result<(), Diagnostic> {
        if self.defined[temp.as_usize()].is_none() && self.temp_slots[temp.as_usize()].is_none() {
            return Err(ice(format!(
                "AMIR temp %t{} is used without a definition",
                temp.as_usize()
            )));
        }
        Ok(())
    }

    pub(super) fn temp_type(&self, temp: TempId) -> ArType {
        self.module
            .interner
            .resolve(self.func.temps[temp.as_usize()].ty)
    }

    pub(super) fn emit(mut self) -> Result<String, Diagnostic> {
        let return_ty = self.module.interner.resolve(self.func.return_type);
        let function_name = self.module.function_name(self.func.symbol);
        let is_main = function_name == "main";
        let return_text = if is_main && self.module.is_void(&return_ty) {
            "i32".to_owned()
        } else {
            self.module.llvm_ty(&return_ty)?
        };

        let params = self
            .func
            .params
            .iter()
            .map(|temp| {
                let ty = self.temp_type(*temp);
                let attribute = if matches!(ty, ArType::Ref(_) | ArType::RefMut(_)) {
                    "nonnull "
                } else {
                    ""
                };
                Ok(format!(
                    "{} {attribute}{}",
                    self.module.llvm_ty(&ty)?,
                    temp_name(*temp)
                ))
            })
            .collect::<Result<Vec<_>, Diagnostic>>()?;

        let linkage = self
            .module
            .definition_linkage(self.func.symbol, &function_name);
        self.writer.line(format!(
            "define {linkage}{return_text} @{}({}) nounwind {{",
            quote_name(&function_name),
            params.join(", ")
        ));

        let Some(entry) = self.func.blocks.first() else {
            return Err(ice("AMIR function has no basic blocks"));
        };
        let entry_id = entry.id;
        if !self.func.block_params(entry.params).is_empty() {
            return Err(ice("AMIR entry block must not have block parameters"));
        }
        self.writer.line(format!("{}:", block_label(entry_id)));

        // AMIR pruning removes load/store traffic for locals that never have
        // their address taken, leaving them as metadata only (symbol/span for
        // diagnostics). Allocate a stack home exclusively for locals that are
        // genuinely memory-backed, matching the Cranelift backend's rule.
        for local in self.func.locals.iter().filter(|local| local.is_memory) {
            let ty = self.module.interner.resolve(local.ty);
            let (alloca, align) = self.module.alloca_type(&ty)?;
            self.writer.line(format!(
                "{} = alloca {alloca}, align {align}",
                local_name(local.id)
            ));
        }
        for temp in 0..self.func.temps.len() {
            if self.temp_slots[temp].is_some() {
                let ty = self.module.interner.resolve(self.func.temps[temp].ty);
                let (alloca, align) = self.module.alloca_type(&ty)?;
                let slot = self.new_value();
                self.writer
                    .line(format!("{slot} = alloca {alloca}, align {align}"));
                self.temp_slots[temp] = Some(slot);
            }
        }

        // Pass 1: lower each block body + terminator into a private buffer so
        // edge arguments are materialized in predecessors before any successor
        // PHI is written (LLVM requires phis to be grouped at the block top).
        let blocks = self.func.blocks.clone();
        let mut bodies = Vec::with_capacity(blocks.len());
        for block in &blocks {
            self.current_block = block.id;
            self.current_llvm_label = block_label(block.id);
            self.current_return = None;
            let mut body = IrWriter::default();
            std::mem::swap(&mut self.writer, &mut body);
            let statements = self.func.block_stmts(block.id).cloned().collect::<Vec<_>>();
            for statement in &statements {
                self.emit_statement(statement)?;
            }
            let terminator = block.terminator.clone();
            self.terminator_labels
                .insert(block.id, self.current_llvm_label.clone());
            self.emit_terminator(&terminator)?;
            std::mem::swap(&mut self.writer, &mut body);
            bodies.push(body.finish());
        }

        // Pass 2: emit labels, PHI nodes from recorded edge args, then bodies.
        for (block, body) in blocks.iter().zip(bodies) {
            if block.id != entry_id {
                self.writer.blank();
                self.writer.line(format!("{}:", block_label(block.id)));
            }
            self.emit_block_phis(block)?;
            self.writer.append_raw(&body);
        }

        self.writer.line("}");
        Ok(self.writer.finish())
    }
}
