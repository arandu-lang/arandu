#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use arandu_middle::types::ArType;
use arandu_semantics::{lower_to_hir, monomorphize_program, resolve_for_test, type_check};

#[test]
fn generic_method_typechecks_and_monos() {
    let src = r#"
struct Holder {
    v: int
}

func Holder.id_val<T>(shared self, x: T): T {
    return x
}

func main(): int {
    let b = Holder { v: 10 }
    let n = b.id_val<int>(32)
    return n + b.v
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        tc.diagnostics.is_empty(),
        "typeck diags: {:?}",
        tc.diagnostics
    );
    let mut hir = lower_to_hir(&mut tc, &program).expect("hir");
    hir.validate_invariants(&hir.pool, &tc.symbols)
        .expect("HIR invariants before mono");
    let n = monomorphize_program(&mut tc, &mut hir).expect("mono");
    assert!(n >= 1, "expected at least one specialization, got {n}");
    hir.validate_invariants(&hir.pool, &tc.symbols)
        .expect("HIR invariants after mono");
}

#[test]
fn generic_method_dual_int_str_specializations() {
    let src = r#"
struct Holder {
    v: int
}

func Holder.id_val<T>(shared self, x: T): T {
    return x
}

func main(): int {
    let b = Holder { v: 1 }
    let n = b.id_val<int>(41)
    let s = b.id_val<str>("hi")
    return n + 1
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        tc.diagnostics.is_empty(),
        "typeck diags: {:?}",
        tc.diagnostics
    );
    let mut hir = lower_to_hir(&mut tc, &program).expect("hir");
    hir.validate_invariants(&hir.pool, &tc.symbols)
        .expect("HIR invariants before mono");
    let n = monomorphize_program(&mut tc, &mut hir).expect("mono");
    assert!(n >= 2, "expected int+str specializations, got {n}");
    hir.validate_invariants(&hir.pool, &tc.symbols)
        .expect("HIR invariants after mono");

    // Specialized methods appear as mangled free funcs (receiver already in params).
    let mut mangled = 0usize;
    for &did in &hir.decls {
        if let arandu_middle::hir::HirDecl::Func(f) = hir.pool.decl(did) {
            let name = tc.symbols.get(f.symbol).name.as_str();
            if name.starts_with("_A$") {
                mangled += 1;
                assert!(
                    !tc.type_info.generic_params.contains_key(&f.symbol),
                    "specialized `{name}` must not keep generic_params"
                );
                let ret = tc.type_info.type_interner.resolve(f.return_type);
                assert!(
                    matches!(
                        ret,
                        ArType::Primitive(_) | ArType::IntLiteral | ArType::FloatLiteral
                    ),
                    "expected concrete return on `{name}`, got {ret:?}"
                );
            }
        }
    }
    assert!(mangled >= 2, "expected >=2 mangled methods, got {mangled}");
}

