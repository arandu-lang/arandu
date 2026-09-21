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
use cranelift_codegen::ir::{AbiParam, Signature, Type};
use cranelift_codegen::isa::CallConv;

/// Returns the appropriate Cranelift [`CallConv`] for the given target triple.
///
/// Uses `WindowsFastcall` on Windows and `SystemV` on all other platforms.
#[must_use]
pub fn call_conv_for_target(triple: &target_lexicon::Triple) -> CallConv {
    match triple.operating_system {
        target_lexicon::OperatingSystem::Windows => CallConv::WindowsFastcall,
        _ => CallConv::SystemV,
    }
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
        ArType::Named(_, _) | ArType::Tuple(_) => {
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
