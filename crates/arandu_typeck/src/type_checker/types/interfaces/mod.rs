mod check;
mod collect;
mod method;
mod satisfy;

use crate::SymbolId;

#[derive(Debug, Clone)]
pub struct InterfaceMethod {
    pub name: smol_str::SmolStr,
    pub sig_id: crate::type_checker::types::TypeId,
    pub generic_params: Vec<SymbolId>,
}

#[derive(Debug, Clone)]
pub struct InterfaceInfo {
    pub self_param: Option<SymbolId>,
    pub sealed: bool,
    /// Method specifications for the interface.
    pub methods: Vec<InterfaceMethod>,
}

pub(crate) use check::check_instantiation_constraints;
pub use collect::collect_interfaces_and_constraints;
pub(crate) use method::instantiate_interface_method;

#[cfg(test)]
use super::ArType;
#[cfg(test)]
use crate::passes::type_checker::TypeChecker;
#[cfg(test)]
use satisfy::missing_interface_methods;

#[cfg(test)]
mod tests {
    use super::satisfy::type_satisfies_interface;
    use super::*;
    use crate::type_checker::ResolvedNames;
    use crate::type_checker::types::Primitive;
    use arandu_lexer::Span;
    use arandu_middle::symbol_table::{Symbol, SymbolTable};
    use arandu_middle::{ScopeId, SymbolId, SymbolKind};
    use arandu_parser::Program;
    use arandu_parser::ast_pool::AstPool;

    #[test]
    fn test_collect_interfaces_and_constraints_empty() {
        let pool = AstPool::default();
        let symbols = SymbolTable::new(0);
        let resolved = ResolvedNames::default();
        let mut checker = TypeChecker::new(
            symbols,
            resolved,
            Vec::new(),
            &pool,
            crate::type_checker::TargetInfo { pointer_width: 64 },
        );

        let program = Program {
            span: Span::new(0, 0, 0),
            module: None,
            imports: Vec::new(),
            interface_impls: Vec::new(),
            decls: Vec::new(),
            docs: Vec::new(),
            pool: AstPool::default(),
        };

        collect_interfaces_and_constraints(&mut checker, &program);
        assert!(checker.type_info.interfaces.is_empty());
    }

    #[test]
    fn test_type_satisfies_interface() {
        let pool = AstPool::default();
        let mut symbols = SymbolTable::new(0);

        let iface_sym = SymbolId::new(1, 0);
        let iface_symbol = Symbol {
            id: iface_sym,
            name: "Reader".into(),
            kind: SymbolKind::Interface,
            span: Span::new(0, 0, 0),
            scope: ScopeId(0),
            visibility: arandu_parser::Visibility::Public,
            lang_item: None,
        };
        symbols.register_imported_symbol(iface_symbol);

        let struct_sym_id = SymbolId::new(1, 1);
        let struct_symbol = Symbol {
            id: struct_sym_id,
            name: "MyStruct".into(),
            kind: SymbolKind::Struct,
            span: Span::new(0, 0, 0),
            scope: ScopeId(0),
            visibility: arandu_parser::Visibility::Public,
            lang_item: None,
        };
        symbols.register_imported_symbol(struct_symbol);

        let self_sym_id = SymbolId::new(1, 99);
        let self_symbol = Symbol {
            id: self_sym_id,
            name: "Self".into(),
            kind: SymbolKind::TypeParam,
            span: Span::new(0, 0, 0),
            scope: ScopeId(0),
            visibility: arandu_parser::Visibility::Public,
            lang_item: None,
        };
        symbols.register_imported_symbol(self_symbol);

        let method_sym_id = SymbolId::new(1, 2);
        let method_symbol = Symbol {
            id: method_sym_id,
            name: "read".into(),
            kind: SymbolKind::Func,
            span: Span::new(0, 0, 0),
            scope: ScopeId(0),
            visibility: arandu_parser::Visibility::Public,
            lang_item: None,
        };
        symbols.register_imported_symbol(method_symbol);

        let mut associated = rustc_hash::FxHashMap::default();
        associated.insert((struct_sym_id, "read".into()), method_sym_id);
        symbols.associated_members = associated;

        let resolved = ResolvedNames::default();
        let mut checker = TypeChecker::new(
            symbols,
            resolved,
            Vec::new(),
            &pool,
            crate::type_checker::TargetInfo { pointer_width: 64 },
        );

        let self_type_id = checker.intern(ArType::named(
            struct_sym_id,
            &[],
            &checker.type_info.type_interner,
        ));
        let self_interface_type_id = checker.intern(ArType::named(
            self_sym_id,
            &[],
            &checker.type_info.type_interner,
        ));
        let req_method_type = ArType::func(
            &[self_interface_type_id],
            checker.intern(ArType::Primitive(Primitive::Int)),
            &checker.type_info.type_interner,
        );
        let prov_method_type = ArType::func(
            &[self_type_id],
            checker.intern(ArType::Primitive(Primitive::Int)),
            &checker.type_info.type_interner,
        );
        let req_method_type_id = checker.intern(req_method_type);
        let prov_method_type_id = checker.intern(prov_method_type);

        checker
            .type_info
            .decl_types
            .insert(method_sym_id, prov_method_type_id);

        let iface_info = InterfaceInfo {
            self_param: Some(self_sym_id),
            sealed: false,
            methods: vec![InterfaceMethod {
                name: "read".into(),
                sig_id: req_method_type_id,
                generic_params: Vec::new(),
            }],
        };
        checker.type_info.interfaces.insert(iface_sym, iface_info);

        let concrete = ArType::named(struct_sym_id, &[], &checker.type_info.type_interner);
        assert!(type_satisfies_interface(
            &mut checker,
            &concrete,
            iface_sym,
            &[],
            Span::new(0, 0, 0)
        ));

        checker
            .type_info
            .interfaces
            .get_mut(&iface_sym)
            .expect("interface collected")
            .sealed = true;
        assert!(!type_satisfies_interface(
            &mut checker,
            &concrete,
            iface_sym,
            &[],
            Span::new(0, 0, 0)
        ));
        std::sync::Arc::make_mut(&mut checker.symbols)
            .interface_implementations
            .insert((struct_sym_id, iface_sym));
        assert!(type_satisfies_interface(
            &mut checker,
            &concrete,
            iface_sym,
            &[],
            Span::new(0, 0, 0)
        ));

        std::sync::Arc::make_mut(&mut checker.symbols)
            .associated_members
            .clear();
        assert!(!type_satisfies_interface(
            &mut checker,
            &concrete,
            iface_sym,
            &[],
            Span::new(0, 0, 0)
        ));

        let missing = missing_interface_methods(&mut checker, &concrete, iface_sym, &[]);
        assert_eq!(missing, vec!["read".to_string()]);
    }
}
