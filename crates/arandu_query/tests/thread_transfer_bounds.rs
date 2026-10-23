//! Canonical concurrency markers must inspect storage, not empty method lists.
use arandu_diagnostics::{DiagCode, Severity};
use arandu_query::{passes::type_check, DatabaseImpl};

fn check_storage(storage: &str, accepted: bool) {
    for capability in ["Send", "Sync"] {
        let mut db = DatabaseImpl::default();
        db.new_stdlib_file(
            "stdlib/core/marker.aru".into(),
            include_str!("../../../stdlib/core/marker.aru").into(),
        );
        let file = db.new_file(
            "transfer.aru".into(),
            format!(
                r#"
import std.core.marker as marker
struct Wrapper<T> {{ value: T }}
enum Choice<T> {{ Empty
Present(T) }}
struct Payload {{ value: {storage} }}
func require<T: marker.{capability}>(value: T): void {{}}
func inspect(value: Payload): void {{ require(value) }}
func main(): int {{ return 0 }}
"#
            ),
        );
        let result = type_check(&db, file);
        if accepted {
            assert!(
                !result
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == Severity::Error),
                "{capability}<{storage}>: {:?}",
                result.diagnostics
            );
        } else {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|d| d.code == DiagCode::T025InterfaceNotSatisfied),
                "{capability}<{storage}> was not rejected: {:?}",
                result.diagnostics
            );
        }
    }
}

#[test]
fn scalar_aggregates_satisfy_transfer_bounds() {
    check_storage("int", true);
    check_storage("Option<bool>", true);
    check_storage("Wrapper<Wrapper<int>>", true);
    check_storage("Choice<int>", true);
}

#[test]
fn copy_is_structural_and_rejects_owned_string_storage() {
    let mut db = DatabaseImpl::default();
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/alloc/string.aru".into(),
        include_str!("../../../stdlib/alloc/string.aru").into(),
    );
    let accepted = db.new_file(
        "copy_scalar.aru".into(),
        r#"
import std.core.marker as marker
struct Pair { left: int right: bool }
func require<T: marker.Copy>(value: T): void {}
func inspect(value: Pair): void { require(value) }
func main(): int { return 0 }
"#
        .into(),
    );
    assert!(!type_check(&db, accepted)
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error));

    let rejected = db.new_file(
        "copy_string.aru".into(),
        r#"
import std.core.marker as marker
import std.alloc.string as strings
func require<T: marker.Copy>(value: T): void {}
func inspect(value: strings.String): void { require(value) }
func main(): int { return 0 }
"#
        .into(),
    );
    let result = type_check(&db, rejected);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagCode::T025InterfaceNotSatisfied),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn raw_pointer_aggregates_do_not_satisfy_transfer_bounds() {
    check_storage("ptr[u8]", false);
    check_storage("Option<ptr[u8]>", false);
    check_storage("Wrapper<ptr[u8]>", false);
    check_storage("Choice<ptr[u8]>", false);
    check_storage("marker.PhantomData<ptr[u8]>", false);
}

#[test]
fn cooperative_task_handle_requires_its_own_transfer_contract() {
    let mut db = DatabaseImpl::default();
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/std/runtime/executor.aru".into(),
        include_str!("../../../stdlib/std/runtime/executor.aru").into(),
    );
    let file = db.new_file(
        "task_handle.aru".into(),
        r#"
import std.core.marker as marker
import std.runtime.executor as rt
func require<T: marker.Send>(value: T): void {}
func inspect(value: rt.TaskHandle<int>): void { require(value) }
func main(): int { return 0 }
"#
        .into(),
    );
    let result = type_check(&db, file);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagCode::T025InterfaceNotSatisfied),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn imported_wrapper_does_not_hide_a_cooperative_handle() {
    let mut db = DatabaseImpl::default();
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/std/runtime/executor.aru".into(),
        include_str!("../../../stdlib/std/runtime/executor.aru").into(),
    );
    db.new_file(
        "bridge.aru".into(),
        r#"
import std.runtime.executor as rt
public struct Envelope { handle: rt.TaskHandle<int> }
"#
        .into(),
    );
    let file = db.new_file(
        "consumer.aru".into(),
        r#"
import bridge
import std.core.marker as marker
func require<T: marker.Send>(value: T): void {}
func inspect(value: bridge.Envelope): void { require(value) }
func main(): int { return 0 }
"#
        .into(),
    );
    let result = type_check(&db, file);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagCode::T025InterfaceNotSatisfied),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn borrowed_storage_requires_a_separate_lifetime_proof() {
    check_storage("str", false);
    check_storage("[]u8", false);
    check_storage("ref int", false);
    check_storage("mut ref int", false);
}

