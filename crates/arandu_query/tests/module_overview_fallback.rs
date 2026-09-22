#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::docs::module_doc;

#[test]
fn module_overview_uses_orphaned_preamble_docs() {
    // The `///` block after `module X` attaches to the following `import`,
    // which is never rendered as a doc item. It must surface as the
    // module overview instead of being dropped silently.
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "overview_sample.aru".into(),
        r#"module tests.overview

/// Module-level summary that precedes the imports.
///
/// More detail here.
import std.core.mem as mem

/// Adds one.
public func addOne(x: int): int {
    return x + 1
}
"#
        .to_string(),
    );

    let mod_doc = module_doc(&db, file);
    let overview = mod_doc.overview.as_deref().expect("module overview");
    assert!(
        overview.contains("Module-level summary that precedes the imports."),
        "unexpected overview: {overview}"
    );
    assert!(overview.contains("More detail here"));
}

#[test]
fn module_overview_prefers_docs_on_module_decl() {
    // Docs written BEFORE `module X` attach directly and must win over
    // orphaned preamble docs.
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "overview_direct.aru".into(),
        r#"/// Direct module docs.
module tests.direct

/// Orphaned preamble that must not override the direct docs.
import std.core.mem as mem

public func f(): int {
    return 1
}
"#
        .to_string(),
    );

    let mod_doc = module_doc(&db, file);
    assert_eq!(mod_doc.overview.as_deref(), Some("Direct module docs."));
}

#[test]
fn module_overview_is_none_without_any_docs() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "overview_empty.aru".into(),
        "module tests.empty\n\npublic func f(): int {\n    return 1\n}\n".to_string(),
    );

    let mod_doc = module_doc(&db, file);
    assert_eq!(mod_doc.overview, None);
}
