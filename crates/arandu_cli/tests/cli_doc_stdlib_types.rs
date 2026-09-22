//! Imported stdlib types must survive documentation rendering.

use std::fs;
use std::process::Command;

#[test]
fn doc_stdlib_renders_imported_types_in_signatures_and_fields() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../stdlib");
    let out = std::env::temp_dir().join(format!("arandu-doc-stdlib-types-{}", std::process::id()));
    fs::create_dir_all(&out).expect("create output directory");

    let output = Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args([
            "doc",
            root.to_str().expect("UTF-8 stdlib path"),
            "--format=json",
            &format!("--out-dir={}", out.display()),
        ])
        .output()
        .expect("run arandu doc");
    assert!(
        output.status.success(),
        "documentation command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    for name in ["std.fs.json", "std.net.json", "std.alloc.hash_map.json"] {
        let rendered = fs::read_to_string(out.join(name)).expect("read generated JSON");
        assert!(
            !rendered.contains("<error>"),
            "{name} contains unresolved types"
        );
        if name == "std.fs.json" {
            let parsed: serde_json::Value =
                serde_json::from_str(&rendered).expect("parse generated JSON");
            let names: Vec<&str> = parsed["items"]
                .as_array()
                .expect("items array")
                .iter()
                .filter_map(|item| item["name"].as_str())
                .collect();
            assert!(names.contains(&"File"));
            assert!(
                !names.contains(&"BufReader"),
                "imported items leaked into std.fs"
            );
        }
    }
    let _ = fs::remove_dir_all(out);
}
