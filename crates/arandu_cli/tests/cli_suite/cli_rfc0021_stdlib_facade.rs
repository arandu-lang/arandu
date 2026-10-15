//! RFC 0021 stdlib prelude facade tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

fn run_cli_in(cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("cli")
}

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn prelude_public_use_facade_check_ok() {
    let dir = std::env::temp_dir();
    let file = dir.join(format!("arandu_rfc0021_prelude_{}.aru", std::process::id()));
    fs::write(
        &file,
        r#"
module tests.rfc0021.prelude

from std.core.prelude import { Option, Result, Poll, checkedAdd, Iterator, PhantomData, phantom, Cell, UnsafeCell, Copy, Send, Sync }
from std.core.prelude import { Option as Maybe }

func checked_value(): Option<int> {
    return checkedAdd(20, 22)
}

func option_value(): int {
    return Option.Some(42).unwrapOr(0)
}

func aliased_option_value(): int {
    return Maybe.Some(42).unwrapOr(0)
}

func result_value(): int {
    let value: Result<int, str> = Result.Ok(42)
    return value.unwrapOr(0)
}

func poll_value(): Poll<int> {
    return Poll.Ready(42)
}

func test_cell(): int {
    let c = Cell.new(42)
    return c.value
}

func test_phantom(): PhantomData<int> {
    return phantom<int>()
}

func test_bounds<T: Copy + Send + Sync>(x: T): T {
    return x
}

func main(): int {
    let p = test_phantom()
    let b = test_bounds(10)
    return test_cell() + option_value() + result_value() + aliased_option_value() - 168
}
"#,
    )
    .unwrap();

    let root = workspace_root();
    let out = run_cli_in(&root, &["check", file.to_str().unwrap()]);
    let _ = fs::remove_file(&file);

    assert!(
        out.status.success(),
        "prelude facade check failed: status: {:?}, stdout: {}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn prelude_facade_program_runs_successfully() {
    let dir = std::env::temp_dir();
    let file = dir.join(format!("arandu_rfc0021_run_{}.aru", std::process::id()));
    fs::write(
        &file,
        r#"
module tests.rfc0021.run

from std.core.prelude import { Cell }

func main(): int {
    let c = Cell.new(84)
    if c.value / 2 == 42 {
        return 0
    }
    return 1
}
"#,
    )
    .unwrap();

    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    let _ = fs::remove_file(&file);

    assert!(
        out.status.success(),
        "prelude run failed: status: {:?}, stdout: {}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn language_item_spellings_can_be_shadowed_by_local_types() {
    let dir = std::env::temp_dir();
    let file = dir.join(format!(
        "arandu_rfc0021_shadow_lang_items_{}.aru",
        std::process::id()
    ));
    fs::write(
        &file,
        r#"
module tests.rfc0021.shadow_lang_items

enum Option { Local }
enum Result { Local }
enum Poll { Local }
struct Coroutine {}

func main(): int {
    let option = Option.Local
    let result = Result.Local
    let poll = Poll.Local
    let coroutine = Coroutine {}
    match option {
        Local => { return 0 }
    }
}
"#,
    )
    .unwrap();

    let root = workspace_root();
    let out = run_cli_in(&root, &["check", file.to_str().unwrap()]);
    let _ = fs::remove_file(&file);

    assert!(
        out.status.success(),
        "local lang item spelling check failed: status: {:?}, stdout: {}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn generic_shadowed_enum_constructors_and_poll_patterns_work() {
    let dir = std::env::temp_dir();
    let file = dir.join(format!(
        "arandu_rfc0021_generic_shadow_poll_{}.aru",
        std::process::id()
    ));
    fs::write(
        &file,
        r#"
module tests.rfc0021.generic_shadow_poll

from std.core.prelude import { Poll }

enum Option<T> { Some(T), None }

func user_option(): int {
    let value: Option<int> = Option.Some(42)
    let empty: Option<int> = Option.None
    match value {
        Option.Some(item) => { return item }
        Option.None => { return 0 }
    }
}

func poll_value(poll: Poll<int>): int {
    match poll {
        Poll.Ready(value) => { return value }
        Poll.Pending => { return 0 }
    }
}

func main(): int {
    let option = user_option()
    let poll = poll_value(Poll.Ready(42))
    if option == 42 && poll == 42 { return 0 }
    return 1
}
"#,
    )
    .unwrap();

    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    let _ = fs::remove_file(&file);

    assert!(
        out.status.success(),
        "generic enum / Poll pattern run failed: status: {:?}, stdout: {}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
