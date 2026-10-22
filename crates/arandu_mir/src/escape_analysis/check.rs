use super::types::{EscapeCheckOptions, EscapeEvent, EscapeKind};
use super::walk::{find_escapes, find_escapes_with_type_info};
use crate::amir::{AmirFunc, AmirRvalue, AmirStmt, BlockId, LocalId};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::{Span, SymbolTable};
use arandu_middle::types::ReturnBorrowSummary;

/// Run escape analysis and produce O004 / O010 diagnostics.
pub fn check_escapes(
    func: &AmirFunc,
    symbols: &SymbolTable,
    interner: &crate::types::TypeInterner,
    opts: EscapeCheckOptions,
) -> Vec<Diagnostic> {
    check_escapes_by_block(func, symbols, interner, opts)
        .into_iter()
        .map(|(_, d)| d)
        .collect()
}

/// Run escape analysis with structural borrow metadata from type checking.
pub fn check_escapes_with_type_info(
    func: &AmirFunc,
    symbols: &SymbolTable,
    type_info: &arandu_typeck::TypeInfo,
    opts: EscapeCheckOptions,
) -> Vec<Diagnostic> {
    check_escapes_by_block_impl(
        func,
        symbols,
        &type_info.type_interner,
        Some(type_info),
        opts,
    )
    .into_iter()
    .map(|(_, diagnostic)| diagnostic)
    .collect()
}

/// Same as [`check_escapes`], tagged with AMIR block.
#[must_use]
pub fn check_escapes_by_block(
    func: &AmirFunc,
    symbols: &SymbolTable,
    interner: &crate::types::TypeInterner,
    opts: EscapeCheckOptions,
) -> Vec<(BlockId, Diagnostic)> {
    check_escapes_by_block_impl(func, symbols, interner, None, opts)
}

fn check_escapes_by_block_impl(
    func: &AmirFunc,
    symbols: &SymbolTable,
    interner: &crate::types::TypeInterner,
    type_info: Option<&arandu_typeck::TypeInfo>,
    opts: EscapeCheckOptions,
) -> Vec<(BlockId, Diagnostic)> {
    if func.blocks.is_empty() {
        return Vec::new();
    }

    let events = type_info.map_or_else(
        || find_escapes(func, interner),
        |type_info| find_escapes_with_type_info(func, type_info),
    );
    let no_fb = opts.effective_no_fallback();
    let mut diags = Vec::new();

    for ev in events {
        if return_escape_is_declared(&ev, opts.return_borrow.as_ref()) {
            continue;
        }
        let name = local_name(ev.place_local, func, symbols);
        match ev.kind {
            EscapeKind::Return => {
                // Returning &local is always a hard error (O010). O004 explains
                // the static-window failure (Magia Inspecionável).
                diags.push((
                    ev.block,
                    Diagnostic::error(
                        DiagCode::O010EscapeOfBorrowedValue,
                        format!(
                            "escape of borrowed value: returning reference to local variable '{name}'"
                        ),
                        ev.span,
                    )
                    .with_label(ev.span, "reference to local would dangle after return")
                    .with_note(ev.reason)
                    .with_note(
                        "keep the owner alive in the caller, or return owned data instead of `ref`",
                    ),
                ));
                diags.push((
                    ev.block,
                    o004_diag(&name, ev.span, ev.block, ev.reason, /*as_error*/ no_fb),
                ));
            }
            EscapeKind::HeapStore => {
                // Controlled fallback path: O004 is Note by default; G2 promotes to Error.
                // Projected refs cannot be represented by the current scalar
                // handle without either snapshot semantics or losing the base
                // owner/path relation. Reject instead of compiling stale data.
                let projected = has_projected_borrow(func, ev.place_local);
                diags.push((
                    ev.block,
                    o004_diag(
                        &name,
                        ev.span,
                        ev.block,
                        ev.reason,
                        /*as_error*/ no_fb || projected,
                    )
                    .with_note(if projected {
                        "projected escaping borrows require a first-class owner/path handle; snapshot promotion is intentionally forbidden"
                    } else {
                        "compiler-managed owner promotion is available for this root place"
                    }),
                ));
                if no_fb {
                    // Extra hint only when hard-failing.
                }
            }
        }
    }

    diags
}

fn return_escape_is_declared(event: &EscapeEvent, summary: Option<&ReturnBorrowSummary>) -> bool {
    event.kind == EscapeKind::Return
        && summary.is_some_and(|summary| {
            summary.dependencies.iter().any(|dependency| {
                dependency.sources.iter().any(|source| {
                    usize::try_from(source.parameter_index)
                        .is_ok_and(|index| event.place_local.as_usize() == index)
                })
            })
        })
}

fn has_projected_borrow(func: &AmirFunc, local: LocalId) -> bool {
    func.blocks
        .iter()
        .flat_map(|block| func.block_stmts(block.id))
        .any(|stmt| {
            matches!(
                stmt,
                AmirStmt::Assign {
                    rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
                    ..
                } if place.local == local && !place.projections.is_empty()
            )
        })
}

fn o004_diag(name: &str, span: Span, block: BlockId, reason: &str, as_error: bool) -> Diagnostic {
    let msg = format!("generational fallback: '{name}' escapes stack-limited borrow window");
    let d = if as_error {
        Diagnostic::error(DiagCode::O004GenerationalFallback, msg, span).with_note(
            "this scope forbids generational fallback (`@NoFallback` or `--no-generational-fallback`)",
        )
    } else {
        Diagnostic::note(DiagCode::O004GenerationalFallback, msg, span).with_note(
            "not a silent heap promotion: this note records why the static borrow window was insufficient",
        )
    };
    d.with_label(span, "escape crosses the stack-only borrow boundary")
        .with_note(format!(
            "escape path: local `{name}` -> AMIR block {} -> storage that may outlive the borrow",
            block.as_usize()
        ))
        .with_note(reason.to_string())
        .with_hint(
            "stack-first alternatives: shorten the borrow, pass the owner, return owned data, or use an explicit heap type",
        )
}

fn local_name(local: LocalId, func: &AmirFunc, symbols: &SymbolTable) -> String {
    func.locals
        .get(local.as_usize())
        .and_then(|l| l.symbol)
        .map_or_else(
            || format!("_{}", local.as_usize()),
            |sym| symbols.get(sym).name.to_string(),
        )
}
