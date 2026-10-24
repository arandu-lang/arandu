//! Narrow CTFE lowering/evaluation queries. Unlike final runtime validation,
//! this staged path admits pure functions and concrete instances
//! (including direct imported callees), and never calls that
//! final lowering query or the body-derived borrow-interface producer.

use std::sync::Arc;

use crate::db::HashEq;
use crate::{ArandCompilerDb, SourceFile};
use arandu_middle::ctfe::ConstValue;
use arandu_middle::types::FunctionInstance;
use arandu_middle::{Diagnostic, SymbolId};
use arandu_mir::ctfe::{Budget, CtfeFunction, EvalError, EvalErrorKind, FunctionProvider};

mod config;
pub use config::{CtfeConfig, CtfeLimits};
mod arguments;
pub(crate) mod contracts;
mod dependency;
pub use dependency::{DependencyContext, MAX_QUERY_DEPENDENCY_DEPTH};
pub(crate) mod globals;
pub(crate) mod headers;
pub use globals::{global_const_value, FrozenConstant, GlobalConstant};
mod branches;
pub(crate) mod instances;
mod loops;
pub use arguments::{item_const_arguments, ConstArguments};
pub(crate) use instances::{instance_staged_hir, instance_staged_symbols};
pub use loops::{item_static_loops, StaticLoops};
pub(crate) mod public;
pub use branches::{item_static_branches, StaticBranches};
mod lower_scalar;
pub(crate) mod roots;
use lower_scalar::lower_scalar_function;
pub use public::{item_staged_typing, PUBLIC_BUDGET};
pub use roots::{
    ctfe_eval_root, ctfe_root_amir, BlockBranch, BlockStep, CtfeRoot, CtfeRootRequest,
    RootEvalError, RootExpectedType, RootSelector,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRuntimeValue {
    pub symbol: SymbolId,
    pub name: Arc<str>,
    pub use_span: arandu_middle::Span,
    pub declaration_span: arandu_middle::Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildFailure {
    MissingFunction,
    GenericFunction,
    InvalidRoot,
    RuntimeCapture(Box<CapturedRuntimeValue>),
    Diagnostics(Vec<Diagnostic>),
    Evaluation(EvalErrorKind),
}

#[derive(Debug, Clone)]
pub struct CtfeLowering {
    pub result: Result<Arc<CtfeFunction>, BuildFailure>,
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "ctfe_func_amir", file = ?file.file_id(db), func = ?symbol,
))]
pub fn ctfe_func_amir(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    symbol: SymbolId,
) -> HashEq<CtfeLowering> {
    lower_scalar_function(db, file, symbol, false, &DependencyContext::default())
}

/// Condition helpers are resolved/typed individually against pre-body headers.
/// They cannot reenter the owner's complete resolution through ordinary item
/// typing. Lowering and VM admission are shared with normal scalar CTFE.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "ctfe_header_func_amir", file = ?file.file_id(db), func = ?symbol,
))]
pub fn ctfe_header_func_amir(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    symbol: SymbolId,
) -> HashEq<CtfeLowering> {
    HashEq::share(ctfe_header_func_amir_in_context(
        db,
        file,
        symbol,
        DependencyContext::default(),
    ))
}

#[salsa::tracked]
fn ctfe_header_func_amir_in_context(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    symbol: SymbolId,
    context: DependencyContext,
) -> HashEq<CtfeLowering> {
    lower_scalar_function(db, file, symbol, true, &context)
}

#[salsa::tracked]
fn scalar_resolution_diagnostics(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    symbol: SymbolId,
) -> HashEq<Vec<Diagnostic>> {
    let source = crate::passes::item_source_input(db, file, symbol);
    let resolution = crate::passes::resolve(db, file);
    let mut span = None;
    source.program.for_each_decl_recursive(|_, declaration| {
        if arandu_semantics::primary_def_key(declaration)
            .and_then(|key| resolution.resolved.definitions.get(&key))
            == Some(&symbol)
        {
            span = Some(arandu_semantics::item_source_span(declaration));
        }
    });
    HashEq::new(
        resolution
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                span.is_some_and(|span| {
                    diagnostic.span.file_id == span.file_id
                        && span.start <= diagnostic.span.start
                        && diagnostic.span.end <= span.end
                }) || source.program.imports.iter().any(|import| {
                    let span = import.span();
                    span.file_id == diagnostic.span.file_id
                        && span.start <= diagnostic.span.start
                        && diagnostic.span.end <= span.end
                })
            })
            .cloned()
            .collect(),
    )
}

