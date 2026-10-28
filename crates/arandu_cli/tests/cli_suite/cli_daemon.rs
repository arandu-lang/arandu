//! The build daemon (HOT.3) and its fail-closed client (HOT.5): warm builds
//! publish what a batch build publishes, program errors reach the client
//! rendered, and a dead, replaced or disabled daemon never fails a build.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use crate::common;

const MANIFEST: &str = "schema = 1\n\n[package]\nname = \"app\"\nversion = \"0.0.1\"\nedition = \"2026\"\n\n[targets.bin]\nname = \"app\"\nroot = \"src/main.aru\"\n";

fn program(value: i32) -> String {
    format!(
        "func helper(): int {{\n    return {value}\n}}\n\nfunc main(): int {{\n    return helper() + 1\n}}\n"
    )
}

struct Project {
    root: PathBuf,
    runtime: PathBuf,
}

impl Project {
    fn new(label: &str, source: &str) -> Self {
        let tmp = common::temp_dir(&format!("arandu_daemon_{label}")).unwrap();
        let root = tmp.join("app");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("arandu.toml"), MANIFEST).unwrap();
        fs::write(root.join("src/main.aru"), source).unwrap();
        // A private runtime directory keeps the sockets of this test apart.
        let runtime = tmp.join("run");
        fs::create_dir_all(&runtime).unwrap();
        fs::set_permissions(
            &runtime,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        Self { root, runtime }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = common::cli_command();
        command
            .args(args)
            .current_dir(&self.root)
            .env("ARANDU_DAEMON", "1")
            .env("ARANDU_DAEMON_IDLE_SECS", "60")
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env_remove("CI");
        command
    }

    fn build(&self) -> Output {
        self.command(&["build"]).output().expect("arandu build")
    }

    fn write(&self, source: &str) {
        fs::write(self.root.join("src/main.aru"), source).unwrap();
    }

    fn artifact(&self) -> PathBuf {
        let state = fs::read_dir(self.root.join("target/dev"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("build-state.json"))
            .find(|path| path.is_file())
            .expect("build state");
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
        state
            .parent()
            .unwrap()
            .join(value["artifact"].as_str().unwrap())
    }

    fn exit_code(&self) -> Option<i32> {
        Command::new(self.artifact()).status().unwrap().code()
    }

    /// Whether a daemon holds the project's lock.
    fn daemon_running(&self) -> bool {
        let lock = self.root.join(".arandu/daemon.lock");
        let Ok(file) = fs::OpenOptions::new().write(true).open(lock) else {
            return false;
        };
        file.try_lock().is_err()
    }

    fn stop(&self) {
        let _ = self.command(&["__daemon-stop"]).output();
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.daemon_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!self.daemon_running(), "the daemon stops when asked");
    }