#[test]
fn generic_method_lowers_to_amir_and_reuses_shared_receiver() {
    let src = r#"
struct Holder {
    v: int
}

func Holder.id_val<T>(shared self, x: T): T {
    return x
}

func main(): int {
    let b = Holder { v: 10 }
    let n = b.id_val<int>(32)
    return n + b.v
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(tc.diagnostics.is_empty(), "{:?}", tc.diagnostics);
    let mut hir = lower_to_hir(&mut tc, &program).expect("hir");
    monomorphize_program(&mut tc, &mut hir).expect("mono");
    match arandu_semantics::lower_to_amir(&tc, &hir, 8) {
        Ok(amir) => {
            assert!(!amir.funcs.is_empty(), "expected AMIR funcs");
            assert!(
                amir.funcs
                    .iter()
                    .any(|f| tc.symbols.get(f.symbol).name == "main"),
                "need main"
            );
            assert!(
                amir.funcs.iter().any(|f| {
                    let name = tc.symbols.get(f.symbol).name.as_str();
                    name.starts_with("_A$") && name.contains("id_val")
                }),
                "need mangled method specialization in AMIR"
            );
        }
        Err(diags) => panic!("lower_to_amir failed: {diags:?}"),
    }
}

#[test]
fn generic_struct_method_monos() {
    let src = r#"
struct BoxG<T> {
    v: T
}

func BoxG.get(shared self): T {
    return self.v
}

func main(): int {
    let b = BoxG { v: 42 }
    return b.get()
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(tc.diagnostics.is_empty(), "typeck: {:?}", tc.diagnostics);
    let mut hir = lower_to_hir(&mut tc, &program).expect("hir");
    let n = monomorphize_program(&mut tc, &mut hir).expect("mono");
    assert!(n >= 1, "expected specialization, got {n}");
    let amir = arandu_semantics::lower_to_amir(&tc, &hir, 8).expect("amir");
    assert!(
        amir.funcs
            .iter()
            .any(|f| tc.symbols.get(f.symbol).name == "main"),
        "need main"
    );
    assert!(
        amir.funcs.iter().any(|f| {
            let name = tc.symbols.get(f.symbol).name.as_str();
            name.starts_with("_A$") && name.contains("get")
        }),
        "need mangled get specialization"
    );
}

#[test]
fn free_func_type_arg_inference_monos() {
    let src = r#"
func id<T>(x: T): T {
    return x
}

func main(): int {
    return id(41) + 1
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(tc.diagnostics.is_empty(), "typeck: {:?}", tc.diagnostics);
    let mut hir = lower_to_hir(&mut tc, &program).expect("hir");
    let n = monomorphize_program(&mut tc, &mut hir).expect("mono");
    assert!(n >= 1, "expected id specialization, got {n}");
}

/// Nested free-func mono: outer `push_t<int>` must specialize inner `ensure_cap<int>`.
#[test]
fn nested_free_func_mono_specializes_callee() {
    let src = r#"
struct V<T> {
    n: int
}

func ensure_cap<T>(mut v: V<T>, min: int): void {
    if v.n < min {
        v.n = min
    }
}

func push_t<T>(mut v: V<T>, _x: T): void {
    ensure_cap<T>(v, 1)
}

func main(): int {
    let mut v = V<int> { n: 0 }
    push_t<int>(v, 10)
    return v.n
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(tc.diagnostics.is_empty(), "typeck: {:?}", tc.diagnostics);
    let mut hir = lower_to_hir(&mut tc, &program).expect("hir");
    let n = monomorphize_program(&mut tc, &mut hir).expect("mono");
    assert!(
        n >= 2,
        "expected push_t + ensure_cap specializations, got {n}"
    );
    let amir = arandu_semantics::lower_to_amir(&tc, &hir, 8).expect("amir");
    let names: Vec<_> = amir
        .funcs
        .iter()
        .map(|f| tc.symbols.get(f.symbol).name.clone())
        .collect();
    assert!(
        names
            .iter()
            .any(|s| s.contains("push_t") && s.contains("int")),
        "missing push_t<int> in {names:?}"
    );
    assert!(
        names
            .iter()
            .any(|s| s.contains("ensure_cap") && s.contains("int")),
        "missing ensure_cap<int> nested mono in {names:?}"
    );
}

#[test]
fn generic_struct_dual_int_str_methods() {
    let src = r#"
struct BoxG<T> {
    v: T
}

func BoxG.get(shared self): T {
    return self.v
}

func main(): int {
    let a = BoxG { v: 41 }
    let b = BoxG { v: "hi" }
    let s = b.get()
    return a.get() + 1
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(tc.diagnostics.is_empty(), "typeck: {:?}", tc.diagnostics);
    let mut hir = lower_to_hir(&mut tc, &program).expect("hir");
    let n = monomorphize_program(&mut tc, &mut hir).expect("mono");
    assert!(n >= 2, "expected int+str specializations, got {n}");
    let amir = arandu_semantics::lower_to_amir(&tc, &hir, 8).expect("amir");
    let mangled: Vec<_> = amir
        .funcs
        .iter()
        .map(|f| tc.symbols.get(f.symbol).name.clone())
        .filter(|n| n.starts_with("_A$"))
        .collect();
    assert!(
        mangled.iter().any(|n| n.contains("int")),
        "missing int mono: {mangled:?}"
    );
    assert!(
        mangled.iter().any(|n| n.contains("str")),
        "missing str mono: {mangled:?}"
    );
}

#[test]
fn generic_destructor_is_registered_per_concrete_receiver() {
    let src = r#"
struct Resource<T> { value: T }

@Destructor
func Resource.dispose(own self): void {}

func consume_int(own value: Resource<int>): void {}
func consume_str(own value: Resource<str>): void {}

func main(): void {
    consume_int(Resource<int> { value: 1 })
    consume_str(Resource<str> { value: "ok" })
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(tc.diagnostics.is_empty(), "typeck: {:?}", tc.diagnostics);
    let mut hir = lower_to_hir(&mut tc, &program).expect("hir");
    monomorphize_program(&mut tc, &mut hir).expect("mono");

    let instances: Vec<_> = tc
        .type_info
        .destructor_instances
        .iter()
        .map(|(ty, symbol)| {
            (
                format!("{:?}", tc.type_info.resolve_type_id(*ty)),
                tc.symbols.get(*symbol).name.clone(),
            )
        })
        .collect();
    assert!(
        instances.iter().any(|(_, name)| name.contains("$I_int_")),
        "missing concrete int destructor: {instances:?}"
    );
    assert!(
        instances.iter().any(|(_, name)| name.contains("$I_str_")),
        "missing concrete str destructor: {instances:?}"
    );
}
