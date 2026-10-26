//! ABI shape checks are exercised behind an explicitly trusted source boundary.
use arandu_middle::{DiagCode, Severity};
use arandu_query::{passes::type_check, DatabaseImpl};

fn rejects_trusted_signature(declaration: &str) {
    let mut db = DatabaseImpl::new();
    let file = db.new_stdlib_file(
        "intrinsic_fixture.aru".into(),
        format!(
            "module std.core.intrinsicFixture\nextern \"arandu-intrinsic\" {{ {declaration} }}"
        ),
    );
    let typed = type_check(&db, file);
    assert!(
        typed.diagnostics.iter().any(|diagnostic| diagnostic.code
            == DiagCode::U001FeatureNotSupported
            && diagnostic.message.contains("unsupported signature")),
        "{declaration}: {:?}",
        typed.diagnostics
    );
    assert!(
        !typed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("requires a trusted")),
        "signature validation must run after the origin check"
    );
}

#[test]
fn trusted_atomic_intrinsics_reject_wrong_width_ownership_arity_result_and_pointer_arithmetic() {
    for declaration in [
        "func atomicFetchAdd32(value: ref u32, operand: u32): u32",
        "func atomicLoad64(value: ref u32): u64",
        "func atomicCompareExchange32(value: mut ref u32, expected: u32): u32",
        "func atomicStoreUsize(value: mut ref usize, replacement: usize): usize",
        "func atomicFence(order: int): void",
        "func atomicFetchAddPtr(value: mut ref ptr[u8], operand: ptr[u8]): ptr[u8]",
    ] {
        rejects_trusted_signature(declaration);
    }
}

#[test]
fn trusted_bit_intrinsics_reject_wrong_width_ownership_arity_result_and_generics() {
    for declaration in [
        "func bitCountOnes32(): u32",
        "func bitCountOnes32(value: u64): u32",
        "func bitCountOnes64(value: u64): u64",
        "func bitReverse64(value: u64): u32",
        "func bitReverse32(value: ref u32): u32",
        "func bitLeadingZeros32<T>(value: u32): u32",
        "func bitTrailingZeros64(a: u64, b: u64): u32",
    ] {
        rejects_trusted_signature(declaration);
    }
}

#[test]
fn trusted_well_formed_atomic_and_bit_intrinsics_are_admitted() {
    let mut db = DatabaseImpl::new();
    let file = db.new_stdlib_file(
        "valid_intrinsics.aru".into(),
        r#"
        module std.core.intrinsicFixture
        extern "arandu-intrinsic" {
            func atomicLoad32(value: ref u32): u32
            func atomicStore64(value: ref u64, replacement: u64): void
            func atomicFence(): void
            func bitCountOnes32(value: u32): u32
            func bitReverse64(value: u64): u64
        }
    "#
        .into(),
    );
    let typed = type_check(&db, file);
    assert!(
        !typed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error),
        "{:?}",
        typed.diagnostics
    );
}
