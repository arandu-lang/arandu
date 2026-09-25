//! Unit tests for the type checker core (extracted from mod.rs).

use super::errors::constraint_to_diagnostic;
use super::provenance::{ProvenanceRole, causal_chain};
use super::types::InterfaceInfo;
use super::types::Primitive;
use super::*;
use crate::Span;
use crate::SymbolKind;
use crate::type_checker::info::translate_type;
use arandu_middle::DiagCode;
use arandu_middle::layout::{StructFieldInfo, StructFields};

// ── helpers ──

fn new_interner() -> TypeInterner {
    TypeInterner::new()
}

// ── PERF.5 Arc sharing ──

#[test]
fn type_check_result_clone_shares_arcs() {
    let a = TypeCheckResult::empty();
    let b = a.clone();
    assert!(Arc::ptr_eq(&a.symbols, &b.symbols));
    assert!(Arc::ptr_eq(&a.resolved, &b.resolved));
    assert!(Arc::ptr_eq(&a.type_info, &b.type_info));
    // make_mut detaches only the mutated field
    let mut c = a.clone();
    c.type_info_mut();
    assert!(!Arc::ptr_eq(&a.type_info, &c.type_info));
    assert!(Arc::ptr_eq(&a.symbols, &c.symbols));
}

fn empty_symbols() -> SymbolTable {
    SymbolTable::new(0)
}

fn dummy_span() -> Span {
    Span::new(0, 0, 0)
}

// ── TyCtx ──

#[test]
fn ty_ctx_bind_and_lookup() {
    let mut ctx = TyCtx::new();
    let sym = SymbolId::new(0, 0);
    let i = new_interner();
    let tid = i.intern(ArType::Primitive(Primitive::Int));
    ctx.bind(sym, tid);
    assert_eq!(ctx.lookup(sym), Some(tid));
}

#[test]
fn ty_ctx_lookup_missing_returns_none() {
    let ctx = TyCtx::new();
    assert_eq!(ctx.lookup(SymbolId::new(0, 999)), None);
}

#[test]
fn ty_ctx_return_stack() {
    let mut ctx = TyCtx::new();
    let i = new_interner();
    let int_id = i.intern(ArType::Primitive(Primitive::Int));
    let bool_id = i.intern(ArType::Primitive(Primitive::Bool));
    assert_eq!(ctx.current_return(), None);
    ctx.push_return(int_id, dummy_span());
    assert_eq!(ctx.current_return(), Some(int_id));
    ctx.push_return(bool_id, dummy_span());
    assert_eq!(ctx.current_return(), Some(bool_id));
    ctx.pop_return();
    assert_eq!(ctx.current_return(), Some(int_id));
    ctx.pop_return();
    assert_eq!(ctx.current_return(), None);
}

#[test]
fn ty_ctx_loop_tracking() {
    let mut ctx = TyCtx::new();
    assert!(!ctx.is_in_loop());
    ctx.enter_loop();
    assert!(ctx.is_in_loop());
    ctx.enter_loop();
    assert!(ctx.is_in_loop());
    ctx.exit_loop();
    assert!(ctx.is_in_loop());
    ctx.exit_loop();
    assert!(!ctx.is_in_loop());
}

#[test]
fn ty_ctx_exit_loop_does_not_underflow() {
    let mut ctx = TyCtx::new();
    ctx.exit_loop();
    assert!(!ctx.is_in_loop());
}

#[test]
fn ty_ctx_bind_sparse_local_id() {
    let mut ctx = TyCtx::new();
    let sym = SymbolId::new(0, 5);
    let i = new_interner();
    let tid = i.intern(ArType::Primitive(Primitive::Int));
    ctx.bind(sym, tid);
    assert_eq!(ctx.lookup(sym), Some(tid));
    assert_eq!(ctx.lookup(SymbolId::new(0, 4)), None);
}

/// Root multi-file fix: same `local_id` in different files must not collide.
#[test]
fn ty_ctx_isolates_by_file_id() {
    let mut ctx = TyCtx::new();
    let i = new_interner();
    let local_ty = i.intern(ArType::Ptr(i.intern(ArType::Primitive(Primitive::Byte))));
    let int_ty = i.intern(ArType::Primitive(Primitive::Int));
    let imported_fn = i.intern(ArType::func(&[int_ty], int_ty, &i));
    // Local binding and imported func share dense local_id=3, different file.
    let local = SymbolId::new(0, 3);
    let imported = SymbolId::new(7, 3);
    ctx.bind(local, local_ty);
    assert_eq!(ctx.lookup(local), Some(local_ty));
    // Import must NOT see the local's type (old bug: index by local_id only).
    assert_eq!(ctx.lookup(imported), None);
    ctx.bind(imported, imported_fn);
    assert_eq!(ctx.lookup(imported), Some(imported_fn));
    assert_eq!(ctx.lookup(local), Some(local_ty));
}

// ── TypeInfo ──

#[test]
fn type_info_record_and_lookup_expr() {
    let i = new_interner();
    let tid = i.intern(ArType::Primitive(Primitive::Int));
    let mut info = TypeInfo::with_interner(i);
    let eid = ExprId::new(3);
    info.record_expr_type(eid, tid);
    assert_eq!(info.expr_type(eid), Some(ArType::Primitive(Primitive::Int)));
    assert_eq!(info.expr_type_id(eid), Some(tid));
}

