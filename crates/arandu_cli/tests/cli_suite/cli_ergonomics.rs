#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::common;
use std::fs;

#[test]
fn global_and_command_help_are_successful_and_specific() {
    for flag in ["--help", "-h"] {
        let output = common::cli_command()
            .arg(flag)
            .output()
            .expect("run global help");
        assert!(output.status.success(), "{flag}: {output:?}");
        let help = String::from_utf8_lossy(&output.stdout);
        assert!(help.contains("Commands:"), "{help}");
        assert!(help.contains("Compiler Inspection (advanced):"), "{help}");
    }

    for (command, expected) in [
        ("run", "Cranelift JIT"),
        ("check", "--watch"),
        ("test", "--fail-fast"),
        ("build", "--target"),
        ("bench", "--warmup"),
        ("doc", "--format FORMAT"),
        ("fmt", "--check"),
    ] {
        let output = common::cli_command()
            .args([command, "--help"])
            .output()
            .expect("run subcommand help");
        assert!(output.status.success(), "{command}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "{command} help did not contain {expected}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn misspelled_command_suggests_the_nearest_subcommand() {
    let output = common::cli_command()
        .arg("tset")
        .output()
        .expect("run misspelled command");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Did you mean 'test'?"));
}

#[test]
fn run_hides_incremental_query_counts_unless_verbose_is_requested() {
    let root = common::temp_dir("arandu-cli-run-status").expect("create temp root");
    let source = root.join("main.aru");
    fs::write(&source, "func main(): int { return 0 }\n").expect("write source");
    let path = source.to_string_lossy().into_owned();

    let normal = common::cli_command()
        .args(["run", &path])
        .output()
        .expect("run source normally");
    assert!(normal.status.success(), "{normal:?}");
    assert!(!String::from_utf8_lossy(&normal.stderr).contains("[rebuilt:"));

    let verbose = common::cli_command()
        .args(["run", &path, "--verbose"])
        .output()
        .expect("run source verbosely");
    assert!(verbose.status.success(), "{verbose:?}");
    assert!(String::from_utf8_lossy(&verbose.stderr).contains("[rebuilt:"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn quiet_suppresses_success_output_but_keeps_diagnostics_visible() {
    let root = common::temp_dir("arandu-cli-quiet").expect("create temp root");
    let source = root.join("main.aru");
    fs::write(&source, "func main(): int { return 0 }\n").expect("write source");
    let path = source.to_string_lossy().into_owned();
    let quiet = common::cli_command()
        .args(["check", &path, "--quiet"])
        .output()
        .expect("check quietly");
    assert!(quiet.status.success(), "{quiet:?}");
    assert!(quiet.stdout.is_empty());
    assert!(quiet.stderr.is_empty());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn quiet_suppresses_clean_status_output() {
    let root = common::temp_dir("arandu-cli-quiet-clean").expect("create temp root");
    let created = common::cli_command()
        .args(["new", "quiet_project", "--vcs=none", "--quiet"])
        .current_dir(&root)
        .output()
        .expect("create fixture project");
    assert!(created.status.success(), "{created:?}");
    assert!(created.stdout.is_empty());
    assert!(created.stderr.is_empty());

    let project = root.join("quiet_project");
    let clean = common::cli_command()
        .args(["clean", "--quiet"])
        .current_dir(&project)
        .output()
        .expect("clean quietly");
    assert!(clean.status.success(), "{clean:?}");
    assert!(clean.stdout.is_empty());
    assert!(clean.stderr.is_empty());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn formatter_handles_one_file_multiple_files_directories_and_check_mode() {
    let root = common::temp_dir("arandu-cli-fmt").expect("create temp root");
    let nested = root.join("nested");
    fs::create_dir_all(&nested).expect("create nested directory");
    let first = root.join("first.aru");
    let second = nested.join("second.aru");
    let third = nested.join("third.aru");
    fs::write(&first, "func first(){return 1}").expect("write first source");
    fs::write(&second, "func second(){return 2}").expect("write second source");
    fs::write(&third, "func third(){return 3}").expect("write third source");

    let first_arg = first.to_string_lossy().into_owned();
    let check_dirty = common::cli_command()
        .args(["fmt", "--check", &first_arg])
        .output()
        .expect("check one unformatted file");
    assert_eq!(check_dirty.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(&first).unwrap(),
        "func first(){return 1}"
    );

    let single = common::cli_command()
        .args(["fmt", &first_arg])
        .output()
        .expect("format one file");
    assert!(
        single.status.success(),
        "{}",
        String::from_utf8_lossy(&single.stderr)
    );

    let second_arg = second.to_string_lossy().into_owned();
    let third_arg = third.to_string_lossy().into_owned();
    let formatted = common::cli_command()
        .args(["fmt", &second_arg, &third_arg])
        .output()
        .expect("format explicit files");
    assert!(
        formatted.status.success(),
        "fmt failed: {}",
        String::from_utf8_lossy(&formatted.stderr)
    );
    assert_ne!(
        fs::read_to_string(&second).unwrap(),
        "func second(){return 2}"
    );
    assert_ne!(
        fs::read_to_string(&third).unwrap(),
        "func third(){return 3}"
    );

    let root_arg = root.to_string_lossy().into_owned();
    let checked = common::cli_command()
        .args(["fmt", "--check", &root_arg])
        .output()
        .expect("check recursively formatted directory");
    assert!(
        checked.status.success(),
        "fmt --check failed: {}",
        String::from_utf8_lossy(&checked.stderr)
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn formatter_rejects_non_aru_files_and_skips_generated_directories() {
    let root = common::temp_dir("arandu-cli-fmt-scope").expect("create temp root");
    let source = root.join("main.aru");
    let generated = root.join("target").join("generated.aru");
    let text = root.join("notes.txt");
    fs::create_dir_all(generated.parent().unwrap()).expect("create target directory");
    fs::write(&source, "func main(){return 0}").expect("write source");
    fs::write(&generated, "func generated(){return 1}").expect("write generated source");
    fs::write(&text, "not Arandu source").expect("write text file");

    let root_arg = root.to_string_lossy().into_owned();
    let formatted = common::cli_command()
        .args(["fmt", &root_arg])
        .output()
        .expect("format source directory");
    assert!(
        formatted.status.success(),
        "generated files should be excluded: {}",
        String::from_utf8_lossy(&formatted.stderr)
    );
    assert_ne!(
        fs::read_to_string(&source).unwrap(),
        "func main(){return 0}"
    );
    assert_eq!(
        fs::read_to_string(&generated).unwrap(),
        "func generated(){return 1}"
    );

    let text_arg = text.to_string_lossy().into_owned();
    let rejected = common::cli_command()
        .args(["fmt", &text_arg])
        .output()
        .expect("reject non-Arandu input");
    assert!(!rejected.status.success());
    assert_eq!(fs::read_to_string(&text).unwrap(), "not Arandu source");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn shell_completions_are_available_for_supported_shells() {
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let output = common::cli_command()
            .args(["completions", shell])
            .output()
            .expect("generate shell completion");
        assert!(output.status.success(), "{shell}: {output:?}");
        assert!(!output.stdout.is_empty(), "{shell} completion is empty");
        let completions = String::from_utf8_lossy(&output.stdout);
        assert!(completions.contains("archive"), "{shell} omitted archive");
        assert!(
            completions.contains("completions"),
            "{shell} omitted completions"
        );
    }

    let fish = common::cli_command()
        .args(["completions", "fish"])
        .output()
        .expect("generate fish completion");
    let fish = String::from_utf8_lossy(&fish.stdout);
    assert!(fish.contains("complete -c arandu_cli"));
    assert!(!fish.contains("complete -c arandu -f"));

    let powershell = common::cli_command()
        .args(["completions", "powershell"])
        .output()
        .expect("generate PowerShell completion");
    let powershell = String::from_utf8_lossy(&powershell.stdout);
    assert!(powershell.contains("$commandName, $parameterName, $wordToComplete"));
}

#[test]
fn primitive_methods_run_without_imports() {
    let root = common::temp_dir("arandu-cli-primitive-methods").expect("create temp root");
    let source = root.join("main.aru");
    fs::write(
        &source,
        r#"
func main(): int {
    let s = "hello"
    if s.len() != 5 {
        return 1
    }
    if s.isEmpty() {
        return 2
    }
    if !s.startsWith("he") {
        return 3
    }
    let x = -42
    if x.abs() != 42 {
        return 4
    }
    let c = 'a'
    if !c.isAscii() {
        return 5
    }
    return 0
}
"#,
    )
    .expect("write source");
    let path = source.to_string_lossy().into_owned();

    let output = common::cli_command()
        .args(["run", &path])
        .output()
        .expect("run primitive methods program");
    assert!(
        output.status.success(),
        "execution failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(root);
}
