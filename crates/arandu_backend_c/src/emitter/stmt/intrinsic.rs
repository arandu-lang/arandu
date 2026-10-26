use super::super::CEmitter;
use arandu_middle::amir::{AmirFunc, AmirOperand, TempId};
use arandu_middle::types::ArType;
use std::fmt::Write;

impl<'a> CEmitter<'a> {
    /// Emit admitted integer parts directly into the materialized result.
    pub(super) fn emit_integer_concat_statement(
        &mut self,
        lhs: TempId,
        rhs: &arandu_middle::amir::AmirRvalue,
        func: &AmirFunc,
    ) -> bool {
        use arandu_codegen::string_interp::IntegerStringKind;
        use arandu_middle::amir::AmirRvalue;
        if let Some(kind) = self.integer_concat_temps[lhs.as_usize()]
            && let AmirRvalue::ToStr { value, .. } = rhs
        {
            let value = self.format_operand(value, func);
            let (ty, helper) = match kind {
                IntegerStringKind::Signed => ("int64_t", "ar_i64_write_digits"),
                IntegerStringKind::Unsigned => ("uint64_t", "ar_u64_write_digits"),
            };
            let index = lhs.as_usize();
            let _ = writeln!(self.output, "    ar_integer_{index} = ({ty})({value});");
            let _ = writeln!(
                self.output,
                "    ar_integer_len_{index} = {helper}(ar_integer_{index}, NULL);"
            );
            return true;
        }
        let AmirRvalue::StringInterp { parts } = rhs else {
            return false;
        };
        let integer_parts: Vec<_> = parts
            .iter()
            .map(|part| match part {
                AmirOperand::Copy(temp) | AmirOperand::Move(temp) => {
                    self.integer_concat_temps[temp.as_usize()].map(|kind| (*temp, kind))
                }
                _ => None,
            })
            .collect();
        if integer_parts.iter().all(Option::is_none) {
            return false;
        }
        let index = lhs.as_usize();
        let max_len = if self.layout.pointer_width() == 4 {
            "INT32_MAX"
        } else {
            "INT64_MAX"
        };
        let _ = writeln!(self.output, "    {{");
        let _ = writeln!(self.output, "        size_t ar_total = 0;");
        for (part_index, (part, integer)) in parts.iter().zip(&integer_parts).enumerate() {
            let length = if let Some((temp, _)) = integer {
                format!("ar_integer_len_{}", temp.as_usize())
            } else {
                let value = self.format_operand(part, func);
                let _ = writeln!(self.output, "        ArStr ar_part_{part_index} = {value};");
                format!("(size_t)ar_part_{part_index}.len")
            };
            let _ = writeln!(
                self.output,
                "        if ({length} > (size_t){max_len} - ar_total) abort();"
            );
            let _ = writeln!(self.output, "        ar_total += {length};");
        }
        let _ = writeln!(
            self.output,
            "        uint8_t *ar_buffer = (uint8_t*)malloc(ar_total + 1);"
        );
        let _ = writeln!(self.output, "        if (!ar_buffer) abort();");
        let _ = writeln!(self.output, "        size_t ar_offset = 0;");
        for (part_index, integer) in integer_parts.iter().enumerate() {
            if let Some((temp, kind)) = integer {
                let helper = match kind {
                    IntegerStringKind::Signed => "ar_i64_write_digits",
                    IntegerStringKind::Unsigned => "ar_u64_write_digits",
                };
                let temp = temp.as_usize();
                let _ = writeln!(
                    self.output,
                    "        {helper}(ar_integer_{temp}, ar_buffer + ar_offset);"
                );
                let _ = writeln!(self.output, "        ar_offset += ar_integer_len_{temp};");
            } else {
                let _ = writeln!(
                    self.output,
                    "        if (ar_part_{part_index}.len > 0) memcpy(ar_buffer + ar_offset, ar_part_{part_index}.ptr, (size_t)ar_part_{part_index}.len);"
                );
                let _ = writeln!(
                    self.output,
                    "        ar_offset += (size_t)ar_part_{part_index}.len;"
                );
            }
        }
        let _ = writeln!(self.output, "        ar_buffer[ar_total] = 0;");
        let _ = writeln!(
            self.output,
            "        t{index} = ar_str_pack(ar_buffer, ar_total);"
        );
        let _ = writeln!(self.output, "    }}");
        true
    }

    /// These external operations are expanded locally and have no C linkage
    /// declaration. Keep declaration filtering and call emission in agreement.
    pub(in crate::emitter) fn inlined_mem_intrinsic(
        &self,
        symbol: arandu_middle::SymbolId,
    ) -> Option<arandu_middle::IntrinsicKind> {
        let symbol = self.symbols.get(symbol);
        if symbol.kind != arandu_middle::SymbolKind::ExternFunc {
            return None;
        }
        let kind =
            arandu_middle::IntrinsicKind::from_declared_name(&symbol.name, symbol.lang_item)?;
        matches!(
            kind,
            arandu_middle::IntrinsicKind::Atomic(..)
                | arandu_middle::IntrinsicKind::AtomicFence
                | arandu_middle::IntrinsicKind::AtomicCompilerFence
                | arandu_middle::IntrinsicKind::Abort
                | arandu_middle::IntrinsicKind::PtrRead
                | arandu_middle::IntrinsicKind::PtrWrite
                | arandu_middle::IntrinsicKind::PtrOffset
                | arandu_middle::IntrinsicKind::SizeOf
                | arandu_middle::IntrinsicKind::AlignOf
                | arandu_middle::IntrinsicKind::CountOnes
                | arandu_middle::IntrinsicKind::LeadingZeros
                | arandu_middle::IntrinsicKind::TrailingZeros
                | arandu_middle::IntrinsicKind::ReverseBits
        )
        .then_some(kind)
    }