/// Only an external declaration container is retained: sibling function spans
/// and bodies are not dependencies of intrinsic identity or ABI metadata.
#[salsa::tracked]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db),
    fields(query = "ctfe_extern_hir", file = ?file.file_id(db))
)]
fn ctfe_extern_hir(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    symbol: SymbolId,
) -> HashEq<crate::passes::PreparedHir> {
    let source = crate::passes::item_source_input(db, file, symbol);
    let declared = crate::passes::seed_header_signatures(db, file);
    let mut checked = (**declared).clone();
    let mut diagnostics = Vec::new();
    let hir = match arandu_semantics::lower_extern_to_hir(&mut checked, &source.program, symbol) {
        Ok(hir) => hir,
        Err(errors) => {
            diagnostics = errors;
            None
        }
    };
    HashEq::new(crate::passes::PreparedHir {
        hir,
        type_check: checked,
        diagnostics,
        source_fingerprint: crate::StableHash::stable_hash(&**source),
    })
}

pub(crate) fn link_extern_headers(
    db: &dyn ArandCompilerDb,
    checked: &mut arandu_semantics::TypeCheckResult,
    hir: &mut arandu_middle::hir::HirProgram,
) -> Result<(), BuildFailure> {
    let mut referenced = hir
        .pool
        .exprs
        .iter()
        .filter_map(|expression| {
            let symbol = match expression.kind {
                arandu_middle::hir::HirExprKind::Path { symbol } => symbol,
                arandu_middle::hir::HirExprKind::TypePath { member_symbol, .. } => member_symbol,
                _ => return None,
            };
            checked
                .symbols
                .try_get(symbol)
                .is_some_and(|symbol| symbol.kind == arandu_middle::SymbolKind::ExternFunc)
                .then_some(symbol)
        })
        .collect::<Vec<_>>();
    referenced.sort_unstable_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    referenced.dedup();
    for symbol in referenced {
        db.unwind_if_revision_cancelled();
        let file = db
            .source_file_by_id(symbol.file_id)
            .ok_or(BuildFailure::MissingFunction)?;
        let context = ctfe_extern_hir(db, file, symbol);
        let source = context
            .hir
            .as_ref()
            .ok_or_else(|| BuildFailure::Diagnostics(context.diagnostics.clone()))?;
        arandu_semantics::link_hir_module(checked, hir, &context.type_check, source);
    }
    Ok(())
}

/// Concrete CTFE units reuse declaration/body staging and the canonical
/// monomorphizer. They never request final runtime units, borrow fixpoints or
/// executable composition while initial typing may still be in progress.
#[salsa::tracked]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db),
    fields(query = "ctfe_instance_amir")
)]
pub fn ctfe_instance_amir<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: crate::runtime::Instance<'db>,
) -> HashEq<CtfeLowering> {
    HashEq::share(ctfe_instance_amir_in_context(
        db,
        instance,
        DependencyContext::default(),
    ))
}

#[salsa::tracked]
fn ctfe_instance_amir_in_context<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: crate::runtime::Instance<'db>,
    context: DependencyContext,
) -> HashEq<CtfeLowering> {
    let layout = *db.target_config().data_layout(db);
    let build = || {
        if instance.key(db).definition.file_id != *instance.file(db).file_id(db) {
            return Err(BuildFailure::MissingFunction);
        }
        layout
            .validate()
            .map_err(|error| BuildFailure::Evaluation(EvalErrorKind::InvalidLayout(error)))?;
        let concrete = crate::runtime::instance_ctfe_hir_in_context(db, instance, context.clone());
        let artifacts = &concrete.artifacts;
        let hir = artifacts
            .hir
            .as_ref()
            .ok_or_else(|| BuildFailure::Diagnostics(artifacts.diagnostics.clone()))?;
        let selected = concrete.function.ok_or(BuildFailure::MissingFunction)?;
        let function = hir
            .decls
            .iter()
            .find_map(|&id| match hir.pool.decl(id) {
                arandu_middle::hir::HirDecl::Func(function)
                    if function.symbol == selected && function.body.is_some() =>
                {
                    Some(function)
                }
                _ => None,
            })
            .ok_or(BuildFailure::MissingFunction)?;
        db.unwind_if_revision_cancelled();
        let unit =
            arandu_semantics::lower_function_unit(&artifacts.type_check, hir, function, layout)
                .map_err(BuildFailure::Diagnostics)?;
        let called = unit
            .function
            .stmts
            .iter_ids()
            .filter_map(|id| match unit.function.try_stmt(id) {
                Some(arandu_middle::amir::AmirStmt::Call {
                    callee: arandu_middle::amir::AmirOperand::FunctionRef(callee),
                    ..
                }) => Some(*callee),
                _ => None,
            })
            .collect::<rustc_hash::FxHashSet<_>>();
        let calls = concrete
            .instances
            .iter()
            .filter(|(symbol, _)| called.contains(symbol))
            .cloned()
            .collect();
        CtfeFunction::new_with_provider(
            Arc::unwrap_or_clone(unit.function),
            unit.literals,
            &artifacts.type_check.type_info.type_interner,
            layout,
            artifacts.type_check.type_info.as_ref(),
        )
        .and_then(|unit| unit.bind_instance(instance.key(db).clone(), calls))
        .map(Arc::new)
        .map_err(BuildFailure::Evaluation)
    };
    HashEq::new(CtfeLowering { result: build() })
}

