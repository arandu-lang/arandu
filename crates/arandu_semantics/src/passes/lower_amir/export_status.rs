//! C status-code lowering for `@Export` functions returning `Result<T, E>`.
//!
//! C has no sum types and no exception channel, so every C SDK in existence
//! models failure with a status integer and an out-parameter (`HRESULT`,
//! `VkResult`, `glib` `GError`). An exported Arandu function returning
//! `Result<T, E>` is rewritten here, once, into exactly that shape:
//!
//! ```text
//! @Export func read(id: u32): Result<Frame, u32>
//! // C: uint32_t read(uint32_t id, Frame *out);   // status 0 == Ok
//! ```
//!
//! Doing it at AMIR level means every backend inherits the transformation and
//! the generated header is derived from the same signature, instead of each
//! backend inventing its own marshalling.
//!
//! The rewrite is deliberately all-or-nothing: a function whose returns are
//! not direct `Ok(..)`/`Err(..)` constructions is rejected with
//! [`DiagCode::T049InvalidFfiExport`] rather than emitting a boundary whose
//! status could be wrong.

use crate::amir::{
    AmirConstant, AmirFunc, AmirLocal, AmirOperand, AmirPlace, AmirProgram, AmirProjection,
    AmirRvalue, AmirStmt, AmirStmtTable, AmirTemp, InstrId, LocalId, TempId,
};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::literal_pool::LiteralId;
use crate::types::{ArType, Primitive, TypeId, TypeInterner};
use crate::{SymbolId, TypeCheckResult};
use arandu_middle::DenseRange;

/// `Result::Ok` is the first variant of `stdlib/core/result.aru`, `Err` the
/// second. AMIR stores the declaration-order tag.
const RESULT_OK_TAG: usize = 0;
const RESULT_ERR_TAG: usize = 1;

/// The return register: `Assign { lhs: return_temp(), .. }` is how AMIR spells
/// `return`; the terminator itself carries no value.
fn return_temp() -> TempId {
    TempId::from_usize(0)
}

/// Rewrites exported `Result`-returning functions into the C status shape.
///
/// Diagnostics are appended to `diagnostics`; a function that cannot be
/// rewritten is left untouched and reports [`DiagCode::T049InvalidFfiExport`].
pub fn lower_export_status_returns(
    program: &mut AmirProgram,
    tc: &TypeCheckResult,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let interner = &tc.type_info.type_interner;
    let exported: Vec<(usize, SymbolId, TypeId, TypeId)> = program
        .funcs
        .iter()
        .enumerate()
        .filter(|(_, func)| tc.symbols.is_exported(func.symbol))
        .filter_map(|(index, func)| match interner.resolve(func.return_type) {
            ArType::Result(ok, err) => Some((index, func.symbol, ok, err)),
            _ => None,
        })
        .collect();

    for (index, symbol, ok_ty, err_ty) in exported {
        let name = tc.symbols.get(symbol).name.to_string();
        let span = tc.symbols.get(symbol).span;
        let ok_is_void = matches!(interner.resolve(ok_ty), ArType::Void);

        if let Some(reason) = status_type_error(interner, err_ty) {
            diagnostics.push(Diagnostic::error(
                DiagCode::T049InvalidFfiExport,
                format!("cannot export `{name}` as a C status code: {reason}"),
                span,
            ));
            continue;
        }
        if let Some(caller) = in_language_caller(program, tc.symbols.as_ref(), symbol) {
            diagnostics.push(Diagnostic::error(
                DiagCode::T049InvalidFfiExport,
                format!(
                    "`{name}` returns `Result`, which crosses the boundary as a C status code, so \
                     its signature changes at the boundary; `{caller}` still calls the Arandu \
                     sum-type form. From Arandu, call a wrapper that returns the status and takes \
                     `out: mut ref T`"
                ),
                span,
            ));
            continue;
        }
        if let Some(reason) = status_shape_error(&program.funcs[index], ok_is_void) {
            diagnostics.push(Diagnostic::error(
                DiagCode::T049InvalidFfiExport,
                format!("cannot export `{name}` as a C status code: {reason}"),
                span,
            ));
            continue;
        }

        let out_ty = if ok_is_void {
            None
        } else {
            Some(interner.intern(ArType::Ptr(ok_ty)))
        };
        let zero = program.literal_pool.intern_int("0");
        rewrite_function(&mut program.funcs[index], err_ty, out_ty, zero, span);
    }
}

/// True when `E` is a fixed-width integer. Target-dependent widths are refused
/// because the status crosses the binary boundary.
fn status_type_error(interner: &TypeInterner, ty: TypeId) -> Option<String> {
    match interner.resolve(ty) {
        ArType::Primitive(
            Primitive::I8
            | Primitive::I16
            | Primitive::I32
            | Primitive::I64
            | Primitive::U8
            | Primitive::U16
            | Primitive::U32
            | Primitive::U64,
        ) => None,
        ArType::Primitive(
            Primitive::Int | Primitive::Uint | Primitive::ISize | Primitive::USize,
        ) => Some(
            "the status type needs a fixed width; use `i32`/`u32` instead of \
                 `int`/`uint`/`isize`/`usize`"
                .to_owned(),
        ),
        _ => Some("the status type must be a fixed-width integer such as `u32`".to_owned()),
    }
}

