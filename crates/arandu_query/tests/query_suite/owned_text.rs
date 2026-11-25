//! Text built at run time is an owned `String` (from the ambient
//! `std.alloc.string`) that lends itself as a `str` view at calls and `let`
//! bindings; returning the view or appending to a `str` is an error.
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
    let file = db.new_file("text.aru".to_string(), source.to_string());
    arandu_query::file_ide_diagnostics(&db, file)
        .iter()
        .filter(|diagnostic| diagnostic.severity == 0)
        .map(|diagnostic| diagnostic.code.clone())
        .collect()
}

#[test]
fn owned_text_lends_itself_as_a_view() {
    let source = "from std.alloc.string import { String }\n\
        func size(text: str): usize { return text.len() }\n\
        func greet(name: str): String { return \"hello, \" + name }\n\
        func main(): int {\n\
            let n = 7\n\
            let built = \"n=${n}\"\n\
            let view: str = \"v${n}\"\n\
            let mut s = String.from(\"a\")\n\
            s += \"b\"\n\
            s += built\n\
            return (size(built) + size(view) + size(s) + size(greet(\"x\"))) as int\n\
        }\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn returning_a_view_of_owned_text_is_t004() {
    let source = "func greet(name: str): str { return \"hello, ${name}\" }\n";
    assert_eq!(errors(source), vec!["T004".to_string()]);
}

#[test]
fn appending_to_a_str_is_t002() {
    let source = "func main(): int {\n    let mut s = \"a\"\n    s += \"b\"\n    return 0\n}\n";
    assert_eq!(errors(source), vec!["T002".to_string()]);
}

#[test]
fn primitive_to_str_produces_owned_string_when_stdlib_loaded() {
    let source = "from std.alloc.string import { String }\n\
        func size(text: str): usize { return text.len() }\n\
        func main(): int {\n\
            let owned: String = (42).toStr()\n\
            let opt: String? = (42).toStr()\n\
            let view: str = (42).toStr()\n\
            let opt_len: usize = if opt is v { v.len() } else { 0 as usize }\n\
            return (owned.len() + size(view) + size((7).toStr()) + opt_len) as int\n\
        }\n";
    let mut db = DatabaseImpl::default();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../stdlib")
        .canonicalize()
        .expect("workspace stdlib");
    register_stdlib(&mut db, &root, &root);
    let file = db.new_file("text.aru".to_string(), source.to_string());
    let all = arandu_query::file_ide_diagnostics(&db, file);
    let diags: Vec<_> = all.iter().filter(|d| d.severity == 0).collect();
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:#?}");
}