/// Arguments and policy are part of query identity, not mutable evaluator state.
#[salsa::interned]
pub struct CtfeRequest<'db> {
    pub file: SourceFile,
    pub symbol: SymbolId,
    #[returns(ref)]
    pub arguments: Vec<ConstValue>,
    pub budget: Budget,
}

#[salsa::interned]
pub struct CtfeInstanceRequest<'db> {
    pub instance: crate::runtime::Instance<'db>,
    #[returns(ref)]
    pub arguments: Vec<ConstValue>,
    pub budget: Budget,
}

struct QueryProvider<'a> {
    db: &'a dyn ArandCompilerDb,
    headers_only: bool,
    context: DependencyContext,
    failures: std::cell::RefCell<Vec<Diagnostic>>,
}

impl FunctionProvider for QueryProvider<'_> {
    fn instance(&self, key: &FunctionInstance) -> Result<Arc<CtfeFunction>, EvalErrorKind> {
        if key.arguments.is_empty() {
            self.function(key.definition)
        } else {
            self.db.unwind_if_revision_cancelled();
            let file = self
                .db
                .source_file_by_id(key.definition.file_id)
                .ok_or(EvalErrorKind::MissingFunction(key.definition))?;
            let instance = crate::runtime::Instance::new(self.db, file, key.clone());
            let lowered = ctfe_instance_amir_in_context(self.db, instance, self.context.clone());
            self.unit(&lowered.result, key.definition)
        }
    }

    fn function(&self, symbol: SymbolId) -> Result<Arc<CtfeFunction>, EvalErrorKind> {
        self.db.unwind_if_revision_cancelled();
        let file = self
            .db
            .source_file_by_id(symbol.file_id)
            .ok_or(EvalErrorKind::MissingFunction(symbol))?;
        let lowered = if self.headers_only || !self.context.is_empty() {
            ctfe_header_func_amir_in_context(self.db, file, symbol, self.context.clone())
        } else {
            ctfe_func_amir(self.db, file, symbol)
        };
        self.unit(&lowered.result, symbol)
    }

    fn intrinsic(&self, symbol: SymbolId) -> Option<arandu_middle::intrinsics::IntrinsicKind> {
        let file = self.db.source_file_by_id(symbol.file_id)?;
        let locals = crate::passes::local_symbols(self.db, file);
        let def = locals.symbols.try_get(symbol)?;
        if def.kind == arandu_middle::SymbolKind::ExternFunc {
            let kind = arandu_middle::intrinsics::IntrinsicKind::from_declared_name(
                &def.name,
                def.lang_item,
            )?;
            if kind.is_pure_bit_operation()
                || kind == arandu_middle::intrinsics::IntrinsicKind::Abort
            {
                return Some(kind);
            }
        }
        None
    }
}

impl QueryProvider<'_> {
    fn unit(
        &self,
        result: &Result<Arc<CtfeFunction>, BuildFailure>,
        symbol: SymbolId,
    ) -> Result<Arc<CtfeFunction>, EvalErrorKind> {
        match result {
            Ok(unit) => Ok(Arc::clone(unit)),
            Err(error) => {
                if let BuildFailure::Diagnostics(diagnostics) = error {
                    self.failures.borrow_mut().extend(
                        diagnostics
                            .iter()
                            .filter(|diagnostic| {
                                matches!(
                                    diagnostic.code,
                                    arandu_middle::DiagCode::T044ComptimeEvaluationFailed
                                        | arandu_middle::DiagCode::T045ComptimeLimitExceeded
                                )
                            })
                            .cloned(),
                    );
                }
                Err(EvalErrorKind::UnavailableFunction(symbol))
            }
        }
    }
}

