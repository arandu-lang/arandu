//! ABI helpers for the Cranelift backend.
//!
//! Utilities for mapping Arandu types to Cranelift calling conventions and
//! building [`Signature`]s used when declaring and calling functions.

use crate::types::clif_types;
use arandu_semantics::layout::{
    AbiScalar, ArgAbi, StructLayoutProvider, TargetAbi, TargetAbiClassifier,
};
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use arandu_semantics::types::TypeInterner;
use cranelift_codegen::ir::{AbiParam, ArgumentPurpose, Signature, Type};
use cranelift_codegen::isa::CallConv;

/// Load only object bytes, not the padding of a trailing ABI register. Shared
/// by ordinary calls/returns and runtime drop shims so their bounds agree.
pub(crate) fn load_aggregate_slot(
    builder: &mut cranelift_frontend::FunctionBuilder<'_>,
    base: cranelift_codegen::ir::Value,
    slot: &arandu_semantics::layout::AbiSlot,
    aggregate_size: u64,
    little_endian: bool,
) -> Result<cranelift_codegen::ir::Value, &'static str> {
    use cranelift_codegen::ir::{InstBuilder, MemFlagsData, types};
    let ty = abi_scalar_to_clif(slot.scalar);
    let bytes = aggregate_size
        .checked_sub(slot.offset)
        .ok_or("ABI slot starts outside aggregate storage")?
        .min(slot.scalar.size());
    let offset =
        i32::try_from(slot.offset).map_err(|_| "ABI slot offset exceeds instruction range")?;
    if bytes == slot.scalar.size() {
        return Ok(builder.ins().load(ty, MemFlagsData::new(), base, offset));
    }
    if !ty.is_int() || bytes == 0 {
        return Err("invalid partial ABI register slot");
    }
    let mut packed = builder.ins().iconst(ty, 0);
    for byte in 0..bytes {
        let byte_offset = i32::try_from(byte)
            .ok()
            .and_then(|byte| offset.checked_add(byte))
            .ok_or("ABI slot offset exceeds instruction range")?;
        let value = builder
            .ins()
            .load(types::I8, MemFlagsData::new(), base, byte_offset);
        let value = builder.ins().uextend(ty, value);
        let position = if little_endian {
            byte
        } else {
            slot.scalar.size() - 1 - byte
        };
        let shift =
            i64::try_from(position * 8).map_err(|_| "ABI slot shift exceeds instruction range")?;
        let shifted = builder.ins().ishl_imm_u(value, shift);
        packed = builder.ins().bor(packed, shifted);
    }
    Ok(packed)
}

/// Determines the [`TargetAbi`] from a `target_lexicon::Triple`.
#[must_use]
pub fn target_abi_for_triple(triple: &target_lexicon::Triple) -> TargetAbi {
    match (triple.architecture, triple.operating_system) {
        (target_lexicon::Architecture::X86_64, target_lexicon::OperatingSystem::Windows) => {
            TargetAbi::WindowsX64
        }
        (target_lexicon::Architecture::X86_64, _) => TargetAbi::SystemVAmd64,
        (target_lexicon::Architecture::Aarch64(_), _) => TargetAbi::Aapcs64,
        _ => TargetAbi::Generic,
    }
}

/// Converts an [`AbiScalar`] to a Cranelift IR [`Type`].
#[must_use]
pub fn abi_scalar_to_clif(scalar: AbiScalar) -> Type {
    match scalar {
        AbiScalar::I8 => cranelift_codegen::ir::types::I8,
        AbiScalar::I16 => cranelift_codegen::ir::types::I16,
        AbiScalar::I32 => cranelift_codegen::ir::types::I32,
        AbiScalar::I64 => cranelift_codegen::ir::types::I64,
        AbiScalar::F32 => cranelift_codegen::ir::types::F32,
        AbiScalar::F64 => cranelift_codegen::ir::types::F64,
    }
}

