//! `static mut` is checked by the front end but has no lowered storage yet:
//! it fails closed instead of dropping writes or linking a missing symbol.
#![allow(clippy::expect_used)]

use arandu_middle::{DiagCode, Severity};
use arandu_query::{passes, DatabaseImpl};

fn codes(diagnostics: &[arandu_middle::Diagnostic]) -> Vec<DiagCode> {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn access_outside_unsafe_is_o017() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "static_mut.aru".into(),
        "static mut COUNTER: int = 0\nfunc bump(): void {\n    COUNTER = COUNTER + 1\n}\nfunc main(): int { return 0 }\n"
            .into(),
    );
    let checked = passes::type_check(&db, file);
    assert!(
        codes(&checked.diagnostics).contains(&DiagCode::O017StaticMutRequiresUnsafe),
        "{:?}",
        checked.diagnostics
    );
}

#[test]
fn reads_and_writes_lower_to_static_storage() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "static_mut.aru".into(),
        "static mut COUNTER: int = 0\nfunc main(): int {\n    unsafe {\n        COUNTER = COUNTER + 1\n    }\n    return unsafe { COUNTER }\n}\n"
            .into(),
    );
    let program = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        codes(&program.diagnostics).is_empty(),
        "{:?}",
        program.diagnostics
    );
    let statics = &program.artifacts.amir.statics;
    assert_eq!(statics.len(), 1);
    assert_eq!(statics[0].name.as_str(), "ar_static_static_mut_COUNTER");
}

#[test]
fn text_and_views_have_data_images_with_relocations() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "static_mut.aru".into(),
        "static mut NAME: str = \"x\"\nfunc main(): int {\n    return unsafe { NAME.len } as int\n}\n".into(),
    );
    let program = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        !codes(&program.diagnostics).contains(&DiagCode::T101StaticWithoutDataImage),
        "{:?}",
        program.diagnostics
    );
    let statics = &program.artifacts.amir.statics;
    assert_eq!(statics.len(), 1);
    let image = arandu_middle::ctfe::const_value_bytes(
        &statics[0].init,
        statics[0].ty,
        &program.artifacts.type_check.type_info.type_interner,
        &*program.artifacts.type_check.type_info,
        arandu_middle::DataLayout::ptr_width(8),
        true,
    )
    .expect("a `str` has a data image");
    // `{data, len}`: the data word points at the bytes, the length is 1.
    assert_eq!(image.objects.len(), 1);
    assert_eq!(image.objects[0].bytes, b"x");
    assert_eq!(image.relocations.len(), 1);
    assert_eq!(image.relocations[0].offset, 0);
    assert_eq!(&image.bytes[8..], &1u64.to_le_bytes());
}

#[test]
fn union_single_field_and_null_pointer_static_initializers_produce_data_images() {
    let mut db = DatabaseImpl::new();
    let source = r#"
@Repr("C")
struct GCObject {
    next: ptr[GCObject],
    tt: u8,
}

@Repr("C")
union Value {
    gc: ptr[GCObject],
    f: ptr[func(ptr[u8]) int],
    n: f64,
}

@Repr("C")
struct NodeKey {
    value: Value,
    tt: u8,
    keyTt: u8,
    next: int,
    keyVal: Value,
}

@Repr("C")
union Node {
    u: NodeKey,
    iVal: Value,
}

@Repr("C")
struct TValue {
    value: Value,
    tt: u8,
}

@Repr("C")
union AnonDummyLittle {
    dummy: int,
    little: i8,
}

static mut dummynode = Node {
    u: NodeKey {
        value: Value { gc: (nil as ptr[GCObject]) },
        tt: ((0 | (1 << (4 as int))) as u8),
        keyTt: (0 as u8),
        next: 5,
        keyVal: Value { gc: (0 as ptr[GCObject]) },
    },
}

static mut absentkey = TValue {
    value: Value { gc: (nil as ptr[GCObject]) },
    tt: ((0 | (2 << (4 as int))) as u8),
}

static mut nativeendian = AnonDummyLittle { dummy: 1 }

func main(): int {
    unsafe {
        let a = dummynode.u.tt as int
        let b = dummynode.u.next
        let c = absentkey.tt as int
        let d = nativeendian.little as int
        return a + b + c + d
    }
}
"#;
    let file = db.new_file("lua_statics.aru".into(), source.into());
    let parsed = passes::parse(&db, file);
    assert!(parsed.is_ok(), "parse failed: {:?}", parsed.as_ref().err());
    let program = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        codes(&program.diagnostics).is_empty(),
        "{:?}",
        program.diagnostics
    );
    let statics = &program.artifacts.amir.statics;
    assert_eq!(statics.len(), 3);

    let type_info = &*program.artifacts.type_check.type_info;
    let find_static = |suffix: &str| {
        statics
            .iter()
            .find(|item| item.name.ends_with(suffix))
            .expect("missing expected static")
    };
    let endian = find_static("nativeendian");
    let endian_bytes = arandu_middle::ctfe::const_value_bytes(
        &endian.init,
        endian.ty,
        &type_info.type_interner,
        type_info,
        type_info.target_layout,
        true,
    )
    .expect("nativeendian static image")
    .bytes;
    assert_eq!(endian_bytes, [1, 0, 0, 0]);

    let absent = find_static("absentkey");
    let absent_bytes = arandu_middle::ctfe::const_value_bytes(
        &absent.init,
        absent.ty,
        &type_info.type_interner,
        type_info,
        type_info.target_layout,
        true,
    )
    .expect("absentkey static image")
    .bytes;
    // Value union (8 bytes zero for nil gc) + tt (32) + 7 bytes tail padding
    assert_eq!(&absent_bytes[..8], &[0; 8]);
    assert_eq!(absent_bytes[8], 32);

    let dummy = find_static("dummynode");
    let dummy_bytes = arandu_middle::ctfe::const_value_bytes(
        &dummy.init,
        dummy.ty,
        &type_info.type_interner,
        type_info,
        type_info.target_layout,
        true,
    )
    .expect("dummynode static image")
    .bytes;
    assert_eq!(&dummy_bytes[..8], &[0; 8]);
    assert_eq!(dummy_bytes[8], 16);
    assert_eq!(dummy_bytes[9], 0);
    assert_eq!(&dummy_bytes[12..16], &5i32.to_le_bytes());
    assert_eq!(&dummy_bytes[16..24], &[0; 8]);
}

#[test]
fn non_null_pointer_cast_in_static_mut_is_rejected() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "non_null_static.aru".into(),
        "@Repr(\"C\")\nstruct Holder { p: ptr[u8] }\nstatic mut BAD = Holder { p: (1 as ptr[u8]) }\nfunc main(): int {\n    return unsafe { if BAD.p == nil { 0 } else { 1 } }\n}\n"
            .into(),
    );
    let parsed = passes::parse(&db, file);
    assert!(parsed.is_ok(), "parse failed: {:?}", parsed.as_ref().err());
    let program = arandu_query::runtime::runtime_program(&db, file);
    assert!(
        !codes(&program.diagnostics).is_empty(),
        "non-zero integer cast to pointer in static mut must be rejected"
    );
}