struct InstanceProvider<'a> {
    db: &'a dyn ArandCompilerDb,
}

impl FunctionProvider for InstanceProvider<'_> {
    fn function(&self, symbol: SymbolId) -> Result<Arc<CtfeFunction>, EvalErrorKind> {
        self.instance(&FunctionInstance {
            definition: symbol,
            arguments: Vec::new(),
        })
    }

    fn instance(&self, key: &FunctionInstance) -> Result<Arc<CtfeFunction>, EvalErrorKind> {
        self.db.unwind_if_revision_cancelled();
        let file = self
            .db
            .source_file_by_id(key.definition.file_id)
            .ok_or(EvalErrorKind::MissingFunction(key.definition))?;
        let instance = crate::runtime::Instance::new(self.db, file, key.clone());
        match &ctfe_instance_amir(self.db, instance).result {
            Ok(unit) => Ok(Arc::clone(unit)),
            Err(_) => Err(EvalErrorKind::UnavailableFunction(key.definition)),
        }
    }

    fn intrinsic(&self, symbol: SymbolId) -> Option<arandu_middle::intrinsics::IntrinsicKind> {
        let file = self.db.source_file_by_id(symbol.file_id)?;
        let locals = crate::passes::local_symbols(self.db, file);
        let def = locals.symbols.try_get(symbol)?;
        if def.kind == arandu_middle::SymbolKind::ExternFunc {
            let kind = arandu_middle::intrinsics::IntrinsicKind::from_declared_name(
                &def.name,
                def.lang_item,
            )?;
            if kind.is_pure_bit_operation()
                || kind == arandu_middle::intrinsics::IntrinsicKind::Abort
            {
                return Some(kind);
            }
        }
        None
    }
}

#[salsa::tracked]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db, request),
    fields(query = "ctfe_eval_instance")
)]
pub fn ctfe_eval_instance<'db>(
    db: &'db dyn ArandCompilerDb,
    request: CtfeInstanceRequest<'db>,
) -> Result<ConstValue, EvalError> {
    let instance = *request.instance(db);
    let key = instance.key(db);
    if key.definition.file_id != *instance.file(db).file_id(db) {
        return Err(EvalError {
            kind: EvalErrorKind::MissingFunction(key.definition),
            function: key.definition,
            block: arandu_middle::amir::BlockId(0),
            span: arandu_middle::Span::new(*instance.file(db).file_id(db), 0, 0),
            trace: Vec::new(),
            trace_truncated: false,
        });
    }
    arandu_mir::ctfe::evaluate_instance(
        &InstanceProvider { db },
        instance.key(db),
        request.arguments(db),
        *request.budget(db),
        || {
            db.unwind_if_revision_cancelled();
            false
        },
    )
}

#[salsa::tracked]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db, request),
    fields(query = "ctfe_eval")
)]
pub fn ctfe_eval<'db>(
    db: &'db dyn ArandCompilerDb,
    request: CtfeRequest<'db>,
) -> Result<ConstValue, EvalError> {
    // Anchor the source identity even if a stale symbol points elsewhere.
    let file = request.file(db);
    let symbol = *request.symbol(db);
    if symbol.file_id != *file.file_id(db) {
        return Err(EvalError {
            kind: EvalErrorKind::MissingFunction(symbol),
            function: symbol,
            block: arandu_middle::amir::BlockId(0),
            span: arandu_middle::Span::new(*file.file_id(db), 0, 0),
            trace: Vec::new(),
            trace_truncated: false,
        });
    }
    arandu_mir::ctfe::evaluate(
        &QueryProvider {
            db,
            headers_only: false,
            context: DependencyContext::default(),
            failures: Default::default(),
        },
        symbol,
        request.arguments(db),
        *request.budget(db),
        || {
            // Salsa cancellation unwinds the query, so it cannot be cached as a
            // normal CTFE error. Pure VM callers can instead supply a boolean hook.
            db.unwind_if_revision_cancelled();
            false
        },
    )
}