    fn kill(&self) {
        let _ = Command::new("pkill")
            .args(["-KILL", "-f"])
            .arg(format!(
                "__daemon {}",
                self.root.canonicalize().unwrap().display()
            ))
            .status();
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.daemon_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = self.command(&["__daemon-stop"]).output();
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn batch_bytes(source: &str) -> Vec<u8> {
    let batch = Project::new("batch", source);
    let output = batch
        .command(&["build", "--no-daemon"])
        .output()
        .expect("batch build");
    assert_success(&output);
    assert!(!batch.daemon_running());
    fs::read(batch.artifact()).unwrap()
}

#[test]
fn a_daemon_serves_warm_builds_with_the_bytes_of_batch_builds() {
    let project = Project::new("warm", &program(10));
    assert_success(&project.build());
    assert!(
        project.daemon_running(),
        "the first build starts the daemon"
    );
    assert_eq!(project.exit_code(), Some(11));

    project.write(&program(41));
    let warm = project.command(&["build", "-v"]).output().unwrap();
    assert_success(&warm);
    assert!(String::from_utf8_lossy(&warm.stdout).contains("built app v0.0.1"));
    assert!(
        String::from_utf8_lossy(&warm.stderr).contains("[daemon] built by the project daemon"),
        "{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    assert_eq!(project.exit_code(), Some(42));
    assert_eq!(
        fs::read(project.artifact()).unwrap(),
        batch_bytes(&program(41))
    );
    project.stop();
}

#[test]
fn a_broken_program_is_reported_by_the_client_and_the_daemon_stays() {
    let project = Project::new("broken", &program(1));
    assert_success(&project.build());
    project.write(&program(1).replace("return 1\n", "return true\n"));
    let broken = project.build();
    assert_eq!(broken.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&broken.stderr);
    assert!(stderr.contains("T004"), "{stderr}");
    assert!(
        stderr.contains("main.aru"),
        "rendered against the source: {stderr}"
    );
    assert!(project.daemon_running());

    project.write(&program(6));
    assert_success(&project.build());
    assert_eq!(project.exit_code(), Some(7));
    project.stop();
}

#[test]
fn a_killed_daemon_never_fails_the_next_build() {
    let project = Project::new("killed", &program(2));
    assert_success(&project.build());
    project.kill();
    assert!(!project.daemon_running());

    project.write(&program(4));
    assert_success(&project.build());
    assert_eq!(project.exit_code(), Some(5));
    assert_eq!(
        fs::read(project.artifact()).unwrap(),
        batch_bytes(&program(4))
    );
    project.stop();
}

#[test]
fn a_daemon_of_another_environment_steps_aside_for_the_new_one() {
    let project = Project::new("replaced", &program(3));
    assert_success(&project.build());
    project.write(&program(8));
    let other = project
        .command(&["build"])
        .env("ARANDU_DAEMON_TEST_TAG", "other")
        .output()
        .expect("arandu build");
    assert_success(&other);
    assert_eq!(project.exit_code(), Some(9));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !project.daemon_running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        project.daemon_running(),
        "the new environment has its daemon"
    );
    let _ = project
        .command(&["__daemon-stop"])
        .env("ARANDU_DAEMON_TEST_TAG", "other")
        .output();
}

#[test]
fn ci_and_the_opt_outs_build_without_a_daemon() {
    let project = Project::new("batch-only", &program(5));
    let ci = project
        .command(&["build"])
        .env_remove("ARANDU_DAEMON")
        .env("CI", "true")
        .output()
        .unwrap();
    assert_success(&ci);
    assert!(!project.daemon_running());
    let disabled = project
        .command(&["build"])
        .env("ARANDU_DAEMON", "0")
        .output()
        .unwrap();
    assert_success(&disabled);
    assert!(!project.daemon_running());
    assert_success(&project.command(&["build", "--no-daemon"]).output().unwrap());
    assert!(!project.daemon_running());
    assert_eq!(project.exit_code(), Some(6));
}

/// A daemon that dies before its build or after publishing, before it
/// answers, leaves the client to build in batch; what is published is the
/// batch result, verified by digest rather than trusted (HOT.5).
#[test]
fn a_daemon_crash_around_a_build_falls_back_to_a_verified_batch_build() {
    for fault in ["before-build", "after-build"] {
        let project = Project::new(fault, &program(20));
        assert_success(&project.build());
        project.stop();
        project.write(&program(30));
        let crashed = project
            .command(&["build", "-v"])
            .env("ARANDU_DAEMON_FAULT", fault)
            .output()
            .unwrap();
        assert_success(&crashed);
        assert!(
            !String::from_utf8_lossy(&crashed.stderr).contains("[daemon] built by"),
            "{fault}: the client built in batch"
        );
        assert_eq!(project.exit_code(), Some(31), "{fault}");
        assert_eq!(
            fs::read(project.artifact()).unwrap(),
            batch_bytes(&program(30)),
            "{fault}"
        );
    }
}

/// Clients that build at once share the daemon: every one succeeds and the
/// published artifact is the batch build of the final source.
#[test]
fn concurrent_clients_all_get_the_newest_build() {
    let project = Project::new("concurrent", &program(1));
    assert_success(&project.build());
    project.write(&program(50));
    let clients = (0..3)
        .map(|_| {
            project
                .command(&["build"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    for client in clients {
        assert_success(&client.wait_with_output().unwrap());
    }
    assert!(project.daemon_running());
    assert_eq!(project.exit_code(), Some(51));
    assert_eq!(
        fs::read(project.artifact()).unwrap(),
        batch_bytes(&program(50))
    );
    project.stop();
}
