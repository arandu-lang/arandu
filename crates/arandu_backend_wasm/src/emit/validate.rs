use arandu_middle::Diagnostic;
use arandu_middle::amir::stmt::AmirStmt;
use arandu_middle::amir::value::{AmirPlace, AmirRvalue};
use arandu_middle::amir::{AmirFunc, AmirProgram};
use arandu_middle::types::TypeInterner;
use arandu_semantics::SymbolTable;

/// Reject `Place`-based operations the wasm memory model cannot represent.
///
/// The model stores aggregate values as pointers to heap cells held in the
/// wasm local itself. Empty-projection `Load`/`Store` are plain value copies
/// (register `local.set`/`local.get`) and always work. Borrows and projected
/// accesses need a real cell address, which requires the base local to be
/// memory-backed or pointer-like (`Ptr`/`Ref`/`RefMut`/`Nullable`); a bare
/// scalar local's value is not an address. This runs before translation so the
/// emitting code stays panic-free: the failure surfaces as a reportable ICE.
#[must_use]
pub(crate) fn validate_place_ops(
    program: &AmirProgram,
    symbols: &SymbolTable,
    interner: &TypeInterner,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for func in &program.funcs {
        let span = symbols
            .try_get(func.symbol)
            .map_or(arandu_base::Span::new(0, 0, 0), |s| s.span);
        for stmt in func.stmts.payloads.iter() {
            match stmt {
                AmirStmt::Store { lhs, .. } => {
                    reject_unaddressable_place(
                        func, interner, lhs, span, false, "Store", &mut diags,
                    );
                }
                AmirStmt::Assign { rhs, .. } => match rhs {
                    AmirRvalue::Load(place) => {
                        reject_unaddressable_place(
                            func, interner, place, span, false, "Load", &mut diags,
                        );
                    }
                    AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
                        reject_unaddressable_place(
                            func, interner, place, span, true, "Borrow", &mut diags,
                        );
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
    diags
}

fn reject_unaddressable_place(
    func: &AmirFunc,
    interner: &TypeInterner,
    place: &AmirPlace,
    span: arandu_base::Span,
    require_address: bool,
    label: &str,
    diags: &mut Vec<Diagnostic>,
) {
    if place.projections.is_empty() && !require_address {
        return;
    }
    let Some(local) = func.locals.get(place.local.as_usize()) else {
        return;
    };
    let ty = interner.resolve(local.ty);
    let pointer_like = matches!(
        ty,
        arandu_middle::types::ArType::Ptr(_)
            | arandu_middle::types::ArType::Ref(_)
            | arandu_middle::types::ArType::RefMut(_)
            | arandu_middle::types::ArType::Nullable(_)
            | arandu_middle::types::ArType::Slice(_)
    );
    if !local.is_memory && !pointer_like {
        diags.push(Diagnostic::ice(
            arandu_middle::diagnostics::DiagCode::ICEGEN002,
            format!(
                "{label} of {} requires a memory-backed or pointer-like base local (PLACE-ADDRESS)",
                local.id.as_usize()
            ),
            span,
        ));
    }
}
