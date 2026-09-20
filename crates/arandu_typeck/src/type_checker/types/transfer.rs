//! Conservative storage proof for the canonical Copy/Send/Sync bounds.
//!
//! This does not prove a task's effects or a borrowed scope's lifetime. Raw
//! storage, views, coroutine captures and resource destructors require separate
//! contracts before they can be admitted by these bounds.
use std::collections::VecDeque;

use arandu_middle::symbol_table::LangItem;
use rustc_hash::FxHashSet;

use super::{ArType, Primitive, build_subst_ids, substitute_type_id};
use crate::SymbolKind;
use crate::type_checker::{TypeChecker, info::EnumPayloadShape};

fn scalar(ty: &ArType) -> Option<bool> {
    match ty {
        ArType::Primitive(primitive) => Some(match primitive {
            Primitive::Str | Primitive::Any => false,
            Primitive::Int
            | Primitive::Uint
            | Primitive::Float
            | Primitive::I8
            | Primitive::I16
            | Primitive::I32
            | Primitive::I64
            | Primitive::U8
            | Primitive::U16
            | Primitive::U32
            | Primitive::U64
            | Primitive::F32
            | Primitive::F64
            | Primitive::Bool
            | Primitive::Byte
            | Primitive::Char => true,
        }),
        // Literal inference can still be pending at bound checking; every
        // numeric representation it can select has scalar storage.
        ArType::Void | ArType::IntLiteral | ArType::FloatLiteral | ArType::Const(_) => Some(true),
        _ => None,
    }
}

