use super::super::TypeChecker;
use super::super::types::{ArType, Primitive};
use arandu_parser::Program;

pub(crate) fn register_prelude(checker: &mut TypeChecker<'_>, _program: &Program) {
    // RC-ANY-PRELUDE: I/O prelude calls stay `(str) -> void`. ToStr v0.1
    // coerces formatable primitives at the call site; user Display is later.
    // Do not use `Any` as a silent universal sink.
    let void_id = checker.intern(ArType::Void);
    let str_id = checker.intern(ArType::Primitive(Primitive::Str));
    let err_literal_id = checker.intern(ArType::Err);

    let result_str_err = checker.intern(ArType::Result(str_id, err_literal_id));
    let result_void_err = checker.intern(ArType::Result(void_id, err_literal_id));

    let println_ty = ArType::func(&[str_id], void_id, &checker.type_info.type_interner);
    let create_ty = ArType::func(&[str_id], result_str_err, &checker.type_info.type_interner);
    let remove_ty = ArType::func(&[str_id], result_void_err, &checker.type_info.type_interner);
    let err_new_ty = ArType::func(&[str_id], err_literal_id, &checker.type_info.type_interner);

    for (module, members_with_types) in [
        (
            "io",
            vec![
                ("println", println_ty.clone()),
                ("create", create_ty),
                ("remove", remove_ty),
                ("eprint", println_ty),
            ],
        ),
        ("err", vec![("new", err_new_ty)]),
    ] {
        for (member, ty) in members_with_types {
            if let Some(symbol_id) = checker.symbols.lookup_module_member(module, member) {
                let ty_id = checker.intern(ty);
                checker.record_decl_type(symbol_id, ty_id);
            }
        }
    }
}