    /// Emit `std.core.mem` intrinsics as C loads/stores/pointer arithmetic.
    ///
    /// Parity with Cranelift JIT: `sizeOf` is normally folded in AMIR lower;
    /// residual calls and `ptrOffset` / `ptrRead` / `ptrWrite` must not become
    /// unresolved external symbols in emit-c.
    pub(super) fn try_emit_mem_intrinsic(
        &mut self,
        lhs: &Option<TempId>,
        callee: &AmirOperand,
        args: &[AmirOperand],
        func: &AmirFunc,
    ) -> bool {
        let kind = match callee {
            AmirOperand::FunctionRef(id) | AmirOperand::GlobalRef(id) => {
                self.inlined_mem_intrinsic(*id)
            }
            _ => return false,
        };

        if let Some(kind) = kind
            && self.emit_atomic_intrinsic(kind, lhs, args, func)
        {
            return true;
        }

        match kind {
            Some(
                arandu_middle::IntrinsicKind::CountOnes
                | arandu_middle::IntrinsicKind::LeadingZeros
                | arandu_middle::IntrinsicKind::TrailingZeros
                | arandu_middle::IntrinsicKind::ReverseBits,
            ) => {
                if let Some(argument) = args.first()
                    && let Some(destination) = lhs
                {
                    let bits = if let AmirOperand::FunctionRef(symbol) = callee
                        && self.symbols.get(*symbol).name.ends_with("64")
                    {
                        64
                    } else {
                        32
                    };
                    let value = self.format_operand(argument, func);
                    let slot = self.temp_slots[destination.as_usize()];
                    if kind == Some(arandu_middle::IntrinsicKind::ReverseBits) {
                        let _ = writeln!(
                            self.output,
                            "    {{ uint{bits}_t ar_value = ({value}); uint{bits}_t ar_reversed = 0; for (unsigned ar_bit = 0; ar_bit < {bits}; ++ar_bit) {{ ar_reversed = (ar_reversed << 1) | (ar_value & 1); ar_value >>= 1; }} t{slot} = ar_reversed; }}"
                        );
                    } else {
                        let operation = match kind {
                            Some(arandu_middle::IntrinsicKind::CountOnes) => "popcount",
                            Some(arandu_middle::IntrinsicKind::LeadingZeros) => "clz",
                            _ => "ctz",
                        };
                        let suffix = if bits == 64 { "ll" } else { "" };
                        let _ = writeln!(
                            self.output,
                            "    {{ uint{bits}_t ar_value = ({value}); t{slot} = ar_value == 0 ? {} : __builtin_{operation}{suffix}(ar_value); }}",
                            if operation == "popcount" { 0 } else { bits }
                        );
                    }
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::Abort) => {
                let _ = writeln!(&mut self.output, "    AR_ABORT();");
                true
            }
            Some(arandu_middle::IntrinsicKind::PtrRead) => {
                if args.is_empty() {
                    return true;
                }
                let p = self.format_operand(&args[0], func);
                if let Some(dest) = lhs {
                    let dest_ty = self.temp_ty(func, *dest);
                    if matches!(dest_ty, ArType::Void) || self.checked_layout(&dest_ty).size == 0 {
                        let _ = writeln!(&mut self.output, "    (void)({p});");
                    } else {
                        let _ = writeln!(
                            &mut self.output,
                            "    t{} = *({});",
                            self.temp_slots[dest.as_usize()],
                            p
                        );
                    }
                } else {
                    let _ = writeln!(&mut self.output, "    (void)({p});");
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::PtrWrite) => {
                if args.len() < 2 {
                    return true;
                }
                let p = self.format_operand(&args[0], func);
                let v = self.format_operand(&args[1], func);
                let val_ty = self.operand_ty(func, &args[1]);
                if matches!(val_ty, ArType::Void) || self.checked_layout(&val_ty).size == 0 {
                    let _ = writeln!(&mut self.output, "    (void)({p}); (void)({v});");
                } else {
                    let _ = writeln!(&mut self.output, "    *({}) = {};", p, v);
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::PtrOffset) => {
                if args.len() < 2 {
                    return true;
                }
                let p = self.format_operand(&args[0], func);
                let i = self.format_operand(&args[1], func);
                // C pointer arithmetic scales by pointee size when `p` is a typed pointer.
                if let Some(dest) = lhs {
                    let dest_ty = self.interner.resolve(func.temps[dest.as_usize()].ty);
                    let c_ty = self.format_type(&dest_ty);
                    let _ = writeln!(
                        &mut self.output,
                        "    t{} = (({c_ty})({})) + ({});",
                        self.temp_slots[dest.as_usize()],
                        p,
                        i
                    );
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::SizeOf | arandu_middle::IntrinsicKind::AlignOf) => {
                // The type operand is no longer present in a runtime call.
                // Guessing pointer width here silently miscompiles non-pointer
                // types: the canonical MIR layout fold must have removed it.
                self.record_codegen_ice(
                    func,
                    "layout intrinsic reached C emission without canonical AMIR folding",
                );
                true
            }
            Some(arandu_middle::IntrinsicKind::DropInPlace) => false,
            _ => false,
        }
    }
}
