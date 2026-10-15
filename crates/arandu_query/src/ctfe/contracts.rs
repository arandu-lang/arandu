//! Canonical instantiated declarations. Only this query boundary evaluates
//! headers; pure consumers publish demands and receive immutable contracts.
use std::sync::Arc;

use crate::{db::HashEq, ArandCompilerDb, SourceFile, StableHash};
use arandu_middle::{
    types::{FunctionInstance, TypeShape},
    Diagnostic, Severity, Span, SymbolId,
};
use arandu_typeck::type_checker::info::{ConcreteHeader, EnumPayloadShape};

#[derive(Debug, Clone)]
struct Field {
    name: String,
    symbol: Option<SymbolId>,
    index: usize,
    shape: TypeShape,
}

#[derive(Debug, Clone)]
pub(crate) struct HeaderContract {
    signature: TypeShape,
    fields: Option<Vec<Field>>,
    variants: Vec<(SymbolId, usize, Vec<TypeShape>)>,
}

#[derive(Debug)]
struct ContractResult {
    result: Result<HeaderContract, Vec<Diagnostic>>,
}

impl StableHash for ContractResult {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = blake3::Hasher::new();
        h.update(b"ConcreteHeader/v1");
        let number = |h: &mut blake3::Hasher, value: usize| {
            h.update(&u64::try_from(value).unwrap_or(u64::MAX).to_le_bytes());
        };
        match &self.result {
            Err(errors) => {
                h.update(&[0]);
                h.update(errors.stable_hash().as_bytes());
            }
            Ok(contract) => {
                h.update(&[1]);
                let shape = |h: &mut blake3::Hasher, shape: &TypeShape| {
                    h.update(
                        FunctionInstance {
                            definition: SymbolId::new(0, 0),
                            arguments: vec![shape.clone()],
                        }
                        .stable_hash()
                        .as_bytes(),
                    );
                };
                shape(&mut h, &contract.signature);
                h.update(&[u8::from(contract.fields.is_some())]);
                if let Some(fields) = &contract.fields {
                    number(&mut h, fields.len());
                    for field in fields {
                        h.update(field.name.as_bytes());
                        h.update(&[0]);
                        number(&mut h, field.index);
                        h.update(&[u8::from(field.symbol.is_some())]);
                        if let Some(symbol) = field.symbol {
                            h.update(&symbol.file_id.to_le_bytes());
                            h.update(&symbol.local_id.0.to_le_bytes());
                        }
                        shape(&mut h, &field.shape);
                    }
                }
                number(&mut h, contract.variants.len());
                for (symbol, tag, items) in &contract.variants {
                    h.update(&symbol.file_id.to_le_bytes());
                    h.update(&symbol.local_id.0.to_le_bytes());
                    number(&mut h, *tag);
                    number(&mut h, items.len());
                    for item in items {
                        shape(&mut h, item);
                    }
                }
            }
        }
        h.finalize()
    }
}

fn failure(span: Span, message: &str) -> Vec<Diagnostic> {
    vec![Diagnostic::error(
        arandu_middle::DiagCode::T042UnsupportedComptime,
        message,
        span,
    )]
}

