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
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagCode::U001FeatureNotSupported && d.message.contains("require AMIR ThreadJob lowering")),
        "valid thread closure spawn must be rejected with U001 pending AMIR ThreadJob lowering: {:?}",
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
    // Because custom_spawn preserves LangItem::ThreadSpawn, it enters synth_thread_spawn_closure
    // and emits the U001 asking for AMIR lowering (instead of T012 wrong arg count / type mismatch)
    assert!(
        result_alias
            .diagnostics
            .iter()
            .any(|d| d.code == DiagCode::U001FeatureNotSupported
                && d.message.contains("require AMIR ThreadJob lowering")),
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
        result_client
            .diagnostics
            .iter()
            .any(|d| d.code == DiagCode::U001FeatureNotSupported
                && d.message.contains("require AMIR ThreadJob lowering")),
        "re-exported symbol must preserve LangItem::ThreadSpawn identity: {:?}",
        result_client.diagnostics
    );
}
