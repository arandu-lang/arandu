//! Presentation, type rendering, and symbol resolution helpers for IDE queries.
//!
//! Editor-agnostic: no `lsp_types` here. The LSP adapter maps these to protocol
//! types; the wasm bridge serializes them directly.

use arandu_middle::types::ArType;
use arandu_middle::{NodeKey, Symbol, SymbolId, SymbolKind};
use arandu_query::{AnalysisSnapshot, SourceFile};
use arandu_semantics::TypeCheckResult;
use rustc_hash::FxHashMap;

/// Type-check the file (composed P1/P2 view).
///
/// Invalid buffers are common at completion time (a trailing `.` or `:` is a
/// syntax error), so when the strict parse fails this analyzes the recovering
/// AST instead of returning an empty symbol table. Compiler queries are
/// untouched: only the IDE path recovers.
#[must_use]
pub fn typecheck(
    snap: &AnalysisSnapshot,
    source: SourceFile,
) -> arandu_query::db::HashEq<TypeCheckResult> {
    if arandu_query::passes::parse(&snap.db, source).is_err() {
        arandu_query::passes::ide_type_check(&snap.db, source).clone()
    } else {
        arandu_query::passes::type_check(&snap.db, source).clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterPresentation {
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolPresentation {
    pub signature: String,
    pub documentation: Option<String>,
    pub parameters: Vec<ParameterPresentation>,
}

pub fn display_type(tc: &TypeCheckResult, ty: &ArType) -> String {
    ty.display(&tc.symbols, &tc.type_info.type_interner)
}

pub fn symbol_presentation(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    tc: &TypeCheckResult,
    symbol: &Symbol,
) -> SymbolPresentation {
    let ty = tc.type_info.decl_type(symbol.id);
    let parameter_names = function_parameter_names(snap, source, symbol);
    let (signature, parameters) = match ty.as_ref() {
        Some(ArType::Func(param_types, return_type)) => {
            let param_types_vec = tc.type_info.type_interner.type_args(*param_types);
            let labels: Vec<_> = param_types_vec
                .iter()
                .enumerate()
                .map(|(index, type_id)| {
                    let ty = tc.type_info.type_interner.resolve(*type_id);
                    let ty = display_type(tc, &ty);
                    parameter_names
                        .get(index)
                        .filter(|name| !name.is_empty())
                        .map_or_else(|| ty.clone(), |name| format!("{name}: {ty}"))
                })
                .collect();
            let return_ty = tc.type_info.type_interner.resolve(*return_type);
            let return_suffix = if matches!(return_ty, ArType::Void) {
                String::new()
            } else {
                format!(": {}", display_type(tc, &return_ty))
            };
            (
                format!("func {}({}){return_suffix}", symbol.name, labels.join(", ")),
                labels
                    .into_iter()
                    .map(|label| ParameterPresentation { label })
                    .collect(),
            )
        }
        Some(ty) => {
            let prefix = match symbol.kind {
                SymbolKind::Const => "const",
                SymbolKind::Field => "field",
                SymbolKind::Param => "param",
                SymbolKind::Local => "let",
                _ => "type",
            };
            (
                format!("{prefix} {}: {}", symbol.name, display_type(tc, ty)),
                Vec::new(),
            )
        }
        None => (
            format!("{} {}", symbol_kind_name(symbol.kind), symbol.name),
            Vec::new(),
        ),
    };
    SymbolPresentation {
        signature,
        documentation: symbol_documentation(snap, source, symbol),
        parameters,
    }
}

pub fn symbol_kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc => "func",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Interface => "interface",
        SymbolKind::TypeAlias => "type",
        SymbolKind::EnumVariant => "variant",
        SymbolKind::Module => "module",
        SymbolKind::NamespaceMember => "member",
        SymbolKind::ImportValue | SymbolKind::ImportType => "import",
        SymbolKind::TypeParam => "type parameter",
        SymbolKind::Const | SymbolKind::ConstParam => "const",
        SymbolKind::Field => "field",
        SymbolKind::Param => "param",
        SymbolKind::Local => "let",
    }
}

pub fn symbol_documentation(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    symbol: &Symbol,
) -> Option<String> {
    let resolved = arandu_query::passes::resolve(&snap.db, source);
    let docs = resolved
        .docs
        .iter()
        .filter(|(target, _)| target.start <= symbol.span.start && symbol.span.end <= target.end)
        .min_by_key(|(target, _)| target.end.saturating_sub(target.start))?
        .1;
    let text = docs
        .iter()
        .map(|line| line.strip_prefix("///").unwrap_or(line).trim_start())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

pub fn function_parameter_names(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    symbol: &Symbol,
) -> Vec<String> {
    let program = arandu_query::passes::parse(&snap.db, source);
    let Ok(program) = &**program else {
        return Vec::new();
    };
    for &decl_id in &program.decls {
        match program.pool.decl(decl_id) {
            arandu_parser::TopLevelDecl::Func(func) => {
                let (span, name) = match &func.name {
                    arandu_parser::FuncName::Free { span, name }
                    | arandu_parser::FuncName::Method { span, name, .. } => (*span, name),
                };
                if span == symbol.span && name.as_str() == symbol.name.as_str() {
                    return func
                        .params
                        .iter()
                        .map(|param| param.name.to_string())
                        .collect();
                }
            }
            arandu_parser::TopLevelDecl::Extern(extern_decl) => {
                if let Some(member) = extern_decl.members.iter().find(|member| {
                    member.name.as_str() == symbol.name.as_str()
                        && member.span.start <= symbol.span.start
                        && symbol.span.end <= member.span.end
                }) {
                    return member
                        .params
                        .iter()
                        .map(|param| param.name.to_string())
                        .collect();
                }
            }
            _ => {}
        }
    }
    Vec::new()
}

/// Tightest name/ref/definition containing `offset`.
#[must_use]
pub fn symbol_at(tc: &TypeCheckResult, offset: u32) -> Option<SymbolId> {
    let mut best: Option<(u32, SymbolId)> = None;
    let consider = |map: &FxHashMap<NodeKey, SymbolId>, best: &mut Option<(u32, SymbolId)>| {
        for (key, &sym) in map {
            if key.start <= offset && offset < key.end {
                let w = key.end.saturating_sub(key.start);
                if best.is_none_or(|(bw, _)| w < bw) {
                    *best = Some((w, sym));
                }
            }
        }
    };
    consider(&tc.resolved.value_refs, &mut best);
    consider(&tc.resolved.type_refs, &mut best);
    consider(&tc.resolved.definitions, &mut best);
    best.map(|(_, s)| s)
}

/// Tightest resolved expression containing `offset`.
///
/// Namespace members such as `util.answer` are recorded by the resolver in
/// the dense `expr_symbols` table rather than in `value_refs`; navigation must
/// consume that existing semantic identity instead of reparsing the text.
#[must_use]
pub fn expr_symbol_at(
    program: &arandu_parser::Program,
    tc: &TypeCheckResult,
    offset: u32,
) -> Option<SymbolId> {
    let mut best: Option<(u32, SymbolId)> = None;
    for (index, symbol) in tc.resolved.expr_symbols.iter().enumerate() {
        let Some(symbol) = *symbol else {
            continue;
        };
        let Some(span) = program.pool.expr_spans.get(index) else {
            continue;
        };
        if span.start <= offset && offset < span.end {
            let width = span.end.saturating_sub(span.start);
            if best.is_none_or(|(best_width, _)| width < best_width) {
                best = Some((width, symbol));
            }
        }
    }
    best.map(|(_, symbol)| symbol)
}

/// Word prefix before `offset` for completion filtering.
#[must_use]
pub fn prefix_at(text: &str, offset: u32) -> String {
    let off = (offset as usize).min(text.len());
    let bytes = text.as_bytes();
    let mut i = off;
    while i > 0 {
        let c = bytes[i - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            i -= 1;
        } else {
            break;
        }
    }
    text[i..off].to_string()
}
