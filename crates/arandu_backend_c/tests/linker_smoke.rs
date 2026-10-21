//! Canonical link smoke for one host toolchain.
//!
//! The C compiler and the library list both come from outside the test body:
//! `CC` selects the driver, and [`arandu_middle::TargetSpec`] selects `-lm`,
//! pthread, and the Windows system libraries. A wrong flag fails here, before
//! the rest of the backend suite.

use std::fs;
use std::process::Command;

fn compiler() -> Option<String> {
    if let Some(cc) = std::env::var_os("CC") {
        let cc = cc.to_string_lossy().into_owned();
        let available = Command::new(&cc)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success());
        assert!(available, "CC={cc} was set but is not a working C compiler");
        return Some(cc);
    }
    for name in ["cc", "clang", "gcc"] {
        if Command::new(name)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return Some(name.to_string());
        }
    }
    None
}

#[test]
fn platform_calls_stay_behind_the_pal() {
    let fs_rs = include_str!("../src/emitter/runtime/fs.rs");
    let env_rs = include_str!("../src/emitter/runtime/env.rs");
    let native = include_str!("../src/emitter/runtime/native_thread.c");
    let task = include_str!("../src/emitter/runtime/task.c");
    let pal = include_str!("../src/emitter/runtime/pal.c");
    let pal_net = include_str!("../src/emitter/runtime/pal_net.c");

    assert!(fs_rs.contains("ar_pal_readdir_open"));
    assert!(!fs_rs.contains("opendir("));
    assert!(!fs_rs.contains("FindFirstFile"));
    assert!(!fs_rs.contains("socket("));
    assert!(env_rs.contains("ar_init_env_args_if_needed"));
    assert!(env_rs.contains("ar_pal_ensure_process_args"));
    assert!(env_rs.contains("ar_pal_monotonic_ns"));
    assert!(!env_rs.contains("__argc"));
    assert!(!env_rs.contains("QueryPerformanceCounter"));
    assert!(native.contains("ar_pal_thread_spawn"));
    assert!(!native.contains("CreateThread"));
    assert!(!native.contains("pthread_create"));
    assert!(task.contains("ar_pal_sleep_ms"));
    assert!(task.contains("ar_pal_yield"));
    assert!(pal.contains("FindFirstFileW"));
    assert!(pal.contains("opendir"));
    assert!(pal_net.contains("socket"));
}

#[test]
fn links_math_clock_and_directory_listing() {
    let Some(cc) = compiler() else {
        return;
    };
    let directory = std::env::temp_dir().join(format!(
        "arandu-linker-smoke-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&directory).expect("create smoke directory");
    fs::write(directory.join("marker.txt"), b"ok").expect("write marker");
    let source_path = directory.join("smoke.c");
    let executable = directory.join(format!("smoke{}", std::env::consts::EXE_SUFFIX));
    let source = format!(
        "{}\n{}\n{}",
        r#"
#if !defined(_WIN32) && !defined(_DEFAULT_SOURCE)
#define _DEFAULT_SOURCE
#endif
#if defined(__APPLE__) && !defined(_DARWIN_C_SOURCE)
#define _DARWIN_C_SOURCE
#endif
#if !defined(_WIN32) && !defined(_POSIX_C_SOURCE)
#define _POSIX_C_SOURCE 200112L
#endif
#include <math.h>
#include <string.h>
"#,
        include_str!("../src/emitter/runtime/pal.c"),
        r#"
int main(int argc, char **argv) {
    if (argc < 2) return 2;
    if (sqrt(4.0) != 2.0) return 3;
    if (ar_pal_thread_id() == 0) return 4;
    if (ar_pal_monotonic_ns() <= 0) return 5;
    int err = 0;
    ArPalDir *dir = ar_pal_readdir_open(argv[1], &err);
    if (!dir) return 6;
    int found = 0;
    for (;;) {
        ArPalDirEnt ent;
        memset(&ent, 0, sizeof(ent));
        int next = ar_pal_readdir_next(dir, &ent, &err);
        if (next == 0) break;
        if (next < 0) {
            ar_pal_readdir_close(dir);
            return 7;
        }
        if (ent.name && strcmp(ent.name, "marker.txt") == 0) found = 1;
        free(ent.name);
    }
    ar_pal_readdir_close(dir);
    return found ? 0 : 8;
}
"#
    );
    fs::write(&source_path, source).expect("write smoke source");

    let spec = arandu_middle::TargetSpec::for_host();
    let mut compile = Command::new(&cc);
    compile
        .arg("-O0")
        .arg(&source_path)
        .arg("-o")
        .arg(&executable)
        .args(spec.cc_link_args());
    if std::env::var_os("ARANDU_SMOKE_LLD").is_some_and(|value| value == "1") {
        compile.arg(spec.linker_plan().cc_fuse_lld_flag());
    }
    let compiled = compile.output().expect("start C compiler");
    assert!(
        compiled.status.success(),
        "CC={cc} failed to link the PAL smoke\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let executed = Command::new(&executable)
        .arg(&directory)
        .output()
        .expect("run PAL smoke");
    let _ = fs::remove_dir_all(&directory);
    assert_eq!(
        executed.status.code(),
        Some(0),
        "PAL smoke exited {:?}\nstdout:\n{}\nstderr:\n{}",
        executed.status.code(),
        String::from_utf8_lossy(&executed.stdout),
        String::from_utf8_lossy(&executed.stderr)
    );
}
