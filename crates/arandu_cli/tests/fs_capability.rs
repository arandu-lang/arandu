//! Kernel confinement is exercised through both public native backends.
#![cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;
use std::{fs, os::unix::fs::symlink, process::Command};

#[test]
#[cfg(target_os = "linux")]
fn dir_capability_confines_open_create_remove_and_subdirectories_in_jit_and_c() {
    let base = common::temp_dir("arandu-capability-contract").unwrap();
    let root = base.join("root");
    let outside = base.join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(root.join("inside"), "inside").unwrap();
    fs::write(outside.join("victim"), "outside").unwrap();
    symlink(&outside, root.join("escape")).unwrap();
    symlink(outside.join("victim"), root.join("link")).unwrap();
    let template = r#"import std.fs as fs
import std.core.intrinsics as intrinsics
import std.core.slice as slice
func rejects(dir: ref fs.Dir, path: str): bool {
    match dir.openFile(path, fs.OpenOptions.writeOnly()) {
        Result.Ok(_) => { return false }
        Result.Err(_) => { return true }
    }
}
func main(): int {
    match fs.openDir(ROOT) {
        Result.Ok(directory) => {
            let mut dir = directory
            if !dir.fileExists("inside") { return 1 }
            let invalid = fs.OpenOptions { read: true, write: false, create: false, truncate: true, append: false }
            match dir.openFile("inside", invalid) { Result.Ok(_) => { return 19 } Result.Err(_) => {} }
            let invalidCreate = fs.OpenOptions { read: true, write: false, create: true, truncate: false, append: false }
            match dir.openFile("missing", invalidCreate) { Result.Ok(_) => { return 20 } Result.Err(_) => {} }
            if !rejects(ref dir, "../outside/victim") { return 2 }
            if !rejects(ref dir, ABSOLUTE) { return 3 }
            if !rejects(ref dir, "escape/victim") { return 4 }
            if !rejects(ref dir, "link") { return 5 }
            match dir.openDir("inside") { Result.Ok(_) => { return 6 } Result.Err(_) => {} }
            match dir.createDir("../outside/pwn") { Result.Ok(_) => { return 7 } Result.Err(_) => {} }
            match dir.removeFile("../outside/victim") { Result.Ok(_) => { return 8 } Result.Err(_) => {} }
                match dir.createDir("made") {
                    Result.Ok(child) => {
                        let mut c = child
                        match c.openFile("file", fs.OpenOptions.writeOnly()) {
                            Result.Ok(f) => {
                                let mut file = f
                                let msg = "hello"
                                let b = intrinsics.strBytes(msg)
                                let written = file.write(slice.asPtr<u8>(b), slice.len<u8>(b))
                                file.close()
                                if written != 5 { return 9 }
                            }
                            Result.Err(_) => { return 9 }
                        }
                        match c.openFile("file", fs.OpenOptions.readOnly()) {
                            Result.Ok(f) => {
                                let mut file = f
                                let mut buf: [16]u8 = [0 as u8; 16]
                                let p = intrinsics.addressOf<u8>(mut ref buf[0])
                                let n = file.read(p, 16 as usize)
                                file.close()
                                if n != 5 { return 16 }
                                if buf[0] != (104 as u8) || buf[4] != (111 as u8) { return 17 }
                            }
                            Result.Err(_) => { return 18 }
                        }
                    }
                    Result.Err(_) => { return 10 }
                }
            if !dir.fileExists("made/file") { return 11 }
            match dir.removeFile("made/file") { Result.Ok(_) => {} Result.Err(_) => { return 12 } }
            match dir.removeDir("made") { Result.Ok(_) => {} Result.Err(_) => { return 13 } }
            dir.close()
            if dir.fileExists("inside") { return 14 }
        }
        Result.Err(_) => { return 15 }
    }
    return 0
}
"#;
    let source = template
        .replace("ROOT", &format!("{:?}", root.to_str().unwrap()))
        .replace(
            "ABSOLUTE",
            &format!("{:?}", outside.join("victim").to_str().unwrap()),
        );
    let path = base.join("capability.aru");
    fs::write(&path, source).unwrap();
    let output = common::cli_command()
        .arg("run")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "JIT {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(outside.join("victim")).unwrap(),
        "outside"
    );
    assert!(!outside.join("pwn").exists());
    let output = common::cli_command()
        .arg("emit-c")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let c_path = base.join("capability.c");
    fs::write(&c_path, output.stdout).unwrap();
    if Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        let executable = base.join("capability");
        let output = Command::new("clang")
            .args(["-O2", "-fsanitize=undefined", "-fno-sanitize-recover=all"])
            .arg(&c_path)
            .arg("-o")
            .arg(&executable)
            .args(arandu_middle::TargetSpec::for_host().cc_link_args())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = Command::new(executable).output().unwrap();
        assert!(
            output.status.success(),
            "C {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        fs::read_to_string(outside.join("victim")).unwrap(),
        "outside"
    );
    assert!(!outside.join("pwn").exists());
    assert_eq!(fs::read_to_string(root.join("inside")).unwrap(), "inside");
    assert!(!root.join("missing").exists());
    fs::write(
        &path,
        "import std.fs as fs\nfunc main(): int { let forged = fs.Dir { fd: 0 }; return 0 }",
    )
    .unwrap();
    let output = common::cli_command()
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("T041"));
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn dir_capability_derives_child_explicitly_in_jit_and_c() {
    let base = common::temp_dir("arandu-capability-component").unwrap();
    let root = base.join("root");
    let outside = base.join("outside");
    fs::create_dir_all(root.join("child")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(root.join("child/inside"), "inside").unwrap();
    fs::write(outside.join("victim"), "outside").unwrap();
    symlink(&outside, root.join("link")).unwrap();
    symlink(outside.join("victim"), root.join("child/file-link")).unwrap();
    let source = r#"import std.fs as fs
func main(): int {
    match fs.openDir(ROOT) {
        Result.Ok(root) => {
            match root.openDir("link") { Result.Ok(_) => { return 1 } Result.Err(_) => {} }
            match root.openDir("child") {
                Result.Ok(child) => {
                    if !child.fileExists("inside") { return 2 }
                    if child.fileExists("../outside/victim") { return 3 }
                    match child.openFile("file-link", fs.OpenOptions.writeOnly()) { Result.Ok(_) => { return 4 } Result.Err(_) => {} }
                    match child.createDir("created") {
                        Result.Ok(created) => {
                            match created.openFile("file", fs.OpenOptions.writeOnly()) { Result.Ok(_) => {} Result.Err(_) => { return 5 } }
                            match created.removeFile("file") { Result.Ok(_) => {} Result.Err(_) => { return 6 } }
                        }
                        Result.Err(_) => { return 7 }
                    }
                    match child.removeDir("created") { Result.Ok(_) => {} Result.Err(_) => { return 8 } }
                }
                Result.Err(_) => { return 9 }
            }
        }
        Result.Err(_) => { return 10 }
    }
    return 0
}
"#.replace("ROOT", &format!("{:?}", root.to_str().unwrap()));
    let path = base.join("component.aru");
    fs::write(&path, source).unwrap();
    let output = common::cli_command()
        .arg("run")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "JIT {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let output = common::cli_command()
        .arg("emit-c")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let c_path = base.join("component.c");
    fs::write(&c_path, output.stdout).unwrap();
    if Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        let executable = base.join("component");
        let output = Command::new("clang")
            .args(["-O2", "-fsanitize=undefined", "-fno-sanitize-recover=all"])
            .arg(&c_path)
            .arg("-o")
            .arg(&executable)
            .args(arandu_middle::TargetSpec::for_host().cc_link_args())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = Command::new(&executable).output().unwrap();
        assert!(
            output.status.success(),
            "C {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        fs::read_to_string(outside.join("victim")).unwrap(),
        "outside"
    );
    fs::remove_dir_all(base).unwrap();
}
