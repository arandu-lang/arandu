//! Pure semantic contract for compiler-discovered tests.
//!
//! Discovery is driven by validated annotations and resolved function types.
//! Filesystem traversal, package-qualified IDs and execution belong to the CLI.

use crate::attributes::{AnnotationId, ValidatedAnnotations, annotation_spec};
use crate::{DiagCode, Diagnostic, SmolStr, SymbolId, TypeInfo, types::ArType};
use arandu_middle::Span;
use arandu_parser::{FuncName, TopLevelDecl};

/// One semantically valid test function in a source module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCase {
    pub symbol: SymbolId,
    pub name: SmolStr,
    pub span: Span,
}

/// One semantically valid benchmark function in a source module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchmarkCase {
    pub symbol: SymbolId,
    pub name: SmolStr,
    pub span: Span,
}

/// Result of applying the `@Test` contract to one declaration.
#[derive(Debug, Default)]
pub struct TestValidation {
    pub case: Option<TestCase>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Result of applying the `@Benchmark` contract to one declaration.
#[derive(Debug, Default)]
pub struct BenchmarkValidation {
    pub case: Option<BenchmarkCase>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Validate and discover one benchmark without performing I/O or querying Salsa.
#[must_use]
pub fn validate_benchmark_case(
    decl: &TopLevelDecl,
    annotations: &ValidatedAnnotations,
    symbol: SymbolId,
    type_info: &TypeInfo,
    symbols: &crate::SymbolTable,
) -> BenchmarkValidation {
    if !annotations.contains(AnnotationId::Benchmark) {
        return BenchmarkValidation::default();
    }
    let TopLevelDecl::Func(function) = decl else {
        return BenchmarkValidation::default();
    };
    let name = match &function.name {
        FuncName::Free { name, .. } => name.clone(),
        FuncName::Method { .. } => return BenchmarkValidation::default(),
    };
    let annotation_span = function
        .attrs
        .iter()
        .find(|attribute| {
            annotation_spec(&attribute.name).is_some_and(|spec| spec.id == AnnotationId::Benchmark)
        })
        .map_or(function.span, |attribute| attribute.name_span);

    let signature = type_info.decl_type(symbol);
    let signature_has_error = matches!(
        signature.as_ref(),
        Some(ArType::Func(parameters, return_type))
            if type_info.type_interner.type_args(*parameters).iter().any(|parameter| {
                matches!(type_info.resolve_type_id(*parameter), ArType::Error)
            }) || matches!(type_info.resolve_type_id(*return_type), ArType::Error)
    );
    let valid_type = match signature {
        Some(ArType::Func(parameters, return_type)) if parameters.len == 1 => {
            let parameter_ty_id = type_info
                .type_interner
                .type_args(parameters)
                .first()
                .copied();
            let parameter = match parameter_ty_id {
                Some(tid) => type_info.resolve_type_id(tid),
                None => ArType::Error,
            };
            let benchmark_named = match parameter {
                ArType::RefMut(inner) => match type_info.resolve_type_id(inner) {
                    ArType::Named(type_symbol, arguments) if arguments.is_empty() => {
                        let fields = type_info.struct_fields.get(&type_symbol);
                        symbols.get(type_symbol).name.rsplit('.').next() == Some("Benchmark")
                            && fields.is_some_and(|fields| {
                                fields.len() == 1
                                    && fields.iter().all(|f| {
                                        matches!(
                                            type_info.resolve_type_id(f.ty),
                                            ArType::Primitive(crate::types::Primitive::Int)
                                        )
                                    })
                            })
                    }
                    _ => false,
                },
                _ => false,
            };
            benchmark_named && matches!(type_info.resolve_type_id(return_type), ArType::Void)
        }
        _ => false,
    };
    let valid = !function.is_async
        && function.generic_params.is_empty()
        && function.params.len() == 1
        && function.params[0].ownership == Some(arandu_parser::Ownership::Mut)
        && valid_type;

    if valid && !annotations.has_errors() {
        return BenchmarkValidation {
            case: Some(BenchmarkCase {
                symbol,
                name,
                span: function.span,
            }),
            diagnostics: Vec::new(),
        };
    }
    if signature_has_error || annotations.has_errors() {
        return BenchmarkValidation::default();
    }

    BenchmarkValidation {
        case: None,
        diagnostics: vec![
            Diagnostic::error(
                DiagCode::T037InvalidBenchmarkContract,
                "invalid @Benchmark contract",
                annotation_span,
            )
            .with_label(
                function.span,
                "benchmark must be a synchronous, non-generic free function with one mutable Benchmark context",
            )
            .with_note("accepted signature is 'func name(mut bench: testing.Benchmark): void'")
            .with_hint("change the function signature or remove '@Benchmark'"),
        ],
    }
}

/// Validate and discover one test without performing I/O or querying Salsa.
#[must_use]
pub fn validate_test_case(
    decl: &TopLevelDecl,
    annotations: &ValidatedAnnotations,
    symbol: SymbolId,
    type_info: &TypeInfo,
) -> TestValidation {
    if !annotations.contains(AnnotationId::Test) {
        return TestValidation::default();
    }
    let TopLevelDecl::Func(function) = decl else {
        return TestValidation::default();
    };
    let name = match &function.name {
        FuncName::Free { name, .. } => name.clone(),
        FuncName::Method { .. } => return TestValidation::default(),
    };
    let annotation_span = function
        .attrs
        .iter()
        .find(|attribute| {
            annotation_spec(&attribute.name).is_some_and(|spec| spec.id == AnnotationId::Test)
        })
        .map_or(function.span, |attribute| attribute.name_span);

    let signature = type_info.decl_type(symbol);
    let signature_has_error = matches!(
        signature.as_ref(),
        Some(ArType::Func(_, return_type))
            if matches!(type_info.resolve_type_id(*return_type), ArType::Error)
    );
    let valid_type = match signature {
        Some(ArType::Func(parameters, return_type)) if parameters.is_empty() => {
            match type_info.resolve_type_id(return_type) {
                ArType::Void => true,
                ArType::Result(ok, _) => {
                    matches!(type_info.resolve_type_id(ok), ArType::Void)
                }
                _ => false,
            }
        }
        _ => false,
    };
    let valid = !function.is_async
        && function.generic_params.is_empty()
        && function.params.is_empty()
        && valid_type;

    if valid && !annotations.has_errors() {
        return TestValidation {
            case: Some(TestCase {
                symbol,
                name,
                span: function.span,
            }),
            diagnostics: Vec::new(),
        };
    }
    if signature_has_error || annotations.has_errors() {
        return TestValidation::default();
    }

    TestValidation {
        case: None,
        diagnostics: vec![
            Diagnostic::error(
                DiagCode::T036InvalidTestContract,
                "invalid @Test contract",
                annotation_span,
            )
            .with_label(
                function.span,
                "test must be a synchronous, non-generic free function with no parameters",
            )
            .with_note("accepted returns are 'void' and 'Result<void, E>'")
            .with_hint("change the function signature or remove '@Test'"),
        ],
    }
}
