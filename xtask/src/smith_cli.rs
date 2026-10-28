//! Bounded differential checks through the shipped CLI, native linker and C tools.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use arandu_fuzz_support::smith::oracle::node_runner_script;
use arandu_fuzz_support::smith::process::{
    parse_backend_result, read_result_channel, run_with_timeout, BoundedOutput,
};
use arandu_fuzz_support::smith::synth::synthesize_staged_arrays_and_casts;
use serde_json::{json, Value};

const TIMEOUT: Duration = Duration::from_secs(120);

struct Config {
    arandu: PathBuf,
    evidence: PathBuf,
    seed: u64,
    iterations: u64,
    native_only: bool,
}

impl Config {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut arandu = None;
        let mut evidence = None;
        let mut seed: u64 = 0;
        let mut iterations: u64 = 1;
        let mut native_only = false;
        while let Some(option) = args.next() {
            if option == "--native-only" {
                native_only = true;
                continue;
            }
            let value = args
                .next()
                .ok_or_else(|| format!("{option} requires a value"))?;
            match option.as_str() {
                "--arandu" => arandu = Some(PathBuf::from(value)),
                "--evidence-dir" => evidence = Some(PathBuf::from(value)),
                "--seed" => seed = value.parse().map_err(|_| "invalid --seed")?,
                "--iterations" => iterations = value.parse().map_err(|_| "invalid --iterations")?,
                _ => return Err(format!("unknown option: {option}")),
            }
        }
        if !(1..=1024).contains(&iterations) {
            return Err("--iterations must be between 1 and 1024".into());
        }
        seed.checked_add(iterations - 1)
            .ok_or("seed range overflow")?;
        Ok(Self {
            arandu: arandu.ok_or("missing --arandu")?,
            evidence: evidence.ok_or("missing --evidence-dir")?,
            seed,
            iterations,
            native_only,
        })
    }
}

pub fn run(args: impl Iterator<Item = String>) -> i32 {
    match Config::parse(args).and_then(execute) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("smith-cli: {error}");
            1
        }
    }
}

struct Evidence {
    directory: PathBuf,
    report: Value,
}

impl Evidence {
    fn save(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&self.report).map_err(|error| error.to_string())?;
        write(&self.directory.join("report.json"), &bytes)
    }

    fn invoke(&mut self, label: &str, command: &mut Command) -> Result<BoundedOutput, String> {
        let invocation = json!({
            "label": label,
            "program": command.get_program().to_string_lossy(),
            "args": command.get_args().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
            "cwd": command.get_current_dir().map(|path| path.to_string_lossy()),
            "environment_overrides": command.get_envs().map(|(key, value)|
                (key.to_string_lossy().into_owned(), value.map(|v| v.to_string_lossy().into_owned()))
            ).collect::<std::collections::BTreeMap<_, _>>(),
            "timeout_seconds": TIMEOUT.as_secs(),
        });
        let result = run_with_timeout(command, TIMEOUT);
        let record = match &result {
            Ok(output) => {
                write(
                    &self.directory.join(format!("{label}.stdout")),
                    &output.stdout,
                )?;
                write(
                    &self.directory.join(format!("{label}.stderr")),
                    &output.stderr,
                )?;
                json!({"invocation": invocation, "status": output.status.to_string(),
                    "stdout_truncated": output.stdout_truncated,
                    "stderr_truncated": output.stderr_truncated})
            }
            Err(error) => json!({"invocation": invocation, "error": error}),
        };
        self.report["commands"]
            .as_array_mut()
            .ok_or("invalid report")?
            .push(record);
        self.save()?;
        let output = result.map_err(|error| format!("{label}: {error}"))?;
        if !output.status.success() || output.stdout_truncated || output.stderr_truncated {
            return Err(format!(
                "{label}: {} (see retained stdout/stderr)",
                output.status
            ));
        }
        Ok(output)
    }

    fn observe(&mut self, lane: &str, output: &BoundedOutput, expected: i32) -> Result<(), String> {
        check_observation(&output.stdout, &output.stderr, expected)
            .map_err(|error| format!("{lane}: {error}"))?;
        self.report["passed_lanes"]
            .as_array_mut()
            .ok_or("invalid report")?
            .push(json!(lane));
        self.save()
    }
}

fn check_observation(stdout: &[u8], stderr: &[u8], expected: i32) -> Result<(), String> {
    if stdout != format!("{expected}\n").as_bytes() || stderr != b"smith-cli\n" {
        return Err(format!("independent oracle mismatch: stdout={stdout:?}, stderr={stderr:?}, expected={expected}"));
    }
    Ok(())
}

