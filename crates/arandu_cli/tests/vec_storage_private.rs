//! Vec storage is an invariant boundary shared by immutable/exclusive views.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;
use std::fs;

#[test]
fn safe_code_cannot_construct_or_modify_vec_raw_storage() {
    let root = common::temp_dir("arandu-vec-storage-private").unwrap();
    let source = root.join("storage.aru");
    for body in [
        "let forged = vec.Vec<int> { data: nil, len: 1 as usize, capacity: 1 as usize }; forged.len()",
        "let mut values = vec.Vec<int>.new(); values.len = 100 as usize",
        "let mut values = vec.Vec<int>.new(); values.capacity = 100 as usize",
        "let mut values = vec.Vec<int>.new(); values.data = nil",
    ] {
        fs::write(
            &source,
            format!("import std.alloc.vec as vec\nfunc main():int {{ {body}; return 0 }}"),
        )
        .unwrap();
        let output = common::cli_command()
            .arg("check")
            .arg(&source)
            .output()
            .unwrap();
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "accepted forged Vec: {body}");
        assert!(diagnostics.contains("T041"), "{body}: {diagnostics}");
    }
    fs::remove_dir_all(root).unwrap();
}
