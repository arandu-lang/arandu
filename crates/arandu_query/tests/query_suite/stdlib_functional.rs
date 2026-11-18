//! Functional combinators and methods on stdlib types: `Option`, `Result`,
//! `Range` and `Vec`.
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
    let file = db.new_file("test_functional.aru".to_string(), source.to_string());
    let diags = arandu_query::file_ide_diagnostics(&db, file);
    diags
        .iter()
        .filter(|diagnostic| diagnostic.severity == 0)
        .map(|diagnostic| diagnostic.code.clone())
        .collect()
}

#[test]
fn option_functional_combinators() {
    let source = "from std.core.option import { Option }\n\
        from std.core.result import { Result }\n\
        func main(): int {\n\
            let a: Option<int> = Option.Some(21)\n\
            let b = a.map((x) => x * 2)\n\
            let c: Option<int> = Option.None\n\
            let d = c.unwrapOrElse(() => 42)\n\
            let e = b.andThen((x) => Option.Some(x + 1))\n\
            let res_ok = a.okOr(404)\n\
            let res_err = c.okOrElse(() => 500)\n\
            let filtered = a.filter((x) => *x > 20)\n\
            a.inspect((x) => { let _ = *x })\n\
            let zipped = a.zip(Option.Some(100))\n\
            return 0\n\
        }\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn result_functional_combinators() {
    let source = "from std.core.result import { Result }\n\
        func main(): int {\n\
            let a: Result<int, int> = Result.Ok(21)\n\
            let b = a.map((x) => x * 2)\n\
            let c = b.andThen((x) => Result.Ok(x + 1))\n\
            let d: Result<int, int> = Result.Err(404)\n\
            let e = d.mapErr((err) => err + 1)\n\
            let val = d.unwrapOrElse((err) => 0)\n\
            let opt_some = a.ok()\n\
            let opt_none = d.ok()\n\
            let opt_err = d.err()\n\
            a.inspect((x) => { let _ = *x })\n\
            d.inspectErr((e) => { let _ = *e })\n\
            return 0\n\
        }\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn range_utility_methods() {
    let source = "from std.core.range import { Range, RangeInclusive }\n\
        func main(): int {\n\
            let r: Range<int> = 10..20\n\
            let has15 = r.contains(15)\n\
            let empty = r.isEmpty()\n\
            let ri: RangeInclusive<int> = 10..=20\n\
            let has20 = ri.contains(20)\n\
            return 0\n\
        }\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn vec_functional_combinators() {
    let source = "from std.alloc.vec import { Vec }\n\
        func main(): int {\n\
            let mut v = Vec.new<int>()\n\
            v.push(1)\n\
            v.push(2)\n\
            v.push(3)\n\
            let mut sum = 0\n\
            v.forEach((x) => { sum += *x })\n\
            let folded = v.fold(0, (acc, x) => acc + *x)\n\
            let has_two = v.anyMatch((x) => *x > 2)\n\
            let all_positive = v.allMatch((x) => *x > 0)\n\
            let doubled = v.map((x) => *x * 2)\n\
            let filtered = v.filter((x) => *x > 1)\n\
            let mapped_trailing = v.map { x => *x * 3 }\n\
            let filtered_trailing = v.filter { (x) => *x > 1 }\n\
            let folded_trailing = v.fold(0) { (acc, x) => acc + *x }\n\
            let slice: []int = [10, 20, 30]\n\
            let v_from = Vec.from(slice)\n\
            return 0\n\
        }\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn vec_filter_requires_copy_element_type() {
    let source = "from std.alloc.vec import { Vec }\n\
        from std.alloc.string import { String }\n\
        func main(): int {\n\
            let mut v = Vec.new<String>()\n\
            v.push(String.from(\"a\"))\n\
            let _filtered = v.filter((x) => !x.isEmpty())\n\
            return 0\n\
        }\n";
    let codes = errors(source);
    assert!(
        codes.iter().any(|c| c == "T025"),
        "expected T025 when calling Vec.filter on non-Copy Vec<String>, got {codes:?}"
    );
}

#[test]
fn user_defined_to_str_and_interpolation() {
    let source = "struct User {\n\
            id: int,\n\
        }\n\
        func User.toStr(self: ref User): str {\n\
            return \"User\"\n\
        }\n\
        func main(): int {\n\
            let u = User { id: 42 }\n\
            let s1 = \"User: ${u.toStr()}\"\n\
            let s2 = \"User direct: ${u}\"\n\
            let n = 100\n\
            let num_str = n.toStr()\n\
            return 0\n\
        }\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn option_null_coalescing_and_try() {
    let source = "from std.core.option import { Option }\n\
        func get_default(opt: Option<int>): int {\n\
            return opt ?? 42\n\
        }\n\
        func step(opt: Option<int>): Option<int> {\n\
            let v = opt?\n\
            return Option.Some(v + 1)\n\
        }\n\
        func main(): int {\n\
            let a = Option.Some(10)\n\
            let res = get_default(a)\n\
            return 0\n\
        }\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn str_and_slice_fluent_methods() {
    let source = "import std.core.str as core_str\n\
        import std.core.slice as core_slice\n\
        func main(): int {\n\
            let s = \"hello, world\"\n\
            let slen = s.len()\n\
            let sempty = s.isEmpty()\n\
            let sstarts = s.startsWith(\"hello\")\n\
            let sends = s.endsWith(\"world\")\n\
            let scontains = s.contains(\"lo\")\n\
            let s_ascii = s.isAscii()\n\
            let s_eq = s.eq(\"hello, world\")\n\
            let arr = [10, 20, 30]\n\
            let arr_len = arr.len()\n\
            let arr_empty = arr.isEmpty()\n\
            let mut sum = 0\n\
            arr.forEach((x) => { sum += *x })\n\
            arr.forEach { x => sum += *x }\n\
            let folded = arr.fold(0, (acc, x) => acc + *x)\n\
            let folded_trailing = arr.fold(0) { (acc, x) => acc + *x }\n\
            let has_twenty = arr.anyMatch((x) => *x == 20)\n\
            let has_twenty_trailing = arr.anyMatch { x => *x == 20 }\n\
            let all_pos = arr.allMatch((x) => *x > 0)\n\
            let all_pos_trailing = arr.allMatch { x => *x > 0 }\n\
            return 0\n\
        }\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn hash_map_fluent_methods() {
    let source = "from std.alloc.hash_map import { HashMap }\n\
        import std.core.hash as hash\n\
        struct Key { id: int }\n\
        func Key.eq(self: ref Key, other: ref Key): bool {\n\
            return self.id == other.id\n\
        }\n\
        func Key.hash<H: hash.Hasher>(self: ref Key, state: mut ref H): void {\n\
            state.writeInt(self.id)\n\
        }\n\
        func main(): int {\n\
            let mut map = HashMap.new<Key, int>()\n\
            let empty = map.isEmpty()\n\
            let k1 = Key { id: 1 }\n\
            let k2 = Key { id: 2 }\n\
            map.put(k1, 100)\n\
            map.put(k2, 200)\n\
            let length = map.len()\n\
            let has_one = map.contains(ref k1)\n\
            let val = map.get(ref k1)\n\
            let val_or_def = map.getOrDefault(ref k1, 0)\n\
            return 0\n\
        }\n";
    assert_eq!(errors(source), Vec::<String>::new());
}