fn check_program(source: &str, rejected: bool) {
    let mut db = DatabaseImpl::default();
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    let file = db.new_file("capability.aru".into(), source.into());
    let result = type_check(&db, file);
    if rejected {
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == DiagCode::T025InterfaceNotSatisfied),
            "{:?}",
            result.diagnostics
        );
    } else {
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error),
            "{:?}",
            result.diagnostics
        );
    }
}

#[test]
fn canonical_marker_survives_named_import_alias() {
    check_program(
        r#"
from std.core.marker import { Send as Transfer }
struct Payload { pointer: ptr[u8] }
func require<T: Transfer>(value: T): void {}
func inspect(value: Payload): void { require(value) }
func main(): int { return 0 }
"#,
        true,
    );
}

#[test]
fn user_interface_named_send_is_not_a_compiler_capability() {
    check_program(
        r#"
interface Send {}
struct Payload { pointer: ptr[u8] }
func require<T: Send>(value: T): void {}
func inspect(value: Payload): void { require(value) }
func main(): int { return 0 }
"#,
        false,
    );
}

#[test]
fn user_bound_cannot_prove_canonical_transfer() {
    check_program(
        r#"
import std.core.marker as marker
interface Send {}
func require<T: marker.Send>(value: T): void {}
func forward<T: Send>(value: T): void { require(value) }
func main(): int { return 0 }
"#,
        true,
    );
}

#[test]
fn generic_fields_use_their_declared_capability() {
    for (bound, rejected) in [("Send", false), ("Sync", true)] {
        check_program(
            &format!(
                r#"
import std.core.marker as marker
struct Wrapper<T> {{ value: T }}
func require<T: marker.Send>(value: T): void {{}}
func forward<T: marker.{bound}>(value: Wrapper<T>): void {{ require(value) }}
func main(): int {{ return 0 }}
"#
            ),
            rejected,
        );
    }
}

#[test]
fn scalar_arguments_can_be_inferred() {
    check_program(
        r#"
import std.core.marker as marker
func require<T: marker.Send>(value: T): void {}
func main(): int { require(42); require(true); require(1.5); return 0 }
"#,
        false,
    );
}

#[test]
fn destructor_resource_is_not_approved_from_scalar_fields() {
    check_program(
        r#"
import std.core.marker as marker
struct Resource { id: int }
@Destructor
func Resource.close(own self): void {}
func require<T: marker.Send>(value: T): void {}
func inspect(value: Resource): void { require(value) }
func main(): int { return 0 }
"#,
        true,
    );
}

#[test]
fn canonical_bound_identity_survives_a_transitive_module_signature() {
    let mut db = DatabaseImpl::default();
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_file(
        "bridge.aru".into(),
        r#"
import std.core.marker as marker
public func requireSync<T: marker.Sync>(value: T): void {}
"#
        .into(),
    );

    let accepted = db.new_file(
        "accepted.aru".into(),
        r#"
import bridge
func main(): int { bridge.requireSync(42); return 0 }
"#
        .into(),
    );
    assert!(!type_check(&db, accepted)
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error));

    let rejected = db.new_file(
        "rejected.aru".into(),
        r#"
import bridge
func inspect(pointer: ptr[u8]): void { bridge.requireSync(pointer) }
func main(): int { return 0 }
"#
        .into(),
    );
    let result = type_check(&db, rejected);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagCode::T025InterfaceNotSatisfied),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn thread_spawn_accepts_send_job_and_rejects_non_send() {
    let mut db = DatabaseImpl::default();
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/core/mem.aru".into(),
        include_str!("../../../stdlib/core/mem.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/std/thread.aru".into(),
        include_str!("../../../stdlib/std/thread.aru").into(),
    );

    let accepted = db.new_file(
        "accepted_spawn.aru".into(),
        r#"
import std.thread as thread

struct CalcJob {
    dados: int
}

func CalcJob.run(own self, arg: void): int {
    return self.dados + 1
}

func main(): int {
    let job = CalcJob { dados: 41 }
    let handleRes = thread.spawn(job)
    return 0
}
"#
        .into(),
    );
    let result = type_check(&db, accepted);
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error),
        "spawn(SendJob) failed: {:?}",
        result.diagnostics
    );

    let rejected = db.new_file(
        "rejected_spawn.aru".into(),
        r#"
import std.thread as thread

struct BadJob {
    pointer: ptr[u8]
}

func BadJob.run(own self, arg: void): int {
    return 0
}

func main(): int {
    let job = BadJob { pointer: nil }
    let handleRes = thread.spawn(job)
    return 0
}
"#
        .into(),
    );
    let result = type_check(&db, rejected);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagCode::T025InterfaceNotSatisfied),
        "non-Send job was not rejected: {:?}",
        result.diagnostics
    );
}