pub(super) fn satisfies(
    checker: &TypeChecker<'_>,
    concrete: &ArType,
    capability: LangItem,
) -> bool {
    if let Some(result) = scalar(concrete) {
        return result;
    }
    let info = &checker.type_info;
    let interner = &info.type_interner;
    // BFS records shortest proof paths, so enum hash-map iteration order cannot
    // change the depth cutoff. Interned IDs prevent repeated structural work.
    const MAX_PROOF_DEPTH: usize = 256;
    const MAX_PROOF_TYPES: usize = 4096;
    let mut pending = VecDeque::from([(interner.intern(concrete.clone()), 0usize)]);
    let mut seen = FxHashSet::default();
    while let Some((id, depth)) = pending.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        if depth > MAX_PROOF_DEPTH || seen.len() > MAX_PROOF_TYPES {
            return false;
        }
        let ty = interner.resolve(id);
        if let Some(result) = scalar(&ty) {
            if !result {
                return false;
            }
            continue;
        }
        match ty {
            ArType::Named(symbol, arguments) => {
                let Some(declaration) = checker.symbols.try_get(symbol) else {
                    return false;
                };
                if declaration.kind == SymbolKind::TypeParam {
                    let proven = info.param_constraints.get(&symbol).is_some_and(|bounds| {
                        bounds.iter().any(|bound| {
                            bound.type_args.is_empty()
                                && checker.symbols.try_get(bound.iface_sym).is_some_and(
                                    |interface| interface.lang_item == Some(capability),
                                )
                        })
                    });
                    if !proven {
                        return false;
                    }
                    continue;
                }
                if declaration.lang_item == Some(LangItem::TaskHandle) {
                    return false;
                }
                let arguments = interner.type_args(arguments);
                match declaration.lang_item {
                    Some(LangItem::String) => {
                        if capability == LangItem::Copy || !arguments.is_empty() {
                            return false;
                        }
                        continue;
                    }
                    Some(LangItem::Vec) => {
                        if capability == LangItem::Copy {
                            return false;
                        }
                        pending.extend(arguments.iter().map(|&argument| (argument, depth + 1)));
                        continue;
                    }
                    _ => {}
                }
                if info.destructors.contains_key(&symbol) {
                    return false;
                }
                // Include phantom arguments: an empty wrapper must not launder
                // a resource's negative transfer constraint.
                pending.extend(arguments.iter().map(|&argument| (argument, depth + 1)));
                let substitution = if let Some(parameters) = info.generic_params.get(&symbol) {
                    if parameters.len() != arguments.len() {
                        return false;
                    }
                    Some(build_subst_ids(parameters, &arguments, interner))
                } else {
                    if !arguments.is_empty() {
                        return false;
                    }
                    None
                };
                let instantiate = |field| {
                    substitution
                        .as_ref()
                        .map_or(field, |subst| substitute_type_id(field, subst, interner))
                };
                match declaration.kind {
                    SymbolKind::Struct => {
                        let Some(fields) = info.struct_fields.get(&symbol) else {
                            return false;
                        };
                        pending.extend(
                            fields
                                .fields
                                .iter()
                                .map(|field| (instantiate(field.ty), depth + 1)),
                        );
                    }
                    SymbolKind::Enum => {
                        for (owner, payload) in info.enum_variants.values() {
                            if *owner == symbol
                                && let EnumPayloadShape::Tuple(fields) = payload
                            {
                                pending.extend(
                                    fields.iter().map(|&field| (instantiate(field), depth + 1)),
                                );
                            }
                        }
                    }
                    _ => return false,
                }
            }
            ArType::Array(_, inner)
            | ArType::ConstArray(_, inner)
            | ArType::Option(inner)
            | ArType::Range(inner) => {
                pending.push_back((inner, depth + 1));
            }
            ArType::Result(ok, error) => {
                pending.push_back((ok, depth + 1));
                pending.push_back((error, depth + 1));
            }
            ArType::Tuple(elements) => {
                pending.extend(
                    interner
                        .type_args(elements)
                        .into_iter()
                        .map(|id| (id, depth + 1)),
                );
            }
            ArType::Ptr(_)
            | ArType::Ref(_)
            | ArType::RefMut(_)
            | ArType::Slice(_)
            | ArType::Nullable(_)
            | ArType::Coroutine(_)
            | ArType::Poll(_)
            | ArType::Func(_, _)
            | ArType::GenRef
            | ArType::ConstParam(_)
            | ArType::Err
            | ArType::Error
            | ArType::IntLiteral
            | ArType::FloatLiteral => return false,
            ArType::Primitive(_) | ArType::Void | ArType::Const(_) => {} // handled by scalar above
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::type_checker::{ResolvedNames, TargetInfo};
    use arandu_lexer::Span;
    use arandu_middle::layout::{StructFieldInfo, StructFields};
    use arandu_middle::symbol_table::SymbolTable;
    use arandu_parser::ast_pool::AstPool;
    use std::sync::Arc;

    #[test]
    fn expanding_generic_storage_exhausts_proof_without_recursive_walk() {
        let pool = AstPool::default();
        let mut symbols = SymbolTable::new(0);
        let scope = symbols.global_scope();
        let node = symbols
            .define(scope, "Node", SymbolKind::Struct, Span::new(0, 0, 0))
            .unwrap();
        let parameter = symbols
            .define(scope, "T", SymbolKind::TypeParam, Span::new(0, 0, 0))
            .unwrap();
        let mut checker = TypeChecker::new(
            symbols,
            ResolvedNames::default(),
            Vec::new(),
            &pool,
            TargetInfo { pointer_width: 64 },
        );
        let interner = &checker.type_info.type_interner;
        let parameter_type = interner.intern(ArType::named(parameter, &[], interner));
        let nested = interner.intern(ArType::Option(parameter_type));
        let field = interner.intern(ArType::named(node, &[nested], interner));
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let root = ArType::named(node, &[int], interner);
        checker
            .type_info
            .generic_params
            .insert(node, Arc::new(vec![parameter]));
        checker.type_info.struct_fields.insert(
            node,
            Arc::new(StructFields::from_entries([StructFieldInfo {
                name: "next".into(),
                symbol: None,
                ty: field,
                index: 0,
            }])),
        );
        // Deliberately bypass declaration validation: Node<T> contains
        // Node<Option<T>>. Every expansion has a fresh TypeId, so deduplication
        // alone cannot terminate this malformed graph.
        assert!(!satisfies(&checker, &root, LangItem::Send));
        assert!(!satisfies(&checker, &root, LangItem::Sync));
    }
}