fn execute(config: Config) -> Result<(), String> {
    let arandu =
        fs::canonicalize(&config.arandu).map_err(|error| format!("resolve CLI: {error}"))?;
    if let Some(parent) = config
        .evidence
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    // Never destroy or overwrite an earlier reproducer, even on a successful rerun.
    fs::create_dir(&config.evidence)
        .map_err(|error| format!("create fresh evidence directory: {error}"))?;
    let directory = fs::canonicalize(&config.evidence).map_err(|error| error.to_string())?;
    for offset in 0..config.iterations {
        let seed = config.seed + offset;
        let case = directory.join(format!("seed-{seed}"));
        fs::create_dir(&case).map_err(|error| error.to_string())?;
        let generated = synthesize_staged_arrays_and_casts(seed);
        // The independent value is printed in full. Returning zero avoids relying
        // on the platform's truncated process exit-code representation of i32.
        let source = format!("import io\n{}\nfunc main(): int {{\n    let value = smith_staged_case()\n    io.println(value.to_str())\n    io.eprint(\"smith-cli\\n\")\n    return 0\n}}\n", generated.source);
        write(&case.join("input.aru"), source.as_bytes())?;
        let mut evidence = Evidence {
            directory: case.clone(),
            report: json!({"schema": "arandu.smith-cli/v1", "seed": seed,
                "generator": "staged-arrays-and-casts",
                "source_blake3": blake3::hash(source.as_bytes()).to_hex().as_str(),
                "expected_result": generated.expected_result, "commands": [], "passed_lanes": [],
                "excluded_lanes": if config.native_only { vec!["CLI JIT Os", "C/GCC", "C/Clang", "core-Wasm/Node", "Wasmtime", "Component Model"] } else { vec!["CLI JIT Os", "Wasmtime", "Component Model"] }}),
        };
        let result = run_case(
            &arandu,
            &source,
            generated.expected_result,
            config.native_only,
            &mut evidence,
        );
        evidence.report["outcome"] = json!(if result.is_ok() { "passed" } else { "failed" });
        if let Err(error) = &result {
            evidence.report["error"] = json!(error);
        }
        evidence.save()?;
        result.map_err(|error| format!("seed {seed}: {error}; evidence={}", case.display()))?;
        println!("smith-cli: seed {seed} passed; evidence={}", case.display());
    }
    Ok(())
}

fn run_case(
    arandu: &Path,
    source: &str,
    expected: i32,
    native_only: bool,
    evidence: &mut Evidence,
) -> Result<(), String> {
    let case = evidence.directory.clone();
    let mut scaffold = Command::new(arandu);
    scaffold
        .current_dir(&case)
        .args(["new", "smith_cli", "--vcs=none", "--quiet"]);
    evidence.invoke("scaffold", &mut scaffold)?;
    let project = case.join("smith_cli");
    let source_path = project.join("src/main.aru");
    write(&source_path, source.as_bytes())?;
    let modes = [
        ("O0", None, "dev"),
        ("O2", Some("--release"), "release"),
        ("Os", Some("--size"), "size"),
    ];
    for (mode, flag, profile) in modes {
        // The CLI's --opt selects AMIR O1; --release selects O2 only for build.
        let ir_mode = if mode == "O2" { "O1" } else { mode };
        // `run` accepts --opt; --release/--size belong to native builds.
        if mode != "Os" {
            let mut jit = Command::new(arandu);
            jit.current_dir(&project)
                .args(["run", "src/main.aru", "--quiet"]);
            if mode == "O2" {
                jit.arg("--opt");
            }
            let label = format!("jit-{ir_mode}");
            let output = evidence.invoke(&label, &mut jit)?;
            evidence.observe(&label, &output, expected)?;
        }

        let mut build = Command::new(arandu);
        // One-shot evidence builds: never leave a build daemon behind.
        build
            .current_dir(&project)
            .args(["build", "--quiet"])
            .env("ARANDU_DAEMON", "0");
        if let Some(flag) = flag {
            build.arg(flag);
        }
        evidence.invoke(&format!("build-{mode}"), &mut build)?;
        let executable = native_executable(&project, profile)?;
        let mut native = Command::new(executable);
        native.current_dir(&project);
        let label = format!("native-{mode}");
        let output = evidence.invoke(&label, &mut native)?;
        evidence.observe(&label, &output, expected)?;

        if native_only {
            continue;
        }
        let emit_flag = match mode {
            "O2" => Some("--opt"),
            "Os" => Some("--size"),
            _ => None,
        };
        let mut emit_c = Command::new(arandu);
        emit_c
            .current_dir(&project)
            .args(["emit-c", "src/main.aru", "--quiet"]);
        if let Some(flag) = emit_flag {
            emit_c.arg(flag);
        }
        let emitted = evidence.invoke(&format!("emit-c-{ir_mode}"), &mut emit_c)?;
        let c_path = case.join(format!("source-{ir_mode}.c"));
        write(&c_path, &emitted.stdout)?;
        for compiler in ["gcc", "clang"] {
            let binary = case.join(format!("{compiler}-{mode}{}", std::env::consts::EXE_SUFFIX));
            let mut compile = Command::new(compiler);
            compile.args([
                "-fwrapv",
                if mode == "Os" {
                    "-Os"
                } else if mode == "O2" {
                    "-O2"
                } else {
                    "-O0"
                },
            ]);
            compile
                .arg(&c_path)
                .arg("-o")
                .arg(&binary)
                .args(arandu_middle::TargetSpec::for_host().cc_link_args());
            evidence.invoke(&format!("compile-{compiler}-{mode}"), &mut compile)?;
            let mut execute = Command::new(&binary);
            let label = format!("c-{compiler}-amir-{ir_mode}-host-{mode}");
            let output = evidence.invoke(&label, &mut execute)?;
            evidence.observe(&label, &output, expected)?;
        }
        let mut emit_wasm = Command::new(arandu);
        emit_wasm.current_dir(&project).args([
            "emit-wasm",
            "src/main.aru",
            "--layout",
            "ptr4",
            "--quiet",
        ]);
        if let Some(flag) = emit_flag {
            emit_wasm.arg(flag);
        }
        let emitted = evidence.invoke(&format!("emit-wasm-{ir_mode}"), &mut emit_wasm)?;
        let wasm_path = case.join(format!("source-{ir_mode}.wasm"));
        let result_path = case.join(format!("node-{ir_mode}.result"));
        write(&wasm_path, &emitted.stdout)?;
        let mut node =
            Command::new(std::env::var_os("ARANDU_NODE").unwrap_or_else(|| "node".into()));
        node.args(["--no-liftoff", "-e", node_runner_script()])
            .arg(&wasm_path)
            .env(
                "ARANDU_SMITH_PHASE_PATH",
                case.join(format!("node-{ir_mode}.phase")),
            )
            .env("ARANDU_SMITH_RESULT_PATH", &result_path);
        let label = format!("wasm-node-{ir_mode}");
        let output = evidence.invoke(&label, &mut node)?;
        evidence.observe(&label, &output, expected)?;
        let result =
            parse_backend_result(&read_result_channel(&result_path, "Node")?, false, "Node")?;
        if result != 0 {
            return Err(format!("{label}: main returned {result}, expected zero"));
        }
    }
    Ok(())
}