#[test]
fn type_info_missing_expr_returns_none() {
    let info = TypeInfo::new();
    assert_eq!(info.expr_type(ExprId::new(0)), None);
}

#[test]
fn type_info_record_and_lookup_decl() {
    let i = new_interner();
    let tid = i.intern(ArType::Primitive(Primitive::Bool));
    let mut info = TypeInfo::with_interner(i);
    let sym = SymbolId::new(0, 1);
    info.record_decl_type(sym, tid);
    assert_eq!(
        info.decl_type(sym),
        Some(ArType::Primitive(Primitive::Bool))
    );
    assert_eq!(info.decl_type_id(sym), Some(tid));
}

#[test]
fn type_info_missing_decl_returns_none() {
    let info = TypeInfo::new();
    assert_eq!(info.decl_type(SymbolId::new(0, 0)), None);
}

#[test]
fn type_info_pod_struct_is_copy_vec_like_is_not() {
    use std::sync::Arc;

    let mut info = TypeInfo::new();
    let i = &mut info.type_interner;
    let u32_ty = i.intern(ArType::Primitive(Primitive::U32));
    let int_ty = i.intern(ArType::Primitive(Primitive::Int));
    let ptr_ty = i.intern(ArType::Ptr(int_ty));
    let u64_ty = i.intern(ArType::Primitive(Primitive::U64));

    // GenRef-like: { index: u32, generation: u32 } → copy
    let gen_sym = SymbolId::new(0, 10);
    let gen_fields = StructFields::from_entries([
        StructFieldInfo {
            name: "index".into(),
            symbol: None,
            ty: u32_ty,
            index: 0,
        },
        StructFieldInfo {
            name: "generation".into(),
            symbol: None,
            ty: u32_ty,
            index: 1,
        },
    ]);
    info.struct_fields.insert(gen_sym, Arc::new(gen_fields));
    let gen_tid = info
        .type_interner
        .intern(ArType::named(gen_sym, &[], &info.type_interner));
    assert!(
        info.is_copy(gen_tid),
        "POD handle struct should be auto-copy"
    );

    // Vec-like: { data: ptr[int], len: u64 } → not copy
    let vec_sym = SymbolId::new(0, 11);
    let vec_fields = StructFields::from_entries([
        StructFieldInfo {
            name: "data".into(),
            symbol: None,
            ty: ptr_ty,
            index: 0,
        },
        StructFieldInfo {
            name: "len".into(),
            symbol: None,
            ty: u64_ty,
            index: 1,
        },
    ]);
    info.struct_fields.insert(vec_sym, Arc::new(vec_fields));
    let vec_tid = info
        .type_interner
        .intern(ArType::named(vec_sym, &[], &info.type_interner));
    assert!(
        !info.is_copy(vec_tid),
        "struct with ptr field must not be auto-copy"
    );

    // Empty unit struct → copy
    let unit_sym = SymbolId::new(0, 12);
    info.struct_fields
        .insert(unit_sym, Arc::new(StructFields::new()));
    let unit_tid = info
        .type_interner
        .intern(ArType::named(unit_sym, &[], &info.type_interner));
    assert!(info.is_copy(unit_tid), "empty struct is POD copy");

    // Bare ptr remains copy (cheap handle)
    assert!(info.is_copy(ptr_ty));

    // Borrow carriers preserve the permission contract structurally: shared
    // refs may be copied, exclusive refs must move.
    let shared_ref = info.type_interner.intern(ArType::Ref(int_ty));
    let exclusive_ref = info.type_interner.intern(ArType::RefMut(int_ty));
    assert!(info.is_copy(shared_ref));
    assert!(!info.is_copy(exclusive_ref));

    let view_sym = SymbolId::new(0, 13);
    let view_fields = StructFields::from_entries([StructFieldInfo {
        name: "value".into(),
        symbol: None,
        ty: shared_ref,
        index: 0,
    }]);
    info.struct_fields.insert(view_sym, Arc::new(view_fields));
    let view_ty = info
        .type_interner
        .intern(ArType::named(view_sym, &[], &info.type_interner));
    assert!(info.is_copy(view_ty), "shared-ref carrier should be copy");

    let mut_view_sym = SymbolId::new(0, 14);
    let mut_view_fields = StructFields::from_entries([StructFieldInfo {
        name: "value".into(),
        symbol: None,
        ty: exclusive_ref,
        index: 0,
    }]);
    info.struct_fields
        .insert(mut_view_sym, Arc::new(mut_view_fields));
    let mut_view_ty =
        info.type_interner
            .intern(ArType::named(mut_view_sym, &[], &info.type_interner));
    assert!(
        !info.is_copy(mut_view_ty),
        "exclusive-ref carrier must remain move-only"
    );
}

#[test]
fn type_info_enum_variant_tag() {
    let mut info = TypeInfo::new();
    info.record_enum_variant_tag(SymbolId::new(0, 0), 0);
    info.record_enum_variant_tag(SymbolId::new(0, 1), 1);
    assert_eq!(info.enum_variant_tags.get(&SymbolId::new(0, 0)), Some(&0));
    assert_eq!(info.enum_variant_tags.get(&SymbolId::new(0, 1)), Some(&1));
}

// ── translate_type ──

#[test]
fn translate_primitive() {
    let from = new_interner();
    let mut to = new_interner();
    let result = translate_type(&ArType::Primitive(Primitive::Int), &from, &mut to);
    assert_eq!(result, ArType::Primitive(Primitive::Int));
}

