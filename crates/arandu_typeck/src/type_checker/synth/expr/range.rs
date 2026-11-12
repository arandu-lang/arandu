//! Range values: `a..b` is a `std.core.range.Range<T>` and `a..=b` a
//! `RangeInclusive<T>`, plain structs with `start` and `end` (as Rust's
//! `core::ops`). A unit compiled without the standard library keeps the
//! built-in `ArType::Range`, which only a loop or an index consumes.

use arandu_middle::symbol_table::LangItem;

use crate::type_checker::TypeChecker;
use crate::type_checker::types::{ArType, TypeId};

/// The type of `start..end` (or `..=`) over `element`.
pub(crate) fn range_type(
    checker: &mut TypeChecker<'_>,
    element: TypeId,
    inclusive: bool,
) -> TypeId {
    let item = if inclusive {
        LangItem::RangeInclusive
    } else {
        LangItem::Range
    };
    let ty = match checker.symbols.lang_item_symbol(item) {
        Some(symbol) => ArType::named(symbol, &[element], &checker.type_info.type_interner),
        None => ArType::Range(element),
    };
    checker.intern(ty)
}

/// The element type of a range type and whether it includes its end.
pub(crate) fn range_parts(checker: &TypeChecker<'_>, ty: &ArType) -> Option<(TypeId, bool)> {
    match ty {
        ArType::Range(element) => Some((*element, false)),
        ArType::Named(symbol, args) => {
            let inclusive = match checker.symbols.try_get(*symbol)?.lang_item? {
                LangItem::Range => false,
                LangItem::RangeInclusive => true,
                _ => return None,
            };
            let element = checker
                .type_info
                .type_interner
                .type_args(*args)
                .first()
                .copied()?;
            Some((element, inclusive))
        }
        _ => None,
    }
}
