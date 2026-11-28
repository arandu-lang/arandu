#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_ide::organize_imports;
use arandu_query::AnalysisHost;

fn apply_edit(text: &str, edit: &arandu_ide::TextEdit) -> String {
    let mut out = String::new();
    out.push_str(&text[..edit.start as usize]);
    out.push_str(&edit.new_text);
    out.push_str(&text[edit.end as usize..]);
    out
}

#[test]
fn organize_imports_drops_unused_groups_siblings_sorts_buckets_and_preserves_reexports() {
    let mut host = AnalysisHost::default();
    let _helper = host.new_file(
        "self/helper.aru".into(),
        "module self.helper\n\npublic func help(): int { return 7 }\n".into(),
    );
    let _extra = host.new_file(
        "pkg/extra.aru".into(),
        "module pkg.extra\n\npublic func one(): int { return 1 }\n".into(),
    );
    let _unused = host.new_file(
        "pkg/unused.aru".into(),
        "module pkg.unused\n\npublic func nope(): int { return 0 }\n".into(),
    );
    let text = "\
module app.demo

import self.helper as helper
import std.io as io
import pkg.unused as unused
import pkg.extra as extra
public use self.helper.{ help }
import std.fs as files

func main(): int {
    let _ = files.exists(\"Cargo.toml\")
    io.println(\"hi\")
    return helper.help() + extra.one()
}
";
    let source = host.new_file("app/demo.aru".into(), text.to_string());
    let snap = host.snapshot();
    let edit = organize_imports(&snap, source, text).expect("expected organize_imports edit");
    let organized = apply_edit(text, &edit);

    let expected = "\
module app.demo

import std.{fs as files, io}

import pkg.extra

import self.helper

public use self.helper.{ help }

func main(): int {
    let _ = files.exists(\"Cargo.toml\")
    io.println(\"hi\")
    return helper.help() + extra.one()
}
";
    assert_eq!(organized, expected);

    // Applying again on the already-organized text is a no-op.
    drop(snap);
    host.set_text(source, organized.clone());
    let snap2 = host.snapshot();
    assert_eq!(organize_imports(&snap2, source, &organized), None);
}

#[test]
fn organize_imports_bails_on_comments_and_syntax_errors() {
    let mut host = AnalysisHost::default();
    let commented = "\
module app.c

import std.io as io
// keep ordering manual
import std.fs as files

func main(): int { return 0 }
";
    let src1 = host.new_file("app/c.aru".into(), commented.to_string());
    let snap1 = host.snapshot();
    assert_eq!(organize_imports(&snap1, src1, commented), None);

    let broken = "module app.broken\nimport std.io as\nfunc main(): int { return 0 }\n";
    let src2 = host.new_file("app/broken.aru".into(), broken.to_string());
    let snap2 = host.snapshot();
    assert_eq!(organize_imports(&snap2, src2, broken), None);
}
