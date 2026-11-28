//! Arandu Minimal 0.1 gold suite — `examples/minimal/*`.
//!
//! Tracking: docs/arandu-compiler-roadmap-v0.1.md (Minimal 0.1 campaign).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .current_dir(workspace_root())
        .args(args)
        .output()
        .expect("cli")
}

#[test]
fn process_exit_preserves_host_status_width() {
    for code in [-2, 300] {
        let path = std::env::temp_dir().join(format!(
            "arandu_process_exit_{}_{}.aru",
            std::process::id(),
            code
        ));
        std::fs::write(
            &path,
            format!(
                "module tests.process_exit\nimport std.process as process\nfunc main(): int {{ process.exit({code}); return 0 }}\n"
            ),
        )
        .expect("write process-exit fixture");
        let output = run_cli(&["run", path.to_str().expect("UTF-8 temp path")]);

        #[cfg(unix)]
        let expected = Some(code & 0xff);
        #[cfg(windows)]
        let expected = Some(code);
        #[cfg(not(any(unix, windows)))]
        let expected = Some(code);

        assert_eq!(
            output.status.code(),
            expected,
            "process.exit({code}) returned stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        if Command::new("gcc")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            let c_source = run_cli(&["emit-c", path.to_str().expect("UTF-8 temp path")]);
            assert!(
                c_source.status.success(),
                "C emission failed: {}",
                String::from_utf8_lossy(&c_source.stderr)
            );
            let c_file = path.with_extension("c");
            let binary = path.with_extension(if cfg!(windows) { "exe" } else { "bin" });
            std::fs::write(&c_file, &c_source.stdout).expect("write emitted C");
            let compile = Command::new("gcc")
                .arg(&c_file)
                .arg("-o")
                .arg(&binary)
                .args(arandu_middle::TargetSpec::for_host().cc_link_args())
                .output()
                .expect("compile emitted C");
            assert!(
                compile.status.success(),
                "C compilation failed: {}",
                String::from_utf8_lossy(&compile.stderr)
            );
            let c_run = Command::new(&binary).output().expect("run emitted C");
            let _ = std::fs::remove_file(c_file);
            let _ = std::fs::remove_file(binary);
            assert_eq!(
                c_run.status.code(),
                expected,
                "C process.exit({code}) stderr: {}",
                String::from_utf8_lossy(&c_run.stderr)
            );
        }
        let _ = std::fs::remove_file(path);
    }
}

struct Gold {
    path: &'static str,
    /// Expected process exit code from `run` (main return value).
    exit: i32,
}

const GOLD: &[Gold] = &[
    Gold {
        path: "examples/minimal/m01_hello.aru",
        exit: 0,
    },
    Gold {
        path: "examples/minimal/m02_structs_enums.aru",
        exit: 3,
    },
    Gold {
        path: "examples/minimal/m03_result_option.aru",
        exit: 7,
    },
    Gold {
        path: "examples/minimal/m04_generics_bounds.aru",
        exit: 10,
    },
    Gold {
        path: "examples/minimal/m05_borrow_shared.aru",
        exit: 5,
    },
    Gold {
        path: "examples/minimal/m06_async_await.aru",
        exit: 42,
    },
    Gold {
        path: "examples/minimal/m07_async_spawn_join.aru",
        exit: 42,
    },
    Gold {
        path: "examples/minimal/m08_modules/main.aru",
        exit: 9,
    },
    Gold {
        path: "examples/minimal/m09_interp_tostr.aru",
        exit: 0,
    },
    Gold {
        path: "examples/minimal/m10_path_empty.aru",
        exit: 0,
    },
    Gold {
        path: "examples/minimal/m11_process_exit.aru",
        exit: 17,
    },
    Gold {
        path: "examples/minimal/m12_time_env.aru",
        exit: 0,
    },
    Gold {
        path: "examples/minimal/m13_vec.aru",
        exit: 78,
    },
    Gold {
        path: "examples/minimal/m14_mem_intrinsics.aru",
        exit: 46,
    },
    Gold {
        path: "examples/minimal/m15_vec_capacity.aru",
        exit: 21,
    },
    Gold {
        path: "examples/minimal/m16_gen_arena.aru",
        exit: 83,
    },
    Gold {
        path: "examples/minimal/m17_pod_copy.aru",
        exit: 60,
    },
    Gold {
        path: "examples/minimal/m18_vec_methods.aru",
        exit: 78,
    },
    Gold {
        path: "examples/minimal/m19_allocator.aru",
        exit: 112,
    },
    Gold {
        path: "examples/minimal/m20_str.aru",
        exit: 0,
    },
    Gold {
        path: "examples/minimal/m21_result_custom_e.aru",
        exit: 7,
    },
    Gold {
        path: "examples/minimal/m22_iface_param.aru",
        exit: 42,
    },
    Gold {
        path: "examples/minimal/m23_match_result.aru",
        exit: 13,
    },
    Gold {
        path: "examples/minimal/m24_expect_or_abort.aru",
        exit: 13,
    },
    Gold {
        path: "examples/minimal/m25_fs_env.aru",
        exit: 0,
    },
    Gold {
        path: "examples/minimal/TEMPLATE_main.aru",
        exit: 0,
    },
];

#[test]
fn minimal_gold_check_and_run() {
    for g in GOLD {
        let path = if cfg!(windows) && g.path == "examples/minimal/m10_path_empty.aru" {
            "examples/minimal/m10_path_empty_windows.aru"
        } else {
            g.path
        };
        let check = run_cli(&["check", path]);
        assert!(
            check.status.success(),
            "check failed {}: {}",
            path,
            String::from_utf8_lossy(&check.stderr)
        );
        let run = run_cli(&["run", path]);
        assert_eq!(
            run.status.code(),
            Some(g.exit),
            "run exit mismatch {}: stderr={}",
            path,
            String::from_utf8_lossy(&run.stderr)
        );
    }
}

#[test]
fn pointer_module_checks() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_minimal_pointer.aru");
    std::fs::write(
        &file,
        r#"
module tests.minimal.pointer
import std.core.pointer as pointer

func main(): int {
    return 0
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .current_dir(&root)
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "pointer module: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn core_numeric_and_unicode_boundaries_run() {
    let (max, min) = ("2147483647", "-2147483647 - 1");
    let source = format!(
        r#"
module tests.stdlib.boundaries
import std.core.num as num
import std.core.char as chars

func main(): int {{
    let max: int = {max}
    let min: int = {min}
    match num.checkedAdd(max, 1) {{
        Some(_) => {{ return 6 }}
        None => {{}}
    }}
    match num.checkedSub(min, 1) {{
        Some(_) => {{ return 1 }}
        None => {{}}
    }}
    match num.checkedMul(min, -1) {{
        Some(_) => {{ return 2 }}
        None => {{}}
    }}
    if num.saturatingAdd(max, 1) != max {{ return 3 }}
    if num.saturatingSub(min, 1) != min {{ return 4 }}
    if chars.lenUtf8('é') != 2 as u32 {{ return 5 }}
    return 0
}}
"#
    );
    let file = std::env::temp_dir().join("arandu_stdlib_boundaries.aru");
    std::fs::write(&file, source).unwrap();

    let out = run_cli(&["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdlib boundaries: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
