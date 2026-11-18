use arandu_middle::Diagnostic;
use arandu_middle::amir::{AmirOperand, TempId};
use arandu_middle::intrinsics::{AtomicOp, AtomicOrdering, AtomicWidth, IntrinsicKind};
use arandu_middle::layout::{ReturnAbi, is_export_aggregate};
use arandu_middle::types::{ArType, Primitive};

use super::export::lanes_ty;
use super::{FuncEmitter, TypedValue, ice, quote_name};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    /// A call through a function value: a code pointer to an Arandu function,
    /// called with the internal convention its `Func` type describes.
    fn emit_indirect_call(
        &mut self,
        lhs: Option<TempId>,
        result: Option<&str>,
        callee: &AmirOperand,
        args: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        let function_ty = self.module.operand_type(callee, self.func);
        let ArType::Func(params, ret) = function_ty else {
            return Err(ice("call through a value that is not a function"));
        };
        let interner = self.module.interner;
        let param_types = interner
            .type_args(params)
            .iter()
            .map(|&id| interner.resolve(id))
            .collect::<Vec<_>>();
        let return_ty = interner.resolve(ret);
        if param_types.len() != args.len() {
            return Err(ice(format!(
                "call through a function value expects {} arguments, got {}",
                param_types.len(),
                args.len()
            )));
        }
        let code = self.operand(callee, &function_ty)?;
        let mut rendered = Vec::with_capacity(args.len());
        for (argument, expected) in args.iter().zip(&param_types) {
            if self.module.is_void(expected) {
                continue;
            }
            rendered.push(self.operand(argument, expected)?.render());
        }
        let return_text = self.module.llvm_ty(&return_ty)?;
        let call = format!("call {return_text} {}({})", code.value, rendered.join(", "));
        match lhs {
            Some(lhs) if !self.module.is_void(&return_ty) => {
                let Some(result) = result else {
                    return Err(ice("call destination has no LLVM name"));
                };
                let expected = self.temp_type(lhs);
                if self.module.llvm_ty(&expected)? == return_text {
                    self.writer.line(format!("{result} = {call}"));
                    return Ok(());
                }
                let raw = self.new_value();
                self.writer.line(format!("{raw} = {call}"));
                self.assign_coerced(result, TypedValue::new(return_text, raw), &expected)
            }
            Some(_) => Err(ice(
                "a void call through a function value has a destination",
            )),
            None => {
                self.writer.line(call);
                Ok(())
            }
        }
    }

    pub(super) fn emit_call(
        &mut self,
        lhs: Option<TempId>,
        result: Option<&str>,
        callee: &AmirOperand,
        args: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        let symbol = match callee {
            AmirOperand::FunctionRef(symbol) | AmirOperand::GlobalRef(symbol) => *symbol,
            other => return self.emit_indirect_call(lhs, result, other, args),
        };
        if let Some(kind) = self.module.intrinsic_kind(symbol) {
            return self.emit_intrinsic(lhs, result, kind, args);
        }
        let is_variadic = self.module.info.is_variadic_function(symbol);
        let (param_types, return_ty) = self.module.signature_for(symbol, args, self.func);
        let fixed_params: &[ArType] = if is_variadic && !param_types.is_empty() {
            &param_types[..param_types.len() - 1]
        } else {
            &param_types
        };
        if (!is_variadic && fixed_params.len() != args.len())
            || (is_variadic && args.len() < fixed_params.len())
        {
            return Err(ice(format!(
                "call to {} expects {}{} arguments, got {}",
                self.module.function_name(symbol),
                if is_variadic { "at least " } else { "" },
                fixed_params.len(),
                args.len()
            )));
        }
        let function_name = self.module.function_name(symbol);
        if !self.module.is_internal(symbol) {
            self.module.require_extern_signature(
                &function_name,
                fixed_params,
                &return_ty,
                is_variadic,
            )?;
        }
        // Extern/host calls use the flattened C ABI; Arandu-defined callees keep
        // first-class fat pointers as `{ ptr, i64 }` values.
        let flatten_host_abi = !self.module.is_internal(symbol);
        let exported = self.module.symbols.is_exported(symbol);
        let span = self.module.symbols.get(symbol).span;
        let allocas_before_call = self.entry_allocas.len();
        let mut rendered = Vec::with_capacity(args.len());
        let mut sret_return = None;
        let mut direct_return = None;
        let mut return_text = self.module.llvm_ty(&return_ty)?;
        if exported && is_export_aggregate(&return_ty) {
            let info = self.module.classify_export_type(&return_ty, span)?;
            let (aggregate, align) = self.module.alloca_type(&return_ty)?;
            match &info.ret {
                ReturnAbi::Void => return_text = "void".to_owned(),
                ReturnAbi::Sret => {
                    let buffer = self.new_value();
                    self.emit_entry_alloca(&buffer, &aggregate, align);
                    rendered.push(format!("ptr sret({aggregate}) align {align} {buffer}"));
                    sret_return = Some((buffer, aggregate, align));
                    return_text = "void".to_owned();
                }
                ReturnAbi::Direct(lanes) => {
                    return_text = lanes_ty(lanes);
                    direct_return = Some((lanes.clone(), aggregate, align));
                }
            }
        }
        for (argument, expected) in args.iter().zip(fixed_params) {
            if self.module.is_void(expected) {
                continue;
            }
            if exported && is_export_aggregate(expected) {
                let info = self.module.classify_export_type(expected, span)?;
                let (aggregate, _) = self.module.alloca_type(expected)?;
                let value = self.operand(argument, expected)?;
                rendered.extend(self.export_argument(&value, &info, &aggregate));
                continue;
            }
            if flatten_host_abi && self.module.is_fat_pointer(expected) {
                let fat = self.operand(argument, expected)?;
                let ptr = self.new_value();
                let len = self.new_value();
                self.writer
                    .line(format!("{ptr} = extractvalue {} {}, 0", fat.ty, fat.value));
                self.writer
                    .line(format!("{len} = extractvalue {} {}, 1", fat.ty, fat.value));
                rendered.push(format!("ptr {ptr}"));
                rendered.push(format!("i64 {len}"));
                continue;
            }
            let value = self.operand(argument, expected)?;
            if exported {
                // The call site repeats the definition's extension attributes;
                // a mismatch is undefined behavior in LLVM.
                let extension = super::export::scalar_extension(expected);
                rendered.push(format!("{} {extension}{}", value.ty, value.value));
            } else {
                rendered.push(value.render());
            }
        }
        for argument in args.iter().skip(fixed_params.len()) {
            let arg_ty = self.module.operand_type(argument, self.func);
            let promoted = promote_c_vararg_type(&arg_ty);
            if self.module.is_void(&promoted) {
                continue;
            }
            if self.module.is_fat_pointer(&promoted) {
                let fat = self.operand(argument, &promoted)?;
                let ptr = self.new_value();
                let len = self.new_value();
                self.writer
                    .line(format!("{ptr} = extractvalue {} {}, 0", fat.ty, fat.value));
                self.writer
                    .line(format!("{len} = extractvalue {} {}, 1", fat.ty, fat.value));
                rendered.push(format!("ptr {ptr}"));
                rendered.push(format!("{} {len}", self.module.ptr_int_ty()));
                continue;
            }
            let value = self.operand(argument, &promoted)?;
            rendered.push(value.render());
        }
        let name = quote_name(&function_name);
        let calling_convention = if flatten_host_abi {
            self.module.runtime_calling_convention(&function_name)
        } else {
            ""
        };
        let return_extension = if exported {
            super::export::scalar_extension(&return_ty)
        } else {
            ""
        };
        let callee_sig = if is_variadic {
            let mut fixed_tys = Vec::new();
            for param in fixed_params {
                fixed_tys.extend(self.module.host_abi_param_llvm_tys(param)?);
            }
            fixed_tys.push("...".to_owned());
            format!(" ({})", fixed_tys.join(", "))
        } else {
            String::new()
        };
        let mut tail_prefix = if self.emit_as_tail_call
            && sret_return.is_none()
            && direct_return.is_none()
            && self.entry_allocas.len() == allocas_before_call
            && !exported
        {
            "tail "
        } else {
            ""
        };
        if let Some(lhs) = lhs
            && !self.module.is_void(&return_ty)
            && self.module.llvm_ty(&self.temp_type(lhs))? != return_text
        {
            tail_prefix = "";
        }
        let call = format!(
            "{tail_prefix}call {calling_convention}{return_extension}{return_text}{callee_sig} @{name}({})",
            rendered.join(", ")
        );
        if let Some((buffer, aggregate, align)) = sret_return {
            self.writer.line(call);
            let Some(lhs) = lhs else {
                return Ok(());
            };
            let Some(result) = result else {
                return Err(ice("call destination has no LLVM name"));
            };
            let expected = self.temp_type(lhs);
            let loaded = self.load_aggregate(&buffer, &aggregate, align);
            return self.assign_coerced(result, TypedValue::new(aggregate, loaded), &expected);
        }
        if let Some((lanes, aggregate, align)) = direct_return {
            let Some(lhs) = lhs else {
                self.writer.line(call);
                return Ok(());
            };
            let Some(result) = result else {
                return Err(ice("call destination has no LLVM name"));
            };
            let expected = self.temp_type(lhs);
            let returned = self.new_value();
            self.writer.line(format!("{returned} = {call}"));
            let buffer = self.new_value();
            self.emit_entry_alloca(&buffer, &aggregate, align);
            if let [lane] = lanes.as_slice() {
                let value = TypedValue::new(return_text.clone(), returned);
                self.store_lane(&buffer, lane, &value);
            } else {
                for (index, lane) in lanes.iter().enumerate() {
                    let value = self.new_value();
                    self.writer.line(format!(
                        "{value} = extractvalue {return_text} {returned}, {index}"
                    ));
                    let lane_value = TypedValue::new(super::export::part_ty(lane.part), value);
                    self.store_lane(&buffer, lane, &lane_value);
                }
            }
            let loaded = self.load_aggregate(&buffer, &aggregate, align);
            return self.assign_coerced(result, TypedValue::new(aggregate, loaded), &expected);
        }
        match lhs {
            Some(lhs) if !self.module.is_void(&return_ty) => {
                let Some(result) = result else {
                    return Err(ice("call destination has no LLVM name"));
                };
                let expected = self.temp_type(lhs);
                let expected_text = self.module.llvm_ty(&expected)?;
                if expected_text == return_text {
                    self.writer.line(format!("{result} = {call}"));
                    return Ok(());
                }
                let raw = self.new_value();
                self.writer.line(format!("{raw} = {call}"));
                self.assign_coerced(result, TypedValue::new(return_text, raw), &expected)
            }
            Some(_) => Err(ice(format!(
                "call to {} has a void return but an assigned destination",
                self.module.function_name(symbol)
            ))),
            None => {
                self.writer.line(call);
                Ok(())
            }
        }
    }

    pub(super) fn emit_intrinsic(
        &mut self,
        lhs: Option<TempId>,
        result: Option<&str>,
        kind: IntrinsicKind,
        args: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        match kind {
            IntrinsicKind::Atomic(op, width, ordering) => {
                self.emit_atomic(result, op, width, ordering, args)
            }
            IntrinsicKind::AtomicFence(ordering) => {
                let order = match ordering {
                    AtomicOrdering::Relaxed | AtomicOrdering::Acquire => "acquire",
                    AtomicOrdering::Release => "release",
                    AtomicOrdering::AcqRel => "acq_rel",
                    AtomicOrdering::SeqCst => "seq_cst",
                };
                self.writer.line(format!("fence {order}"));
                Ok(())
            }
            IntrinsicKind::AtomicCompilerFence(_) => {
                self.writer.line("fence seq_cst");
                Ok(())
            }
            IntrinsicKind::Abort => {
                self.module.require_llvm_trap();
                self.writer.line("call void @llvm.trap()");
                Ok(())
            }
            IntrinsicKind::CountOnes
            | IntrinsicKind::LeadingZeros
            | IntrinsicKind::TrailingZeros
            | IntrinsicKind::ReverseBits
            | IntrinsicKind::RotateLeft
            | IntrinsicKind::RotateRight
            | IntrinsicKind::SwapBytes => self.emit_bit_intrinsic(result, kind, args),
            IntrinsicKind::Fma => {
                let Some(argument) = args.first() else {
                    return Err(ice("fma requires three arguments"));
                };
                let ty = self.module.operand_type(argument, self.func);
                let (ty_text, intrinsic_name) = match ty {
                    ArType::Primitive(Primitive::F64 | Primitive::Float) => {
                        ("double", "llvm.fma.f64")
                    }
                    _ => ("float", "llvm.fma.f32"),
                };
                self.module.require_declaration(
                    intrinsic_name,
                    format!("declare {ty_text} @{intrinsic_name}({ty_text}, {ty_text}, {ty_text})"),
                );
                let a = self.operand(&args[0], &ty)?;
                let b = self.operand(&args[1], &ty)?;
                let c = self.operand(&args[2], &ty)?;
                let call = format!(
                    "call {ty_text} @{intrinsic_name}({ty_text} {}, {ty_text} {}, {ty_text} {})",
                    a.value, b.value, c.value
                );
                if let Some(result) = result {
                    self.writer.line(format!("{result} = {call}"));
                } else {
                    self.writer.line(call);
                }
                Ok(())
            }
            IntrinsicKind::SpinLoopHint => {
                self.writer
                    .line("tail call void asm sideeffect \"/* pause */\", \"~{memory}\"()");
                Ok(())
            }
            IntrinsicKind::Likely | IntrinsicKind::Unlikely => {
                let Some(cond) = args.first() else {
                    return Err(ice("branch hint requires one boolean argument"));
                };
                self.module
                    .require_declaration("llvm.expect.i1", "declare i1 @llvm.expect.i1(i1, i1)");
                let bool_ty = ArType::Primitive(Primitive::Bool);
                let cond = self.operand(cond, &bool_ty)?;
                let expected = if kind == IntrinsicKind::Likely {
                    "true"
                } else {
                    "false"
                };
                let call = format!("call i1 @llvm.expect.i1(i1 {}, i1 {expected})", cond.value);
                if let Some(result) = result {
                    self.writer.line(format!("{result} = {call}"));
                } else {
                    self.writer.line(call);
                }
                Ok(())
            }
            IntrinsicKind::OverflowArithmetic(op, arith_int) => {
                use arandu_middle::intrinsics::OverflowOp;
                let width = arith_int.bit_width();
                let sign = if arith_int.is_signed() { "s" } else { "u" };
                let op_name = match op {
                    OverflowOp::Add => "add",
                    OverflowOp::Sub => "sub",
                    OverflowOp::Mul => "mul",
                };
                let intrinsic_name = format!("llvm.{sign}{op_name}.with.overflow.i{width}");
                let pair_ty = format!("{{ i{width}, i1 }}");
                self.module.require_declaration(
                    &intrinsic_name,
                    format!("declare {pair_ty} @{intrinsic_name}(i{width}, i{width})"),
                );
                let input_ty = ArType::Primitive(arith_int.primitive());
                let a = self.operand(&args[0], &input_ty)?;
                let b = self.operand(&args[1], &input_ty)?;
                let pair = self.new_value();
                self.writer.line(format!(
                    "{pair} = call {pair_ty} @{intrinsic_name}(i{width} {}, i{width} {})",
                    a.value, b.value
                ));
                if let (Some(result), Some(lhs_temp)) = (result, lhs) {
                    let tuple_ty = self.temp_type(lhs_temp);
                    let agg_ty = self.module.llvm_ty(&tuple_ty)?;
                    let align = self.module.align_of(&tuple_ty);
                    let layout = self
                        .module
                        .layout
                        .layout_of_type(&tuple_ty, self.module.interner, self.module.info)
                        .map_err(|error| {
                            ice(format!("LLVM layout failed for {tuple_ty:?}: {error}"))
                        })?;
                    let val = self.new_value();
                    self.writer
                        .line(format!("{val} = extractvalue {pair_ty} {pair}, 0"));
                    let ov = self.new_value();
                    self.writer
                        .line(format!("{ov} = extractvalue {pair_ty} {pair}, 1"));
                    let slot = self.scratch_alloca(&tuple_ty)?;
                    self.writer.line(format!(
                        "store {agg_ty} zeroinitializer, ptr {slot}, align {align}"
                    ));
                    let addr0 =
                        self.byte_gep(&slot, layout.field_offsets.first().copied().unwrap_or(0));
                    self.store_value(
                        &addr0,
                        &super::TypedValue::new(format!("i{width}"), val),
                        &input_ty,
                    )?;
                    let addr1 =
                        self.byte_gep(&slot, layout.field_offsets.get(1).copied().unwrap_or(0));
                    self.store_value(
                        &addr1,
                        &super::TypedValue::new("i1".to_owned(), ov),
                        &ArType::Primitive(Primitive::Bool),
                    )?;
                    self.writer.line(format!(
                        "{result} = load {agg_ty}, ptr {slot}, align {align}"
                    ));
                }
                Ok(())
            }
            IntrinsicKind::SaturatingArithmetic(op, arith_int) => {
                use arandu_middle::intrinsics::SaturatingOp;
                let width = arith_int.bit_width();
                let sign = if arith_int.is_signed() { "s" } else { "u" };
                let op_name = match op {
                    SaturatingOp::Add => "add",
                    SaturatingOp::Sub => "sub",
                };
                let intrinsic_name = format!("llvm.{sign}{op_name}.sat.i{width}");
                self.module.require_declaration(
                    &intrinsic_name,
                    format!("declare i{width} @{intrinsic_name}(i{width}, i{width})"),
                );
                let input_ty = ArType::Primitive(arith_int.primitive());
                let a = self.operand(&args[0], &input_ty)?;
                let b = self.operand(&args[1], &input_ty)?;
                let call = format!(
                    "call i{width} @{intrinsic_name}(i{width} {}, i{width} {})",
                    a.value, b.value
                );
                if let Some(result) = result {
                    self.writer.line(format!("{result} = {call}"));
                } else {
                    self.writer.line(call);
                }
                Ok(())
            }
            IntrinsicKind::PtrRead => {
                let Some(lhs) = lhs else {
                    return Err(ice("ptrRead requires a destination"));
                };
                let ty = self.temp_type(lhs);
                let Some(result) = result else {
                    return Err(ice("ptrRead destination has no LLVM name"));
                };
                let pointer_ty = self.module.void_ptr_ty();
                let pointer = self.operand(&args[0], &pointer_ty)?;
                let text = self.module.llvm_ty(&ty)?;
                let align = self.module.align_of(&ty);
                self.writer.line(format!(
                    "{result} = load {text}, ptr {}, align {align}",
                    pointer.value
                ));
                Ok(())
            }
            IntrinsicKind::PtrWrite => {
                let ty = self.module.operand_type(&args[1], self.func);
                let pointer_ty = self.module.void_ptr_ty();
                let pointer = self.operand(&args[0], &pointer_ty)?;
                let value = self.operand(&args[1], &ty)?;
                let align = self.module.align_of(&ty);
                self.writer.line(format!(
                    "store {}, ptr {}, align {align}",
                    value.render(),
                    pointer.value
                ));
                Ok(())
            }
            IntrinsicKind::PtrOffset => {
                let Some(lhs) = lhs else {
                    return Err(ice("ptrOffset requires a destination"));
                };
                let target = self.temp_type(lhs);
                let Some(result) = result else {
                    return Err(ice("ptrOffset destination has no LLVM name"));
                };
                let pointee = match &target {
                    ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                        Some(self.module.interner.resolve(*inner))
                    }
                    _ => None,
                };
                let stride = pointee.as_ref().map_or(1, |ty| {
                    self.module
                        .layout
                        .layout_of_type(ty, self.module.interner, self.module.info)
                        .map_or(1, |layout| layout.size.max(1))
                });
                let pointer_ty = self.module.void_ptr_ty();
                let pointer = self.operand(&args[0], &pointer_ty)?;
                let index_ty = self.module.operand_type(&args[1], self.func);
                let index = self.operand(&args[1], &index_ty)?;
                let i64_ty = self.module.i64_ty();
                let index = self.coerce(index, &i64_ty)?;
                let offset = self.new_value();
                self.writer
                    .line(format!("{offset} = mul i64 {}, {stride}", index.value));
                self.writer.line(format!(
                    "{result} = getelementptr i8, ptr {}, i64 {offset}",
                    pointer.value
                ));
                Ok(())
            }
            IntrinsicKind::AddressOf => {
                if lhs.is_none() {
                    return Ok(());
                }
                let Some(result) = result else {
                    return Err(ice("addressOf destination has no LLVM name"));
                };
                let ty = self.module.operand_type(&args[0], self.func);
                let value = self.operand(&args[0], &ty)?;
                self.assign_value(result, value)
            }
            other => Err(ice(format!(
                "LLVM backend does not yet lower intrinsic {other:?}"
            ))),
        }
    }

    pub(super) fn emit_atomic(
        &mut self,
        result: Option<&str>,
        op: AtomicOp,
        width: AtomicWidth,
        ordering: AtomicOrdering,
        args: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        if args.len() != op.argument_count() {
            return Err(ice(format!(
                "atomic {op:?} expects {} arguments, got {}",
                op.argument_count(),
                args.len()
            )));
        }
        let ty_text = match width {
            AtomicWidth::U8 => "i8".to_owned(),
            AtomicWidth::U32 => "i32".to_owned(),
            AtomicWidth::U64 => "i64".to_owned(),
            AtomicWidth::Usize => self.module.llvm_ty(&self.module.usize_ty())?,
            AtomicWidth::Pointer => "ptr".to_owned(),
        };
        let value_ty = match width {
            AtomicWidth::U8 => ArType::Primitive(Primitive::U8),
            AtomicWidth::U32 => ArType::Primitive(Primitive::U32),
            AtomicWidth::U64 => ArType::Primitive(Primitive::U64),
            AtomicWidth::Usize => self.module.usize_ty(),
            AtomicWidth::Pointer => ArType::Ptr(self.module.interner.intern(ArType::Void)),
        };
        let align = match width {
            AtomicWidth::U8 => 1,
            AtomicWidth::U32 => 4,
            AtomicWidth::U64 => 8,
            AtomicWidth::Usize => self.module.pointer_align(),
            AtomicWidth::Pointer => self.module.pointer_align(),
        };
        let pointer_ty = self.module.void_ptr_ty();
        let pointer = self.operand(&args[0], &pointer_ty)?;
        let pointer_value = pointer.value;
        match op {
            AtomicOp::Load => {
                let order = match ordering {
                    AtomicOrdering::Relaxed => "monotonic",
                    AtomicOrdering::Acquire | AtomicOrdering::AcqRel => "acquire",
                    _ => "seq_cst",
                };
                if let Some(result) = result {
                    self.writer.line(format!(
                        "{result} = load atomic {ty_text}, ptr {pointer_value} {order}, align {align}"
                    ));
                } else {
                    self.writer.line(format!(
                        "load atomic {ty_text}, ptr {pointer_value} {order}, align {align}"
                    ));
                }
            }
            AtomicOp::Store => {
                let order = match ordering {
                    AtomicOrdering::Relaxed => "monotonic",
                    AtomicOrdering::Release | AtomicOrdering::AcqRel => "release",
                    _ => "seq_cst",
                };
                let value = self.operand(&args[1], &value_ty)?;
                self.writer.line(format!(
                    "store atomic {ty_text} {}, ptr {pointer_value} {order}, align {align}",
                    value.value
                ));
            }
            AtomicOp::CompareExchange => {
                let (succ_order, fail_order) = match ordering {
                    AtomicOrdering::Relaxed => ("monotonic", "monotonic"),
                    AtomicOrdering::Acquire => ("acquire", "acquire"),
                    AtomicOrdering::Release => ("release", "monotonic"),
                    AtomicOrdering::AcqRel => ("acq_rel", "acquire"),
                    AtomicOrdering::SeqCst => ("seq_cst", "seq_cst"),
                };
                let expected = self.operand(&args[1], &value_ty)?;
                let desired = self.operand(&args[2], &value_ty)?;
                let pair = self.new_value();
                self.writer.line(format!(
                    "{pair} = cmpxchg ptr {pointer_value}, {} {}, {} {} {succ_order} {fail_order}",
                    expected.ty, expected.value, desired.ty, desired.value
                ));
                if let Some(result) = result {
                    self.writer.line(format!(
                        "{result} = extractvalue {{ {ty_text}, i1 }} {pair}, 0"
                    ));
                }
            }
            AtomicOp::Swap
            | AtomicOp::Add
            | AtomicOp::Sub
            | AtomicOp::And
            | AtomicOp::Or
            | AtomicOp::Xor => {
                let order = match ordering {
                    AtomicOrdering::Relaxed => "monotonic",
                    AtomicOrdering::Acquire => "acquire",
                    AtomicOrdering::Release => "release",
                    AtomicOrdering::AcqRel => "acq_rel",
                    AtomicOrdering::SeqCst => "seq_cst",
                };
                let value = self.operand(&args[1], &value_ty)?;
                let instruction = match op {
                    AtomicOp::Swap => "xchg",
                    AtomicOp::Add => "add",
                    AtomicOp::Sub => "sub",
                    AtomicOp::And => "and",
                    AtomicOp::Or => "or",
                    AtomicOp::Xor => "xor",
                    AtomicOp::Load | AtomicOp::Store | AtomicOp::CompareExchange => unreachable!(),
                };
                let text = format!(
                    "atomicrmw {instruction} ptr {pointer_value}, {} {} {order}",
                    value.ty, value.value
                );
                if let Some(result) = result {
                    self.writer.line(format!("{result} = {text}"));
                } else {
                    self.writer.line(text);
                }
            }
        }
        Ok(())
    }

    pub(super) fn emit_bit_intrinsic(
        &mut self,
        result: Option<&str>,
        kind: IntrinsicKind,
        args: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        let Some(argument) = args.first() else {
            return Err(ice(format!("bit intrinsic {kind:?} requires one argument")));
        };
        let ty = self.module.operand_type(argument, self.func);
        let width = self
            .module
            .integer_width(&ty)
            .ok_or_else(|| ice(format!("bit intrinsic requires an integer, got {ty:?}")))?;
        let argument = self.operand(argument, &ty)?;
        let name = match kind {
            IntrinsicKind::CountOnes => format!("llvm.ctpop.i{width}"),
            IntrinsicKind::LeadingZeros => format!("llvm.ctlz.i{width}"),
            IntrinsicKind::TrailingZeros => format!("llvm.cttz.i{width}"),
            IntrinsicKind::ReverseBits => format!("llvm.bitreverse.i{width}"),
            // A funnel shift of a value with itself is a rotation; LLVM takes
            // the amount modulo the width, matching the intrinsic contract.
            IntrinsicKind::RotateLeft => format!("llvm.fshl.i{width}"),
            IntrinsicKind::RotateRight => format!("llvm.fshr.i{width}"),
            IntrinsicKind::SwapBytes => format!("llvm.bswap.i{width}"),
            _ => return Err(ice(format!("unexpected bit intrinsic {kind:?}"))),
        };
        self.module.require_bit_intrinsic(&name)?;
        let extra = match kind {
            IntrinsicKind::LeadingZeros | IntrinsicKind::TrailingZeros => ", i1 false".to_owned(),
            IntrinsicKind::RotateLeft | IntrinsicKind::RotateRight => {
                let amount = args
                    .get(1)
                    .ok_or_else(|| ice(format!("bit intrinsic {kind:?} requires an amount")))?;
                // The `u32` amount is zero-extended to the value width.
                let amount = self.operand(amount, &ty)?;
                format!(
                    ", {} {}, {} {}",
                    argument.ty, argument.value, amount.ty, amount.value
                )
            }
            _ => String::new(),
        };
        let call = format!(
            "call {} @{name}({} {}{extra})",
            argument.ty, argument.ty, argument.value
        );
        // Counts of a 64-bit value are `i64` in LLVM but `u32` in the
        // intrinsic contract, so they are narrowed before binding the result.
        if let Some(result) = result
            && !kind.bit_result_is_input_width()
            && width != 32
        {
            let wide = self.new_value();
            self.writer.line(format!("{wide} = {call}"));
            self.writer
                .line(format!("{result} = trunc {} {wide} to i32", argument.ty));
        } else if let Some(result) = result {
            self.writer.line(format!("{result} = {call}"));
        } else {
            self.writer.line(call);
        }
        Ok(())
    }
}

fn promote_c_vararg_type(ty: &ArType) -> ArType {
    match ty {
        ArType::Primitive(Primitive::Bool | Primitive::I8 | Primitive::I16) => {
            ArType::Primitive(Primitive::I32)
        }
        ArType::Primitive(Primitive::U8 | Primitive::U16 | Primitive::Byte) => {
            ArType::Primitive(Primitive::U32)
        }
        ArType::Primitive(Primitive::F32) => ArType::Primitive(Primitive::F64),
        other => other.clone(),
    }
}