#[test]
fn translate_named_with_args() {
    let from = new_interner();
    let int_id = from.intern(ArType::Primitive(Primitive::Int));
    let named = ArType::named(SymbolId::new(0, 0), &[int_id], &from);
    let mut to = new_interner();
    let result = translate_type(&named, &from, &mut to);
    let expected_int = to.intern(ArType::Primitive(Primitive::Int));
    assert_eq!(
        result,
        ArType::named(SymbolId::new(0, 0), &[expected_int], &to)
    );
}

#[test]
fn translate_func() {
    let from = new_interner();
    let int_id = from.intern(ArType::Primitive(Primitive::Int));
    let void_id = from.intern(ArType::Void);
    let func = ArType::func(&[int_id], void_id, &from);
    let mut to = new_interner();
    let result = translate_type(&func, &from, &mut to);
    let expected_int = to.intern(ArType::Primitive(Primitive::Int));
    let expected_void = to.intern(ArType::Void);
    assert_eq!(result, ArType::func(&[expected_int], expected_void, &to));
}

#[test]
fn translate_nullable() {
    let from = new_interner();
    let inner = from.intern(ArType::Primitive(Primitive::Int));
    let nullable = ArType::Nullable(inner);
    let mut to = new_interner();
    let result = translate_type(&nullable, &from, &mut to);
    assert_eq!(
        result,
        ArType::Nullable(to.intern(ArType::Primitive(Primitive::Int)))
    );
}

#[test]
fn translate_slice_ptr_array_tuple_result_option_coroutine_range() {
    let from = new_interner();
    let int_id = from.intern(ArType::Primitive(Primitive::Int));
    let bool_id = from.intern(ArType::Primitive(Primitive::Bool));
    let variants: &[ArType] = &[
        ArType::Slice(int_id),
        ArType::Ptr(int_id),
        ArType::Array(5, int_id),
        ArType::tuple(&[int_id, bool_id], &from),
        ArType::Result(int_id, bool_id),
        ArType::Option(int_id),
        ArType::Coroutine(int_id),
        ArType::Range(int_id),
    ];
    let mut to = new_interner();
    for ty in variants {
        let result = translate_type(ty, &from, &mut to);
        assert!(!matches!(result, ArType::Error), "failed for {ty:?}");
    }
}

#[test]
fn translate_err_void_error_int_literal_float_literal() {
    let from = new_interner();
    let mut to = new_interner();
    for ty in &[
        ArType::Err,
        ArType::Void,
        ArType::Error,
        ArType::IntLiteral,
        ArType::FloatLiteral,
    ] {
        assert_eq!(translate_type(ty, &from, &mut to), *ty);
    }
}

// ── merge_from ──

#[test]
fn merge_from_decl_types() {
    let i = TypeInterner::new();
    let int_id = i.intern(ArType::Primitive(Primitive::Int));
    let mut from_info = TypeInfo::with_interner(i);
    let mut to_info = TypeInfo::new();
    from_info.decl_types.insert(SymbolId::new(0, 1), int_id);
    to_info.merge_from(&from_info);
    assert_eq!(
        to_info.decl_type(SymbolId::new(0, 1)),
        Some(ArType::Primitive(Primitive::Int))
    );
}

#[test]
fn merge_from_struct_fields() {
    let mut from_info = TypeInfo::new();
    let int_id = from_info
        .type_interner
        .intern(ArType::Primitive(Primitive::Int));
    from_info.struct_fields.insert(
        SymbolId::new(0, 0),
        std::sync::Arc::new(StructFields::from_entries([StructFieldInfo {
            name: "x".into(),
            symbol: None,
            ty: int_id,
            index: 0,
        }])),
    );
    let mut to_info = TypeInfo::new();
    to_info.merge_from(&from_info);
    let fields = to_info.struct_fields.get(&SymbolId::new(0, 0));
    assert!(fields.is_some());
    let tid = fields.unwrap().get("x").unwrap().ty;
    assert_eq!(
        to_info.type_interner.resolve(tid),
        ArType::Primitive(Primitive::Int)
    );
}

#[test]
fn merge_from_enum_variants() {
    let mut from_info = TypeInfo::new();
    from_info.enum_variants.insert(
        SymbolId::new(0, 1),
        (SymbolId::new(0, 0), EnumPayloadShape::Unit),
    );
    let mut to_info = TypeInfo::new();
    to_info.merge_from(&from_info);
    assert_eq!(
        to_info.enum_variants.get(&SymbolId::new(0, 1)),
        Some(&(SymbolId::new(0, 0), EnumPayloadShape::Unit))
    );
}

#[test]
fn merge_from_enum_payload_shape_tuple() {
    let mut from_info = TypeInfo::new();
    from_info.enum_variants.insert(
        SymbolId::new(0, 2),
        (
            SymbolId::new(0, 0),
            EnumPayloadShape::Tuple(vec![
                from_info
                    .type_interner
                    .intern(ArType::Primitive(Primitive::Int)),
            ]),
        ),
    );
    let mut to_info = TypeInfo::new();
    to_info.merge_from(&from_info);
    let variant = to_info.enum_variants.get(&SymbolId::new(0, 2));
    assert!(matches!(variant, Some((_, EnumPayloadShape::Tuple(_)))));
}

