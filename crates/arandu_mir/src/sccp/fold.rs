use super::LatticeVal;
use crate::amir::{AmirConstant, AmirOperand, AmirRvalue};
use crate::literal_pool::{AmirLiteralEntry, AmirLiteralPool};
use crate::ops::{BinaryOp, UnaryOp};

// ---------------------------------------------------------------------------
// Rvalue evaluation
// ---------------------------------------------------------------------------

pub(super) fn eval_rvalue(
    rvalue: &AmirRvalue,
    lattice: &[LatticeVal],
    pool: &mut AmirLiteralPool,
) -> LatticeVal {
    match rvalue {
        AmirRvalue::Use(op) => operand_lattice(op, lattice),
        AmirRvalue::Unary { op, operand } => match operand_lattice(operand, lattice) {
            LatticeVal::Constant(c) => fold_unary(*op, c, pool)
                .map(LatticeVal::Constant)
                .unwrap_or(LatticeVal::Overdefined),
            LatticeVal::Overdefined => LatticeVal::Overdefined,
            LatticeVal::Undefined => LatticeVal::Undefined,
        },
        AmirRvalue::Binary { op, left, right } => {
            let l = operand_lattice(left, lattice);
            let r = operand_lattice(right, lattice);
            match (l, r) {
                (LatticeVal::Constant(lc), LatticeVal::Constant(rc)) => {
                    fold_binary(*op, lc, rc, pool)
                        .map(LatticeVal::Constant)
                        .unwrap_or(LatticeVal::Overdefined)
                }
                (LatticeVal::Overdefined, _) | (_, LatticeVal::Overdefined) => {
                    LatticeVal::Overdefined
                }
                _ => LatticeVal::Undefined,
            }
        }
        // Everything else (Load, Borrow, FieldAccess, Alloc, Call, etc.)
        // is treated as non-foldable.
        _ => LatticeVal::Overdefined,
    }
}

pub(super) fn operand_lattice(op: &AmirOperand, lattice: &[LatticeVal]) -> LatticeVal {
    match op {
        AmirOperand::Constant(c) => LatticeVal::Constant(*c),
        AmirOperand::Copy(t) | AmirOperand::Move(t) => lattice[t.as_usize()],
        AmirOperand::FunctionRef(_) | AmirOperand::GlobalRef(_) => LatticeVal::Overdefined,
    }
}

// ---------------------------------------------------------------------------
// Constant-folding helpers (adapted from optimize.rs)
// ---------------------------------------------------------------------------

fn fold_unary(
    op: UnaryOp,
    value: AmirConstant,
    pool: &mut AmirLiteralPool,
) -> Option<AmirConstant> {
    match (op, value) {
        (UnaryOp::Not, AmirConstant::Bool(b)) => Some(AmirConstant::Bool(!b)),
        (UnaryOp::Neg, v) => {
            let val = const_as_i128(&v, pool)?;
            checked_int(val.checked_neg(), pool)
        }
        (UnaryOp::BitNot, v) => {
            let val = const_as_i128(&v, pool)?;
            Some(AmirConstant::Pool(pool.intern_int((!val).to_string())))
        }
        _ => None,
    }
}

pub(super) fn fold_binary(
    op: BinaryOp,
    left: AmirConstant,
    right: AmirConstant,
    pool: &mut AmirLiteralPool,
) -> Option<AmirConstant> {
    match (left, right) {
        (AmirConstant::Bool(a), AmirConstant::Bool(b)) => match op {
            BinaryOp::And => Some(AmirConstant::Bool(a && b)),
            BinaryOp::Or => Some(AmirConstant::Bool(a || b)),
            BinaryOp::Equal => Some(AmirConstant::Bool(a == b)),
            BinaryOp::NotEqual => Some(AmirConstant::Bool(a != b)),
            _ => None,
        },
        (l, r) => {
            let lv = const_as_i128(&l, pool)?;
            let rv = const_as_i128(&r, pool)?;
            match op {
                BinaryOp::Add => checked_int(lv.checked_add(rv), pool),
                BinaryOp::Sub => checked_int(lv.checked_sub(rv), pool),
                BinaryOp::Mul => checked_int(lv.checked_mul(rv), pool),
                BinaryOp::Div if rv != 0 => checked_int(lv.checked_div(rv), pool),
                BinaryOp::Mod if rv != 0 => checked_int(lv.checked_rem(rv), pool),
                BinaryOp::BitOr => Some(int_const(lv | rv, pool)),
                BinaryOp::BitXor => Some(int_const(lv ^ rv, pool)),
                BinaryOp::BitAnd => Some(int_const(lv & rv, pool)),
                BinaryOp::ShiftLeft if (0..128).contains(&rv) => {
                    checked_int(lv.checked_shl(rv as u32), pool)
                }
                // Constants do not carry their Arandu signedness here. A
                // negative i128 bit pattern may represent either a negative
                // `int` or a high-bit `uint`; arithmetic and logical right
                // shifts differ for that case, so leave it for the backend.
                BinaryOp::ShiftRight if lv >= 0 && (0..128).contains(&rv) => {
                    checked_int(lv.checked_shr(rv as u32), pool)
                }
                BinaryOp::Equal => Some(AmirConstant::Bool(lv == rv)),
                BinaryOp::NotEqual => Some(AmirConstant::Bool(lv != rv)),
                BinaryOp::Lt => Some(AmirConstant::Bool(lv < rv)),
                BinaryOp::Gt => Some(AmirConstant::Bool(lv > rv)),
                BinaryOp::LtEqual => Some(AmirConstant::Bool(lv <= rv)),
                BinaryOp::GtEqual => Some(AmirConstant::Bool(lv >= rv)),
                _ => None,
            }
        }
    }
}

