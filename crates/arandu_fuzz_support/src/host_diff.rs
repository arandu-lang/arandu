//! Cranelift-vs-C differential for host string/path/ToStr helpers.
//!
//! The C backend emits standalone mirrors of the Rust host symbols in
//! `arandu_runtime` (`ar_path_join`, `ar_path_file_name`, `ar_str_split_last`,
//! `ar_str_concat`, ToStr formatters). Fuzz bytes choose literal operands from
//! an edge-case alphabet; every result is printed so the shared oracle
//! compares stdout and exit codes across optimisation levels and backends.

use crate::smith::oracle::check_source;
use crate::smith::writer::CodeWriter;

/// Path fragments that exercise separators, dot components, trailing
/// slashes, empty operands and multi-byte UTF-8.
const FRAGMENTS: &[&str] = &[
    "", "a", "/", "//", ".", "..", "a/", "/a", "a/b", "a/b/", "a/.", "a/..", "./", "../x", "é",
    "a/é", "x.y", "a//b", "/.",
];

const INTS: &[i64] = &[
    0,
    1,
    -1,
    9,
    10,
    i64::MAX,
    i64::MIN,
    i32::MAX as i64,
    i32::MIN as i64,
];

/// Values beyond the integer fast path are excluded: the C backend's `%.15g`
/// spelling intentionally differs from Rust `Display` there
/// (docs/arandu-jit-memory-v0.1.md).
const FLOATS: &[&str] = &[
    "0.0",
    "-0.0",
    "1.5",
    "-2.25",
    "0.5",
    "100.0",
    "999999999999999.0",
    "-999999999999999.0",
    "123456789.125",
];

pub(crate) fn run(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let source = synthesize(data);
    if let Err(failure) = check_source(&source, true, false) {
        if failure.shrinkable {
            panic!(
                "{}: {}\n--- reproducer ---\n{source}",
                failure.kind, failure.message
            );
        }
    }
}

/// Build a straight-line program from fuzz bytes (deterministic).
pub fn synthesize(data: &[u8]) -> String {
    let mut out = CodeWriter::new();
    out.writeln("import io");
    out.writeln("import std.path as path");
    out.newline();
    out.writeln("func main(): int {");
    out.indent();
    for (step, op) in data.chunks(3).take(24).enumerate() {
        let pick = |index: usize| usize::from(op.get(index).copied().unwrap_or(0));
        let a = FRAGMENTS[pick(1) % FRAGMENTS.len()];
        let b = FRAGMENTS[pick(2) % FRAGMENTS.len()];
        match pick(0) % 6 {
            0 => {
                emit_path(&mut out, step, a);
                emit_str_let(&mut out, &format!("t{step}"), b);
                out.writeln(&format!("let r{step}: str = p{step}.joinStr(t{step})"));
                emit_print(&mut out, step, &format!("r{step}"));
            }
            1 => {
                emit_path(&mut out, step, a);
                out.writeln(&format!("let n{step} = p{step}.fileName()"));
                out.writeln(&format!("let r{step}: str = n{step}.asStr()"));
                emit_print(&mut out, step, &format!("r{step}"));
            }
            2 => {
                emit_path(&mut out, step, a);
                emit_str_let(&mut out, &format!("t{step}"), b);
                out.writeln(&format!("let o{step} = p{step}.joinOwned(t{step})"));
                out.writeln(&format!("let r{step}: str = o{step}.asStr()"));
                emit_print(&mut out, step, &format!("r{step}"));
            }
            3 => {
                let value = INTS[pick(1) % INTS.len()];
                // `i64::MIN` has no positive literal; spell it as an expression.
                let literal = if value == i64::MIN {
                    "(-9223372036854775807 - 1)".to_owned()
                } else {
                    value.to_string()
                };
                out.writeln(&format!("let v{step}: int = {literal}"));
                emit_print(&mut out, step, &format!("v{step}"));
            }
            4 => {
                let value = FLOATS[pick(1) % FLOATS.len()];
                out.writeln(&format!("let f{step}: float = {value}"));
                emit_print(&mut out, step, &format!("f{step}"));
            }
            _ => {
                emit_str_let(&mut out, &format!("s{step}"), a);
                emit_str_let(&mut out, &format!("t{step}"), b);
                out.writeln(&format!(
                    "io.println(\"[{step}]${{s{step}}}${{t{step}}}|\")"
                ));
            }
        }
    }
    out.writeln("return 0");
    out.dedent();
    out.writeln("}");
    out.finish()
}

fn emit_str_let(out: &mut CodeWriter, name: &str, value: &str) {
    out.write(&format!("let {name}: str = "));
    out.write_string_literal(value);
    out.newline();
}

fn emit_path(out: &mut CodeWriter, step: usize, value: &str) {
    out.write(&format!("let p{step} = path.Path.from("));
    out.write_string_literal(value);
    out.write(")");
    out.newline();
}

fn emit_print(out: &mut CodeWriter, step: usize, name: &str) {
    out.writeln(&format!("io.println(\"[{step}]${{{name}}}|\")"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_agrees(source: &str) {
        if let Err(failure) = check_source(source, true, false) {
            panic!("{}: {}\n{source}", failure.kind, failure.message);
        }
    }

    #[test]
    fn synthesized_program_is_well_formed() {
        let source = synthesize(&[0, 1, 2, 1, 3, 0, 2, 5, 0, 3, 4, 0, 4, 14, 1, 5, 8, 9]);
        assert!(source.contains("joinStr"));
        assert!(source.contains("fileName"));
        assert!(source.contains("joinOwned"));
        assert_agrees(&source);
    }

    #[test]
    fn path_join_with_empty_segment_matches_host() {
        assert_agrees(
            "import io\nimport std.path as path\nfunc main(): int {\n    let a: str = path.Path.from(\"a\").joinStr(\"\")\n    let b: str = path.Path.from(\"\").joinStr(\"\")\n    let c: str = path.Path.from(\"/\").joinStr(\"\")\n    io.println(\"${a}|${b}|${c}|\")\n    return 0\n}\n",
        );
    }

    #[test]
    fn path_file_name_handles_trailing_and_dot_components() {
        assert_agrees(
            "import io\nimport std.path as path\nfunc main(): int {\n    let an = path.Path.from(\"a/b/\").fileName()\n    let a: str = an.asStr()\n    let bn = path.Path.from(\"a/..\").fileName()\n    let b: str = bn.asStr()\n    let cn = path.Path.from(\"a/.\").fileName()\n    let c: str = cn.asStr()\n    let dn = path.Path.from(\".\").fileName()\n    let d: str = dn.asStr()\n    let en = path.Path.from(\"/\").fileName()\n    let e: str = en.asStr()\n    let gn = path.Path.from(\"a//b\").fileName()\n    let f: str = gn.asStr()\n    io.println(\"${a}|${b}|${c}|${d}|${e}|${f}|\")\n    return 0\n}\n",
        );
    }
}
