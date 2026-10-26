use std::sync::Arc;

use super::super::types::{ArType, TypeId, TypeInterner};
use super::TypeInfo;
use arandu_middle::layout::{StructFieldInfo, StructFields};

/// Re-intern `ty` from `from` into `to`, recursively translating nested TypeIds.
///
/// Used when merging type info across files / HIR module linking so TypeIds from
/// one interner become valid in another.
pub(super) fn reintern_type(dest: &mut TypeInfo, source: &TypeInfo, id: TypeId) -> TypeId {
    let ty = source.type_interner.resolve(id);
    let translated = translate_type(&ty, &source.type_interner, &mut dest.type_interner);
    dest.type_interner.intern(translated)
}

/// Copy synthesized capture symbols and their field layouts into `dest`.
///
/// Generic instances such as `std.thread.spawn` live in another file. Their
/// monomorphization needs the capture struct, `run`, and the field types, and
/// must not merge the caller's expression-type vector.
pub fn import_thread_captures(
    dest: &mut super::super::TypeCheckResult,
    src: &super::super::TypeCheckResult,
) {
    if src.type_info.thread_captures.is_empty() {
        return;
    }
    for symbol in src.symbols.iter() {
        if symbol.id.is_synthetic() {
            dest.symbols_mut().ensure_visible(symbol.clone());
        }
    }
    for ((parent, member), method) in &src.symbols.associated_members {
        if parent.is_synthetic() || method.is_synthetic() {
            dest.symbols_mut()
                .associated_members
                .insert((*parent, member.clone()), *method);
        }
    }
    for (symbol, name) in &src.symbols.host_function_names {
        if symbol.is_synthetic() {
            dest.symbols_mut()
                .host_function_names
                .insert(*symbol, name.clone());
        }
    }
    let declarations = src
        .type_info
        .decl_types
        .iter()
        .filter(|(symbol, _)| symbol.is_synthetic())
        .map(|(symbol, ty)| (*symbol, *ty))
        .collect::<Vec<_>>();
    for (symbol, ty) in declarations {
        let translated = reintern_type(dest.type_info_mut(), &src.type_info, ty);
        dest.type_info_mut().record_decl_type(symbol, translated);
    }
    let owners = src
        .type_info
        .struct_fields
        .keys()
        .copied()
        .filter(|symbol| symbol.is_synthetic())
        .collect::<Vec<_>>();
    for owner in owners {
        if dest.type_info.struct_fields.contains_key(&owner) {
            continue;
        }
        let Some(fields) = src.type_info.struct_fields.get(&owner) else {
            continue;
        };
        let translated = StructFields::from_entries(fields.iter().map(|field| StructFieldInfo {
            name: field.name.clone(),
            symbol: field.symbol,
            index: field.index,
            ty: reintern_type(dest.type_info_mut(), &src.type_info, field.ty),
        }));
        dest.type_info_mut()
            .struct_fields
            .insert(owner, Arc::new(translated));
    }
}

pub fn translate_type(ty: &ArType, from: &TypeInterner, to: &mut TypeInterner) -> ArType {
    match ty {
        ArType::Primitive(p) => ArType::Primitive(*p),
        ArType::Named(id, args) => {
            let old_args = from.type_args(*args);
            let new_args = old_args
                .iter()
                .map(|&arg_id| {
                    let resolved = from.resolve(arg_id);
                    let translated = translate_type(&resolved, from, to);
                    to.intern(translated)
                })
                .collect::<Vec<_>>();
            ArType::named(*id, &new_args, to)
        }
        ArType::Func(params, ret) => {
            let old_params = from.type_args(*params);
            let new_params = old_params
                .iter()
                .map(|&param_id| {
                    let resolved = from.resolve(param_id);
                    let translated = translate_type(&resolved, from, to);
                    to.intern(translated)
                })
                .collect::<Vec<_>>();
            let resolved_ret = from.resolve(*ret);
            let translated_ret = translate_type(&resolved_ret, from, to);
            let new_ret = to.intern(translated_ret);
            ArType::func(&new_params, new_ret, to)
        }
        ArType::Nullable(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Nullable(new_inner)
        }
        ArType::Slice(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Slice(new_inner)
        }
        ArType::MutSlice(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::MutSlice(new_inner)
        }
        ArType::Array(n, inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Array(*n, new_inner)
        }
        ArType::ConstArray(param, inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::ConstArray(*param, new_inner)
        }
        ArType::Const(value) => ArType::Const(*value),
        ArType::FrozenConst(value) => ArType::FrozenConst(Arc::clone(value)),
        ArType::ConstParam(param) => ArType::ConstParam(*param),
        ArType::Ptr(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Ptr(new_inner)
        }
        ArType::Ref(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Ref(new_inner)
        }
        ArType::RefMut(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::RefMut(new_inner)
        }
        ArType::GenRef => ArType::GenRef,
        ArType::Tuple(items) => {
            let old_items = from.type_args(*items);
            let new_items = old_items
                .iter()
                .map(|&item_id| {
                    let resolved = from.resolve(item_id);
                    let translated = translate_type(&resolved, from, to);
                    to.intern(translated)
                })
                .collect::<Vec<_>>();
            ArType::tuple(&new_items, to)
        }
        ArType::Result(ok, err) => {
            let resolved_ok = from.resolve(*ok);
            let translated_ok = translate_type(&resolved_ok, from, to);
            let new_ok = to.intern(translated_ok);

            let resolved_err = from.resolve(*err);
            let translated_err = translate_type(&resolved_err, from, to);
            let new_err = to.intern(translated_err);

            ArType::Result(new_ok, new_err)
        }
        ArType::Option(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Option(new_inner)
        }
        ArType::Coroutine(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Coroutine(new_inner)
        }
        ArType::Poll(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Poll(new_inner)
        }
        ArType::Range(inner) => {
            let resolved = from.resolve(*inner);
            let translated = translate_type(&resolved, from, to);
            let new_inner = to.intern(translated);
            ArType::Range(new_inner)
        }
        ArType::Err => ArType::Err,
        ArType::Void => ArType::Void,
        ArType::Error => ArType::Error,
        ArType::IntLiteral => ArType::IntLiteral,
        ArType::Vector(vector) => ArType::Vector(*vector),
        ArType::FloatLiteral => ArType::FloatLiteral,
    }
}
