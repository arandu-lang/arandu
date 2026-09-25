mod collect;
mod demangle;
mod expand;
mod graph;

pub use collect::analyze_instantiations;
pub use demangle::{demangle_symbol, mangle_symbol};
pub use expand::expand_specializations;
pub use graph::{
    InstantiationGraph, InstantiationKey, InstantiationNode, InstantiationNodeId, MonoError,
};

use arandu_diagnostics::Diagnostic;
use arandu_middle::hir::HirProgram;
use arandu_typeck::TypeCheckResult;

/// Full monomorphization: analyze instantiation graph, expand free-function
/// and method specializations into the HIR, and rewrite call sites.
///
/// Call after HIR lower and before AMIR lower. Generic **templates** stay in
/// the HIR but are skipped by AMIR lowering.
#[tracing::instrument(level = "debug", target = "arandu_semantics::mono", skip_all)]
pub fn monomorphize_program(
    tc: &mut TypeCheckResult,
    hir: &mut HirProgram,
) -> Result<usize, Vec<Diagnostic>> {
    let bump = bumpalo::Bump::new();
    let graph = analyze_instantiations(tc, hir, &bump)?;
    let result = expand_specializations(tc, hir, &graph, &bump);

    if arandu_base::perf::PRINT_ALLOC_STATS.load(std::sync::atomic::Ordering::Relaxed) {
        arandu_base::perf::track_alloc(bump.allocated_bytes());
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_lexer::Span;
    use arandu_middle::symbol_table::{SymbolId, SymbolKind, SymbolTable};
    use arandu_middle::types::{ArType, Primitive, TypeInterner};

    fn setup() -> (SymbolTable, TypeInterner) {
        (SymbolTable::new(0), TypeInterner::new())
    }

    #[test]
    fn test_graph_deduplication() {
        let (mut st, interner) = setup();
        let sym = define_symbol(&mut st, "identity");
        let int_id = interner.intern(ArType::Primitive(Primitive::Int));

        let bump = bumpalo::Bump::new();
        let mut graph = InstantiationGraph::new(&bump);
        let key = InstantiationKey {
            symbol: sym,
            type_args: bump.alloc_slice_copy(&[int_id]),
        };

        let id1 = graph.get_or_insert(&key, &bump, &interner, &st).unwrap();
        let id2 = graph.get_or_insert(&key, &bump, &interner, &st).unwrap();
        assert_eq!(id1, id2);
        assert!(!graph.is_empty());
    }

    #[test]
    fn test_different_type_args_different_nodes() {
        let (mut st, interner) = setup();
        let sym = define_symbol(&mut st, "identity");
        let int_id = interner.intern(ArType::Primitive(Primitive::Int));
        let str_id = interner.intern(ArType::Primitive(Primitive::Str));

        let bump = bumpalo::Bump::new();
        let mut graph = InstantiationGraph::new(&bump);
        let id1 = graph
            .get_or_insert(
                &InstantiationKey {
                    symbol: sym,
                    type_args: bump.alloc_slice_copy(&[int_id]),
                },
                &bump,
                &interner,
                &st,
            )
            .unwrap();
        let id2 = graph
            .get_or_insert(
                &InstantiationKey {
                    symbol: sym,
                    type_args: bump.alloc_slice_copy(&[str_id]),
                },
                &bump,
                &interner,
                &st,
            )
            .unwrap();
        assert_ne!(id1, id2);
        assert_eq!(graph.len(), 2);
    }

    #[test]
    fn test_recursion_limit() {
        let (mut st, interner) = setup();
        let sym = define_symbol(&mut st, "recursive");

        let bump = bumpalo::Bump::new();
        let mut graph = InstantiationGraph::with_recursion_limit(&bump, 3);
        let int_id = interner.intern(ArType::Primitive(Primitive::Int));
        for i in 0..3 {
            let tid = interner.intern(ArType::Array(i, int_id));
            graph
                .get_or_insert(
                    &InstantiationKey {
                        symbol: sym,
                        type_args: bump.alloc_slice_copy(&[tid]),
                    },
                    &bump,
                    &interner,
                    &st,
                )
                .unwrap();
        }

        let int_id = interner.intern(ArType::Primitive(Primitive::Int));
        let tid = interner.intern(ArType::Array(99, int_id));
        let result = graph.get_or_insert(
            &InstantiationKey {
                symbol: sym,
                type_args: bump.alloc_slice_copy(&[tid]),
            },
            &bump,
            &interner,
            &st,
        );
        assert_eq!(
            result,
            Err(MonoError::RecursionLimitExceeded {
                symbol: sym,
                limit: 3,
            })
        );
    }

    #[test]
    fn test_cycle_detection() {
        let (mut st, interner) = setup();
        let sym_a = define_symbol(&mut st, "funcA");
        let sym_b = define_symbol(&mut st, "funcB");
        let tid = interner.intern(ArType::Primitive(Primitive::Int));

        let bump = bumpalo::Bump::new();
        let mut graph = InstantiationGraph::new(&bump);
        let id_a = graph
            .get_or_insert(
                &InstantiationKey {
                    symbol: sym_a,
                    type_args: bump.alloc_slice_copy(&[tid]),
                },
                &bump,
                &interner,
                &st,
            )
            .unwrap();
        let id_b = graph
            .get_or_insert(
                &InstantiationKey {
                    symbol: sym_b,
                    type_args: bump.alloc_slice_copy(&[tid]),
                },
                &bump,
                &interner,
                &st,
            )
            .unwrap();

        graph.add_edge(id_a, id_b);
        graph.add_edge(id_b, id_a);

        let cycle = graph.find_cycle();
        assert!(cycle.is_some(), "expected cycle to be detected");
    }

    #[test]
    fn test_no_cycle_in_dag() {
        let (mut st, interner) = setup();
        let sym_a = define_symbol(&mut st, "funcA");
        let sym_b = define_symbol(&mut st, "funcB");
        let sym_c = define_symbol(&mut st, "funcC");
        let tid = interner.intern(ArType::Primitive(Primitive::Int));

        let bump = bumpalo::Bump::new();
        let mut graph = InstantiationGraph::new(&bump);
        let id_a = graph
            .get_or_insert(
                &InstantiationKey {
                    symbol: sym_a,
                    type_args: bump.alloc_slice_copy(&[tid]),
                },
                &bump,
                &interner,
                &st,
            )
            .unwrap();
        let id_b = graph
            .get_or_insert(
                &InstantiationKey {
                    symbol: sym_b,
                    type_args: bump.alloc_slice_copy(&[tid]),
                },
                &bump,
                &interner,
                &st,
            )
            .unwrap();
        let id_c = graph
            .get_or_insert(
                &InstantiationKey {
                    symbol: sym_c,
                    type_args: bump.alloc_slice_copy(&[tid]),
                },
                &bump,
                &interner,
                &st,
            )
            .unwrap();

        graph.add_edge(id_a, id_b);
        graph.add_edge(id_a, id_c);
        graph.add_edge(id_b, id_c);

        assert!(graph.find_cycle().is_none());
    }

    #[test]
    fn test_mangling_simple() {
        let (mut st, interner) = setup();
        let sym = define_symbol(&mut st, "identity");
        let tid = interner.intern(ArType::Primitive(Primitive::Int));

        let bump = bumpalo::Bump::new();
        let key = InstantiationKey {
            symbol: sym,
            type_args: bump.alloc_slice_copy(&[tid]),
        };

        let mangled = mangle_symbol(&key, &interner, &st);
        assert!(mangled.starts_with("_A$identity$I"));
        assert!(mangled.ends_with("$E"));
        assert!(mangled.contains("int"));
    }

    #[test]
    fn test_mangling_multi_arg() {
        let (mut st, interner) = setup();
        let sym = define_symbol(&mut st, "swap");
        let int_id = interner.intern(ArType::Primitive(Primitive::Int));
        let str_id = interner.intern(ArType::Primitive(Primitive::Str));

        let bump = bumpalo::Bump::new();
        let key = InstantiationKey {
            symbol: sym,
            type_args: bump.alloc_slice_copy(&[int_id, str_id]),
        };

        let mangled = mangle_symbol(&key, &interner, &st);
        assert!(mangled.contains("int"));
        assert!(mangled.contains("str"));
    }

    #[test]
    fn test_demangle_roundtrip() {
        let demangled = demangle_symbol("_A$identity$I_int_$E");
        assert!(demangled.is_some());
        let s = demangled.unwrap();
        assert_eq!(s, "identity<int>");
    }

    #[test]
    fn test_demangle_with_underscores() {
        let demangled = demangle_symbol("_A$identity$I_my_struct_T_other_struct_$E");
        assert!(demangled.is_some());
        let s = demangled.unwrap();
        assert_eq!(s, "identity<my_struct, other_struct>");
    }

    #[test]
    fn test_mangled_names_are_unique() {
        let (mut st, interner) = setup();
        let sym = define_symbol(&mut st, "identity");
        let int_id = interner.intern(ArType::Primitive(Primitive::Int));
        let bool_id = interner.intern(ArType::Primitive(Primitive::Bool));

        let bump = bumpalo::Bump::new();
        let key_int = InstantiationKey {
            symbol: sym,
            type_args: bump.alloc_slice_copy(&[int_id]),
        };
        let key_bool = InstantiationKey {
            symbol: sym,
            type_args: bump.alloc_slice_copy(&[bool_id]),
        };

        let mangled_int = mangle_symbol(&key_int, &interner, &st);
        let mangled_bool = mangle_symbol(&key_bool, &interner, &st);
        assert_ne!(mangled_int, mangled_bool);
    }

    #[test]
    fn generic_instances_with_same_function_name_in_different_modules_do_not_collide() {
        let interner = TypeInterner::new();
        let int_id = interner.intern(ArType::Primitive(Primitive::Int));
        let (mut vec_symbols, _) = setup();
        let vec_len = define_symbol(&mut vec_symbols, "len");
        vec_symbols
            .host_function_names
            .insert(vec_len, "std.alloc.vec.len".into());
        let (mut slice_symbols, _) = setup();
        let slice_len = define_symbol(&mut slice_symbols, "len");
        slice_symbols
            .host_function_names
            .insert(slice_len, "std.core.slice.len".into());

        let bump = bumpalo::Bump::new();
        let vec_key = InstantiationKey {
            symbol: vec_len,
            type_args: bump.alloc_slice_copy(&[int_id]),
        };
        let slice_key = InstantiationKey {
            symbol: slice_len,
            type_args: bump.alloc_slice_copy(&[int_id]),
        };

        assert_ne!(
            mangle_symbol(&vec_key, &interner, &vec_symbols),
            mangle_symbol(&slice_key, &interner, &slice_symbols),
            "module-qualified function identities must distinguish generic instances"
        );
    }

    #[test]
    fn test_analyze_instantiations_collects_hir_generic_call() {
        let src = r#"
func identity<T>(value: T): T {
    return value
}

func main() {
    let x: int = identity<int>(42)
}
"#;
        let program = arandu_parser::parse(src).expect("parse failed");
        let resolution = arandu_resolve::resolve_for_test(0, &program);
        let mut tc = crate::passes::type_checker::type_check(
            resolution,
            &program,
            arandu_typeck::type_checker::TargetInfo { pointer_width: 64 },
        );
        assert!(
            tc.diagnostics.is_empty(),
            "type check failed: {:?}",
            tc.diagnostics
        );
        let hir =
            crate::passes::lower_hir::lower_to_hir(&mut tc, &program).expect("HIR lowering failed");

        let bump = bumpalo::Bump::new();
        let graph = analyze_instantiations(&tc, &hir, &bump).expect("analysis failed");

        assert!(!graph.is_empty());
        assert!(
            graph
                .iter()
                .any(|node| node.mangled_name.contains("identity"))
        );
    }

    #[test]
    fn test_deep_generic_recursion_and_arena_performance() {
        let mut src = String::new();
        src.push_str("func f0() {\n    f1<int>(42)\n}\n");
        for i in 1..10 {
            src.push_str(&format!(
                "func f{i}<T>(val: T) {{\n    f{}<T>(val)\n}}\n",
                i + 1
            ));
        }
        src.push_str("func f10<T>(val: T) {\n}\n");

        let program = arandu_parser::parse(&src).expect("parse failed");
        let resolution = arandu_resolve::resolve_for_test(0, &program);
        let mut tc = crate::passes::type_checker::type_check(
            resolution,
            &program,
            arandu_typeck::type_checker::TargetInfo { pointer_width: 64 },
        );
        assert!(
            tc.diagnostics.is_empty(),
            "type check failed: {:?}",
            tc.diagnostics
        );
        let hir =
            crate::passes::lower_hir::lower_to_hir(&mut tc, &program).expect("HIR lowering failed");

        let bump = bumpalo::Bump::new();
        let graph = analyze_instantiations(&tc, &hir, &bump).expect("analysis failed");
        assert!(!graph.is_empty());

        let allocated_bytes = bump.allocated_bytes();
        assert!(
            allocated_bytes > 0,
            "Expected bump arena to have allocated bytes, got 0"
        );
    }

    fn define_symbol(st: &mut SymbolTable, name: &str) -> SymbolId {
        st.define(
            st.global_scope(),
            name,
            SymbolKind::Func,
            Span::new(0, 0, 0),
        )
        .unwrap()
    }
}
