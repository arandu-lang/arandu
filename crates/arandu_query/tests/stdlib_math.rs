//! Integration tests for std.math (view, matrix, linalg) - RFC 0012 Arandu Math v1.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const VIEW_ARU: &str = include_str!("../../../stdlib/math/view.aru");
const MATRIX_ARU: &str = include_str!("../../../stdlib/math/matrix.aru");
const LINALG_ARU: &str = include_str!("../../../stdlib/math/linalg.aru");
const ARENA_ARU: &str = include_str!("../../../stdlib/alloc/arena.aru");
const SLICE_ARU: &str = include_str!("../../../stdlib/core/slice.aru");
const MEM_ARU: &str = include_str!("../../../stdlib/core/mem.aru");
const INTRINSICS_ARU: &str = include_str!("../../../stdlib/core/intrinsics.aru");
const NUM_ARU: &str = include_str!("../../../stdlib/core/num.aru");

#[test]
fn stdlib_math_view_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/math/view.aru".to_string(), VIEW_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("view.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "ArrayView",
        "ArrayViewMut",
        "fromSlice",
        "fromRaw",
        "fromSliceMut",
        "fromRawMut",
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
fn stdlib_math_matrix_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/math/matrix.aru".to_string(), MATRIX_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("matrix.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "StaticMatrix",
        "Matrix",
        "staticFill",
        "staticIdentityInto",
        "staticAddInto",
        "staticMulInto",
        "staticTransposeInto",
        "staticDet2",
        "newMatrix",
    ];
    for key in expected {
        assert!(
            exports.symbols.contains_key(key),
            "expected exported symbol `{key}`, got {:?}",
            exports.symbols.keys().collect::<Vec<_>>()
        );
    }
    for removed in [
        "StaticMat2",
        "StaticMat3",
        "StaticMat4",
        "eye2",
        "zeros2",
        "eye3",
        "zeros3",
        "eye4",
        "zeros4",
    ] {
        assert!(
            !exports.symbols.contains_key(removed),
            "legacy compatibility symbol `{removed}` must not be exported"
        );
    }
}

#[test]
fn stdlib_math_linalg_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/math/linalg.aru".to_string(), LINALG_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("linalg.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "addInto",
        "subInto",
        "scaleInto",
        "mulAddInto",
        "gemmRequirements",
        "gemmInto",
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
fn stdlib_math_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let _ = db.new_file(
        "stdlib/core/intrinsics.aru".to_string(),
        INTRINSICS_ARU.to_string(),
    );
    let _ = db.new_file("stdlib/core/mem.aru".to_string(), MEM_ARU.to_string());
    let _ = db.new_file("stdlib/core/num.aru".to_string(), NUM_ARU.to_string());
    let _ = db.new_file("stdlib/core/slice.aru".to_string(), SLICE_ARU.to_string());
    let _ = db.new_file("stdlib/alloc/arena.aru".to_string(), ARENA_ARU.to_string());
    let view_file = db.new_file("stdlib/math/view.aru".to_string(), VIEW_ARU.to_string());
    let matrix_file = db.new_file("stdlib/math/matrix.aru".to_string(), MATRIX_ARU.to_string());
    let linalg_file = db.new_file("stdlib/math/linalg.aru".to_string(), LINALG_ARU.to_string());

    let main_src = r#"
import std.math.matrix as matrix
import std.math.view as view
import std.math.linalg as linalg
import std.alloc.arena as arena

func testStaticMatrices(): int {
    let left: matrix.StaticMatrix<float, 2, 3> = matrix.StaticMatrix<float, 2, 3> {
        data: [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]],
    }
    let right: matrix.StaticMatrix<float, 3, 2> = matrix.StaticMatrix<float, 3, 2> {
        data: [[7.0, 8.0], [9.0, 10.0], [11.0, 12.0]],
    }
    let mut product: matrix.StaticMatrix<float, 2, 2> = matrix.StaticMatrix<float, 2, 2> {
        data: [[0.0, 0.0], [0.0, 0.0]],
    }
    matrix.staticMulInto(left, right, product)
    if product.get(0, 0) != 58.0 {
        return 1
    }
    if product.get(1, 1) != 154.0 {
        return 1
    }

    product.set(0, 0, 3.0)
    product.set(0, 1, 8.0)
    product.set(1, 0, 4.0)
    product.set(1, 1, 6.0)
    if matrix.staticDet2(product) != -14.0 {
        return 2
    }

    matrix.staticIdentityInto(product)
    if product.rows() != 2 {
        return 3
    }
    if product.cols() != 2 {
        return 3
    }
    if product.get(1, 1) != 1.0 {
        return 3
    }

    let mut transposed: matrix.StaticMatrix<float, 3, 2> = matrix.StaticMatrix<float, 3, 2> {
        data: [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
    }
    matrix.staticTransposeInto(left, transposed)
    if transposed.get(2, 1) != 6.0 {
        return 4
    }
    return 0
}

func testDynamicMatrixAndViews(): int {
    let optM = matrix.newMatrix<float>(3, 3, 0.0)
    if optM is Option.None {
        return 10
    }
    return 0
}

func testLinalgKernels(): int {
    let optA = matrix.newMatrix<float>(2, 2, 1.0)
    let optB = matrix.newMatrix<float>(2, 2, 2.0)
    let optC = matrix.newMatrix<float>(2, 2, 0.0)

    if optA is Option.None {
        return 20
    }
    if optB is Option.None {
        return 20
    }
    if optC is Option.None {
        return 20
    }

    if optA is Option.Some(mut a) && optB is Option.Some(mut b) && optC is Option.Some(mut c) {
        let va = a.asView()
        let vb = b.asView()
        let mut vc = c.asViewMut()
        let okAdd = linalg.addInto(va, vb, vc)
        if !okAdd {
            return 21
        }
        if vc.get(0, 0) is Option.Some(val) {
            if val != 3.0 {
                return 22
            }
        } else {
            return 23
        }

        let mut scratch = arena.withCapacity(4096)
        let okGemm = linalg.gemmInto(va, vb, vc, scratch)
        if !okGemm {
            return 24
        }
        if vc.get(0, 0) is Option.Some(gval) {
            if gval != 4.0 {
                return 25
            }
        } else {
            return 26
        }
    }
    return 0
}

func main(): int {
    let r1 = testStaticMatrices()
    if r1 != 0 {
        return r1
    }
    let r2 = testDynamicMatrixAndViews()
    if r2 != 0 {
        return r2
    }
    return testLinalgKernels()
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_view = file_ide_diagnostics(&db, view_file);
    let diags_matrix = file_ide_diagnostics(&db, matrix_file);
    let diags_linalg = file_ide_diagnostics(&db, linalg_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_view: Vec<_> = diags_view.iter().filter(|d| d.severity == 0).collect();
    let error_matrix: Vec<_> = diags_matrix.iter().filter(|d| d.severity == 0).collect();
    let error_linalg: Vec<_> = diags_linalg.iter().filter(|d| d.severity == 0).collect();
    let error_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(error_view.is_empty(), "errors in view.aru: {error_view:?}");
    assert!(
        error_matrix.is_empty(),
        "errors in matrix.aru: {error_matrix:?}"
    );
    assert!(
        error_linalg.is_empty(),
        "errors in linalg.aru: {error_linalg:?}"
    );
    assert!(error_main.is_empty(), "errors in main.aru: {error_main:?}");
}