#[salsa::tracked]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db, file, context),
    fields(query = "concrete_header_contract")
)]
fn concrete_header_contract(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    key: FunctionInstance,
    context: super::DependencyContext,
) -> HashEq<ContractResult> {
    let build = || {
        let parsed = crate::passes::parse(db, file);
        let program = (**parsed).as_ref().map_err(|_| {
            failure(
                Span::new(*file.file_id(db), 0, 0),
                "invalid declaration source",
            )
        })?;
        let seed = crate::passes::seed_header_signatures(db, file);
        let span = seed
            .symbols
            .try_get(key.definition)
            .map_or(Span::new(*file.file_id(db), 0, 0), |symbol| symbol.span);
        let child_context = context
            .enter(super::dependency::DependencyKey::Header(key.clone()), span)
            .map_err(|error| vec![error])?;
        let arguments =
            super::headers::evaluate_declaration_arguments(db, file, key.clone(), context.clone());
        if arguments
            .diagnostics
            .iter()
            .any(|error| error.severity == Severity::Error)
        {
            return Err(arguments.diagnostics.clone());
        }
        let mut resolution = crate::passes::resolved_headers(db, file)
            .declarations
            .clone();
        let names = Arc::make_mut(&mut resolution.resolved);
        names
            .comptime_arguments
            .extend(arguments.values.iter().map(|(&key, &value)| (key, value)));
        names.typed_comptime_arguments.extend(
            arguments
                .typed_values
                .iter()
                .map(|(&key, value)| (key, value.clone())),
        );
        let mut checked =
            crate::passes::signatures_from_program(db, file, program, &resolution, true);
        let substitution = if key.arguments.is_empty()
            && !checked
                .type_info
                .generic_params
                .contains_key(&key.definition)
        {
            arandu_middle::types::GenericSubst::new()
        } else {
            super::roots::instance_substitution(&checked, &key)
                .map_err(|_| failure(span, "invalid concrete declaration arguments"))?
        };
        let info = &checked.type_info;
        let shape = |ty| {
            let concrete =
                arandu_middle::types::substitute_type_id(ty, &substitution, &info.type_interner);
            TypeShape::from_id(concrete, &info.type_interner)
                .map_err(|_| failure(span, "declaration type exceeds structural limits"))
        };
        let signature = if let Some(ty) = info.decl_type_id(key.definition) {
            shape(ty)?
        } else if checked
            .symbols
            .try_get(key.definition)
            .is_some_and(|symbol| {
                matches!(
                    symbol.kind,
                    arandu_middle::SymbolKind::Struct | arandu_middle::SymbolKind::Enum
                )
            })
        {
            TypeShape::Named(key.definition, key.arguments.clone())
        } else {
            return Err(failure(span, "missing declaration type"));
        };
        let fields = info
            .struct_fields
            .get(&key.definition)
            .map(|fields| {
                fields
                    .iter()
                    .map(|field| {
                        Ok(Field {
                            name: field.name.to_string(),
                            symbol: field.symbol,
                            index: field.index,
                            shape: shape(field.ty)?,
                        })
                    })
                    .collect::<Result<Vec<_>, Vec<Diagnostic>>>()
            })
            .transpose()?;
        let mut variants = info
            .enum_variants
            .iter()
            .filter(|(_, (owner, _))| *owner == key.definition)
            .map(|(&symbol, (_, payload))| {
                let items = match payload {
                    EnumPayloadShape::Unit => Vec::new(),
                    EnumPayloadShape::Tuple(items) => items
                        .iter()
                        .map(|&ty| shape(ty))
                        .collect::<Result<Vec<_>, _>>()?,
                };
                Ok((
                    symbol,
                    info.enum_variant_tags.get(&symbol).copied().unwrap_or(0),
                    items,
                ))
            })
            .collect::<Result<Vec<_>, Vec<Diagnostic>>>()?;
        variants.sort_by_key(|(symbol, tag, _)| (*tag, symbol.file_id, symbol.local_id.0));
        let mut contract = HeaderContract {
            signature,
            fields,
            variants,
        };
        // Resolve dependent nominal metadata before publishing the contract.
        checked.type_info_mut().header_requests.clear();
        demand_shapes(&contract, &key, &mut checked, span);
        freeze_requests(db, &mut checked, &child_context)?;
        normalize_aliases(
            &mut contract,
            &mut checked,
            program,
            crate::passes::database_target_info(db),
            span,
        )?;
        if !checked.type_info.is_closed_shape(&contract.signature)
            || contract.fields.as_ref().is_some_and(|fields| {
                fields
                    .iter()
                    .any(|field| !checked.type_info.is_closed_shape(&field.shape))
            })
            || contract.variants.iter().any(|(_, _, items)| {
                items
                    .iter()
                    .any(|item| !checked.type_info.is_closed_shape(item))
            })
        {
            return Err(failure(
                span,
                "declaration header did not produce closed concrete types",
            ));
        }
        Ok(contract)
    };
    HashEq::new(ContractResult { result: build() })
}

fn normalize_aliases(
    contract: &mut HeaderContract,
    checked: &mut arandu_typeck::TypeCheckResult,
    program: &arandu_parser::Program,
    target: arandu_typeck::type_checker::TargetInfo,
    span: Span,
) -> Result<(), Vec<Diagnostic>> {
    let mut checker = arandu_typeck::TypeChecker::new(
        Arc::clone(&checked.symbols),
        Arc::clone(&checked.resolved),
        Vec::new(),
        &program.pool,
        target,
    );
    checker.type_info = Arc::unwrap_or_clone(std::mem::take(&mut checked.type_info));
    let mut normalize = |shape: &mut TypeShape| -> Result<(), Vec<Diagnostic>> {
        let id = shape
            .intern(&checker.type_info.type_interner)
            .map_err(|_| failure(span, "invalid concrete header"))?;
        let ty = checker.type_info.type_interner.resolve(id);
        let ty = arandu_typeck::type_checker::types::expand_aliases(&mut checker, ty);
        let id = checker.type_info.type_interner.intern(ty);
        *shape = TypeShape::from_id(id, &checker.type_info.type_interner)
            .map_err(|_| failure(span, "invalid concrete header"))?;
        Ok(())
    };
    normalize(&mut contract.signature)?;
    if let Some(fields) = &mut contract.fields {
        for field in fields {
            normalize(&mut field.shape)?;
        }
    }
    for (_, _, items) in &mut contract.variants {
        for item in items {
            normalize(item)?;
        }
    }
    let normalized = checker.finish();
    if normalized
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return Err(normalized.diagnostics);
    }
    checked.type_info = normalized.type_info;
    Ok(())
}

