//! C function body emission, local/temp declarations, and basic block lowering.

use std::fmt::Write;

use arandu_middle::amir::{
    AmirFunc, AmirOperand, AmirProjection, AmirRvalue, AmirStmt, AmirTerminator,
};

use super::{CEmitter, sanitize_c_ident};

impl<'a> CEmitter<'a> {
    pub(super) fn emit_func(&mut self, func: &AmirFunc) {
        let sym = self.symbols.get(func.symbol);
        let name = sanitize_c_ident(self.symbols.host_func_name(sym));
        let ret_ty = self.c_func_return_type(func);
        let _ = write!(&mut self.output, "{} {}(", ret_ty, name);
        for (i, param) in func.params.iter().enumerate() {
            if i > 0 {
                let _ = write!(&mut self.output, ", ");
            }
            let ty = self.temp_ty(func, *param);
            let ty_str = self.format_type(&ty);
            let _ = write!(&mut self.output, "{} p{}", ty_str, param.as_usize());
        }
        if func.params.is_empty() {
            let _ = write!(&mut self.output, "void");
        }
        let _ = writeln!(&mut self.output, ") {{");

        // Declare locals and temps strictly at the top
        let mut used_locals = rustc_hash::FxHashSet::default();
        let mut used_temps = rustc_hash::FxHashSet::default();

        for stmt in func.stmts.payloads.iter() {
            match stmt {
                AmirStmt::Assign { lhs, rhs } => {
                    used_temps.insert(lhs.as_usize());
                    match rhs {
                        AmirRvalue::Use(op)
                        | AmirRvalue::Unary { operand: op, .. }
                        | AmirRvalue::Discriminant { value: op }
                        | AmirRvalue::EnumPayload { value: op, .. }
                        | AmirRvalue::Len(op)
                        | AmirRvalue::Alloc(op)
                        | AmirRvalue::ToStr { value: op, .. }
                        | AmirRvalue::BlackBox { value: op, .. }
                        | AmirRvalue::StrBytes { source: op }
                        | AmirRvalue::StrView { owner: op }
                        | AmirRvalue::CoroutineReady { value: op, .. } => {
                            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                                used_temps.insert(t.as_usize());
                            }
                        }
                        AmirRvalue::Binary { left, right, .. } => {
                            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = left {
                                used_temps.insert(t.as_usize());
                            }
                            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = right {
                                used_temps.insert(t.as_usize());
                            }
                        }
                        AmirRvalue::SliceView { owner, data, len } => {
                            for operand in [owner, data, len] {
                                if let AmirOperand::Copy(t) | AmirOperand::Move(t) = operand {
                                    used_temps.insert(t.as_usize());
                                }
                            }
                        }
                        AmirRvalue::SliceSubslice { slice, start, len } => {
                            for operand in [slice, start, len] {
                                if let AmirOperand::Copy(t) | AmirOperand::Move(t) = operand {
                                    used_temps.insert(t.as_usize());
                                }
                            }
                        }
                        AmirRvalue::SliceData(operand) => {
                            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = operand {
                                used_temps.insert(t.as_usize());
                            }
                        }
                        AmirRvalue::FieldAccess { base, .. } => {
                            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = base {
                                used_temps.insert(t.as_usize());
                            }
                        }
                        AmirRvalue::StructLiteral { fields, .. } => {
                            for (_, op) in fields {
                                if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                                    used_temps.insert(t.as_usize());
                                }
                            }
                        }
                        AmirRvalue::IndexAccess { base, index } => {
                            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = base {
                                used_temps.insert(t.as_usize());
                            }
                            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = index {
                                used_temps.insert(t.as_usize());
                            }
                        }
                        AmirRvalue::Array { items } | AmirRvalue::Tuple { items } => {
                            for op in items {
                                if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                                    used_temps.insert(t.as_usize());
                                }
                            }
                        }
                        AmirRvalue::EnumConstruct { payload, .. } => {
                            if let Some(AmirOperand::Copy(t) | AmirOperand::Move(t)) = payload {
                                used_temps.insert(t.as_usize());
                            }
                        }
                        AmirRvalue::Load(place)
                        | AmirRvalue::Borrow(place)
                        | AmirRvalue::BorrowMut(place) => {
                            used_locals.insert(place.local.as_usize());
                            for proj in &place.projections {
                                if let AmirProjection::Index(
                                    AmirOperand::Copy(t) | AmirOperand::Move(t),
                                ) = proj
                                {
                                    used_temps.insert(t.as_usize());
                                }
                            }
                        }
                        AmirRvalue::RelativeBorrow { local, .. } => {
                            used_locals.insert(local.as_usize());
                        }
                        AmirRvalue::GenInsert { value, .. }
                        | AmirRvalue::GenGet { gen_ref: value, .. }
                        | AmirRvalue::GenRemove { gen_ref: value, .. } => {
                            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = value {
                                used_temps.insert(t.as_usize());
                            }
                        }
                        AmirRvalue::GenSet { gen_ref, value, .. }
                        | AmirRvalue::GenUpsert { gen_ref, value, .. } => {
                            for operand in [gen_ref, value] {
                                if let AmirOperand::Copy(t) | AmirOperand::Move(t) = operand {
                                    used_temps.insert(t.as_usize());
                                }
                            }
                        }
                        AmirRvalue::StringInterp { parts } => {
                            for op in parts {
                                if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                                    used_temps.insert(t.as_usize());
                                }
                            }
                        }
                    }
                }
                AmirStmt::Store { lhs, rhs } => {
                    used_locals.insert(lhs.local.as_usize());
                    for proj in &lhs.projections {
                        if let AmirProjection::Index(AmirOperand::Copy(t) | AmirOperand::Move(t)) =
                            proj
                        {
                            used_temps.insert(t.as_usize());
                        }
                    }
                    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = rhs {
                        used_temps.insert(t.as_usize());
                    }
                }
                AmirStmt::Call {
                    lhs, callee, args, ..
                } => {
                    if let Some(t) = lhs {
                        used_temps.insert(t.as_usize());
                    }
                    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = callee {
                        used_temps.insert(t.as_usize());
                    }
                    for arg in args {
                        if let AmirOperand::Copy(t) | AmirOperand::Move(t) = arg {
                            used_temps.insert(t.as_usize());
                        }
                    }
                }
                AmirStmt::Free(op) => {
                    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                        used_temps.insert(t.as_usize());
                    }
                }
                AmirStmt::StorageLive(local) | AmirStmt::StorageDead(local) => {
                    used_locals.insert(local.as_usize());
                }
                AmirStmt::Destroy(place) => {
                    used_locals.insert(place.local.as_usize());
                    for proj in &place.projections {
                        if let AmirProjection::Index(AmirOperand::Copy(t) | AmirOperand::Move(t)) =
                            proj
                        {
                            used_temps.insert(t.as_usize());
                        }
                    }
                }
                AmirStmt::Nop => {}
            }
        }
        for block in &func.blocks {
            for param in func.block_params(block.params) {
                used_temps.insert(param.id.as_usize());
                used_locals.insert(param.local.as_usize());
            }
            match &block.terminator {
                AmirTerminator::Goto { args, .. } => {
                    for arg in args {
                        if let AmirOperand::Copy(t) | AmirOperand::Move(t) = arg {
                            used_temps.insert(t.as_usize());
                        }
                    }
                }
                AmirTerminator::Suspend { future, args, .. } => {
                    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = future {
                        used_temps.insert(t.as_usize());
                    }
                    for arg in args {
                        if let AmirOperand::Copy(t) | AmirOperand::Move(t) = arg {
                            used_temps.insert(t.as_usize());
                        }
                    }
                }
                AmirTerminator::Branch {
                    condition,
                    true_args,
                    false_args,
                    ..
                } => {
                    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = condition {
                        used_temps.insert(t.as_usize());
                    }
                    for arg in true_args {
                        if let AmirOperand::Copy(t) | AmirOperand::Move(t) = arg {
                            used_temps.insert(t.as_usize());
                        }
                    }
                    for arg in false_args {
                        if let AmirOperand::Copy(t) | AmirOperand::Move(t) = arg {
                            used_temps.insert(t.as_usize());
                        }
                    }
                }
                AmirTerminator::SwitchInt {
                    discriminant,
                    targets,
                    otherwise,
                    ..
                } => {
                    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = discriminant {
                        used_temps.insert(t.as_usize());
                    }
                    for (_, _, args) in targets {
                        for arg in args {
                            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = arg {
                                used_temps.insert(t.as_usize());
                            }
                        }
                    }
                    for arg in &otherwise.1 {
                        if let AmirOperand::Copy(t) | AmirOperand::Move(t) = arg {
                            used_temps.insert(t.as_usize());
                        }
                    }
                }
                _ => {}
            }
        }
        for param in &func.params {
            used_temps.insert(param.as_usize());
        }

        for (i, local) in func.locals.iter().enumerate() {
            if used_locals.contains(&i) {
                let ty = self.interner.resolve(local.ty);
                let ty_str = self.format_type(&ty);
                let _ = writeln!(&mut self.output, "    {} l{};", ty_str, i);
            }
        }
        for (i, temp) in func.temps.iter().enumerate() {
            if used_temps.contains(&i) {
                let ty = self.interner.resolve(temp.ty);
                let ty_str = self.format_type(&ty);
                let _ = writeln!(&mut self.output, "    {} t{};", ty_str, i);
            }
        }

        let _ = writeln!(&mut self.output);

        // Initialize temps from params
        for (i, _) in func.temps.iter().enumerate() {
            if func.params.iter().any(|&p| p.as_usize() == i) {
                let _ = writeln!(&mut self.output, "    t{} = p{};", i, i);
            }
        }

        // Labels only for blocks that are jump targets (avoids -Wunused-label).
        let mut jump_targets = rustc_hash::FxHashSet::default();
        for block in &func.blocks {
            match &block.terminator {
                AmirTerminator::Goto { target, .. } => {
                    jump_targets.insert(target.as_usize());
                }
                AmirTerminator::Suspend { resume, .. } => {
                    jump_targets.insert(resume.as_usize());
                }
                AmirTerminator::Branch {
                    if_true, if_false, ..
                } => {
                    jump_targets.insert(if_true.as_usize());
                    jump_targets.insert(if_false.as_usize());
                }
                AmirTerminator::SwitchInt {
                    targets, otherwise, ..
                } => {
                    for (_, t, _) in targets {
                        jump_targets.insert(t.as_usize());
                    }
                    jump_targets.insert(otherwise.0.as_usize());
                }
                AmirTerminator::Return | AmirTerminator::Unreachable => {}
            }
        }

        // Emit blocks
        for block in &func.blocks {
            let bid = block.id.as_usize();
            if jump_targets.contains(&bid) {
                let _ = writeln!(&mut self.output, "bb{bid}:");
            }
            for stmt in func.block_stmts(block.id) {
                self.emit_stmt(stmt, func);
            }
            self.emit_terminator(&block.terminator, func);
        }

        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(&mut self.output);
    }
}