pub(super) fn const_as_i128(c: &AmirConstant, pool: &AmirLiteralPool) -> Option<i128> {
    match c {
        AmirConstant::Pool(id) => match pool.get(*id) {
            AmirLiteralEntry::Int(val) => arandu_middle::literal_pool::parse_int_literal(val),
            AmirLiteralEntry::Float(_)
            | AmirLiteralEntry::FloatBits(_)
            | AmirLiteralEntry::Str(_)
            | AmirLiteralEntry::Bytes(_)
            | AmirLiteralEntry::Char(_) => None,
        },
        AmirConstant::Bool(_) | AmirConstant::Nil => None,
    }
}

fn integer_shape(
    ty: crate::types::TypeId,
    interner: &crate::types::TypeInterner,
) -> Option<(u32, bool)> {
    use crate::types::{ArType, Primitive};
    let primitive = match interner.resolve(ty) {
        ArType::IntLiteral => Primitive::Int,
        ArType::Primitive(Primitive::Char) => return Some((32, false)),
        ArType::Primitive(Primitive::ISize | Primitive::USize) => return None,
        ArType::Primitive(primitive) => primitive,
        _ => return None,
    };
    // Pointer-sized integers were excluded above. For these fixed-width types,
    // IntegerType's canonical width is independent of this explicit layout.
    let integer =
        arandu_middle::ctfe::IntegerType::new(primitive, crate::layout::DataLayout::ptr_width(4))
            .ok()?;
    Some((u32::from(integer.bit_width()), integer.is_signed()))
}

pub(super) fn normalize_constant(
    value: AmirConstant,
    ty: crate::types::TypeId,
    interner: Option<&crate::types::TypeInterner>,
    pool: &mut AmirLiteralPool,
) -> Option<AmirConstant> {
    use crate::types::{ArType, Primitive};
    if let Some(interner) = interner {
        if matches!(interner.resolve(ty), ArType::Primitive(Primitive::Bool)) {
            return match value {
                AmirConstant::Bool(_) => Some(value),
                _ => const_as_i128(&value, pool).map(|number| AmirConstant::Bool(number != 0)),
            };
        }
        if let Some((width, signed)) = integer_shape(ty, interner) {
            let number = match value {
                AmirConstant::Bool(boolean) => i128::from(boolean),
                _ => const_as_i128(&value, pool)?,
            };
            let modulus = 1i128 << width;
            let bits = number.rem_euclid(modulus);
            let normalized = if signed && bits >= modulus / 2 {
                bits - modulus
            } else {
                bits
            };
            if normalized == number && matches!(value, AmirConstant::Pool(_)) {
                return Some(value);
            }
            return Some(int_const(normalized, pool));
        }
    }
    match value {
        AmirConstant::Pool(id) => match pool.get(id) {
            AmirLiteralEntry::Int(_)
            | AmirLiteralEntry::Float(_)
            | AmirLiteralEntry::FloatBits(_) => None,
            // These constants have no arithmetic-width-dependent conversion.
            AmirLiteralEntry::Str(_) | AmirLiteralEntry::Char(_) | AmirLiteralEntry::Bytes(_) => {
                Some(value)
            }
        },
        AmirConstant::Bool(_) | AmirConstant::Nil => Some(value),
    }
}

pub(super) fn evaluation_safe(
    rhs: &AmirRvalue,
    ty: crate::types::TypeId,
    interner: Option<&crate::types::TypeInterner>,
    lattice: &[LatticeVal],
    pool: &AmirLiteralPool,
) -> bool {
    if let AmirRvalue::Binary { op, left, right } = rhs
        && matches!(
            op,
            BinaryOp::Div | BinaryOp::Mod | BinaryOp::ShiftLeft | BinaryOp::ShiftRight
        )
    {
        let Some((width, signed)) = interner.and_then(|interner| integer_shape(ty, interner))
        else {
            return false;
        };
        if let (LatticeVal::Constant(left), LatticeVal::Constant(right)) = (
            operand_lattice(left, lattice),
            operand_lattice(right, lattice),
        ) && let (Some(left), Some(right)) =
            (const_as_i128(&left, pool), const_as_i128(&right, pool))
        {
            if matches!(op, BinaryOp::Div | BinaryOp::Mod) {
                return right != 0 && !(signed && left == -(1i128 << (width - 1)) && right == -1);
            }
            return right >= 0 && right < i128::from(width);
        }
    }
    true
}

fn int_const(val: i128, pool: &mut AmirLiteralPool) -> AmirConstant {
    AmirConstant::Pool(pool.intern_int(val.to_string()))
}

fn checked_int(val: Option<i128>, pool: &mut AmirLiteralPool) -> Option<AmirConstant> {
    val.map(|v| int_const(v, pool))
}