#[test]
fn merge_from_enum_variant_tags() {
    let mut from_info = TypeInfo::new();
    from_info.enum_variant_tags.insert(SymbolId::new(0, 0), 0);
    from_info.enum_variant_tags.insert(SymbolId::new(0, 1), 1);
    let mut to_info = TypeInfo::new();
    to_info.merge_from(&from_info);
    assert_eq!(
        to_info.enum_variant_tags.get(&SymbolId::new(0, 0)),
        Some(&0)
    );
    assert_eq!(
        to_info.enum_variant_tags.get(&SymbolId::new(0, 1)),
        Some(&1)
    );
}

#[test]
fn merge_from_generic_params() {
    let mut from_info = TypeInfo::new();
    from_info.generic_params.insert(
        SymbolId::new(0, 0),
        std::sync::Arc::new(vec![SymbolId::new(0, 1), SymbolId::new(0, 2)]),
    );
    let mut to_info = TypeInfo::new();
    to_info.merge_from(&from_info);
    assert_eq!(
        to_info
            .generic_params
            .get(&SymbolId::new(0, 0))
            .map(|a| a.as_slice()),
        Some([SymbolId::new(0, 1), SymbolId::new(0, 2)].as_slice())
    );
}

#[test]
fn merge_from_param_constraints() {
    let mut from_info = TypeInfo::new();
    let constraint = crate::type_checker::info::InterfaceConstraint {
        iface_sym: SymbolId::new(0, 2),
        type_args: smallvec::SmallVec::new(),
    };
    from_info.param_constraints.insert(
        SymbolId::new(0, 1),
        std::sync::Arc::new(vec![constraint.clone()]),
    );
    let mut to_info = TypeInfo::new();
    to_info.merge_from(&from_info);
    assert_eq!(
        to_info
            .param_constraints
            .get(&SymbolId::new(0, 1))
            .map(|a| a.as_slice()),
        Some([constraint].as_slice())
    );
}

#[test]
fn merge_from_interfaces() {
    let mut from_info = TypeInfo::new();
    from_info.interfaces.insert(
        SymbolId::new(0, 0),
        InterfaceInfo {
            self_param: None,
            methods: Vec::new(),
        },
    );
    let mut to_info = TypeInfo::new();
    to_info.merge_from(&from_info);
    assert!(to_info.interfaces.contains_key(&SymbolId::new(0, 0)));
}

#[test]
fn merge_from_empty_does_nothing() {
    let mut to_info = TypeInfo::new();
    let from_info = TypeInfo::new();
    to_info.merge_from(&from_info);
    assert!(to_info.decl_types.is_empty());
    assert!(to_info.struct_fields.is_empty());
}

// ── constraint_to_diagnostic ──

#[test]
fn constraint_assignment() {
    let i = new_interner();
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Str),
        origin: ConstraintOrigin::Assignment {
            lhs_span: Span::new(0, 0, 3),
            rhs_span: Span::new(0, 6, 9),
        },
    };
    let symbols = empty_symbols();
    let type_info = TypeInfo::with_interner(i);
    let diag = constraint_to_diagnostic(&constraint, &symbols, &type_info);
    assert_eq!(diag.code, DiagCode::T002IncompatibleAssignment);
    assert!(diag.message.contains("int"));
    assert!(diag.message.contains("str"));
    assert_eq!(diag.labels.len(), 2);
}

#[test]
fn constraint_call_arg() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Bool),
        origin: ConstraintOrigin::CallArg {
            call_span: dummy_span(),
            param_span: dummy_span(),
            arg_span: dummy_span(),
            arg_index: 0,
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T003IncompatibleCallArg);
    assert!(diag.message.contains("argument 1"));
    assert_eq!(diag.labels.len(), 2);
}

#[test]
fn constraint_return_type() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Void,
        found: ArType::Primitive(Primitive::Int),
        origin: ConstraintOrigin::ReturnType {
            return_span: dummy_span(),
            declared_span: dummy_span(),
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T004IncompatibleReturnType);
    assert_eq!(diag.hints.len(), 1);
}

#[test]
fn constraint_if_branches() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Str),
        origin: ConstraintOrigin::IfBranches {
            then_span: dummy_span(),
            else_span: dummy_span(),
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T007IfBranchMismatch);
}

#[test]
fn constraint_match_arms() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Bool),
        origin: ConstraintOrigin::MatchArms {
            first_span: dummy_span(),
            mismatch_span: dummy_span(),
            arm_index: 1,
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T008MatchArmMismatch);
    assert!(diag.message.contains("arm 2"));
}

#[test]
fn constraint_binary_op() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Str),
        origin: ConstraintOrigin::BinaryOp {
            op_span: dummy_span(),
            left_span: dummy_span(),
            right_span: dummy_span(),
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T005OperatorNotApplicable);
    // str + int suggests interpolation
    assert!(
        diag.hints
            .iter()
            .any(|h| h.message.contains("interpolation"))
    );
}

#[test]
fn constraint_unary_op() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Bool),
        found: ArType::Primitive(Primitive::Int),
        origin: ConstraintOrigin::UnaryOp {
            op: arandu_parser::UnaryOp::Neg,
            op_span: dummy_span(),
            operand_span: dummy_span(),
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T005OperatorNotApplicable);
}

