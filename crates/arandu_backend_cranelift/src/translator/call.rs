use arandu_middle::amir::{AmirOperand, TempId};
use arandu_middle::types::{ArType, Primitive};
use cranelift_codegen::ir::{InstBuilder, Type, Value};

use super::FunctionTranslator;
use super::operand::FatOperandKind;

enum CallTarget {
    Direct(cranelift_codegen::ir::FuncRef),
    Indirect(Value),
}

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn translate_call(
        &mut self,
        lhs: &Option<TempId>,
        callee: &AmirOperand,
        args: &[AmirOperand],
    ) {
        if let AmirOperand::FunctionRef(sym_id) = callee {
            let sym = self.symbol_table.get(*sym_id);
            let bare = sym.name.rsplit('.').next().unwrap_or(sym.name.as_str());
            // L6.1: mem intrinsics (short mono names + qualified std.core.mem.*).
            if let Some(kind) =
                arandu_middle::IntrinsicKind::from_declared_name(bare, sym.lang_item)
                && self.translate_mem_intrinsic(kind, bare, &sym.name, lhs, args)
            {
                return;
            }
        }

        let mut indirect_result = None;
        // Apple arm64 passes variadic arguments on the stack: register
        // padding inserted after this many fixed arguments.
        let mut apple_variadic_padding: Option<(usize, Vec<Type>)> = None;
        // A direct call takes its signature from the callee's declaration; a
        // call through a function value takes it from the value's `Func`
        // type, built with the same internal ABI (sret, aggregate slots).
        let target = match callee {
            AmirOperand::FunctionRef(sym_id) => {
                if (self.type_info.is_cold_function(*sym_id) || self.symbol_table.is_cold(*sym_id))
                    && let Some(clif_block) = self.builder.current_block()
                    && self.builder.func.layout.entry_block() != Some(clif_block)
                {
                    self.builder.func.layout.set_cold(clif_block);
                }
                let sym = self.symbol_table.get(*sym_id);
                let host_name = self.symbol_table.host_func_name(sym);
                let Some(func_id) = self.resolve_function(host_name) else {
                    self.record_ice(
                        format!("function '{}' was not declared in the JIT module", sym.name),
                        sym.span,
                    );
                    return;
                };
                let local_ref = self.module.declare_func_in_func(func_id, self.builder.func);
                let sig_id = self.builder.func.dfg.ext_funcs[local_ref].signature;
                let function_ty = self
                    .type_info
                    .decl_type_id(*sym_id)
                    .map(|id| self.resolve_ty(id));
                if self.type_info.is_variadic_function(*sym_id)
                    && let Some(ArType::Func(param_range, result)) = &function_ty
                {
                    let interner = self.type_info.type_interner();
                    let fixed_len = (param_range.len as usize).saturating_sub(1);
                    let mut full_params: Vec<ArType> = interner.type_args(*param_range)
                        [..fixed_len]
                        .iter()
                        .map(|&id| interner.resolve(id))
                        .collect();
                    let triple = self.module.isa().triple();
                    let apple_arm64 = self.module.isa().default_call_conv()
                        == cranelift_codegen::isa::CallConv::AppleAarch64;
                    for arg in args.iter().skip(fixed_len) {
                        let promoted = promote_c_vararg_type(self.get_operand_ar_type(arg));
                        // Each Apple arm64 variadic argument fills an 8-byte slot.
                        full_params.push(if apple_arm64 {
                            widen_apple_vararg_type(promoted)
                        } else {
                            promoted
                        });
                    }
                    let ret_ty = interner.resolve(*result);
                    let classifier = arandu_middle::layout::TargetAbiClassifier::new(
                        crate::abi::target_abi_for_triple(triple),
                        u64::from(self.ptr_type.bytes()),
                    );
                    let mut call_sig = crate::abi::build_signature_with_classifier(
                        &full_params,
                        &ret_ty,
                        self.module.isa().default_call_conv(),
                        self.ptr_type,
                        &classifier,
                        interner,
                        self.type_info,
                    );
                    if apple_arm64 && args.len() > fixed_len {
                        // As rustc's Cranelift backend does: exhaust the
                        // argument registers with padding so every variadic
                        // argument goes on the stack, where the callee's
                        // `va_arg` reads it.
                        let fixed_sig = crate::abi::build_signature_with_classifier(
                            &full_params[..fixed_len],
                            &ret_ty,
                            self.module.isa().default_call_conv(),
                            self.ptr_type,
                            &classifier,
                            interner,
                            self.type_info,
                        );
                        let fixed_values = fixed_sig.params.len();
                        let used_float = fixed_sig
                            .params
                            .iter()
                            .filter(|param| param.value_type.is_float())
                            .count();
                        let used_int = fixed_values - used_float;
                        let variadic_float = call_sig.params[fixed_values..]
                            .iter()
                            .any(|param| param.value_type.is_float());
                        let mut padding = vec![
                            cranelift_codegen::ir::types::I64;
                            8usize.saturating_sub(used_int)
                        ];
                        if variadic_float {
                            padding.extend(std::iter::repeat_n(
                                cranelift_codegen::ir::types::F64,
                                8usize.saturating_sub(used_float),
                            ));
                        }
                        for (offset, &ty) in padding.iter().enumerate() {
                            call_sig.params.insert(
                                fixed_values + offset,
                                cranelift_codegen::ir::AbiParam::new(ty),
                            );
                        }
                        apple_variadic_padding = Some((fixed_len, padding));
                    }
                    let call_sig_id = self.builder.import_signature(call_sig);
                    let code = self.builder.ins().func_addr(self.ptr_type, local_ref);
                    let promoted_param_ids: Vec<_> = full_params
                        .into_iter()
                        .map(|ty| interner.intern(ty))
                        .collect();
                    let promoted_func_ty = ArType::func(&promoted_param_ids, *result, interner);
                    (
                        CallTarget::Indirect(code),
                        call_sig_id,
                        Some(promoted_func_ty),
                    )
                } else {
                    (CallTarget::Direct(local_ref), sig_id, function_ty)
                }
            }
            _ => {
                let function_ty = self.get_operand_ar_type(callee);
                let ArType::Func(params, ret) = &function_ty else {
                    self.record_ice(
                        "call through a value that is not a function",
                        self.func_span(),
                    );
                    return;
                };
                let interner = self.type_info.type_interner();
                let params = interner
                    .type_args(*params)
                    .iter()
                    .map(|&id| interner.resolve(id))
                    .collect::<Vec<_>>();
                let ret = interner.resolve(*ret);
                let triple = self.module.isa().triple();
                let classifier = arandu_middle::layout::TargetAbiClassifier::new(
                    crate::abi::target_abi_for_triple(triple),
                    u64::from(self.ptr_type.bytes()),
                );
                let signature = crate::abi::build_internal_signature(
                    &params,
                    &ret,
                    self.module.isa().default_call_conv(),
                    self.ptr_type,
                    &classifier,
                    interner,
                    self.type_info,
                );
                let sig_id = self.builder.import_signature(signature);
                let code = self.translate_operand(callee, Some(self.ptr_type));
                (CallTarget::Indirect(code), sig_id, Some(function_ty))
            }
        };
        let (target, sig_id, function_ty) = target;
        let (callee_param_types, result_ty) = match function_ty {
            Some(ArType::Func(param_range, result)) => {
                let interner = self.type_info.type_interner();
                let params = interner
                    .type_args(param_range)
                    .iter()
                    .map(|&pid| interner.resolve(pid))
                    .collect::<Vec<_>>();
                (Some(params), Some(interner.resolve(result)))
            }
            _ => (None, None),
        };
        let expected_tys: Vec<Type> = self.builder.func.dfg.signatures[sig_id]
            .params
            .iter()
            .map(|param| param.value_type)
            .collect();

        let mut clif_args = Vec::new();
        let mut clif_param_idx = 0;
        if self.builder.func.dfg.signatures[sig_id]
            .params
            .first()
            .is_some_and(|param| {
                param.purpose == cranelift_codegen::ir::ArgumentPurpose::StructReturn
            })
        {
            let Some(result_ty) = result_ty else {
                self.record_ice(
                    "missing type for indirect aggregate result",
                    self.func_span(),
                );
                return;
            };
            let destination = self.allocate_independent_aggregate(&result_ty);
            indirect_result = Some(destination);
            clif_args.push(destination);
            clif_param_idx = 1;
        }
        for (arg_idx, arg) in args.iter().enumerate() {
            if let Some((fixed, padding)) = &apple_variadic_padding
                && arg_idx == *fixed
            {
                for &ty in padding {
                    let zero = if ty.is_float() {
                        self.builder.ins().f64const(0.0)
                    } else {
                        self.builder.ins().iconst(ty, 0)
                    };
                    clif_args.push(zero);
                    clif_param_idx += 1;
                }
            }
            let arg_ty = callee_param_types
                .as_ref()
                .and_then(|tys| tys.get(arg_idx))
                .cloned()
                .unwrap_or_else(|| self.get_operand_ar_type(arg));
            if matches!(arg_ty, ArType::Void) {
                continue;
            }
            match self.fat_operand_kind(&arg_ty) {
                FatOperandKind::Str => {
                    let (ptr_val, len_val) = self.translate_str_operand(arg);
                    clif_args.push(ptr_val);
                    clif_args.push(len_val);
                    clif_param_idx += 2;
                }
                FatOperandKind::Slice => {
                    let (data, len) = self.translate_slice_operand(arg);
                    clif_args.push(data);
                    clif_args.push(len);
                    clif_param_idx += 2;
                }
                FatOperandKind::None => {
                    if matches!(
                        arg_ty,
                        ArType::Named(_, _) | ArType::Tuple(_) | ArType::Array(_, _)
                    ) {
                        let arg_abi = self.classify_arg_abi(&arg_ty);
                        match arg_abi {
                            arandu_middle::layout::ArgAbi::ZeroSized => {}
                            arandu_middle::layout::ArgAbi::Direct(direct) => {
                                let base_ptr = self.translate_operand(arg, Some(self.ptr_type));
                                let layout = self.checked_layout(&arg_ty);
                                for abi_slot in &direct.slots {
                                    let chunk_val =
                                        self.load_abi_slot(base_ptr, abi_slot, layout.size);
                                    clif_args.push(chunk_val);
                                    clif_param_idx += 1;
                                }
                            }
                            arandu_middle::layout::ArgAbi::Indirect => {
                                let val = self.translate_operand(arg, Some(self.ptr_type));
                                clif_args.push(val);
                                clif_param_idx += 1;
                            }
                        }
                    } else {
                        let expected = expected_tys.get(clif_param_idx).copied();
                        let val = self.translate_typed_operand(arg, expected, Some(&arg_ty));
                        clif_args.push(val);
                        clif_param_idx += 1;
                    }
                }
            }
        }

        let call_inst = match target {
            CallTarget::Direct(local_ref) => self.builder.ins().call(local_ref, &clif_args),
            CallTarget::Indirect(code) => {
                self.builder.ins().call_indirect(sig_id, code, &clif_args)
            }
        };
        if let Some(lhs_temp) = lhs {
            if let Some(destination) = indirect_result {
                if let Some(&var) = self.temp_map.get(lhs_temp) {
                    self.builder.def_var(var, destination);
                }
                return;
            }
            let lhs_ty = self.temp_ar_ty(*lhs_temp);
            if matches!(&lhs_ty, ArType::Primitive(Primitive::Str)) {
                let results = self.builder.inst_results(call_inst);
                if results.len() >= 2 {
                    let res0 = results[0];
                    let res1 = results[1];
                    if let Some(&(var_ptr, var_len)) = self.str_temp_map.get(lhs_temp) {
                        self.builder.def_var(var_ptr, res0);
                        self.builder.def_var(var_len, res1);
                    }
                }
            } else if matches!(self.fat_operand_kind(&lhs_ty), FatOperandKind::Slice) {
                let results = self.builder.inst_results(call_inst);
                if results.len() >= 2 {
                    let descriptor = self.materialize_slice_descriptor(results[0], results[1]);
                    if let Some(&var) = self.temp_map.get(lhs_temp) {
                        self.builder.def_var(var, descriptor);
                    }
                }
            } else if matches!(
                &lhs_ty,
                ArType::Named(_, _) | ArType::Tuple(_) | ArType::Array(_, _)
            ) {
                let arg_abi = self.classify_arg_abi(&lhs_ty);
                match arg_abi {
                    arandu_middle::layout::ArgAbi::ZeroSized => {
                        let null_ptr = self.builder.ins().iconst(self.ptr_type, 0);
                        if let Some(&var) = self.temp_map.get(lhs_temp) {
                            self.builder.def_var(var, null_ptr);
                        }
                    }
                    arandu_middle::layout::ArgAbi::Direct(direct) => {
                        let results = self.builder.inst_results(call_inst).to_vec();
                        let layout = self.checked_layout(&lhs_ty);
                        let size = u32::try_from(layout.size.max(1)).unwrap_or(1);
                        let align_shift = layout.align.max(1).trailing_zeros() as u8;
                        let slot = self.builder.create_sized_stack_slot(
                            cranelift_codegen::ir::StackSlotData {
                                kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                                size,
                                align_shift,
                                key: None,
                            },
                        );
                        let addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
                        for (i, abi_slot) in direct.slots.iter().enumerate() {
                            if let Some(&res_val) = results.get(i) {
                                self.store_abi_slot(res_val, addr, abi_slot, layout.size);
                            }
                        }
                        if let Some(&var) = self.temp_map.get(lhs_temp) {
                            self.builder.def_var(var, addr);
                        }
                    }
                    arandu_middle::layout::ArgAbi::Indirect => {
                        if let Some(destination) = indirect_result {
                            if let Some(&var) = self.temp_map.get(lhs_temp) {
                                self.builder.def_var(var, destination);
                            }
                            return;
                        }
                        let results = self.builder.inst_results(call_inst);
                        if let (Some(&var), Some(&res0)) =
                            (self.temp_map.get(lhs_temp), results.first())
                        {
                            let mut res = res0;
                            if let Some(var_ty) = self.get_temp_clif_type(*lhs_temp) {
                                let res_ty = self.builder.func.dfg.value_type(res);
                                if var_ty != res_ty && var_ty.is_int() && res_ty.is_int() {
                                    if res_ty.bits() > var_ty.bits() {
                                        res = self.builder.ins().ireduce(var_ty, res);
                                    } else if res_ty.bits() < var_ty.bits() {
                                        res = self.builder.ins().sextend(var_ty, res);
                                    }
                                }
                            }
                            self.builder.def_var(var, res);
                        }
                    }
                }
            } else if let Some(&var) = self.temp_map.get(lhs_temp) {
                let results = self.builder.inst_results(call_inst);
                if !results.is_empty() {
                    let mut res0 = results[0];
                    if let Some(var_ty) = self.get_temp_clif_type(*lhs_temp) {
                        let res_ty = self.builder.func.dfg.value_type(res0);
                        if var_ty != res_ty && var_ty.is_int() && res_ty.is_int() {
                            if res_ty.bits() > var_ty.bits() {
                                res0 = self.builder.ins().ireduce(var_ty, res0);
                            } else if res_ty.bits() < var_ty.bits() {
                                res0 = self.builder.ins().sextend(var_ty, res0);
                            }
                        }
                    }
                    self.builder.def_var(var, res0);
                }
            }
        }
    }

    /// Lower `ptrOffset` / `ptrRead` / `ptrWrite` / residual `sizeOf`/`alignOf`.
    ///
    /// Returns `true` when the call was handled as an intrinsic.
    fn translate_mem_intrinsic(
        &mut self,
        declared: arandu_middle::IntrinsicKind,
        bare: &str,
        full_name: &str,
        lhs: &Option<TempId>,
        args: &[AmirOperand],
    ) -> bool {
        // The caller already matched the declared ABI marker. A second lookup
        // by spelling would let a colliding extern name pick a different kind.
        let kind = Some(declared);

        if self.translate_atomic_intrinsic(declared, lhs, args) {
            return true;
        }

        match kind {
            Some(
                arandu_middle::IntrinsicKind::CountOnes
                | arandu_middle::IntrinsicKind::LeadingZeros
                | arandu_middle::IntrinsicKind::TrailingZeros
                | arandu_middle::IntrinsicKind::ReverseBits,
            ) => {
                if let Some(argument) = args.first() {
                    let input_ty = if bare.ends_with("64") {
                        cranelift_codegen::ir::types::I64
                    } else {
                        cranelift_codegen::ir::types::I32
                    };
                    let value = self.translate_operand(argument, Some(input_ty));
                    let result = match kind {
                        Some(arandu_middle::IntrinsicKind::CountOnes) => {
                            self.builder.ins().popcnt(value)
                        }
                        Some(arandu_middle::IntrinsicKind::LeadingZeros) => {
                            self.builder.ins().clz(value)
                        }
                        Some(arandu_middle::IntrinsicKind::TrailingZeros) => {
                            self.builder.ins().ctz(value)
                        }
                        _ => self.builder.ins().bitrev(value),
                    };
                    if let Some(destination) = lhs
                        && let Some(&variable) = self.temp_map.get(destination)
                        && let Some(result_ty) = self.get_temp_clif_type(*destination)
                    {
                        let result = self.cast_int_width(result, result_ty);
                        self.builder.def_var(variable, result);
                    }
                }
                true
            }
            Some(
                arandu_middle::IntrinsicKind::RotateLeft
                | arandu_middle::IntrinsicKind::RotateRight
                | arandu_middle::IntrinsicKind::SwapBytes,
            ) => {
                let input_ty = if bare.ends_with("64") {
                    cranelift_codegen::ir::types::I64
                } else {
                    cranelift_codegen::ir::types::I32
                };
                let is_swap = kind == Some(arandu_middle::IntrinsicKind::SwapBytes);
                let expected_args = if is_swap { 1 } else { 2 };
                if args.len() == expected_args {
                    let value = self.translate_operand(&args[0], Some(input_ty));
                    let result = if is_swap {
                        self.builder.ins().bswap(value)
                    } else {
                        // `rotl`/`rotr` reduzem a quantidade módulo a largura,
                        // igual a `llvm.fshl`/`fshr` e ao backend C.
                        let amount = self.translate_operand(&args[1], Some(input_ty));
                        if kind == Some(arandu_middle::IntrinsicKind::RotateLeft) {
                            self.builder.ins().rotl(value, amount)
                        } else {
                            self.builder.ins().rotr(value, amount)
                        }
                    };
                    if let Some(destination) = lhs
                        && let Some(&variable) = self.temp_map.get(destination)
                        && let Some(result_ty) = self.get_temp_clif_type(*destination)
                    {
                        let result = self.cast_int_width(result, result_ty);
                        self.builder.def_var(variable, result);
                    }
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::Fma) => {
                if args.len() == 3 {
                    let ty = if bare == "fma" || bare.ends_with("64") {
                        cranelift_codegen::ir::types::F64
                    } else {
                        cranelift_codegen::ir::types::F32
                    };
                    let a = self.translate_operand(&args[0], Some(ty));
                    let b = self.translate_operand(&args[1], Some(ty));
                    let c = self.translate_operand(&args[2], Some(ty));
                    let result = self.builder.ins().fma(a, b, c);
                    if let Some(destination) = lhs
                        && let Some(&variable) = self.temp_map.get(destination)
                    {
                        self.builder.def_var(variable, result);
                    }
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::SpinLoopHint) => {
                // Cranelift targets have no direct pause instruction; emit a safe no-op.
                true
            }
            Some(arandu_middle::IntrinsicKind::Likely | arandu_middle::IntrinsicKind::Unlikely) => {
                if let [arg] = args {
                    let val = self.translate_operand(arg, Some(cranelift_codegen::ir::types::I8));
                    if let Some(destination) = lhs
                        && let Some(&variable) = self.temp_map.get(destination)
                    {
                        self.builder.def_var(variable, val);
                    }
                }
                true
            }
            Some(arandu_middle::intrinsics::IntrinsicKind::OverflowArithmetic(op, arith_int)) => {
                use arandu_middle::intrinsics::OverflowOp;
                if args.len() == 2 {
                    let int_ty = if arith_int.is_64_bit() {
                        cranelift_codegen::ir::types::I64
                    } else {
                        cranelift_codegen::ir::types::I32
                    };
                    let a = self.translate_operand(&args[0], Some(int_ty));
                    let b = self.translate_operand(&args[1], Some(int_ty));
                    let (res, ov) = match (op, arith_int.is_signed()) {
                        (OverflowOp::Add, true) => self.builder.ins().sadd_overflow(a, b),
                        (OverflowOp::Add, false) => self.builder.ins().uadd_overflow(a, b),
                        (OverflowOp::Sub, true) => self.builder.ins().ssub_overflow(a, b),
                        (OverflowOp::Sub, false) => self.builder.ins().usub_overflow(a, b),
                        (OverflowOp::Mul, true) => self.builder.ins().smul_overflow(a, b),
                        (OverflowOp::Mul, false) => self.builder.ins().umul_overflow(a, b),
                    };
                    if let Some(destination) = lhs
                        && let Some(&variable) = self.temp_map.get(destination)
                    {
                        let tuple_ty =
                            self.resolve_ty(self.current_func.temps[destination.as_usize()].ty);
                        let layout = self.checked_layout(&tuple_ty);
                        let ptr_val = self.allocate_aggregate(&tuple_ty);
                        let ov_i8 = self.cast_int_width(ov, cranelift_codegen::ir::types::I8);
                        let off0 = layout.field_offsets.first().copied().unwrap_or(0) as i32;
                        let off1 = layout.field_offsets.get(1).copied().unwrap_or(0) as i32;
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            res,
                            ptr_val,
                            off0,
                        );
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            ov_i8,
                            ptr_val,
                            off1,
                        );
                        self.builder.def_var(variable, ptr_val);
                    }
                }
                true
            }
            Some(arandu_middle::intrinsics::IntrinsicKind::SaturatingArithmetic(op, arith_int)) => {
                use arandu_middle::intrinsics::SaturatingOp;
                if args.len() == 2 {
                    let is_64 = arith_int.is_64_bit();
                    let int_ty = if is_64 {
                        cranelift_codegen::ir::types::I64
                    } else {
                        cranelift_codegen::ir::types::I32
                    };
                    let signed = arith_int.is_signed();
                    let a = self.translate_operand(&args[0], Some(int_ty));
                    let b = self.translate_operand(&args[1], Some(int_ty));
                    // Cranelift's `*_sat` opcodes accept vector types only; use hardware
                    // overflow flags + branchless `select` (`cmov` / `csel`).
                    let (val, ov) = match (op, signed) {
                        (SaturatingOp::Add, true) => self.builder.ins().sadd_overflow(a, b),
                        (SaturatingOp::Add, false) => self.builder.ins().uadd_overflow(a, b),
                        (SaturatingOp::Sub, true) => self.builder.ins().ssub_overflow(a, b),
                        (SaturatingOp::Sub, false) => self.builder.ins().usub_overflow(a, b),
                    };
                    let sat = if signed {
                        let zero = self.builder.ins().iconst(int_ty, 0);
                        let res_neg = self.builder.ins().icmp(
                            cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                            val,
                            zero,
                        );
                        let min_v = self
                            .builder
                            .ins()
                            .iconst(int_ty, if is_64 { i64::MIN } else { i64::from(i32::MIN) });
                        let max_v = self
                            .builder
                            .ins()
                            .iconst(int_ty, if is_64 { i64::MAX } else { i64::from(i32::MAX) });
                        self.builder.ins().select(res_neg, max_v, min_v)
                    } else {
                        let clamp = match op {
                            SaturatingOp::Add => -1_i64,
                            SaturatingOp::Sub => 0,
                        };
                        self.builder.ins().iconst(int_ty, clamp)
                    };
                    let res = self.builder.ins().select(ov, sat, val);
                    if let Some(destination) = lhs
                        && let Some(&variable) = self.temp_map.get(destination)
                    {
                        self.builder.def_var(variable, res);
                    }
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::PtrRead) => {
                if args.is_empty() {
                    return true;
                }
                let ptr_val = self.translate_operand(&args[0], Some(self.ptr_type));
                let pointee = self.ptr_pointee_ty(&args[0]);
                let lhs_is_str = lhs.is_some_and(|temp| {
                    matches!(self.temp_ar_ty(temp), ArType::Primitive(Primitive::Str))
                });
                if matches!(pointee, ArType::Primitive(Primitive::Str)) || lhs_is_str {
                    if let Some(lhs_temp) = lhs
                        && let Some(&(var_ptr, var_len)) = self.str_temp_map.get(lhs_temp)
                    {
                        let flags = cranelift_codegen::ir::MemFlagsData::new();
                        let loaded_ptr = self.builder.ins().load(self.ptr_type, flags, ptr_val, 0);
                        let loaded_len = self.builder.ins().load(
                            self.ptr_type,
                            flags,
                            ptr_val,
                            self.ptr_type.bytes() as i32,
                        );
                        self.builder.def_var(var_ptr, loaded_ptr);
                        self.builder.def_var(var_len, loaded_len);
                    }
                    return true;
                }
                let clif_ty = lhs
                    .and_then(|temp| self.get_temp_clif_type(temp))
                    .unwrap_or(self.ptr_type);
                // Named aggregates present raw-memory values at `*p`. The JIT
                // represents them as pointers, so read the full payload into a
                // fresh blob rather than loading only the first pointer-sized
                // word (which truncates enums and larger aggregates).
                if self.is_inline_aggregate_ty(&pointee)
                    && let Some(value) = self.materialize_ptr_read_copy(ptr_val, &pointee)
                {
                    if let Some(lhs_temp) = lhs
                        && let Some(&var) = self.temp_map.get(lhs_temp)
                    {
                        self.builder.def_var(var, value);
                    }
                    return true;
                }
                let loaded_val = self.builder.ins().load(
                    clif_ty,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    0,
                );
                if let Some(lhs_temp) = lhs
                    && let Some(&var) = self.temp_map.get(lhs_temp)
                {
                    self.builder.def_var(var, loaded_val);
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::PtrWrite) => {
                if args.len() < 2 {
                    return true;
                }
                let ptr_val = self.translate_operand(&args[0], Some(self.ptr_type));
                let val_ty = self.get_operand_ar_type(&args[1]);
                if matches!(val_ty, ArType::Primitive(Primitive::Str)) {
                    let (elem_ptr, elem_len) = self.translate_str_operand(&args[1]);
                    let flags = cranelift_codegen::ir::MemFlagsData::new();
                    self.builder.ins().store(flags, elem_ptr, ptr_val, 0);
                    self.builder.ins().store(
                        flags,
                        elem_len,
                        ptr_val,
                        self.ptr_type.bytes() as i32,
                    );
                    return true;
                }
                let val_to_store = self.translate_operand(&args[1], None);
                // Match `PtrRead`: writing a named struct by value copies the
                // object bytes from the value's blob into `*p` instead of
                // storing the 8-byte object pointer.
                if self.is_inline_aggregate_ty(&val_ty) {
                    let layout = self.checked_layout(&val_ty);
                    self.copy_aggregate_bytes(ptr_val, val_to_store, layout.size);
                    return true;
                }
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    val_to_store,
                    ptr_val,
                    0,
                );
                true
            }
            Some(arandu_middle::IntrinsicKind::PtrOffset) => {
                if args.len() < 2 {
                    return true;
                }
                let base = self.translate_operand(&args[0], Some(self.ptr_type));
                let idx = self.translate_operand(&args[1], Some(cranelift_codegen::ir::types::I32));
                // Element size from pointer pointee type of the base operand.
                let base_ty = self.get_operand_ar_type(&args[0]);
                let elem_ty = match &base_ty {
                    ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                        self.resolve_ty(*inner)
                    }
                    _ => ArType::Primitive(Primitive::Int),
                };
                let elem_layout = self.checked_layout(&elem_ty);
                let elem_size = elem_layout.size.max(1) as i64;
                let size_val = self.builder.ins().iconst(self.ptr_type, elem_size);
                // Widen idx to pointer width if necessary.
                let idx_ext = if self.ptr_type == cranelift_codegen::ir::types::I64 {
                    self.builder.ins().sextend(self.ptr_type, idx)
                } else {
                    idx
                };
                let byte_off = self.builder.ins().imul(idx_ext, size_val);
                let result = self.builder.ins().iadd(base, byte_off);
                if let Some(lhs_temp) = lhs
                    && let Some(&var) = self.temp_map.get(lhs_temp)
                {
                    self.builder.def_var(var, result);
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::SizeOf | arandu_middle::IntrinsicKind::AlignOf) => {
                let is_size = kind == Some(arandu_middle::IntrinsicKind::SizeOf);
                let ty = ArType::Primitive(Primitive::Int);
                let layout = self.checked_layout(&ty);
                let value = if is_size { layout.size } else { layout.align };
                let clif_ty = lhs
                    .and_then(|t| self.get_temp_clif_type(t))
                    .unwrap_or(self.ptr_type);
                let c = self.builder.ins().iconst(clif_ty, value as i64);
                if let Some(lhs_temp) = lhs
                    && let Some(&var) = self.temp_map.get(lhs_temp)
                {
                    self.builder.def_var(var, c);
                }
                true
            }
            Some(arandu_middle::IntrinsicKind::Abort) => {
                // PAN / Invariant 5: Unix lowers directly to a CPU trap (UD2/BRK)
                // with zero metadata overhead. Windows calls `ar_rt_trap` first:
                // an inline UD2 in a JIT frame is reported as a stack overflow.
                let code = if bare == "abortGenerationalMismatch"
                    || full_name.ends_with("abortGenerationalMismatch")
                {
                    cranelift_codegen::ir::TrapCode::unwrap_user(2)
                } else {
                    cranelift_codegen::ir::TrapCode::unwrap_user(1)
                };
                #[cfg(windows)]
                {
                    let Some(trap_id) = self.resolve_function("ar_rt_trap") else {
                        self.record_ice("ar_rt_trap was not declared", self.func_span());
                        self.builder.ins().trap(code);
                        return true;
                    };
                    let trap_ref = self.module.declare_func_in_func(trap_id, self.builder.func);
                    self.builder.ins().call(trap_ref, &[]);
                }
                if code == cranelift_codegen::ir::TrapCode::unwrap_user(2) {
                    self.print_trap_message(arandu_middle::amir::TrapKind::GenerationalMismatch);
                }
                self.builder.ins().trap(code);
                true
            }
            Some(arandu_middle::IntrinsicKind::DropInPlace) => false,
            _ => false,
        }
    }

    /// The pointee `ArType` of a pointer/ref operand (`ptr[T]` → `T`).
    fn ptr_pointee_ty(&self, operand: &AmirOperand) -> ArType {
        match &self.get_operand_ar_type(operand) {
            ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                self.resolve_ty(*inner)
            }
            other => other.clone(),
        }
    }

    /// Whether `ty` is a named **struct** (not enum/interface) aggregate.
    pub(super) fn is_named_struct_ty(&self, ty: &ArType) -> bool {
        matches!(
            ty,
            ArType::Named(sym_id, _)
                if self.type_info.get_struct_fields(*sym_id).is_some()
                    || matches!(
                        self.symbol_table.get(*sym_id).kind,
                        arandu_middle::SymbolKind::Struct
                    )
        )
    }

    /// Whether `ty` is stored inline as an aggregate (struct, enum, tuple, array, slice).
    pub(super) fn is_inline_aggregate_ty(&self, ty: &ArType) -> bool {
        self.is_named_struct_ty(ty)
            || matches!(
                ty,
                ArType::Named(sym_id, _)
                    if matches!(
                        self.symbol_table.get(*sym_id).kind,
                        arandu_middle::SymbolKind::Enum
                    )
            )
            || matches!(
                ty,
                ArType::Option(_) | ArType::Result(_, _) | ArType::Poll(_)
            )
            || matches!(
                ty,
                ArType::Tuple(_)
                    | ArType::Array(_, _)
                    | ArType::ConstArray(_, _)
                    | ArType::Slice(_)
                    | ArType::MutSlice(_)
            )
    }

    /// Copies `layout.size` payload bytes into independent storage (or the
    /// caller's result destination), fed into the aggregate pointer-repr
    /// value slot used by every other path (`StructLiteral` etc.).
    pub(super) fn materialize_ptr_read_copy(&mut self, src: Value, ty: &ArType) -> Option<Value> {
        let layout = self.checked_layout(ty);
        let dest = self.allocate_aggregate(ty);
        self.copy_aggregate_bytes(dest, src, layout.size);
        Some(dest)
    }
}

/// Apple arm64 gives every variadic argument an 8-byte stack slot.
fn widen_apple_vararg_type(ty: ArType) -> ArType {
    match ty {
        ArType::Primitive(Primitive::I32 | Primitive::Int) => ArType::Primitive(Primitive::I64),
        ArType::Primitive(Primitive::U32 | Primitive::Uint) => ArType::Primitive(Primitive::U64),
        other => other,
    }
}

fn promote_c_vararg_type(ty: ArType) -> ArType {
    match ty {
        ArType::Primitive(Primitive::Bool | Primitive::I8 | Primitive::I16) => {
            ArType::Primitive(Primitive::I32)
        }
        ArType::Primitive(Primitive::U8 | Primitive::U16 | Primitive::Byte) => {
            ArType::Primitive(Primitive::U32)
        }
        ArType::Primitive(Primitive::F32) => ArType::Primitive(Primitive::F64),
        other => other,
    }
}
