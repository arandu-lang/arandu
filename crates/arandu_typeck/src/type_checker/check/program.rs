use arandu_parser::{Program, TopLevelDecl};

use super::super::TypeChecker;
use super::super::types::collect_interfaces_and_constraints;
use super::collect::{collect_signature_types, collect_type_shapes};
use super::func::check_func_body;
use super::prelude::register_prelude;
use super::validate::validate_top_level_any;

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, program))]
pub fn check_signatures(checker: &mut TypeChecker<'_>, program: &Program) {
    register_prelude(checker, program);
    collect_type_shapes(checker, program);
    // T2.1: register `generic_defaults` / constraints before func signature types
    // so `func new<T>(): Vec<T>` expands to `Vec<T, GlobalAllocator>` at collect time.
    collect_interfaces_and_constraints(checker, program);
    collect_signature_types(checker, program);

    duplicate_module_member_info(checker, program);
    super::validate::validate_type_constraints_in_program(checker, program);
}

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, program))]
pub fn check_bodies(checker: &mut TypeChecker<'_>, program: &Program) {
    for decl_id in &program.decls {
        let decl = checker.pool.decl(*decl_id);
        validate_top_level_any(checker, decl);

        match decl {
            TopLevelDecl::Func(func_decl) => {
                check_func_body(checker, func_decl);
            }
            TopLevelDecl::Const(const_decl) => {
                let val_ty = super::super::synth::synth_expr(checker, const_decl.value);
                let const_key = crate::NodeKey::from(const_decl.span);
                if let Some(&symbol_id) = checker.resolved.definitions.get(&const_key) {
                    checker.record_decl_type(symbol_id, val_ty);
                    if let Some(var_id) = checker.literal_table.var_for_expr(const_decl.value) {
                        checker.literal_table.bind_symbol(symbol_id, var_id);
                    }
                }
                checker.finalize_literal_vars();
            }
            TopLevelDecl::Extern(extern_decl)
                if arandu_parser::AbiKind::from_abi_str(&extern_decl.abi)
                    == arandu_parser::AbiKind::AranduIntrinsic =>
            {
                let module_name = program
                    .module
                    .as_ref()
                    .map(|m| m.path.join("."))
                    .unwrap_or_default();
                if !module_name.starts_with("std.core") {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::U001FeatureNotSupported,
                        "the 'arandu-intrinsic' ABI is restricted to the std.core module"
                            .to_string(),
                        extern_decl.span,
                    ));
                }
            }
            _ => {}
        }
    }
}

fn duplicate_module_member_info(checker: &mut TypeChecker<'_>, program: &Program) {
    let Some(module) = &program.module else {
        return;
    };
    let module_name = module.path.join(".");

    for decl_id in &program.decls {
        let decl = checker.pool.decl(*decl_id);
        let (name, span) = match decl {
            TopLevelDecl::Struct(d) => (&d.name, d.span),
            TopLevelDecl::Enum(d) => (&d.name, d.span),
            TopLevelDecl::TypeAlias(d) => (&d.name, d.span),
            TopLevelDecl::Func(d) => match &d.name {
                arandu_parser::FuncName::Free { name, span } => (name, *span),
                _ => continue,
            },
            TopLevelDecl::Extern(d) => {
                for member in &d.members {
                    let name = &member.name;
                    let span = member.span;
                    let name_key = crate::NodeKey::from(span);
                    if let Some(&free_id) = checker.resolved.definitions.get(&name_key)
                        && let Some(member_id) =
                            checker.symbols.lookup_module_member(&module_name, name)
                        && free_id != member_id
                    {
                        if let Some(ty_id) = checker.decl_type_id(free_id) {
                            checker.record_decl_type(member_id, ty_id);
                        }
                        if let Some(params) =
                            checker.type_info.generic_params.get(&free_id).cloned()
                        {
                            checker.type_info.generic_params.insert(member_id, params);
                        }
                    }
                }
                continue;
            }
            _ => continue,
        };

        let name_key = crate::NodeKey::from(span);
        if let Some(&free_id) = checker.resolved.definitions.get(&name_key)
            && let Some(member_id) = checker.symbols.lookup_module_member(&module_name, name)
            && free_id != member_id
        {
            if let Some(ty_id) = checker.decl_type_id(free_id) {
                checker.record_decl_type(member_id, ty_id);
            }
            if let Some(params) = checker.type_info.generic_params.get(&free_id).cloned() {
                checker.type_info.generic_params.insert(member_id, params);
            }
            if let Some(fields) = checker.type_info.struct_fields.get(&free_id).cloned() {
                checker.type_info.struct_fields.insert(member_id, fields);
            }
        }
    }
}