#[test]
fn constraint_condition() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Bool),
        found: ArType::Primitive(Primitive::Int),
        origin: ConstraintOrigin::Condition { span: dummy_span() },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T009ConditionNotBool);
    assert!(diag.hints.iter().any(|h| h.message.contains("!=")));
}

#[test]
fn constraint_field_init() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Str),
        origin: ConstraintOrigin::FieldInit {
            struct_span: dummy_span(),
            field_name: "name".to_string(),
            field_span: dummy_span(),
            value_span: dummy_span(),
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T002IncompatibleAssignment);
    assert!(diag.message.contains("name"));
}

#[test]
fn constraint_set_target() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Bool),
        origin: ConstraintOrigin::SetTarget {
            place_span: dummy_span(),
            value_span: dummy_span(),
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T002IncompatibleAssignment);
}

#[test]
fn constraint_cast_expr() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Str),
        origin: ConstraintOrigin::CastExpr {
            expr_span: dummy_span(),
            target_span: dummy_span(),
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T010InvalidCast);
}

#[test]
fn constraint_implicit_widening() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Float),
        found: ArType::Primitive(Primitive::Int),
        origin: ConstraintOrigin::ImplicitWidening {
            source_span: dummy_span(),
            target_span: dummy_span(),
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T015ImplicitWidening);
    assert!(diag.hints.iter().any(|h| h.message.contains("explicit")));
}

#[test]
fn constraint_try_invalid() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Str),
        origin: ConstraintOrigin::TryInvalid { span: dummy_span() },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T016TryInvalid);
}

#[test]
fn constraint_await_invalid() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Str),
        origin: ConstraintOrigin::AwaitInvalid { span: dummy_span() },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T032AwaitInvalid);
}

#[test]
fn constraint_invalid_index_base_error() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Str),
        origin: ConstraintOrigin::InvalidIndex {
            base_span: dummy_span(),
            index_span: dummy_span(),
            is_base_error: true,
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T017InvalidIndex);
    assert!(diag.message.contains("cannot be indexed"));
}

#[test]
fn constraint_invalid_index_type() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Str),
        origin: ConstraintOrigin::InvalidIndex {
            base_span: dummy_span(),
            index_span: dummy_span(),
            is_base_error: false,
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T017InvalidIndex);
    assert!(diag.message.contains("index must be"));
}

#[test]
fn constraint_undefined_field() {
    let mut symbols = SymbolTable::new(0);
    let sym = symbols
        .define(ScopeId(0), "MyStruct", SymbolKind::Struct, dummy_span())
        .unwrap();
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Named(sym, arandu_middle::hir::IndexRange::empty()),
        found: ArType::Void,
        origin: ConstraintOrigin::UndefinedField {
            base_span: dummy_span(),
            field_span: dummy_span(),
            field_name: "x".to_string(),
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &symbols, &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T018UndefinedField);
    assert!(diag.message.contains("x"));
}

#[test]
fn constraint_array_literal() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Str),
        origin: ConstraintOrigin::ArrayLiteral {
            array_span: dummy_span(),
            item_span: dummy_span(),
            item_index: 1,
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T002IncompatibleAssignment);
    assert!(diag.message.contains("element 2"));
}

#[test]
fn constraint_null_coalesce() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Bool),
        origin: ConstraintOrigin::NullCoalesce {
            left_span: dummy_span(),
            right_span: dummy_span(),
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T002IncompatibleAssignment);
    assert!(diag.message.contains("??"));
}

#[test]
fn constraint_catch_handler() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Str),
        origin: ConstraintOrigin::CatchHandler {
            expr_span: dummy_span(),
            handler_span: dummy_span(),
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.code, DiagCode::T002IncompatibleAssignment);
    assert!(diag.message.contains("handler"));
}

// ── ConstraintOrigin ──

#[test]
fn constraint_origin_debug() {
    let origins: &[ConstraintOrigin] = &[
        ConstraintOrigin::Assignment {
            lhs_span: dummy_span(),
            rhs_span: dummy_span(),
        },
        ConstraintOrigin::CallArg {
            call_span: dummy_span(),
            param_span: dummy_span(),
            arg_span: dummy_span(),
            arg_index: 0,
        },
        ConstraintOrigin::ReturnType {
            return_span: dummy_span(),
            declared_span: dummy_span(),
        },
        ConstraintOrigin::IfBranches {
            then_span: dummy_span(),
            else_span: dummy_span(),
        },
        ConstraintOrigin::MatchArms {
            first_span: dummy_span(),
            mismatch_span: dummy_span(),
            arm_index: 0,
        },
        ConstraintOrigin::BinaryOp {
            op_span: dummy_span(),
            left_span: dummy_span(),
            right_span: dummy_span(),
        },
        ConstraintOrigin::UnaryOp {
            op: arandu_parser::UnaryOp::Neg,
            op_span: dummy_span(),
            operand_span: dummy_span(),
        },
        ConstraintOrigin::Condition { span: dummy_span() },
        ConstraintOrigin::FieldInit {
            struct_span: dummy_span(),
            field_name: "f".into(),
            field_span: dummy_span(),
            value_span: dummy_span(),
        },
        ConstraintOrigin::SetTarget {
            place_span: dummy_span(),
            value_span: dummy_span(),
        },
        ConstraintOrigin::CastExpr {
            expr_span: dummy_span(),
            target_span: dummy_span(),
        },
        ConstraintOrigin::ImplicitWidening {
            source_span: dummy_span(),
            target_span: dummy_span(),
        },
        ConstraintOrigin::TryInvalid { span: dummy_span() },
        ConstraintOrigin::TryReturnInvalid {
            span: dummy_span(),
            return_span: dummy_span(),
        },
        ConstraintOrigin::AwaitInvalid { span: dummy_span() },
        ConstraintOrigin::InvalidIndex {
            base_span: dummy_span(),
            index_span: dummy_span(),
            is_base_error: true,
        },
        ConstraintOrigin::UndefinedField {
            base_span: dummy_span(),
            field_span: dummy_span(),
            field_name: "f".into(),
        },
        ConstraintOrigin::ArrayLiteral {
            array_span: dummy_span(),
            item_span: dummy_span(),
            item_index: 0,
        },
        ConstraintOrigin::NullCoalesce {
            left_span: dummy_span(),
            right_span: dummy_span(),
        },
        ConstraintOrigin::CatchHandler {
            expr_span: dummy_span(),
            handler_span: dummy_span(),
        },
    ];
    for origin in origins {
        let _ = format!("{origin:?}");
    }
}