#[test]
fn thread_spawn_with_closure_accepted_when_move_zero_params_and_send() {
    let mut db = DatabaseImpl::default();
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/core/mem.aru".into(),
        include_str!("../../../stdlib/core/mem.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/std/thread.aru".into(),
        include_str!("../../../stdlib/std/thread.aru").into(),
    );

    let file = db.new_file(
        "closure_spawn.aru".into(),
        r#"
import std.thread as thread

func main(): int {
    let dados: int = 41
    let handleRes = thread.spawn(move || dados + 1)
    return 0
}
"#
        .into(),
    );
    let result = type_check(&db, file);
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error),
        "move || with an owned Send capture must type-check: {:?}",
        result.diagnostics
    );
}

#[test]
fn thread_spawn_with_closure_rejects_non_move_and_params() {
    let mut db = DatabaseImpl::default();
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/core/mem.aru".into(),
        include_str!("../../../stdlib/core/mem.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/std/thread.aru".into(),
        include_str!("../../../stdlib/std/thread.aru").into(),
    );

    // 1. Missing `move`
    let non_move = db.new_file(
        "non_move.aru".into(),
        r#"
import std.thread as thread

func main(): int {
    let dados: int = 41
    let handleRes = thread.spawn(|| dados + 1)
    return 0
}
"#
        .into(),
    );
    let result_non_move = type_check(&db, non_move);
    assert!(
        result_non_move
            .diagnostics
            .iter()
            .any(|d| d.code == DiagCode::U001FeatureNotSupported),
        "non-move thread closure must report U001: {:?}",
        result_non_move.diagnostics
    );

    // 2. Closure with parameters
    let with_params = db.new_file(
        "with_params.aru".into(),
        r#"
import std.thread as thread

func main(): int {
    let handleRes = thread.spawn(move |x: int| x + 1)
    return 0
}
"#
        .into(),
    );
    let result_with_params = type_check(&db, with_params);
    assert!(
        result_with_params
            .diagnostics
            .iter()
            .any(|d| d.code == DiagCode::U001FeatureNotSupported),
        "thread closure with parameters must report U001: {:?}",
        result_with_params.diagnostics
    );

    // 3. Closure outside thread.spawn
    let outside = db.new_file(
        "outside.aru".into(),
        r#"
func main(): int {
    let f = move || 42
    return 0
}
"#
        .into(),
    );
    let result_outside = type_check(&db, outside);
    assert!(
        result_outside
            .diagnostics
            .iter()
            .any(|d| d.code == DiagCode::U001FeatureNotSupported),
        "general closure outside thread spawn must report U001: {:?}",
        result_outside.diagnostics
    );
}

#[test]
fn thread_spawn_with_closure_rejects_borrowed_and_non_send() {
    let mut db = DatabaseImpl::default();
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/core/mem.aru".into(),
        include_str!("../../../stdlib/core/mem.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/std/thread.aru".into(),
        include_str!("../../../stdlib/std/thread.aru").into(),
    );

    let borrowed = db.new_file(
        "borrowed_capture.aru".into(),
        r#"
import std.thread as thread

func check(r: ref int): void {
    let handleRes = thread.spawn(move || *r)
}

func main(): int {
    return 0
}
"#
        .into(),
    );
    let result_borrowed = type_check(&db, borrowed);
    assert!(
        result_borrowed
            .diagnostics
            .iter()
            .any(|d| d.code == DiagCode::T025InterfaceNotSatisfied),
        "borrowed reference capture must be rejected with T025: {:?}",
        result_borrowed.diagnostics
    );
}

