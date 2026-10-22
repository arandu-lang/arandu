use arandu_middle::Diagnostic;
use arandu_middle::amir::{AmirTerminator, TempId};
use arandu_middle::types::{ArType, Primitive};

use super::{FuncEmitter, block_label, ice};

mod coerce;
mod operand;
mod place;

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(super) fn emit_terminator(
        &mut self,
        terminator: &AmirTerminator,
    ) -> Result<(), Diagnostic> {
        match terminator {
            AmirTerminator::Return => {
                let return_ty = self.module.interner.resolve(self.func.return_type);
                let function_name = self.module.function_name(self.func.symbol);
                if self.module.is_void(&return_ty) {
                    if function_name == "main" {
                        self.writer.line("ret i32 0");
                    } else {
                        self.writer.line("ret void");
                    }
                    return Ok(());
                }
                let Some(value) = ({
                    if let Some(slot) = self.temp_slot(TempId::from_usize(0)).map(str::to_owned) {
                        let ty_text = self.module.llvm_ty(&return_ty)?;
                        let align = self.module.align_of(&return_ty);
                        let loaded = self.new_value();
                        self.writer.line(format!(
                            "{loaded} = load {ty_text}, ptr {slot}, align {align}"
                        ));
                        Some(loaded)
                    } else {
                        self.return_value()
                    }
                }) else {
                    return Err(ice(
                        "AMIR return terminator has no preceding assignment to the return register",
                    ));
                };
                let ty_text = self.module.llvm_ty(&return_ty)?;
                self.writer.line(format!("ret {ty_text} {value}"));
                Ok(())
            }
            AmirTerminator::Goto { target, args } => {
                self.record_edge_args(*target, args)?;
                self.writer
                    .line(format!("br label %{}", block_label(*target)));
                Ok(())
            }
            AmirTerminator::Branch {
                condition,
                if_true,
                true_args,
                if_false,
                false_args,
                ..
            } => {
                let condition = self.operand(condition, &ArType::Primitive(Primitive::Bool))?;
                self.record_edge_args(*if_true, true_args)?;
                self.record_edge_args(*if_false, false_args)?;
                self.writer.line(format!(
                    "br i1 {}, label %{}, label %{}",
                    condition.value,
                    block_label(*if_true),
                    block_label(*if_false)
                ));
                Ok(())
            }
            AmirTerminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => {
                let ty = self.module.operand_type(discriminant, self.func);
                let value = self.operand(discriminant, &ty)?;
                let mut cases = Vec::with_capacity(targets.len());
                for (case, target, args) in targets {
                    self.record_edge_args(*target, args)?;
                    cases.push(format!(
                        "{} {}, label %{}",
                        value.ty,
                        case,
                        block_label(*target)
                    ));
                }
                self.record_edge_args(otherwise.0, &otherwise.1)?;
                self.writer.line(format!(
                    "switch {} {}, label %{} [ {} ]",
                    value.ty,
                    value.value,
                    block_label(otherwise.0),
                    cases.join(" ")
                ));
                Ok(())
            }
            AmirTerminator::Unreachable => {
                self.module.require_llvm_trap();
                self.writer.line("call void @llvm.trap()");
                self.writer.line("unreachable");
                Ok(())
            }
            AmirTerminator::Suspend { resume, args, .. } => {
                // Ready-only coroutines: suspension is a direct edge to the
                // resume block, matching the C backend's A3.1 lowering.
                self.record_edge_args(*resume, args)?;
                self.writer
                    .line(format!("br label %{}", block_label(*resume)));
                Ok(())
            }
        }
    }
}
