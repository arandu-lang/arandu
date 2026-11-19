//! A helper evaluated at compile time may call a generic function of another
//! module (`str.len` calls `slice.len<u8>`): the call is bound to its
//! instance, and helpers that call no generic keep the narrow lowering.
#![allow(clippy::expect_used)]
use arandu_query::db::DatabaseImpl;
use std::path::{Path, PathBuf};

fn register_stdlib(db: &mut DatabaseImpl, directory: &Path, root: &Path) {
    let mut entries: Vec<_> = std::fs::read_dir(directory)
        .expect("stdlib directory")
        .map(|entry| entry.expect("stdlib entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            register_stdlib(db, &path, root);
        } else if path.extension().is_some_and(|extension| extension == "aru") {
            let relative = path
                .strip_prefix(root)
                .expect("beneath the stdlib root")
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(&path).expect("stdlib source");
            db.new_stdlib_file(format!("stdlib/{relative}"), source);
        }
    }
}

fn errors(source: &str) -> Vec<String> {
    let mut db = DatabaseImpl::default();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../stdlib")
        .canonicalize()
        .expect("workspace stdlib");
    register_stdlib(&mut db, &root, &root);
    let file = db.new_file("comptime.aru".to_string(), source.to_string());
    arandu_query::file_ide_diagnostics(&db, file)
        .iter()
        .filter(|diagnostic| diagnostic.severity == 0)
        .map(|diagnostic| diagnostic.code.clone())
        .collect()
}

#[test]
fn a_helper_calling_a_generic_of_another_module_evaluates() {
    let source = "import std.core.str as text\n\
        func f(): int {\n\
            let s = \"abc\"\n\
            if s.len() != (3 as usize) { return 1 }\n\
            if !text.eq(\"abc\", \"abc\") || text.isAscii(\"á\") { return 2 }\n\
            return 42\n\
        }\n\
        func main(): int {\n    let r = comptime f()\n    return r\n}\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn shared_references_evaluate_and_mutable_ones_stay_outside() {
    // As Rust's `const fn`: a shared borrow reads its frozen owner, which
    // cannot change while it is borrowed; a mutable borrow is not evaluated.
    let shared = "func f(): int {\n    let x = 5\n    let r = ref x\n    return *r\n}\n\
        func main(): int {\n    let r = comptime f()\n    return r\n}\n";
    assert!(errors(shared).is_empty(), "{:?}", errors(shared));
    let mutable =
        "func f(): int {\n    let mut x = 5\n    let r = mut ref x\n    *r = 6\n    return x\n}\n\
        func main(): int {\n    let r = comptime f()\n    return r\n}\n";
    assert_eq!(errors(mutable), ["T044"]);
}