#[test]
fn lang_item_identity_preserved_across_imports_aliases_and_reexports() {
    let mut db = DatabaseImpl::default();
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/core/mem.aru".into(),
        include_str!("../../../stdlib/core/mem.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/std/thread.aru".into(),
        include_str!("../../../stdlib/std/thread.aru").into(),
    );

    // 1. Direct module import and alias import
    let alias_file = db.new_file(
        "alias_client.aru".into(),
        r#"
from std.thread import { spawn as custom_spawn }

func main(): int {
    let handle = custom_spawn(move || 42)
    return 0
}
"#
        .into(),
    );
    let result_alias = type_check(&db, alias_file);
    // custom_spawn preserves LangItem::ThreadSpawn, so the zero-capture closure
    // is lowered as a ThreadJob instead of a wrong-arity mismatch.
    assert!(
        !result_alias.diagnostics.iter().any(|d| {
            d.code == DiagCode::U001FeatureNotSupported
                && d.message.contains("require AMIR ThreadJob lowering")
        }) && !result_alias
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error),
        "alias import must preserve LangItem::ThreadSpawn identity: {:?}",
        result_alias.diagnostics
    );

    // 2. Re-export across files
    let reexport_file = db.new_file(
        "reexport_lib.aru".into(),
        r#"
public use std.thread.{ spawn }
"#
        .into(),
    );
    let client_file = db.new_file(
        "reexport_client.aru".into(),
        r#"
import reexport_lib as lib

func main(): int {
    let handle = lib.spawn(move || 42)
    return 0
}
"#
        .into(),
    );
    let _ = type_check(&db, reexport_file);
    let result_client = type_check(&db, client_file);
    assert!(
        !result_client.diagnostics.iter().any(|d| {
            d.code == DiagCode::U001FeatureNotSupported
                && d.message.contains("require AMIR ThreadJob lowering")
        }) && !result_client
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error),
        "re-exported symbol must preserve LangItem::ThreadSpawn identity: {:?}",
        result_client.diagnostics
    );
}

#[test]
fn thread_spawn_move_closure_lowers_capture_struct_and_run() {
    let mut db = DatabaseImpl::default();
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/core/mem.aru".into(),
        include_str!("../../../stdlib/core/mem.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/std/thread.aru".into(),
        include_str!("../../../stdlib/std/thread.aru").into(),
    );
    let file = db.new_file(
        "capture_amir.aru".into(),
        r#"
import std.thread as thread

func main(): int {
    let dados: int = 41
    let handleRes = thread.spawn(move || dados + 1)
    return 0
}
"#
        .into(),
    );
    let runtime = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        !runtime
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error),
        "capture closure failed to lower: {:?}",
        runtime.diagnostics
    );
    let symbols = &runtime.artifacts.type_check.symbols;
    let run = symbols.iter().find(|symbol| {
        symbols
            .host_function_names
            .get(&symbol.id)
            .is_some_and(|name| name.starts_with("ar_thread_capture_") && name.ends_with("_run"))
    });
    let Some(run) = run else {
        panic!("synthesized run is missing from the runtime symbol table");
    };
    assert!(
        runtime
            .artifacts
            .amir
            .funcs
            .iter()
            .any(|func| func.symbol == run.id),
        "AMIR is missing the capture run body"
    );
    let capture = symbols.iter().find(|symbol| {
        symbol.kind == arandu_middle::SymbolKind::Struct
            && symbol.name.starts_with("__ThreadCapture_")
    });
    let Some(capture) = capture else {
        panic!("capture struct is missing");
    };
    let fields = runtime
        .artifacts
        .type_check
        .type_info
        .struct_fields
        .get(&capture.id);
    assert!(
        fields.is_some_and(|fields| fields.iter().any(|field| field.name == "dados")),
        "capture struct must store dados"
    );
}