/// Validates the AMIR shape needed for the rewrite without mutating anything.
fn status_shape_error(func: &AmirFunc, ok_is_void: bool) -> Option<String> {
    let mut returns = 0usize;
    for block in &func.blocks {
        let start = block.statements.start as usize;
        let end = start + block.statements.len as usize;
        for position in start..end {
            let Some(AmirStmt::Assign { lhs, rhs }) = func.stmts.get(InstrId::from_usize(position))
            else {
                continue;
            };
            if *lhs != return_temp() {
                continue;
            }
            returns += 1;
            let AmirRvalue::EnumConstruct {
                variant_tag,
                payload,
            } = rhs
            else {
                return Some(
                    "every `return` must write `Ok(value)` or `Err(status)` directly; assign the \
                     expression to a local and return the variant explicitly"
                        .to_owned(),
                );
            };
            match *variant_tag {
                RESULT_OK_TAG if !ok_is_void && payload.is_none() => {
                    return Some("`Ok(..)` is missing its payload".to_owned());
                }
                RESULT_ERR_TAG if payload.is_none() => {
                    return Some("`Err(..)` is missing its status value".to_owned());
                }
                RESULT_OK_TAG | RESULT_ERR_TAG => {}
                other => return Some(format!("`Result` has no variant tag {other}")),
            }
        }
    }
    (returns == 0).then(|| "the function never returns a `Result` value".to_owned())
}

/// Finds an Arandu-level call to `callee`, which the rewritten signature would
/// silently break.
fn in_language_caller(
    program: &AmirProgram,
    symbols: &arandu_middle::SymbolTable,
    callee: SymbolId,
) -> Option<String> {
    for func in &program.funcs {
        for block in &func.blocks {
            let start = block.statements.start as usize;
            let end = start + block.statements.len as usize;
            for position in start..end {
                let Some(AmirStmt::Call {
                    callee: AmirOperand::FunctionRef(target),
                    ..
                }) = func.stmts.get(InstrId::from_usize(position))
                else {
                    continue;
                };
                if *target == callee {
                    return Some(symbols.get(func.symbol).name.to_string());
                }
            }
        }
    }
    None
}

/// Applies the rewrite: status return type, trailing out-pointer parameter and
/// `Ok`/`Err` construction replacement. The statement table is rebuilt because
/// one construction becomes two statements.
fn rewrite_function(
    func: &mut AmirFunc,
    status_ty: TypeId,
    out_ty: Option<TypeId>,
    zero: LiteralId,
    span: arandu_middle::Span,
) {
    let out = out_ty.map(|ty| {
        let temp = TempId::from_usize(func.temps.len());
        func.temps.push(AmirTemp {
            id: temp,
            ty,
            is_copy: true,
            is_nullable: false,
            span,
        });
        func.params.push(temp);
        let local = LocalId::from_usize(func.locals.len());
        func.locals.push(AmirLocal {
            id: local,
            ty,
            is_memory: true,
            symbol: None,
            span,
            use_span: Some(span),
        });
        let place = AmirPlace {
            local,
            projections: [AmirProjection::Deref].into_iter().collect(),
        };
        // Where the incoming pointer itself lives: the prologue binds the
        // parameter into the stack home, and every payload store goes through
        // `place` above.
        let binding = AmirPlace {
            local,
            projections: Default::default(),
        };
        (temp, place, binding)
    });

    func.return_type = status_ty;
    // The return register carries the status now: leaving the old sum type on
    // this temp makes the backend coerce an `i32` status into a `Result`
    // aggregate (ICE-GEN-001).
    if let Some(temp) = func.temps.get_mut(return_temp().as_usize()) {
        temp.ty = status_ty;
    }

    let mut table = AmirStmtTable::new();
    for (index, block) in func.blocks.iter_mut().enumerate() {
        let start = block.statements.start as usize;
        let end = start + block.statements.len as usize;
        let new_start = table.len();
        if index == 0
            && let Some((temp, _, binding)) = &out
        {
            table.push(AmirStmt::Store {
                lhs: binding.clone(),
                rhs: AmirOperand::Copy(*temp),
            });
        }
        for position in start..end {
            let Some(statement) = func.stmts.get(InstrId::from_usize(position)) else {
                continue;
            };
            let rewritten = match statement {
                AmirStmt::Assign {
                    lhs,
                    rhs:
                        AmirRvalue::EnumConstruct {
                            variant_tag,
                            payload,
                        },
                } if *lhs == return_temp() => {
                    if *variant_tag == RESULT_OK_TAG {
                        if let (Some(payload), Some((_, place, _))) = (payload, &out) {
                            table.push(AmirStmt::Store {
                                lhs: place.clone(),
                                rhs: *payload,
                            });
                        }
                        AmirStmt::Assign {
                            lhs: return_temp(),
                            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(zero))),
                        }
                    } else {
                        AmirStmt::Assign {
                            lhs: return_temp(),
                            rhs: AmirRvalue::Use(
                                payload.unwrap_or(AmirOperand::Constant(AmirConstant::Nil)),
                            ),
                        }
                    }
                }
                other => other.clone(),
            };
            table.push(rewritten);
        }
        let len = table.len() - new_start;
        block.statements = DenseRange::try_new(new_start, len).unwrap_or_else(DenseRange::empty);
    }
    func.stmts = table;
}
