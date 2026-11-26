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
fn value_options_do_not_consume_the_next_flag_as_their_value() {
    for (command, option, expected) in [
        (
            "build",
            "--target",
            "--target requires a target triple argument",
        ),
        (
            "check",
            "--stdlib-path",
            "--stdlib-path requires a path argument",
        ),
        (
            "check",
            "--cache-dir",
            "--cache-dir requires a path argument",
        ),
    ] {
        let output = common::cli_command()
            .args([command, option, "--quiet"])
            .output()
            .expect("run command with missing option value");

        assert_eq!(
            output.status.code(),
            Some(2),
            "{command} {option}: {output:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{command} {option}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn explain_prints_long_form_documentation_for_a_documented_code() {
    for args in [
        vec!["explain", "T018"],
        vec!["--explain", "T018"],
        vec!["--explain=T018"],
    ] {
        let output = common::cli_command()
            .args(&args)
            .output()
            .expect("print diagnostic explanation");
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("# Error T018:"),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn explain_rejects_unknown_and_undocumented_codes_with_actionable_errors() {
    let typo = common::cli_command()
        .args(["explain", "T01B"])
        .output()
        .expect("explain misspelled code");
    assert_eq!(typo.status.code(), Some(2), "{typo:?}");
    let stderr = String::from_utf8_lossy(&typo.stderr);
    assert!(
        stderr.contains("unknown diagnostic code 'T01B'"),
        "{stderr}"
    );
    // T01B is equally close to several T01x codes: guessing one would be
    // misleading, so no suggestion is offered.
    assert!(!stderr.contains("Did you mean"), "{stderr}");

    let unique = common::cli_command()
        .args(["explain", "T033XX"])
        .output()
        .expect("explain unique typo");
    assert_eq!(unique.status.code(), Some(2), "{unique:?}");
    assert!(
        String::from_utf8_lossy(&unique.stderr).contains("Did you mean 'T033'?"),
        "{}",
        String::from_utf8_lossy(&unique.stderr)
    );

    let missing = common::cli_command()
        .arg("explain")
        .output()
        .expect("explain without code");
    assert_eq!(missing.status.code(), Some(2), "{missing:?}");

    let ice = common::cli_command()
        .args(["explain", "ICE-T-001"])
        .output()
        .expect("explain ICE code");
    assert_eq!(ice.status.code(), Some(2), "{ice:?}");
    assert!(
        String::from_utf8_lossy(&ice.stderr).contains("no long-form explanation"),
        "{}",
        String::from_utf8_lossy(&ice.stderr)
    );
}

#[test]
fn non_host_jit_layout_error_suggests_an_accepted_value() {
    let root = common::temp_dir("arandu-cli-layout-hint").expect("create temp root");
    let source = root.join("main.aru");
    fs::write(&source, "func main(): int { return 0 }\n").expect("write source");
    let path = source.to_string_lossy().into_owned();
    let non_host = if cfg!(target_pointer_width = "64") {
        "--layout=ptr4"
    } else {
        "--layout=ptr8"
    };

    let output = common::cli_command()
        .args(["run", &path, non_host])
        .output()
        .expect("run with non-host layout");

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--layout=host"), "{stderr}");
    assert!(!stderr.contains("--layout=by-host"), "{stderr}");
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
        for flag in [
            "--help",
            "--version",
            "--stdlib-path",
            "--cache-dir",
            "--color",
            "--explain",
            "-Werror",
            "--watch",
        ] {
            assert!(completions.contains(flag), "{shell} omitted {flag}");
        }
        let extra: &[&str] = match shell {
            "bash" => &["--layout", "--verbose"],
            "zsh" => &["--ctfe-fuel", "-Zdebug-typeck"],
            "fish" => &["--quiet", "--no-generational-fallback"],
            "powershell" => &["--release", "-V"],
            _ => &[],
        };
        for flag in extra {
            assert!(completions.contains(flag), "{shell} omitted {flag}");
        }
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
fn bench_help_documents_policy_flags() {
    let output = common::cli_command()
        .args(["bench", "--help"])
        .output()
        .expect("run bench help");
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8_lossy(&output.stdout);
    for flag in [
        "--timeout",
        "--output",
        "--save-baseline",
        "--baseline",
        "--compare",
        "--dry-run",
        "--max-regression",
        "--noise-threshold",
    ] {
        assert!(help.contains(flag), "bench help omitted {flag}: {help}");
    }
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

#[test]
fn warnings_as_errors_promote_w006_without_overriding_suppress() {
    let root = common::temp_dir("arandu-cli-werror").expect("create temp root");
    let source = root.join("main.aru");
    fs::write(
        &source,
        r#"from std.core.result import { Result }
func risky(): Result<int, int> { return Result.Ok(1) }
func plain() {
    let r = risky()
}
func main(): int { return 0 }
"#,
    )
    .expect("write warning source");
    let path = source.to_string_lossy().into_owned();

    let warning = common::cli_command()
        .args(["check", &path])
        .output()
        .expect("check warning source");
    assert!(warning.status.success(), "{warning:?}");
    assert!(
        String::from_utf8_lossy(&warning.stderr).contains("W006"),
        "{}",
        String::from_utf8_lossy(&warning.stderr)
    );

    for alias in ["-Werror", "--warnings-as-errors"] {
        let promoted = common::cli_command()
            .args(["check", &path, alias])
            .output()
            .expect("check with warnings as errors");
        assert_eq!(promoted.status.code(), Some(1), "{alias}: {promoted:?}");
        let stderr = String::from_utf8_lossy(&promoted.stderr).to_ascii_lowercase();
        assert!(stderr.contains("w006"), "{alias}: {stderr}");
        assert!(stderr.contains("error"), "{alias}: {stderr}");
        assert!(
            !String::from_utf8_lossy(&promoted.stdout).contains("ok"),
            "{alias}: {}",
            String::from_utf8_lossy(&promoted.stdout)
        );
    }

    let suppressed = root.join("suppressed.aru");
    fs::write(
        &suppressed,
        r#"from std.core.result import { Result }
func risky(): Result<int, int> { return Result.Ok(1) }
@Suppress("W006")
func quiet() {
    let r = risky()
}
func main(): int { return 0 }
"#,
    )
    .expect("write suppressed source");
    let suppressed_path = suppressed.to_string_lossy().into_owned();
    let suppressed = common::cli_command()
        .args(["check", &suppressed_path, "-Werror"])
        .output()
        .expect("check suppressed source");
    assert!(suppressed.status.success(), "{suppressed:?}");
    assert!(
        !String::from_utf8_lossy(&suppressed.stderr).contains("W006"),
        "{}",
        String::from_utf8_lossy(&suppressed.stderr)
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn help_command_prints_global_and_command_help() {
    let global = common::cli_command()
        .arg("help")
        .output()
        .expect("run help command");
    assert!(global.status.success(), "{global:?}");
    assert!(
        String::from_utf8_lossy(&global.stdout).contains("Commands:"),
        "{}",
        String::from_utf8_lossy(&global.stdout)
    );

    let check = common::cli_command()
        .args(["help", "check"])
        .output()
        .expect("run help for check");
    assert!(check.status.success(), "{check:?}");
    assert!(
        String::from_utf8_lossy(&check.stdout).contains("--watch"),
        "{}",
        String::from_utf8_lossy(&check.stdout)
    );

    let typo = common::cli_command()
        .args(["help", "tset"])
        .output()
        .expect("run help for a misspelled command");
    assert_eq!(typo.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&typo.stderr).contains("Did you mean 'test'?"));
}

#[test]
fn unknown_options_name_the_flag_and_suggest_a_fix() {
    let global = common::cli_command()
        .arg("--whatever")
        .output()
        .expect("run unknown global option");
    assert_eq!(global.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&global.stderr);
    assert!(stderr.contains("unknown option '--whatever'"), "{stderr}");
    assert!(stderr.contains("arandu --help"), "{stderr}");

    let typo = common::cli_command()
        .args(["check", "--layot", "missing.aru"])
        .output()
        .expect("run misspelled check option");
    assert_eq!(typo.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&typo.stderr);
    assert!(
        stderr.contains("unknown option '--layot' for 'check'"),
        "{stderr}"
    );
    assert!(stderr.contains("Did you mean '--layout'?"), "{stderr}");

    let test_typo = common::cli_command()
        .args(["test", "--fail-fas"])
        .output()
        .expect("run misspelled test option");
    assert_eq!(test_typo.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&test_typo.stderr);
    assert!(
        stderr.contains("unknown option '--fail-fas' for 'test'"),
        "{stderr}"
    );
    assert!(stderr.contains("Did you mean '--fail-fast'?"), "{stderr}");
}
