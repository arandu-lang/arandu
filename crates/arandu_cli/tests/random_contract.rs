//! Deterministic PRNG vectors and failure injection for host entropy.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;
use std::{fs, process::Command};

#[test]
fn random_vectors_and_os_entropy_match_in_jit_and_c() {
    let root = common::temp_dir("arandu-random-contract").unwrap();
    for (name, source) in [
        (
            "vectors",
            include_str!("../../../tests/corpus/stdlib_random.aru"),
        ),
        (
            "entropy",
            "import std.random as random\nimport std.alloc.vec as vec\npublic func main(): int { let mut buffer = vec.Vec<u8>.new(); buffer.push(0 as u8); buffer.push(0 as u8); buffer.push(0 as u8); let bytes = buffer.asMutSlice(); if !random.fillEntropy(bytes) { return 1 }; match random.Xoshiro256PlusPlus.tryFromEntropy() { Result.Ok(_) => { return 0 } Result.Err(_) => { return 2 } }; return 3 }",
        ),
    ] {
        let path = root.join(format!("{name}.aru"));
        fs::write(&path, source).unwrap();
        for optimized in [false, true] {
            let mut command = common::cli_command();
            command.arg("run").arg(&path);
            if optimized {
                command.arg("--opt");
            }
            let run = command.output().unwrap();
            assert!(
                run.status.success(),
                "JIT {name}: {:?}\n{}",
                run.status.code(),
                String::from_utf8_lossy(&run.stderr)
            );
        }
        let emitted = common::cli_command()
            .arg("emit-c")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            emitted.status.success(),
            "{}",
            String::from_utf8_lossy(&emitted.stderr)
        );
        if Command::new("clang")
            .arg("--version")
            .output()
            .is_ok_and(|v| v.status.success())
        {
            let c = root.join(format!("{name}.c"));
            fs::write(&c, emitted.stdout).unwrap();
            for optimization in ["-O0", "-O2"] {
                compile_and_run(
                    &c,
                    &root.join(if cfg!(windows) {
                        "random.exe"
                    } else {
                        "random"
                    }),
                    optimization,
                    false,
                );
            }
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn float_range_stays_finite_for_opposite_large_bounds() {
    let root = common::temp_dir("arandu-random-float-range").unwrap();
    let path = root.join("wide_range.aru");
    fs::write(
        &path,
        r#"import std.random as random

public func main(): int {
    let mut rng = random.Xoshiro256PlusPlus.fromSeed(91 as u64)
    let mut i: usize = 0
    while i < 512 as usize {
        let crossed = rng.genRangeFloat(-1e308, 1e308)
        if !(crossed >= -1e308 && crossed < 1e308) { return 1 }
        let negative = rng.genRangeFloat(-1.7e308, -1e308)
        if !(negative >= -1.7e308 && negative < -1e308) { return 2 }
        let positive = rng.genRangeFloat(1e308, 1.7e308)
        if !(positive >= 1e308 && positive < 1.7e308) { return 3 }
        i = i + 1
    }
    return 0
}
"#,
    )
    .unwrap();

    for optimized in [false, true] {
        let mut command = common::cli_command();
        command.arg("run").arg(&path);
        if optimized {
            command.arg("--opt");
        }
        let run = command.output().unwrap();
        assert!(
            run.status.success(),
            "JIT float range (optimized={optimized}): {:?}\n{}",
            run.status.code(),
            String::from_utf8_lossy(&run.stderr)
        );
    }

    let emitted = common::cli_command()
        .arg("emit-c")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        emitted.status.success(),
        "{}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    if Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        let c = root.join("wide_range.c");
        fs::write(&c, emitted.stdout).unwrap();
        compile_and_run(&c, &root.join("wide_range"), "-O0", false);
        compile_and_run(&c, &root.join("wide_range_opt"), "-O2", false);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn entropy_source_failure_is_reported_in_generated_c() {
    let root = common::temp_dir("arandu-entropy-failure").unwrap();
    let path = root.join("failure.aru");
    fs::write(&path, "import std.random as random\nimport std.alloc.vec as vec\npublic func main(): int { let mut buffer = vec.Vec<u8>.new(); buffer.push(127 as u8); buffer.push(127 as u8); buffer.push(127 as u8); if random.fillEntropy(buffer.asMutSlice()) { return 1 }; let bytes = buffer.asSlice(); if bytes[0] != (0 as u8) || bytes[1] != (0 as u8) || bytes[2] != (0 as u8) { return 2 }; match random.Xoshiro256PlusPlus.tryFromEntropy() { Result.Ok(_) => { return 3 } Result.Err(_) => { return 0 } }; return 4 }").unwrap();
    let emitted = common::cli_command()
        .arg("emit-c")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        emitted.status.success(),
        "{}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    if Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|v| v.status.success())
    {
        let mut c = String::from_utf8(emitted.stdout).unwrap();
        // Linker interposition denies the actual OS source without changing
        // the emitted failure path or substituting an entropy implementation.
        // getrandom reports "unavailable" so the device fallback is exercised
        // and denied too.
        c.push_str("\nFILE *__wrap_fopen(const char *path, const char *mode) { (void)path; (void)mode; return NULL; }\n");
        c.push_str("ssize_t __wrap_getrandom(void *buf, size_t len, unsigned flags) { (void)buf; (void)len; (void)flags; errno = ENOSYS; return -1; }\n");
        let cpath = root.join("failure.c");
        fs::write(&cpath, c).unwrap();
        for optimization in ["-O0", "-O2"] {
            compile_and_run(&cpath, &root.join("failure"), optimization, true);
        }
    }
    fs::remove_dir_all(root).unwrap();
}

fn compile_and_run(
    source: &std::path::Path,
    output: &std::path::Path,
    optimization: &str,
    deny_entropy: bool,
) {
    let mut command = Command::new("clang");
    command.args([optimization, "-fwrapv"]);
    if cfg!(unix) {
        command.args(["-fsanitize=undefined", "-fno-sanitize-recover=all"]);
    }
    if deny_entropy {
        command.args(["-Wl,--wrap=fopen", "-Wl,--wrap=getrandom"]);
    }
    command
        .arg(source)
        .arg("-o")
        .arg(output)
        .args(arandu_middle::TargetSpec::for_host().cc_link_args());
    let compile = command.output().unwrap();
    assert!(
        compile.status.success(),
        "clang: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(output).output().unwrap();
    assert!(
        run.status.success(),
        "generated C exit={:?}: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}
