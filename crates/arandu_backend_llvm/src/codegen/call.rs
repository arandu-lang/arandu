use arandu_middle::Diagnostic;
use arandu_middle::amir::{AmirOperand, TempId};
use arandu_middle::intrinsics::{AtomicOp, AtomicWidth, IntrinsicKind};
use arandu_middle::layout::{ReturnAbi, is_export_aggregate};
use arandu_middle::types::{ArType, Primitive};

use super::export::lanes_ty;
use super::{FuncEmitter, TypedValue, ice, quote_name};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(super) fn emit_call(
        &mut self,
        lhs: Option<TempId>,
        result: Option<&str>,
        callee: &AmirOperand,
        args: &[AmirOperand],
    ) -> Result<(), Diagnostic> {
        let symbol = match callee {
            AmirOperand::FunctionRef(symbol) | AmirOperand::GlobalRef(symbol) => *symbol,
            other => {
                return Err(ice(format!(
                    "LLVM backend only lowers direct calls, got {other:?}"
                )));
            }
        };
        if let Some(kind) = self.module.intrinsic_kind(symbol) {
            return self.emit_intrinsic(lhs, result, kind, args);
        }
        let (param_types, return_ty) = self.module.signature_for(symbol, args, self.func);
        if param_types.len() != args.len() {
            return Err(ice(format!(
                "call to {} expects {} arguments, got {}",
                self.module.function_name(symbol),
                param_types.len(),
                args.len()
            )));
        }
        let function_name = self.module.function_name(symbol);
        if !self.module.is_internal(symbol) {
            self.module
                .require_extern_signature(&function_name, &param_types, &return_ty)?;
        }
        // Extern/host calls use the flattened C ABI; Arandu-defined callees keep
        // first-class fat pointers as `{ ptr, i64 }` values.
        let flatten_host_abi = !self.module.is_internal(symbol);
        let exported = self.module.symbols.is_exported(symbol);
        let span = self.module.symbols.get(symbol).span;
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
                    self.writer
                        .line(format!("{buffer} = alloca {aggregate}, align {align}"));
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
        for (argument, expected) in args.iter().zip(&param_types) {
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
        let call = format!(
            "call {calling_convention}{return_extension}{return_text} @{name}({})",
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
            self.writer
                .line(format!("{buffer} = alloca {aggregate}, align {align}"));
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
            IntrinsicKind::Atomic(op, width) => self.emit_atomic(result, op, width, args),
            IntrinsicKind::AtomicFence | IntrinsicKind::AtomicCompilerFence => {
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
            | IntrinsicKind::ReverseBits => self.emit_bit_intrinsic(result, kind, args),
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
                if let Some(result) = result {
                    self.writer.line(format!(
                        "{result} = load atomic {ty_text}, ptr {pointer_value} seq_cst, align {align}"
                    ));
                } else {
                    self.writer.line(format!(
                        "load atomic {ty_text}, ptr {pointer_value} seq_cst, align {align}"
                    ));
                }
            }
            AtomicOp::Store => {
                let value = self.operand(&args[1], &value_ty)?;
                self.writer.line(format!(
                    "store atomic {ty_text} {}, ptr {pointer_value} seq_cst, align {align}",
                    value.value
                ));
            }
            AtomicOp::CompareExchange => {
                let expected = self.operand(&args[1], &value_ty)?;
                let desired = self.operand(&args[2], &value_ty)?;
                let pair = self.new_value();
                self.writer.line(format!(
                    "{pair} = cmpxchg ptr {pointer_value}, {} {}, {} {} seq_cst seq_cst",
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
                    "atomicrmw {instruction} ptr {pointer_value}, {} {} seq_cst",
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
            _ => return Err(ice(format!("unexpected bit intrinsic {kind:?}"))),
        };
        self.module.require_bit_intrinsic(&name)?;
        let extra = if matches!(
            kind,
            IntrinsicKind::LeadingZeros | IntrinsicKind::TrailingZeros
        ) {
            ", i1 false"
        } else {
            ""
        };
        let call = format!("call {} @{name}({}{extra})", argument.ty, argument.value);
        if let Some(result) = result {
            self.writer.line(format!("{result} = {call}"));
        } else {
            self.writer.line(call);
        }
        Ok(())
    }
}