fn demand_shapes(
    contract: &HeaderContract,
    owner: &FunctionInstance,
    checked: &mut arandu_typeck::TypeCheckResult,
    span: Span,
) {
    let mut shapes = if matches!(&contract.signature, TypeShape::Named(symbol, args) if *symbol == owner.definition && args == &owner.arguments)
    {
        Vec::new()
    } else {
        vec![&contract.signature]
    };
    if let Some(fields) = &contract.fields {
        shapes.extend(fields.iter().map(|field| &field.shape));
    }
    for (_, _, items) in &contract.variants {
        shapes.extend(items);
    }
    while let Some(shape) = shapes.pop() {
        match shape {
            TypeShape::Named(symbol, args) => {
                if args
                    .iter()
                    .all(|arg| checked.type_info.is_closed_shape(arg))
                    && checked.type_info.deferred_headers.contains(symbol)
                {
                    let key = FunctionInstance {
                        definition: *symbol,
                        arguments: args.clone(),
                    };
                    if &key != owner && !checked.type_info.concrete_headers.contains_key(&key) {
                        checked.type_info_mut().header_requests.insert(key, span);
                    }
                }
                shapes.extend(args);
            }
            TypeShape::Func(items, result) => {
                shapes.extend(items);
                shapes.push(result);
            }
            TypeShape::Tuple(items) => shapes.extend(items),
            TypeShape::Result(ok, err) => {
                shapes.push(ok);
                shapes.push(err);
            }
            TypeShape::Array(_, inner)
            | TypeShape::Nullable(inner)
            | TypeShape::Slice(inner)
            | TypeShape::Ptr(inner)
            | TypeShape::Ref(inner)
            | TypeShape::RefMut(inner)
            | TypeShape::Option(inner)
            | TypeShape::Coroutine(inner)
            | TypeShape::Poll(inner)
            | TypeShape::Range(inner)
            | TypeShape::ConstArray(_, inner) => shapes.push(inner),
            TypeShape::Primitive(_)
            | TypeShape::Const(_)
            | TypeShape::FrozenConst(_)
            | TypeShape::ConstParam(_)
            | TypeShape::GenRef
            | TypeShape::Err
            | TypeShape::Void
            | TypeShape::IntLiteral
            | TypeShape::FloatLiteral
            | TypeShape::Error => {}
        }
    }
}

pub(crate) fn freeze_requests(
    db: &dyn ArandCompilerDb,
    checked: &mut arandu_typeck::TypeCheckResult,
    context: &super::DependencyContext,
) -> Result<usize, Vec<Diagnostic>> {
    let mut count = 0;
    while !checked.type_info.header_requests.is_empty() {
        let requests = std::mem::take(&mut checked.type_info_mut().header_requests);
        let mut requests: Vec<_> = requests.into_iter().collect();
        requests.sort_by_key(|(key, _)| key.stable_hash().as_bytes().to_owned());
        for (key, span) in requests {
            db.unwind_if_revision_cancelled();
            if checked.type_info.concrete_headers.contains_key(&key) {
                continue;
            }
            let file = db
                .source_file_by_id(key.definition.file_id)
                .ok_or_else(|| failure(span, "declaration source is unavailable"))?;
            let result = concrete_header_contract(db, file, key.clone(), context.clone());
            let contract = result.result.as_ref().map_err(Clone::clone)?;
            install(checked, key.clone(), contract, span)?;
            demand_shapes(contract, &key, checked, span);
            count += 1;
            if count > 4096 {
                return Err(vec![Diagnostic::error(
                    arandu_middle::DiagCode::T045ComptimeLimitExceeded,
                    "too many concrete declaration contracts",
                    span,
                )]);
            }
        }
    }
    Ok(count)
}