// ── Solver log (roadmap 4.1) ──

#[test]
fn solver_logs_failed_constraints_in_generation_order() {
    let pool = AstPool::default();
    let symbols = SymbolTable::new(0);
    let resolved = ResolvedNames::default();
    let mut checker = TypeChecker::new(
        symbols,
        resolved,
        Vec::new(),
        &pool,
        TargetInfo { pointer_width: 64 },
    );

    let int_id = checker.intern(ArType::Primitive(Primitive::Int));
    let str_id = checker.intern(ArType::Primitive(Primitive::Str));
    let s = dummy_span();

    checker.add_constraint(
        ArTypeOrId::Id(int_id),
        ArTypeOrId::Id(str_id),
        ConstraintOrigin::Assignment {
            lhs_span: s,
            rhs_span: s,
        },
    );
    checker.add_subtype_constraint(
        ArTypeOrId::Type(ArType::Primitive(Primitive::Int)),
        ArTypeOrId::Type(ArType::Primitive(Primitive::Str)),
        ConstraintOrigin::ReturnType {
            return_span: s,
            declared_span: s,
        },
    );

    assert_eq!(checker.diagnostics.len(), 2);
    assert_eq!(checker.solved_constraints().len(), 2);

    let first = &checker.solved_constraints()[0];
    let second = &checker.solved_constraints()[1];
    assert!(!first.constraint.is_subtype);
    assert!(second.constraint.is_subtype);
    assert_eq!(first.diag_index, 0);
    assert_eq!(second.diag_index, 1);
    assert_eq!(
        checker.diagnostics[first.diag_index].code,
        DiagCode::T002IncompatibleAssignment
    );
    assert_eq!(
        checker.diagnostics[second.diag_index].code,
        DiagCode::T004IncompatibleReturnType
    );
}

// ── Causal provenance (roadmap 4.2) ──

fn all_constraint_origins() -> Vec<ConstraintOrigin> {
    let s = dummy_span();
    vec![
        ConstraintOrigin::Assignment {
            lhs_span: s,
            rhs_span: s,
        },
        ConstraintOrigin::CallArg {
            call_span: s,
            param_span: s,
            arg_span: s,
            arg_index: 0,
        },
        ConstraintOrigin::ReturnType {
            return_span: s,
            declared_span: s,
        },
        ConstraintOrigin::IfBranches {
            then_span: s,
            else_span: s,
        },
        ConstraintOrigin::MatchArms {
            first_span: s,
            mismatch_span: s,
            arm_index: 1,
        },
        ConstraintOrigin::BinaryOp {
            op_span: s,
            left_span: s,
            right_span: s,
        },
        ConstraintOrigin::UnaryOp {
            op: arandu_parser::UnaryOp::Neg,
            op_span: s,
            operand_span: s,
        },
        ConstraintOrigin::Condition { span: s },
        ConstraintOrigin::FieldInit {
            struct_span: s,
            field_name: "name".to_string(),
            field_span: s,
            value_span: s,
        },
        ConstraintOrigin::SetTarget {
            place_span: s,
            value_span: s,
        },
        ConstraintOrigin::CastExpr {
            expr_span: s,
            target_span: s,
        },
        ConstraintOrigin::ImplicitWidening {
            source_span: s,
            target_span: s,
        },
        ConstraintOrigin::TryInvalid { span: s },
        ConstraintOrigin::TryReturnInvalid {
            span: s,
            return_span: s,
        },
        ConstraintOrigin::AwaitInvalid { span: s },
        ConstraintOrigin::InvalidIndex {
            base_span: s,
            index_span: s,
            is_base_error: true,
        },
        ConstraintOrigin::InvalidIndex {
            base_span: s,
            index_span: s,
            is_base_error: false,
        },
        ConstraintOrigin::UndefinedField {
            base_span: s,
            field_span: s,
            field_name: "nam".to_string(),
        },
        ConstraintOrigin::ArrayLiteral {
            array_span: s,
            item_span: s,
            item_index: 1,
        },
        ConstraintOrigin::NullCoalesce {
            left_span: s,
            right_span: s,
        },
        ConstraintOrigin::CatchHandler {
            expr_span: s,
            handler_span: s,
        },
    ]
}

