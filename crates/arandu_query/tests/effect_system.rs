#![allow(clippy::expect_used, clippy::unwrap_used)]

use arandu_middle::DiagCode;
use arandu_query::passes::type_check;
use arandu_query::DatabaseImpl;

#[test]
fn pure_function_calling_pure_succeeds() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_pure.aru".into(),
        r#"
@Effects(Pure)
func helper(x: int): int {
    return x + 1
}

@Effects(Pure)
func compute(x: int): int {
    return helper(x) * 2
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let effect_errors: Vec<_> = checked
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagCode::T039UnsatisfiedEffect)
        .collect();
    assert!(
        effect_errors.is_empty(),
        "Expected zero effect errors, found: {effect_errors:?}"
    );
}

#[test]
fn pure_function_calling_foreign_extern_fails_with_t039() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_foreign.aru".into(),
        r#"
extern "C" {
    func puts(s: str): int
}

@Effects(Pure)
func run(): int {
    unsafe {
        puts("hello")
    }
    return 0
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let unsatisfied = checked
        .diagnostics
        .iter()
        .find(|d| d.code == DiagCode::T039UnsatisfiedEffect)
        .expect("T039UnsatisfiedEffect for foreign call");
    assert!(unsatisfied.message.contains("Foreign"));
}

#[test]
fn effect_propagation_detects_undeclared_transitive_effect() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_transitive.aru".into(),
        r#"
@Effects(Net)
func fetch_data(): int {
    return 42
}

@Effects(FileRead)
func process(): int {
    let data = fetch_data()
    return data
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let unsatisfied = checked
        .diagnostics
        .iter()
        .find(|d| d.code == DiagCode::T039UnsatisfiedEffect)
        .expect("T039 for missing Net effect in caller");
    assert!(unsatisfied.message.contains("Net"));
}

#[test]
fn effect_matching_declared_effects_succeeds() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_matching.aru".into(),
        r#"
@Effects(Net)
func fetch_data(): int {
    return 42
}

@Effects(Net, FileRead)
func process(): int {
    let data = fetch_data()
    return data
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let effect_errors: Vec<_> = checked
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagCode::T039UnsatisfiedEffect)
        .collect();
    assert!(
        effect_errors.is_empty(),
        "Expected zero effect errors, found: {effect_errors:?}"
    );
}

#[test]
fn unknown_effect_name_reports_n012() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_unknown.aru".into(),
        r#"
@Effects(Telepathy)
func mind_read(): int {
    return 0
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let unknown_err = checked
        .diagnostics
        .iter()
        .find(|d| d.code == DiagCode::N012UnknownAnnotation)
        .expect("N012UnknownAnnotation for unknown effect name");
    assert!(unknown_err.message.contains("Telepathy"));
}

#[test]
fn pure_function_awaiting_coroutine_fails_with_t039() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_await.aru".into(),
        r#"
async func async_task(): int {
    return 10
}

@Effects(Pure)
async func run(): int {
    let x = await async_task()
    return x
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let unsatisfied = checked
        .diagnostics
        .iter()
        .find(|d| d.code == DiagCode::T039UnsatisfiedEffect)
        .expect("T039UnsatisfiedEffect for await in Pure function");
    assert!(unsatisfied.message.contains("Suspend"));
}

#[test]
fn noalloc_function_allocating_heap_fails_with_t039() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_noalloc.aru".into(),
        r#"
@Effects(NoAlloc)
func allocate(): int {
    unsafe {
        let p = alloc(64)
        free(p)
    }
    return 0
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let unsatisfied = checked
        .diagnostics
        .iter()
        .find(|d| d.code == DiagCode::T039UnsatisfiedEffect)
        .expect("T039UnsatisfiedEffect for heap allocation in NoAlloc function");
    assert!(unsatisfied.message.contains("Heap"));
    assert!(!unsatisfied.labels.is_empty());
    assert!(!unsatisfied.hints.is_empty());
}

#[test]
fn nosuspend_function_awaiting_fails_with_t039() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_nosuspend.aru".into(),
        r#"
async func compute(): int {
    return 42
}

@Effects(NoSuspend)
async func execute(): int {
    let v = await compute()
    return v
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let unsatisfied = checked
        .diagnostics
        .iter()
        .find(|d| d.code == DiagCode::T039UnsatisfiedEffect)
        .expect("T039UnsatisfiedEffect for await in NoSuspend function");
    assert!(unsatisfied.message.contains("Suspend"));
}

#[test]
fn pure_function_allocating_heap_fails_with_t039() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_pure_heap.aru".into(),
        r#"
@Effects(Pure)
func make_heap(): int {
    unsafe {
        let p = alloc(64)
        free(p)
    }
    return 1
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let unsatisfied = checked
        .diagnostics
        .iter()
        .find(|d| d.code == DiagCode::T039UnsatisfiedEffect)
        .expect("T039 for heap allocation in Pure function");
    assert!(unsatisfied.message.contains("Heap"));
}

#[test]
fn unsafe_function_requires_an_explicit_unsafe_block() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "unsafe_api.aru".into(),
        r#"
@Unsafe
public func rawOperation(pointer: ptr[u8]): int {
    return 0
}

func safeCaller(): int {
    let pointer: ptr[u8] = nil
    return rawOperation(pointer)
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagCode::O013ExternRequiresUnsafe),
        "calling an @Unsafe API outside unsafe must report O013: {:?}; unsafe symbols: {:?}",
        checked.diagnostics,
        checked.type_info.unsafe_functions
    );
}

#[test]
fn imported_unsafe_function_requires_an_explicit_unsafe_block() {
    let mut db = DatabaseImpl::new();
    let _ = db.new_file(
        "stdlib/std/unsafe_api.aru".into(),
        r#"
module std.unsafe_api

@Unsafe
public func rawOperation(pointer: ptr[u8]): int {
    return 0
}
"#
        .into(),
    );
    let caller = db.new_file(
        "main.aru".into(),
        r#"
import std.unsafe_api as raw

func main(): int {
    let pointer: ptr[u8] = nil
    return raw.rawOperation(pointer)
}
"#
        .into(),
    );

    let checked = type_check(&db, caller);
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagCode::O013ExternRequiresUnsafe),
        "an imported @Unsafe call outside unsafe must be rejected: {:?}; symbols: {:?}",
        checked.diagnostics,
        checked.type_info.unsafe_functions
    );
}