fn install(
    checked: &mut arandu_typeck::TypeCheckResult,
    key: FunctionInstance,
    contract: &HeaderContract,
    span: Span,
) -> Result<(), Vec<Diagnostic>> {
    let info = checked.type_info_mut();
    let intern = |shape: &TypeShape| {
        shape
            .intern(&info.type_interner)
            .map_err(|_| failure(span, "concrete header exceeds structural limits"))
    };
    let signature = intern(&contract.signature)?;
    let fields = contract
        .fields
        .as_ref()
        .map(|fields| {
            fields
                .iter()
                .map(|field| {
                    Ok(arandu_middle::layout::StructFieldInfo {
                        name: field.name.clone().into(),
                        symbol: field.symbol,
                        index: field.index,
                        ty: intern(&field.shape)?,
                    })
                })
                .collect::<Result<Vec<_>, Vec<Diagnostic>>>()
        })
        .transpose()?
        .map(|fields| Arc::new(arandu_middle::layout::StructFields::from_entries(fields)));
    let variants = contract
        .variants
        .iter()
        .map(|(symbol, tag, items)| {
            Ok((
                *symbol,
                *tag,
                if items.is_empty() {
                    EnumPayloadShape::Unit
                } else {
                    EnumPayloadShape::Tuple(
                        items.iter().map(intern).collect::<Result<Vec<_>, _>>()?,
                    )
                },
            ))
        })
        .collect::<Result<Vec<_>, Vec<Diagnostic>>>()?;
    // A first discovery pass may have cached a template constructor containing
    // deferred dimensions. It cannot survive installation of its frozen owner.
    info.variant_instantiations.retain(|(variant, _), _| {
        !contract
            .variants
            .iter()
            .any(|(symbol, _, _)| symbol == variant)
    });
    info.concrete_headers.insert(
        key,
        Arc::new(ConcreteHeader {
            signature,
            fields,
            variants,
        }),
    );
    Ok(())
}

pub(crate) fn check_body(
    db: &dyn ArandCompilerDb,
    initial: &arandu_typeck::TypeCheckResult,
    program: &arandu_parser::Program,
    owner: SymbolId,
    substitution: &arandu_middle::types::GenericSubst,
    context: &super::DependencyContext,
) -> arandu_typeck::TypeCheckResult {
    let mut checked = arandu_typeck::type_checker::check::check_item_body_with_substitution(
        initial,
        program,
        owner,
        crate::passes::database_target_info(db),
        substitution,
    );
    for _ in 0..super::MAX_QUERY_DEPENDENCY_DEPTH {
        match freeze_requests(db, &mut checked, context) {
            Ok(0) => return checked,
            Ok(_) => {
                checked = arandu_typeck::type_checker::check::check_item_body_with_substitution(
                    &checked,
                    program,
                    owner,
                    crate::passes::database_target_info(db),
                    substitution,
                );
            }
            Err(errors) => {
                checked.diagnostics.extend(errors);
                return checked;
            }
        }
    }
    checked.diagnostics.push(Diagnostic::error(
        arandu_middle::DiagCode::T045ComptimeLimitExceeded,
        "concrete header discovery exceeds its continuation limit",
        program.span,
    ));
    checked
}

pub(crate) fn prepare_owner(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    key: &FunctionInstance,
    checked: &mut arandu_typeck::TypeCheckResult,
    context: &super::DependencyContext,
) -> Result<(), Vec<Diagnostic>> {
    if !checked.type_info.deferred_headers.contains(&key.definition) {
        return Ok(());
    }
    let span = checked
        .symbols
        .try_get(key.definition)
        .map_or(Span::new(*file.file_id(db), 0, 0), |symbol| symbol.span);
    let result = concrete_header_contract(db, file, key.clone(), context.clone());
    let contract = result.result.as_ref().map_err(Clone::clone)?;
    install(checked, key.clone(), contract, span)?;
    let signature = contract
        .signature
        .intern(&checked.type_info.type_interner)
        .map_err(|_| failure(span, "invalid concrete signature"))?;
    checked
        .type_info_mut()
        .record_decl_type(key.definition, signature);
    let arguments =
        super::headers::evaluate_declaration_arguments(db, file, key.clone(), context.clone());
    let names = Arc::make_mut(&mut checked.resolved);
    names
        .comptime_arguments
        .extend(arguments.values.iter().map(|(&key, &value)| (key, value)));
    names.typed_comptime_arguments.extend(
        arguments
            .typed_values
            .iter()
            .map(|(&key, value)| (key, value.clone())),
    );
    Ok(())
}