#[test]
fn provenance_chain_is_total_and_ordered() {
    for origin in all_constraint_origins() {
        let constraint = Constraint {
            is_subtype: false,
            expected: ArType::Void,
            found: ArType::Primitive(Primitive::Int),
            origin,
        };
        let chain = causal_chain(&constraint);
        assert!(!chain.is_empty());
        let found_pos = chain
            .iter()
            .position(|step| step.role == ProvenanceRole::FoundOrigin)
            .expect("chain must contain a found-origin");
        if let Some(expected_pos) = chain
            .iter()
            .position(|step| step.role == ProvenanceRole::ExpectedOrigin)
        {
            assert!(
                expected_pos < found_pos,
                "expected-origin must precede found-origin"
            );
        }
        assert!(chain.iter().all(|step| !step.description.is_empty()));
    }
}

#[test]
fn assignment_note_renders_clean_flow_without_debug_spans() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Str),
        origin: ConstraintOrigin::Assignment {
            lhs_span: Span::new(0, 0, 3),
            rhs_span: Span::new(0, 6, 9),
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert!(
        diag.notes
            .iter()
            .any(|note| note == "flow: declaration → value"),
        "notes: {:?}",
        diag.notes
    );
    assert!(
        diag.notes.iter().all(|note| !note.contains("Span")),
        "notes must not leak debug span formatting"
    );
}

#[test]
fn field_init_diagnostic_carries_struct_context_label() {
    let constraint = Constraint {
        is_subtype: false,
        expected: ArType::Primitive(Primitive::Int),
        found: ArType::Primitive(Primitive::Str),
        origin: ConstraintOrigin::FieldInit {
            struct_span: dummy_span(),
            field_name: "name".to_string(),
            field_span: dummy_span(),
            value_span: dummy_span(),
        },
    };
    let diag = constraint_to_diagnostic(&constraint, &empty_symbols(), &TypeInfo::new());
    assert_eq!(diag.labels.len(), 3);
    assert!(diag.labels[0].message.contains("initializing field 'name'"));
}

#[test]
fn test_contextual_literal_overflow_diagnostic() {
    let source = r#"
    module test;
    func take(x: u8): u8 { return x; }
    func main(): u8 {
        let a: u8 = 255;
        let b: u8 = 256;
        return take(300);
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 64 });
    let overflow_diags: Vec<_> = check_res
        .diagnostics
        .iter()
        .filter(|d| d.code == crate::DiagCode::T038IntegerLiteralOutOfRange)
        .collect();
    assert_eq!(
        overflow_diags.len(),
        2,
        "256 and 300 must be flagged as out-of-range for u8"
    );
}

#[test]
fn test_contextual_literal_uint_overflow() {
    let source = r#"
    module test;
    func take(x: uint): uint { return x; }
    func main(): uint {
        let a: uint = 4_000_000_000;
        let b: uint = 4_000_000_001;
        return take(5_000_000_000);
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);

    // 64-bit target: todos os literais cabem em uint (< u64::MAX).
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 64 });
    let overflow_diags: Vec<_> = check_res
        .diagnostics
        .iter()
        .filter(|d| d.code == crate::DiagCode::T038IntegerLiteralOutOfRange)
        .collect();
    assert_eq!(
        overflow_diags.len(),
        0,
        "4_000_000_000 / 4_000_000_001 / 5_000_000_000 devem caber em uint 64-bit"
    );

    // 32-bit target: valores acima de u32::MAX estouram uint.
    let source = r#"
    module test;
    func take(x: uint): uint { return x; }
    func main(): uint {
        let a: uint = 4_294_967_296;
        let b: uint = 7_000_000_000;
        return take(8_000_000_000);
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 32 });
    let overflow_diags: Vec<_> = check_res
        .diagnostics
        .iter()
        .filter(|d| d.code == crate::DiagCode::T038IntegerLiteralOutOfRange)
        .collect();
    assert_eq!(
        overflow_diags.len(),
        3,
        "4_294_967_296 / 7_000_000_000 / 8_000_000_000 devem estourar uint 32-bit"
    );

    // 32-bit target: valores <= u32::MAX continuam válidos.
    let source = r#"
    module test;
    func main(): uint {
        return 4_294_967_295;
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 32 });
    let overflow_diags: Vec<_> = check_res
        .diagnostics
        .iter()
        .filter(|d| d.code == crate::DiagCode::T038IntegerLiteralOutOfRange)
        .collect();
    assert_eq!(overflow_diags.len(), 0, "u32::MAX cabe em uint 32-bit");
}

#[test]
fn test_impl_multiple_methods() {
    let source = r#"
    module test;
    struct Point { x: float; y: float }
    impl Point {
        public func new(x: float, y: float): Point { return Point { x, y } }
        public func getX(self: ref): float { return self.x }
        public func setX(self: mut ref, x: float) { self.x = x }
    }
    func main(): float {
        let p = Point.new(1.0, 2.0);
        return p.getX()
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 64 });
    let names: Vec<_> = program
        .decls
        .iter()
        .filter_map(|id| match program.pool.decl(*id) {
            arandu_parser::TopLevelDecl::Func(func) => match &func.name {
                arandu_parser::FuncName::Method { name, .. } => Some(name.as_str()),
                arandu_parser::FuncName::Free { .. } => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        names.len(),
        3,
        "impl should have 3 methods: new, getX, setX"
    );
    assert!(names.contains(&"new"), "new should be present");
    assert!(names.contains(&"getX"), "getX should be present");
    assert!(names.contains(&"setX"), "setX should be present");
    assert_eq!(
        check_res.diagnostics.len(),
        0,
        "impl with multiple methods should have zero diagnostics"
    );
}

#[test]
fn test_binary_op_contextual_literal_inference() {
    let source = r#"
    module test;
    func add_one(x: uint): uint {
        return x + 1;
    }
    func one_plus(x: uint): uint {
        return 1 + x;
    }
    func add_floats(x: f32): f32 {
        return x + 2.5;
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 64 });
    assert_eq!(
        check_res.diagnostics.len(),
        0,
        "x + 1, 1 + x, x + 2.5 should typecheck cleanly: {:?}",
        check_res.diagnostics
    );
}