#[test]
fn bare_metal_rejects_std_thread() {
    let mut db = DatabaseImpl::default();
    db.set_target(
        arandu_middle::DataLayout::ptr_width(4),
        arandu_middle::db::TargetIdentity::from_triple("thumbv7em-none-eabihf")
            .expect("canonical triple"),
    );
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/core/mem.aru".into(),
        include_str!("../../../stdlib/core/mem.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/std/thread.aru".into(),
        include_str!("../../../stdlib/std/thread.aru").into(),
    );
    let file = db.new_file(
        "bare_thread.aru".into(),
        r#"
import std.thread as thread

func main(): int {
    let handleRes = thread.spawn(move || 1)
    return 0
}
"#
        .into(),
    );
    let result = type_check(&db, file);
    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DiagCode::T099RequiresOperatingSystem
                && diagnostic.message
                    == "The module 'std.thread' requires an underlying operating system, but target 'thumbv7em-none-eabihf' is bare-metal."
        }),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn bare_metal_rejects_std_fs_read_to_string() {
    let mut db = DatabaseImpl::default();
    db.set_target(
        arandu_middle::DataLayout::ptr_width(4),
        arandu_middle::db::TargetIdentity::from_triple("thumbv7em-none-eabihf")
            .expect("canonical triple"),
    );
    db.new_stdlib_file(
        "stdlib/std/fs.aru".into(),
        "module std.fs\npublic func readToString(path: str): int {\n    return 0\n}\n".into(),
    );
    let file = db.new_file(
        "bare_fs.aru".into(),
        r#"
from std.fs import { readToString }

func main(): int {
    let content = readToString("memory.ld")
    return 0
}
"#
        .into(),
    );
    let result = type_check(&db, file);
    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DiagCode::T099RequiresOperatingSystem
                && diagnostic.message
                    == "The module 'std.fs' requires an underlying operating system, but target 'thumbv7em-none-eabihf' is bare-metal."
        }),
        "{:?}",
        result.diagnostics
    );
}
fn thread_db() -> DatabaseImpl {
    let mut db = DatabaseImpl::default();
    db.new_stdlib_file(
        "stdlib/core/marker.aru".into(),
        include_str!("../../../stdlib/core/marker.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/core/mem.aru".into(),
        include_str!("../../../stdlib/core/mem.aru").into(),
    );
    db.new_stdlib_file(
        "stdlib/std/thread.aru".into(),
        include_str!("../../../stdlib/std/thread.aru").into(),
    );
    db
}

/// Capture discovery reads resolved uses, so a non-Send or borrowed value is
/// rejected wherever it appears in the body, not only in the forms a syntax
/// walker happens to visit.
#[test]
fn move_closure_captures_are_found_in_every_body_form() {
    let mut db = thread_db();
    let bodies = [
        ("interpolation", "let s = \"p=${p}\"\n        return 0"),
        (
            "match_statement_arm",
            "let mut out = 0\n        match out {\n            0 => { let q = p }\n            _ => {}\n        }\n        return out",
        ),
        ("assignment_target", "p = nil\n        return 0"),
        (
            "nested_closure",
            "let inner = thread.spawn(move || { let q = p\n return 0 })\n        return 0",
        ),
    ];
    for (name, body) in bodies {
        let file = db.new_file(
            format!("capture_{name}.aru"),
            format!(
                r#"
import std.thread as thread

func main(): int {{
    let mut p: ptr[u8] = nil
    let handleRes = thread.spawn(move || {{
        {body}
    }})
    return 0
}}
"#
            ),
        );
        let result = type_check(&db, file);
        assert!(
            result.diagnostics.iter().any(|d| {
                d.code == DiagCode::T025InterfaceNotSatisfied && d.message.contains("'p'")
            }),
            "{name}: non-Send capture must be rejected with T025: {:?}",
            result.diagnostics
        );
    }

    let borrowed = db.new_file(
        "capture_nested_ref.aru".into(),
        r#"
import std.thread as thread

func check(r: ref int): void {
    let handleRes = thread.spawn(move || {
        let inner = thread.spawn(move || *r)
        return 0
    })
}

func main(): int {
    return 0
}
"#
        .into(),
    );
    let result = type_check(&db, borrowed);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagCode::T025InterfaceNotSatisfied),
        "a reference captured by a nested closure must stay T025: {:?}",
        result.diagnostics
    );
}

/// `return` inside a `move ||` body leaves the job: it is checked against the
/// closure result, never against the enclosing function's signature.
#[test]
fn move_closure_return_belongs_to_the_closure() {
    let mut db = thread_db();
    let accepted = db.new_file(
        "closure_return_void_outer.aru".into(),
        r#"
import std.thread as thread

func observe(n: int): void {
    let handleRes = thread.spawn(move || {
        if n > 0 { return n }
        return 0 - n
    })
}

func main(): int {
    return 0
}
"#
        .into(),
    );
    let result = type_check(&db, accepted);
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error),
        "return inside the closure must not use the outer void signature: {:?}",
        result.diagnostics
    );

    let mismatched = db.new_file(
        "closure_return_mismatch.aru".into(),
        r#"
import std.thread as thread

func main(): int {
    let n: int = 1
    let handleRes = thread.spawn(move || {
        if n > 0 { return n }
        return true
    })
    return 0
}
"#
        .into(),
    );
    let result = type_check(&db, mismatched);
    assert!(
        result.diagnostics.iter().any(|d| {
            d.code == DiagCode::T004IncompatibleReturnType
                && d.message.starts_with("incompatible closure result")
        }),
        "closure exits with different types must report the closure result: {:?}",
        result.diagnostics
    );
}