/// Appends Cranelift [`AbiParam`] entries for `ty` to `params`.
fn append_abi_params(
    params: &mut Vec<AbiParam>,
    ty: &ArType,
    ptr_type: Type,
    classifier_ctx: Option<(
        &TargetAbiClassifier,
        &TypeInterner,
        &dyn StructLayoutProvider,
    )>,
) {
    let is_slice_view =
        classifier_ctx.is_some_and(|(_, interner, _)| ty.slice_abi_element(interner).is_some());
    if matches!(ty, ArType::Primitive(Primitive::Str)) || is_slice_view {
        params.push(AbiParam::new(ptr_type));
        params.push(AbiParam::new(ptr_type));
        return;
    }
    match ty {
        ArType::Void | ArType::Error => {}
        ArType::Named(_, _) | ArType::Tuple(_) | ArType::Array(_, _) => {
            if let Some((classifier, interner, provider)) = classifier_ctx {
                match classifier.classify_type(ty, interner, provider) {
                    ArgAbi::ZeroSized => {}
                    ArgAbi::Direct(direct) => {
                        for slot in &direct.slots {
                            params.push(AbiParam::new(abi_scalar_to_clif(slot.scalar)));
                        }
                    }
                    ArgAbi::Indirect => {
                        params.push(AbiParam::new(ptr_type));
                    }
                }
            } else {
                for &clif_ty in &clif_types(ty, ptr_type) {
                    params.push(AbiParam::new(clif_ty));
                }
            }
        }
        _ => {
            for &clif_ty in &clif_types(ty, ptr_type) {
                params.push(AbiParam::new(clif_ty));
            }
        }
    }
}

/// Builds a Cranelift [`Signature`] from Arandu parameter and return types,
/// using the target-aware ABI classifier.
#[must_use]
pub fn build_signature_with_classifier(
    params: &[ArType],
    return_type: &ArType,
    call_conv: CallConv,
    ptr_type: Type,
    classifier: &TargetAbiClassifier,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
) -> Signature {
    let mut sig = Signature::new(call_conv);
    let ctx = Some((classifier, interner, provider));
    for param in params {
        append_abi_params(&mut sig.params, param, ptr_type, ctx);
    }
    append_abi_params(&mut sig.returns, return_type, ptr_type, ctx);
    sig
}

/// Arandu-to-Arandu calls use caller-owned storage for indirect aggregate
/// results. Keep this separate from host imports, whose existing ABI must not
/// silently change when the compiler's internal representation changes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_internal_signature(
    params: &[ArType],
    return_type: &ArType,
    call_conv: CallConv,
    ptr_type: Type,
    classifier: &TargetAbiClassifier,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
) -> Signature {
    let mut sig = build_signature_with_classifier(
        params,
        return_type,
        call_conv,
        ptr_type,
        classifier,
        interner,
        provider,
    );
    let carrier = matches!(
        return_type,
        ArType::Option(_) | ArType::Result(..) | ArType::Poll(_)
    );
    if carrier
        || (matches!(
            return_type,
            ArType::Named(..) | ArType::Tuple(..) | ArType::Array(..)
        ) && matches!(
            classifier.classify_type(return_type, interner, provider),
            ArgAbi::Indirect
        ))
    {
        sig.params.insert(
            0,
            AbiParam::special(ptr_type, ArgumentPurpose::StructReturn),
        );
        sig.returns.clear();
    }
    sig
}

/// Builds a Cranelift [`Signature`] from Arandu parameter and return types
/// with legacy fallback (for tests or callers without struct metadata).
#[must_use]
pub fn build_signature(
    params: &[ArType],
    return_type: &ArType,
    call_conv: CallConv,
    ptr_type: Type,
) -> Signature {
    let mut sig = Signature::new(call_conv);
    for param in params {
        append_abi_params(&mut sig.params, param, ptr_type, None);
    }
    append_abi_params(&mut sig.returns, return_type, ptr_type, None);
    sig
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod bounded_slot_tests {
    use super::*;
    use cranelift_codegen::ir::{Function, InstBuilder, Opcode, types};
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

    #[test]
    fn trailing_register_loads_only_live_bytes_for_both_byte_orders() {
        for little in [false, true] {
            let mut function = Function::new();
            let mut context = FunctionBuilderContext::new();
            let mut builder = FunctionBuilder::new(&mut function, &mut context);
            let block = builder.create_block();
            builder.switch_to_block(block);
            let base = builder.ins().iconst(types::I64, 0);
            let slot = arandu_semantics::layout::AbiSlot {
                scalar: AbiScalar::I64,
                offset: 0,
            };
            load_aggregate_slot(&mut builder, base, &slot, 3, little).unwrap();
            let loads: Vec<_> = builder
                .func
                .layout
                .block_insts(block)
                .filter(|inst| builder.func.dfg.insts[*inst].opcode() == Opcode::Load)
                .collect();
            assert_eq!(loads.len(), 3);
            for inst in loads {
                assert_eq!(
                    builder
                        .func
                        .dfg
                        .value_type(builder.func.dfg.inst_results(inst)[0]),
                    types::I8
                );
            }
            let outside = arandu_semantics::layout::AbiSlot {
                scalar: AbiScalar::I64,
                offset: 4,
            };
            assert_eq!(
                load_aggregate_slot(&mut builder, base, &outside, 3, little),
                Err("ABI slot starts outside aggregate storage")
            );
        }
    }
}