#[test]
fn test_int_literal_to_float_expected() {
    let source = r#"
    module test;
    func main(): f32 {
        let a: f32 = 10;
        let b: f64 = 20;
        return a;
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 64 });
    assert_eq!(
        check_res.diagnostics.len(),
        0,
        "integer literals 10 and 20 under float context should synthesize cleanly as float: {:?}",
        check_res.diagnostics
    );
}

#[test]
fn test_array_literal_contextual_inference() {
    let source = r#"
    module test;
    func main(): uint {
        let arr: [3]uint = [1, 2, 3];
        return arr[0];
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 64 });
    assert_eq!(
        check_res.diagnostics.len(),
        0,
        "array literal [1, 2, 3] under [3]uint context should synthesize elements as uint: {:?}",
        check_res.diagnostics
    );
}

#[test]
fn test_vec_indexing_typecheck() {
    let source = r#"
    module test;
    public struct Vec<T> {
        data: ptr[T];
        len: uint;
        capacity: uint;
    }
    func test_indexing(v: Vec<int>): int {
        return v[0];
    }
    func test_indexed_assign(v: mut ref Vec<int>) {
        v[1] = 42;
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 64 });
    assert_eq!(
        check_res.diagnostics.len(),
        0,
        "Vec<T> indexing and assignment should typecheck cleanly: {:?}",
        check_res.diagnostics
    );
}

#[test]
fn test_option_try_and_null_coalesce_typecheck() {
    let source = r#"
    module test;
    func unwrap_or_default(opt: Option<int>, default_val: int): int {
        return opt ?? default_val;
    }
    func try_option(opt: Option<int>): Option<int> {
        let val: int = opt?;
        return .Some(val + 1);
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 64 });
    assert_eq!(
        check_res.diagnostics.len(),
        0,
        "Option `?` and `??` should typecheck cleanly: {:?}",
        check_res.diagnostics
    );
}

#[test]
fn result_try_requires_a_compatible_enclosing_return_type() {
    let source = r#"
    module test;
    func source(): Result<int, int> {
        return Result.Ok(1);
    }
    func valid_wrapper(): Result<int, int> {
        let value = source()?;
        return Result.Ok(value);
    }
    func invalid_wrapper(): int {
        let value = source()?;
        return value;
    }
    func incompatible_error(): Result<int, str> {
        let value = source()?;
        return Result.Ok(value);
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 64 });

    let try_diags: Vec<_> = check_res
        .diagnostics
        .iter()
        .filter(|diag| diag.code == DiagCode::T016TryInvalid)
        .collect();
    assert_eq!(
        try_diags.len(),
        2,
        "diagnostics: {:?}",
        check_res.diagnostics
    );
    assert_eq!(check_res.diagnostics.len(), 2);
    assert!(try_diags[0].message.contains("cannot propagate"));
}

#[test]
fn test_range_slicing_typecheck() {
    let source = r#"
    module test;
    public struct Vec<T> { data: ptr[T]; len: uint; capacity: uint; }
    func test_slicing(v: Vec<int>, arr: [5]int): []int {
        let s1: []int = v[1..4];
        let s2: []int = arr[0..2];
        return s1;
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 64 });
    assert_eq!(
        check_res.diagnostics.len(),
        0,
        "Range slicing on Vec and Array should typecheck cleanly: {:?}",
        check_res.diagnostics
    );
}

#[test]
fn test_option_safe_navigation_typecheck() {
    let source = r#"
    module test;
    public struct Address { street: str; zip: int; }
    public struct User { name: str; address: Address; }
    func get_zip(user: Option<User>): Option<int> {
        return user?.address?.zip;
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 64 });
    assert_eq!(
        check_res.diagnostics.len(),
        0,
        "Safe navigation on Option should typecheck cleanly: {:?}",
        check_res.diagnostics
    );
}

#[test]
fn test_struct_update_and_punning_typecheck() {
    let source = r#"
    module test;
    public struct Config { host: str; port: int; debug: bool; }
    func modify_config(original: Config, port: int): Config {
        let debug = true;
        let c1 = Config { host: "localhost", port, debug };
        let c2 = Config { port: 9000, ..original };
        return c2;
    }
    "#;
    let program = arandu_parser::parse(source).unwrap();
    let res = arandu_resolve::resolve_for_test(0, &program);
    let check_res = crate::type_check(res, &program, TargetInfo { pointer_width: 64 });
    assert_eq!(
        check_res.diagnostics.len(),
        0,
        "Struct update and field punning should typecheck cleanly: {:?}",
        check_res.diagnostics
    );
}
