//! Failed run-time checks: `panic: <message>` on the standard error stream,
//! then a trap, so the exit status stays the platform's trap signal.

use super::FunctionTranslator;
use arandu_middle::amir::TrapKind;
use cranelift_codegen::ir::{InstBuilder, TrapCode, Value};
impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    /// Prints the message of `kind`, then traps. Ends the current block.
    pub(super) fn trap_with_message(&mut self, kind: TrapKind) {
        self.print_trap_message(kind);
        self.builder.ins().trap(TrapCode::unwrap_user(1));
    }

    /// Writes `panic: <message>` on the standard error stream.
    pub(super) fn print_trap_message(&mut self, kind: TrapKind) {
        let report = kind.report();
        let message = self.trap_message_address(kind, report.as_bytes());
        if let (Some(message), Some(eprint)) = (message, self.resolve_function("eprint")) {
            let length = self
                .builder
                .ins()
                .iconst(cranelift_codegen::ir::types::I64, report.len() as i64);
            let eprint = self.module.declare_func_in_func(eprint, self.builder.func);
            self.builder.ins().call(eprint, &[message, length]);
        }
    }

    /// Traps with the message of `kind` when `condition` is non-zero, and
    /// continues in a fresh block otherwise.
    pub(super) fn trap_if(&mut self, condition: Value, kind: TrapKind) {
        let failed = self.builder.create_block();
        let passed = self.builder.create_block();
        self.builder.ins().brif(condition, failed, &[], passed, &[]);
        self.builder.switch_to_block(failed);
        self.builder.set_cold_block(failed);
        self.builder.seal_block(failed);
        self.trap_with_message(kind);
        self.builder.switch_to_block(passed);
        self.builder.seal_block(passed);
    }

    /// Traps with the message of `kind` when `condition` is zero.
    pub(super) fn trap_unless(&mut self, condition: Value, kind: TrapKind) {
        let failed = self.builder.ins().icmp_imm_u(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            condition,
            0,
        );
        self.trap_if(failed, kind);
    }

    /// Checks the divisor of an integer `/` or `%`: zero always traps, and a
    /// signed `MIN / -1` overflows.
    pub(super) fn check_divisor(&mut self, lhs: Value, rhs: Value, signed: bool, division: bool) {
        let zero =
            self.builder
                .ins()
                .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, rhs, 0);
        self.trap_if(zero, TrapKind::DivisionByZero);
        if signed && division {
            let ty = self.builder.func.dfg.value_type(lhs);
            let min = i64::MIN >> (64 - ty.bits());
            let is_min = self.builder.ins().icmp_imm_s(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                lhs,
                min,
            );
            let is_minus_one = self.builder.ins().icmp_imm_s(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                rhs,
                -1,
            );
            let overflow = self.builder.ins().band(is_min, is_minus_one);
            self.trap_if(overflow, TrapKind::DivisionOverflow);
        }
    }

    /// The module-level bytes of a trap message, declared once per kind.
    fn trap_message_address(&mut self, kind: TrapKind, bytes: &[u8]) -> Option<Value> {
        let data_id = match self.module.declare_data(
            &format!("ar_trap_{kind:?}"),
            cranelift_module::Linkage::Local,
            false,
            false,
        ) {
            Ok(id) => id,
            Err(error) => {
                self.record_ice(
                    format!("failed to declare a trap message: {error:?}"),
                    self.func_span(),
                );
                return None;
            }
        };
        let mut data = cranelift_module::DataDescription::new();
        data.define(bytes.to_vec().into_boxed_slice());
        match self.module.define_data(data_id, &data) {
            Ok(()) | Err(cranelift_module::ModuleError::DuplicateDefinition(_)) => {}
            Err(error) => {
                self.record_ice(
                    format!("failed to define a trap message: {error:?}"),
                    self.func_span(),
                );
                return None;
            }
        }
        let data = self.module.declare_data_in_func(data_id, self.builder.func);
        Some(self.builder.ins().symbol_value(self.ptr_type, data))
    }
}
