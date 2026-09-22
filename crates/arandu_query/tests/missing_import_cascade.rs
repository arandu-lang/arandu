use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;

#[test]
fn missing_module_reports_import_without_member_cascade() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "main.aru".into(),
        "import std.missing as gone\nfunc make(): gone.Missing { return gone.nope() }\n".into(),
    );
    let diagnostics = file_ide_diagnostics(&db, file);
    let codes: Vec<_> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect();
    assert!(
        codes.contains(&"M001"),
        "missing import must be reported: {codes:?}"
    );
    for duplicate in ["M002", "N002", "T018"] {
        assert!(
            !codes.contains(&duplicate),
            "{duplicate} cascaded after M001: {codes:?}"
        );
    }
}
