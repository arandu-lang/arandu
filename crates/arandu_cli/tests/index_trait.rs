//! E2E and diagnostic regressions for `std.core.ops.Index` and `IndexMut` (Onda C).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;
use std::process::Output;

fn run_source(name: &str, source: &str) -> Output {
    let directory = common::temp_dir(&format!("arandu-index-{name}")).unwrap();
    let path = directory.join("main.aru");
    fs::write(&path, source).unwrap();
    let output = common::cli_command()
        .arg("run")
        .arg(&path)
        .output()
        .unwrap();
    fs::remove_dir_all(directory).unwrap();
    output
}

fn check_source(name: &str, source: &str) -> Output {
    let directory = common::temp_dir(&format!("arandu-index-check-{name}")).unwrap();
    let path = directory.join("main.aru");
    fs::write(&path, source).unwrap();
    let output = common::cli_command()
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    fs::remove_dir_all(directory).unwrap();
    output
}

#[test]
fn trait_indexing_works_e2e_across_containers_custom_keys_and_generics() {
    let source = r#"module index_trait_e2e

import std.alloc.deque as deque
import std.alloc.smallvec as smallvec
import std.core.ops as ops
import std.core.io as io

struct Entry {
    public score: int,
}

struct Table {
    first: Entry,
    second: Entry,
}

func Table.index(self: ref Table, key: str): ref Entry {
    if key == "first" {
        return ref self.first
    }
    return ref self.second
}

func Table.indexMut(self: mut ref Table, key: str): mut ref Entry {
    if key == "first" {
        return mut ref self.first
    }
    return mut ref self.second
}

struct PairBox {
    a: int,
    b: int,
}

func PairBox.index(self: ref PairBox, idx: usize): ref int {
    if idx == (0 as usize) {
        return ref self.a
    }
    return ref self.b
}

func PairBox.indexMut(self: mut ref PairBox, idx: usize): mut ref int {
    if idx == (0 as usize) {
        return mut ref self.a
    }
    return mut ref self.b
}

func sumFirstTwo<C: ops.Index<usize, int>>(c: ref C): int {
    return c[0] + c[1]
}

func setAt<C: ops.IndexMut<usize, int>>(c: mut ref C, idx: usize, val: int): void {
    c[idx] = val
}

func main(): int {
    // 1. VecDeque<int>: read, write, compound assign, ref, mut ref
    let mut dq = deque.VecDeque<int>.new()
    dq.pushBack(10)
    dq.pushBack(20)
    dq.pushFront(5)
    // dq is [5, 10, 20]
    dq[1] = 42
    dq[2] += 8
    let r_dq = ref dq[0]
    let m_dq = mut ref dq[2]
    *m_dq = *m_dq + 1 // 29

    // 2. SmallVec4<int> inline (<= 4) & heap (> 4)
    let mut sv = smallvec.SmallVec4<int>.new()
    sv.push(1)
    sv.push(2)
    sv.push(3)
    sv[1] = 22
    sv[2] += 7
    let inline_sum = sumFirstTwo<smallvec.SmallVec4<int>>(ref sv) // 1 + 22 = 23

    sv.push(4)
    sv.push(5)
    sv[4] = 55
    let m_sv = mut ref sv[3]
    *m_sv = 44
    let heap_check = sv[3] + sv[4] // 44 + 55 = 99

    // 3. Vec<int> & PairBox through generic Index & IndexMut
    let mut v = Vec<int>.new()
    v.push(100)
    v.push(200)
    setAt<Vec<int>>(mut ref v, 0 as usize, 111)
    let v_sum = sumFirstTwo<Vec<int>>(ref v) // 111 + 200 = 311

    let dq_sum = sumFirstTwo<deque.VecDeque<int>>(ref dq) // 5 + 42 = 47

    let mut pb = PairBox { a: 7, b: 8 }
    setAt<PairBox>(mut ref pb, 1 as usize, 18)
    let pb_sum = sumFirstTwo<PairBox>(ref pb) // 7 + 18 = 25

    // 4. Custom non-usize key + field mutation through index
    let mut tbl = Table {
        first: Entry { score: 10 },
        second: Entry { score: 20 },
    }
    tbl["first"].score = 33
    let total = *r_dq + dq_sum + inline_sum + heap_check + v_sum + pb_sum + tbl["first"].score + tbl["second"].score + dq[2]
    // 5 + 47 + 23 + 99 + 311 + 25 + 33 + 20 + 29 = 592
    if total != 592 {
        io.println(total)
        return 1
    }

    // 5. Non-Copy String container: borrow & method calls allowed
    let mut words = deque.VecDeque<String>.new()
    words.pushBack("alpha".toString())
    words.pushBack("beta".toString())
    let borrowed = ref words[0]
    let len0 = words[0].len()
    let len1 = borrowed.len()
    if len0 != (5 as usize) || len1 != (5 as usize) {
        return 2
    }
    return 0
}
"#;

    let out = run_source("e2e", source);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status.code()
    );
}

#[test]
fn move_out_of_trait_index_reports_o015_and_immutable_reports_t026() {
    let o015_source = r#"module o015_trait_index

import std.alloc.deque as deque

func main(): int {
    let mut dq = deque.VecDeque<String>.new()
    dq.pushBack("hello".toString())
    let moved = dq[0]
    return moved.len() as int
}
"#;
    let out = check_source("o015", o015_source);
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success() && combined.contains("O015"),
        "expected O015 error on moving String out of VecDeque[0], got:\n{combined}"
    );

    let t026_assign_source = r#"module t026_trait_assign

import std.alloc.deque as deque

func touch(d: ref deque.VecDeque<int>): void {
    d[0] = 1
}

func main(): int {
    return 0
}
"#;
    let out = check_source("t026-assign", t026_assign_source);
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success() && combined.contains("T026"),
        "expected T026 error on assigning through immutable VecDeque ref, got:\n{combined}"
    );

    let t026_mut_ref_source = r#"module t026_trait_mut_ref

import std.alloc.deque as deque

func main(): int {
    let dq = deque.VecDeque<int>.new()
    let r = mut ref dq[0]
    *r = 5
    return 0
}
"#;
    let out = check_source("t026-mut-ref", t026_mut_ref_source);
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success() && combined.contains("T026"),
        "expected T026 error on taking mut ref from immutable VecDeque, got:\n{combined}"
    );
}
