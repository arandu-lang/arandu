//! Tests for std.alloc.hash_map.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

use crate::common;

const HASH_MAP_ARU: &str = include_str!("../../../../stdlib/alloc/hash_map.aru");

#[test]
fn stdlib_hash_map_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "stdlib/alloc/hash_map.aru".to_string(),
        HASH_MAP_ARU.to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("hash_map.aru must parse; got {e:?}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "Entry",
        "HashMap",
        "HashMap.new",
        "HashMap.withCapacity",
        "HashMap.len",
        "HashMap.isEmpty",
        "HashMap.capacity",
        "HashMap.clear",
        "HashMap.insert",
        "HashMap.put",
        "HashMap.contains",
        "HashMap.get",
        "HashMap.getRef",
        "HashMap.remove",
    ];
    for key in expected {
        assert!(
            exports.symbols.contains_key(key),
            "expected exported symbol `{key}`, got {:?}",
            exports.symbols.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn stdlib_hash_map_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let mut map_file = None;
    for (path, source) in common::STDLIB_MODULES {
        let file = db.new_file((*path).to_string(), (*source).to_string());
        if *path == "stdlib/alloc/hash_map.aru" {
            map_file = Some(file);
        }
    }
    let map_file = map_file.expect("hash_map module is in the canonical stdlib graph");

    let main_src = r#"
import std.alloc.hash_map as hash_map
import std.core.hash as hash

struct Key {
    id: int
}

public func Key.eq(self: ref Key, other: ref Key): bool {
    return self.id == other.id
}

public func Key.hash<H: hash.Hasher>(self: ref Key, state: mut ref H): void {
    state.writeInt(self.id)
}

func main(): int {
    let mut map = hash_map.HashMap<Key, int>.new()
    if !map.isEmpty() || map.len() != 0 {
        return 1
    }

    let k1 = Key { id: 10 }
    let k2 = Key { id: 20 }
    let k3 = Key { id: 30 }

    map.put(k1, 100)
    map.put(k2, 200)
    map.put(k3, 300)

    if map.len() != 3 {
        return 2
    }

    if !map.contains(ref k1) || !map.contains(ref k2) || !map.contains(ref k3) {
        return 3
    }

    let k_missing = Key { id: 999 }
    if map.contains(ref k_missing) {
        return 4
    }

    // Update existing key
    match map.put(Key { id: 10 }, 105) {
        Some(oldVal) => {
            if oldVal != 100 {
                return 5
            }
        }
        None => {
            return 6
        }
    }

    // Remove key
    match map.remove(ref k2) {
        Some(val) => {
            if val != 200 {
                return 7
            }
        }
        None => {
            return 8
        }
    }

    if map.len() != 2 || map.contains(ref k2) {
        return 9
    }

    return 0
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_map = file_ide_diagnostics(&db, map_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_map: Vec<_> = diags_map.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_map.is_empty(),
        "unexpected errors in hash_map.aru: {error_diags_map:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}
