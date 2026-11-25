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
                | arandu_middle::IntrinsicKind::AtomicFence(..)
                | arandu_middle::IntrinsicKind::AtomicCompilerFence(..)
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
                | arandu_middle::IntrinsicKind::RotateLeft
                | arandu_middle::IntrinsicKind::RotateRight
                | arandu_middle::IntrinsicKind::SwapBytes
                | arandu_middle::IntrinsicKind::FloatToBits(..)
                | arandu_middle::IntrinsicKind::FloatFromBits(..)
                | arandu_middle::IntrinsicKind::Fma
                | arandu_middle::IntrinsicKind::SpinLoopHint
                | arandu_middle::IntrinsicKind::Likely
                | arandu_middle::IntrinsicKind::Unlikely
                | arandu_middle::intrinsics::IntrinsicKind::OverflowArithmetic(..)
                | arandu_middle::intrinsics::IntrinsicKind::SaturatingArithmetic(..)
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
                | arandu_middle::IntrinsicKind::ReverseBits
                | arandu_middle::IntrinsicKind::RotateLeft
                | arandu_middle::IntrinsicKind::RotateRight
                | arandu_middle::IntrinsicKind::SwapBytes,
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
                    let mask = bits - 1;
                    if let Some(
                        rotation @ (arandu_middle::IntrinsicKind::RotateLeft
                        | arandu_middle::IntrinsicKind::RotateRight),
                    ) = kind
                    {
                        // UB-free rotate idiom; GCC and Clang lower it to ROL/ROR.
                        let amount = args
                            .get(1)
                            .map_or_else(|| "0".to_string(), |a| self.format_operand(a, func));
                        let (toward, away) = if rotation == arandu_middle::IntrinsicKind::RotateLeft
                        {
                            ("<<", ">>")
                        } else {
                            (">>", "<<")
                        };
                        let _ = writeln!(
                            self.output,
                            "    {{ uint{bits}_t ar_value = ({value}); uint32_t ar_amount = (uint32_t)({amount}); t{slot} = (uint{bits}_t)((ar_value {toward} (ar_amount & {mask}u)) | (ar_value {away} ((0u - ar_amount) & {mask}u))); }}"
                        );
                    } else if kind == Some(arandu_middle::IntrinsicKind::SwapBytes) {
                        let _ = writeln!(
                            self.output,
                            "    t{slot} = __builtin_bswap{bits}((uint{bits}_t)({value}));"
                        );
                    } else if kind == Some(arandu_middle::IntrinsicKind::ReverseBits) {
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
            Some(arandu_middle::IntrinsicKind::FloatToBits(width)) => {
                if let Some(argument) = args.first()
                    && let Some(destination) = lhs
                {
                    let (in_ty, out_ty) = match width {
                        arandu_middle::intrinsics::FloatWidth::F64 => ("double", "uint64_t"),
                        arandu_middle::intrinsics::FloatWidth::F32 => ("float", "uint32_t"),
                    };
                    let value = self.format_operand(argument, func);
                    let slot = self.temp_slots[destination.as_usize()];
                    let _ = writeln!(
                        self.output,
                        "    {{ {out_ty} ar_bits; {in_ty} ar_val = ({value}); memcpy(&ar_bits, &ar_val, sizeof(ar_bits)); t{slot} = ar_bits; }}"
                    );
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::FloatFromBits(width)) => {
                if let Some(argument) = args.first()
                    && let Some(destination) = lhs
                {
                    let (in_ty, out_ty) = match width {
                        arandu_middle::intrinsics::FloatWidth::F64 => ("uint64_t", "double"),
                        arandu_middle::intrinsics::FloatWidth::F32 => ("uint32_t", "float"),
                    };
                    let value = self.format_operand(argument, func);
                    let slot = self.temp_slots[destination.as_usize()];
                    let _ = writeln!(
                        self.output,
                        "    {{ {out_ty} ar_val; {in_ty} ar_bits = ({value}); memcpy(&ar_val, &ar_bits, sizeof(ar_val)); t{slot} = ar_val; }}"
                    );
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::Fma) => {
                if args.len() == 3
                    && let Some(destination) = lhs
                {
                    let is_64 = if let AmirOperand::FunctionRef(symbol) = callee {
                        let name = &self.symbols.get(*symbol).name;
                        name == "fma" || name.ends_with("64")
                    } else {
                        false
                    };
                    let a = self.format_operand(&args[0], func);
                    let b = self.format_operand(&args[1], func);
                    let c = self.format_operand(&args[2], func);
                    let slot = self.temp_slots[destination.as_usize()];
                    let fn_name = if is_64 {
                        "__builtin_fma"
                    } else {
                        "__builtin_fmaf"
                    };
                    let _ = writeln!(self.output, "    t{slot} = {fn_name}({a}, {b}, {c});");
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::SpinLoopHint) => {
                let _ = writeln!(
                    self.output,
                    "    #if defined(__x86_64__) || defined(_M_X64)\n    __builtin_ia32_pause();\n    #elif defined(__aarch64__)\n    __asm__ __volatile__(\"yield\");\n    #endif"
                );
                true
            }
            Some(
                branch_kind @ (arandu_middle::IntrinsicKind::Likely
                | arandu_middle::IntrinsicKind::Unlikely),
            ) => {
                if let Some(argument) = args.first()
                    && let Some(destination) = lhs
                {
                    let cond = self.format_operand(argument, func);
                    let slot = self.temp_slots[destination.as_usize()];
                    let macro_name = if branch_kind == arandu_middle::IntrinsicKind::Likely {
                        "AR_LIKELY"
                    } else {
                        "AR_UNLIKELY"
                    };
                    let _ = writeln!(self.output, "    t{slot} = {macro_name}({cond});");
                }
                true
            }
            Some(arandu_middle::intrinsics::IntrinsicKind::OverflowArithmetic(op, arith_int)) => {
                use arandu_middle::intrinsics::OverflowOp;
                if args.len() == 2
                    && let Some(destination) = lhs
                {
                    let c_ty = match (arith_int.is_signed(), arith_int.bit_width()) {
                        (true, 32) => "int32_t",
                        (false, 32) => "uint32_t",
                        (true, 64) => "int64_t",
                        _ => "uint64_t",
                    };
                    let builtin = match op {
                        OverflowOp::Add => "__builtin_add_overflow",
                        OverflowOp::Sub => "__builtin_sub_overflow",
                        OverflowOp::Mul => "__builtin_mul_overflow",
                    };
                    let a = self.format_operand(&args[0], func);
                    let b = self.format_operand(&args[1], func);
                    let lhs_ty = self.temp_ty(func, *destination);
                    let lhs_c_ty = self.format_type(&lhs_ty);
                    let layout = self.checked_layout(&lhs_ty);
                    let off0 = layout.field_offsets.first().copied().unwrap_or(0);
                    let off1 = layout.field_offsets.get(1).copied().unwrap_or(0);
                    let slot = self.temp_slots[destination.as_usize()];
                    let _ = writeln!(
                        self.output,
                        "    {{ {c_ty} ar_a = ({c_ty})({a}); {c_ty} ar_b = ({c_ty})({b}); {c_ty} ar_val = 0; bool ar_ov = {builtin}(ar_a, ar_b, &ar_val); {lhs_c_ty} ar_tup = {{0}}; memcpy(ar_tup.memory + {off0}, &ar_val, sizeof(ar_val)); memcpy(ar_tup.memory + {off1}, &ar_ov, sizeof(ar_ov)); t{slot} = ar_tup; }}"
                    );
                }
                true
            }
            Some(arandu_middle::intrinsics::IntrinsicKind::SaturatingArithmetic(op, arith_int)) => {
                use arandu_middle::intrinsics::SaturatingOp;
                if args.len() == 2
                    && let Some(destination) = lhs
                {
                    let bits = arith_int.bit_width();
                    let signed = arith_int.is_signed();
                    let c_ty = match (signed, bits) {
                        (true, 32) => "int32_t",
                        (false, 32) => "uint32_t",
                        (true, 64) => "int64_t",
                        _ => "uint64_t",
                    };
                    let builtin = match op {
                        SaturatingOp::Add => "__builtin_add_overflow",
                        SaturatingOp::Sub => "__builtin_sub_overflow",
                    };
                    let a = self.format_operand(&args[0], func);
                    let b = self.format_operand(&args[1], func);
                    let slot = self.temp_slots[destination.as_usize()];
                    if signed {
                        let min_lit = if bits == 64 { "INT64_MIN" } else { "INT32_MIN" };
                        let max_lit = if bits == 64 { "INT64_MAX" } else { "INT32_MAX" };
                        let _ = writeln!(
                            self.output,
                            "    {{ {c_ty} ar_a = ({c_ty})({a}); {c_ty} ar_b = ({c_ty})({b}); {c_ty} ar_val = 0; t{slot} = {builtin}(ar_a, ar_b, &ar_val) ? (ar_a < 0 ? {min_lit} : {max_lit}) : ar_val; }}"
                        );
                    } else {
                        let sat_expr = match op {
                            SaturatingOp::Add => {
                                if bits == 64 {
                                    "UINT64_MAX"
                                } else {
                                    "UINT32_MAX"
                                }
                            }
                            SaturatingOp::Sub => "0",
                        };
                        let _ = writeln!(
                            self.output,
                            "    {{ {c_ty} ar_a = ({c_ty})({a}); {c_ty} ar_b = ({c_ty})({b}); {c_ty} ar_val = 0; t{slot} = {builtin}(ar_a, ar_b, &ar_val) ? ({c_ty}){sat_expr} : ar_val; }}"
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
                    if matches!(dest_ty, ArType::Void | ArType::Never)
                        || self.checked_layout(&dest_ty).size == 0
                    {
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
                if matches!(val_ty, ArType::Void | ArType::Never)
                    || self.checked_layout(&val_ty).size == 0
                {
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