fn native_executable(project: &Path, profile: &str) -> Result<PathBuf, String> {
    let target = project.join("target").join(profile);
    let mut candidates = Vec::new();
    for entry in fs::read_dir(&target).map_err(|error| error.to_string())? {
        let profile_root = entry.map_err(|error| error.to_string())?.path();
        if !profile_root.is_dir() {
            continue;
        }
        let state_path = profile_root.join("build-state.json");
        let state: Value = serde_json::from_slice(
            &fs::read(&state_path)
                .map_err(|error| format!("read {}: {error}", state_path.display()))?,
        )
        .map_err(|error| error.to_string())?;
        let relative = Path::new(
            state["artifact"]
                .as_str()
                .ok_or("build state missing artifact")?,
        );
        if !relative
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(
                "build state artifact must remain relative to the profile directory".into(),
            );
        }
        let path = profile_root.join(relative);
        if path.is_file() {
            candidates.push(path);
        }
    }
    match candidates.as_slice() {
        [path] => Ok(path.clone()),
        _ => Err(format!(
            "expected one native artifact in {}, found {}",
            target.display(),
            candidates.len()
        )),
    }
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|error| format!("write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_matrix_preserves_required_environment_on_startup_failure() {
        let id = arandu_fuzz_support::smith::artifact::NEXT_TEMP_ID
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("arandu-cli-evidence-{}-{id}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let mut evidence = Evidence {
            directory: directory.clone(),
            report: json!({"commands": []}),
        };
        let mut command = Command::new(directory.join("missing-tool"));
        command.env("ARANDU_SMITH_RESULT_PATH", "explicit-result-channel");
        assert!(evidence.invoke("missing", &mut command).is_err());
        let saved: Value =
            serde_json::from_slice(&fs::read(directory.join("report.json")).unwrap()).unwrap();
        assert_eq!(
            saved["commands"][0]["invocation"]["environment_overrides"]["ARANDU_SMITH_RESULT_PATH"],
            "explicit-result-channel"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cli_matrix_rejects_wrapped_results_or_stderr_drift() {
        assert!(check_observation(b"-2040\n", b"smith-cli\n", -2040).is_ok());
        assert!(check_observation(b"8\n", b"smith-cli\n", -2040).is_err());
        assert!(check_observation(b"-2040\n", b"", -2040).is_err());
    }

    #[test]
    fn cli_matrix_bounds_campaign_and_requires_explicit_paths() {
        let args = |suffix: &[&str]| {
            ["--arandu", "cli", "--evidence-dir", "evidence"]
                .into_iter()
                .chain(suffix.iter().copied())
                .map(str::to_owned)
                .collect::<Vec<_>>()
                .into_iter()
        };
        assert!(Config::parse(args(&[
            "--seed",
            "18446744073709551615",
            "--iterations",
            "2"
        ]))
        .is_err());
        assert!(Config::parse(args(&["--iterations", "0"])).is_err());
        assert!(Config::parse(args(&["--native-only"])).unwrap().native_only);
        assert!(Config::parse(Vec::new().into_iter()).is_err());
    }
}
