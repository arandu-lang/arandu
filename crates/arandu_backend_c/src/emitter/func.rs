//! C function body emission, local/temp declarations, and basic block lowering.

use std::fmt::Write;

use arandu_middle::amir::reachability::terminator_targets;
use arandu_middle::amir::{AmirFunc, AmirOperand, AmirRvalue, AmirStmt};

use super::CEmitter;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_func(&mut self, func: &AmirFunc) {
        self.integer_concat_temps =
            arandu_codegen::string_interp::integer_concat_temps(func, self.interner);
        self.temp_slots = super::temp_slots::scalar_slots(func, |temp| {
            matches!(self.interner.resolve(func.temps[temp.as_usize()].ty),
                arandu_middle::types::ArType::Primitive(primitive)
                    if primitive.is_numeric() || matches!(primitive,
                        arandu_middle::types::Primitive::Bool | arandu_middle::types::Primitive::Char))
        });
        self.expression_candidates = super::expression_plan::candidates(func, |index| {
            matches!(self.interner.resolve(func.temps[index].ty),
                arandu_middle::types::ArType::Primitive(primitive)
                if primitive.is_numeric() || matches!(primitive,
                    arandu_middle::types::Primitive::Bool | arandu_middle::types::Primitive::Char))
        });
        self.temp_expressions = vec![None; func.temps.len()];
        (self.rotations, self.rotation_elided) =
            super::rotations::plan(func, &self.program.literal_pool, self.interner);
        let name = self.c_function_name(func.symbol);
        let linkage = self.c_function_linkage(func.symbol);
        let ret_ty = self.c_func_return_type(func);
        let _ = write!(&mut self.output, "{linkage}{} {}(", ret_ty, name);
        self.emit_func_parameters(func);
        let _ = writeln!(&mut self.output, ") {{");

        // Declare locals and temps strictly at the top
        let mut used_locals = rustc_hash::FxHashSet::default();
        let mut used_temps = rustc_hash::FxHashSet::default();

        for stmt in func.stmts.payloads.iter() {
            arandu_middle::amir::visit::for_each_stmt_operand(stmt, |op| {
                if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = op {
                    used_temps.insert(temp.as_usize());
                }
            });
            match stmt {
                AmirStmt::Assign { lhs, rhs } => {
                    used_temps.insert(lhs.as_usize());
                    arandu_middle::amir::visit::for_each_rvalue_place(rhs, |place| {
                        used_locals.insert(place.local.as_usize());
                    });
                    if let AmirRvalue::RelativeBorrow { local, .. } = rhs {
                        used_locals.insert(local.as_usize());
                    }
                }
                AmirStmt::Call { lhs, .. } => {
                    if let Some(temp) = lhs {
                        used_temps.insert(temp.as_usize());
                    }
                }
                AmirStmt::Store { lhs, .. } | AmirStmt::Destroy(lhs) => {
                    used_locals.insert(lhs.local.as_usize());
                }
                // Storage markers emit no C, and block parameters bind temps:
                // neither names a local, so neither may declare one.
                AmirStmt::StorageLive(_)
                | AmirStmt::StorageDead(_)
                | AmirStmt::Free(_)
                | AmirStmt::Nop => {}
            }
        }
        for block in &func.blocks {
            for param in func.block_params(block.params) {
                used_temps.insert(param.id.as_usize());
            }
            arandu_middle::amir::visit::for_each_terminator_operand(&block.terminator, |op| {
                if let AmirOperand::Copy(temp) | AmirOperand::Move(temp) = op {
                    used_temps.insert(temp.as_usize());
                }
            });
        }
        for param in &func.params {
            used_temps.insert(param.as_usize());
        }

        for (i, local) in func.locals.iter().enumerate() {
            if used_locals.contains(&i) {
                let ty = self.interner.resolve(local.ty);
                if matches!(ty, arandu_middle::types::ArType::Void) {
                    continue;
                }
                let ty_str = self.format_type(&ty);
                let _ = writeln!(&mut self.output, "    {} l{};", ty_str, i);
            }
        }
        let used_slots: rustc_hash::FxHashSet<_> = used_temps
            .iter()
            .filter(|&&index| !self.expression_candidates[index] && !self.rotation_elided[index])
            .map(|&index| self.temp_slots[index])
            .collect();
        for (i, temp) in func.temps.iter().enumerate() {
            if self.temp_slots[i] == i && used_slots.contains(&i) {
                let ty = self.interner.resolve(temp.ty);
                if matches!(ty, arandu_middle::types::ArType::Void) {
                    continue;
                }
                if let Some(kind) = self.integer_concat_temps[i] {
                    let numeric_type = match kind {
                        arandu_codegen::string_interp::IntegerStringKind::Signed => "int64_t",
                        arandu_codegen::string_interp::IntegerStringKind::Unsigned => "uint64_t",
                    };
                    let _ = writeln!(&mut self.output, "    {numeric_type} ar_integer_{i};");
                    let _ = writeln!(&mut self.output, "    size_t ar_integer_len_{i};");
                } else {
                    let ty_str = self.format_type(&ty);
                    let _ = writeln!(&mut self.output, "    {} t{};", ty_str, i);
                }
            }
        }

        let _ = writeln!(&mut self.output);

        // Initialize temps from params
        for (i, _) in func.temps.iter().enumerate() {
            if func.params.iter().any(|&p| p.as_usize() == i) {
                if matches!(
                    self.interner.resolve(func.temps[i].ty),
                    arandu_middle::types::ArType::Void
                ) {
                    continue;
                }
                let _ = writeln!(&mut self.output, "    t{} = p{};", i, i);
            }
        }

        // Labels only for blocks that are jump targets (avoids -Wunused-label).
        let mut jump_targets = rustc_hash::FxHashSet::default();
        for block in &func.blocks {
            for target in terminator_targets(&block.terminator) {
                jump_targets.insert(target.as_usize());
            }
        }

        // Emit blocks
        for block in &func.blocks {
            let bid = block.id.as_usize();
            if jump_targets.contains(&bid) {
                let _ = writeln!(&mut self.output, "bb{bid}:");
            }
            for id in block.statements.iter_ids::<arandu_middle::amir::InstrId>() {
                self.current_initializer = Some(id);
                self.emit_stmt(func.stmt(id), func);
            }
            self.current_initializer = None;
            self.emit_terminator(&block.terminator, func);
        }

        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(&mut self.output);
    }
}
