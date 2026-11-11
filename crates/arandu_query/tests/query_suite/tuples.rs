//! Tuples as values: `(a, b)`, `(A, B)`, `t.0` and `for i, x in xs`.
use arandu_query::db::DatabaseImpl;

fn errors(source: &str) -> Vec<String> {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("tuples.aru".to_string(), source.to_string());
    arandu_query::file_ide_diagnostics(&db, file)
        .iter()
        .filter(|diagnostic| diagnostic.severity == 0)
        .map(|diagnostic| diagnostic.code.clone())
        .collect()
}

#[test]
fn tuples_are_typed_by_position() {
    let source = "func swap(p: (int, bool)): (bool, int) { return (p.1, p.0) }\n\
        func main(): int {\n    let t = (1, true)\n    let s: (bool, int) = swap(t)\n\
        let (flag, n) = s\n    let mut m = (1, 2)\n    m.1 = 5\n\
        if flag { return n + m.0 + m.1 - 7 }\n    return 1\n}\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn an_element_outside_the_tuple_is_t018() {
    let source = "func main(): int {\n    let t = (1, 2)\n    return t.2\n}\n";
    assert_eq!(errors(source), ["T018"]);
}

#[test]
fn elements_keep_their_own_types() {
    let source = "func main(): int {\n    let t = (1, true)\n    return t.1\n}\n";
    assert_eq!(errors(source), ["T004"]);
}

#[test]
fn a_for_loop_binds_a_position_and_an_element() {
    let ok = "func main(): int {\n    let xs = [1, 2]\n    let mut n: usize = 0\n\
        for i, x in xs { n += i + x as usize }\n    return n as int - 4\n}\n";
    assert_eq!(errors(ok), Vec::<String>::new());
    let three =
        "func main(): int {\n    let xs = [1, 2]\n    for i, x, y in xs { }\n    return 0\n}\n";
    assert_eq!(errors(three), ["T012"]);
}
