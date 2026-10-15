//! Bounded, deterministic admission of the retained CTFE call closure.

use super::*;

struct AdmittedUnit {
    unit: Arc<CtfeFunction>,
    parent: Option<(usize, EvalLocation)>,
}

/// Source-order closure, not an execution trace: every retained direct call
/// is inspected, including calls in untaken ordinary branches. Recursion is
/// legal and visits each source function only once. No Rust recursion is used.
pub(super) struct Admission {
    units: Vec<AdmittedUnit>,
    indices: FxHashMap<FunctionInstance, usize>,
}

impl Admission {
    fn error(&self, mut index: usize, mut error: EvalError) -> EvalError {
        if error.trace.try_reserve(MAX_TRACE).is_err() {
            error.trace_truncated = true;
            return error;
        }
        while let Some((parent, site)) = self.units[index].parent {
            if error.trace.len() == MAX_TRACE {
                error.trace_truncated = true;
                break;
            }
            error.trace.push(site);
            index = parent;
        }
        error.trace.reverse();
        error
    }

    fn insert<C: FnMut() -> bool>(
        &mut self,
        unit: Arc<CtfeFunction>,
        parent: Option<(usize, EvalLocation)>,
        meter: &mut Meter<'_, C>,
    ) -> Result<(), EvalErrorKind> {
        meter.reserve(1)?;
        self.units
            .try_reserve(1)
            .map_err(|_| EvalErrorKind::AllocationFailed)?;
        self.indices
            .try_reserve(1)
            .map_err(|_| EvalErrorKind::AllocationFailed)?;
        self.indices.insert(unit.identity.clone(), self.units.len());
        self.units.push(AdmittedUnit { unit, parent });
        meter.values += 1;
        Ok(())
    }

    pub(super) fn inspect<P: FunctionProvider, C: FnMut() -> bool>(
        provider: &P,
        root: Arc<CtfeFunction>,
        source_root: bool,
        meter: &mut Meter<'_, C>,
    ) -> Result<Self, EvalError> {
        let origin = root.origin();
        let layout = root.layout;
        let mut admitted = Self {
            units: Vec::new(),
            indices: FxHashMap::default(),
        };
        admitted
            .insert(root, None, meter)
            .map_err(|kind| origin.error(kind))?;
        if !source_root {
            // An expression root carries its owner's location, not the identity
            // of a callable definition. A call back to that owner must resolve
            // through the provider, never recursively execute the expression.
            admitted.indices.clear();
        }
        let mut index = 0;
        while index < admitted.units.len() {
            let unit = Arc::clone(&admitted.units[index].unit);
            let check = (|| -> Result<(), EvalError> {
                meter.step().map_err(|kind| unit.origin().error(kind))?;
                Frame::validate(&unit, meter).map_err(|kind| unit.origin().error(kind))?;
                for block in &unit.function.blocks {
                    // Ranges/IDs were checked by validate; retain checked access
                    // here as this boundary also accepts hand-constructed AMIR.
                    for raw in
                        slice_range(block.statements).map_err(|kind| unit.origin().error(kind))?
                    {
                        let id = u32::try_from(raw)
                            .map(InstrId)
                            .map_err(|_| unit.origin().error(EvalErrorKind::InvalidIr))?;
                        let stmt = unit
                            .function
                            .try_stmt(id)
                            .ok_or_else(|| unit.origin().error(EvalErrorKind::InvalidIr))?;
                        let site = unit.statement_origin(block.id, stmt);
                        meter.step().map_err(|kind| site.error(kind))?;
                        let callee = match stmt {
                            AmirStmt::Assign { rhs, .. } => {
                                admitted_rvalue(rhs).map_err(|kind| site.error(kind))?;
                                match rhs {
                                    AmirRvalue::Array { items } | AmirRvalue::Tuple { items } => {
                                        for _ in items {
                                            meter.step().map_err(|kind| site.error(kind))?;
                                        }
                                    }
                                    AmirRvalue::StructLiteral { fields, .. } => {
                                        for (name, _) in fields {
                                            meter.step().map_err(|kind| site.error(kind))?;
                                            for _ in name.bytes() {
                                                meter.step().map_err(|kind| site.error(kind))?;
                                            }
                                        }
                                    }
                                    AmirRvalue::Load(place) => {
                                        if place.projections.len()
                                            > arandu_middle::types::TypeShape::MAX_DEPTH
                                        {
                                            return Err(site.error(EvalErrorKind::ValueLimit));
                                        }
                                        for _ in &place.projections {
                                            meter.step().map_err(|kind| site.error(kind))?;
                                        }
                                    }
                                    _ => {}
                                }
                                let mut bad_operand = false;
                                for_each_rvalue_operand(rhs, |operand| {
                                    bad_operand |= !scalar_operand(*operand);
                                });
                                if bad_operand {
                                    return Err(site.error(EvalErrorKind::UnsupportedOperation));
                                }
                                None
                            }
                            AmirStmt::Call {
                                callee: AmirOperand::FunctionRef(callee),
                                args,
                                return_borrow: None,
                                ..
                            } => {
                                for &arg in args {
                                    meter.step().map_err(|kind| site.error(kind))?;
                                    if !scalar_operand(arg) {
                                        return Err(site.error(EvalErrorKind::UnsupportedOperation));
                                    }
                                }
                                Some(*callee)
                            }
                            AmirStmt::Store { rhs, .. } if scalar_operand(*rhs) => None,
                            AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {
                                None
                            }
                            AmirStmt::Store { .. }
                            | AmirStmt::Call { .. }
                            | AmirStmt::Free(_)
                            | AmirStmt::Destroy(_) => {
                                return Err(site.error(EvalErrorKind::UnsupportedOperation));
                            }
                        };
                        if let Some(callee) = callee {
                            let key = unit.call_identity(callee);
                            if admitted.indices.contains_key(&key) {
                                continue;
                            }
                            // Charge even unavailable calls and poll before the
                            // provider, which may force an incremental query.
                            meter.step().map_err(|kind| site.error(kind))?;
                            let child = provider.instance(&key).map_err(|kind| site.error(kind))?;
                            if child.identity != key {
                                return Err(site.error(EvalErrorKind::InvalidIr));
                            }
                            if child.layout != layout {
                                return Err(site.error(EvalErrorKind::TargetMismatch));
                            }
                            admitted
                                .insert(child, Some((index, site)), meter)
                                .map_err(|kind| site.error(kind))?;
                        }
                    }
                    let mut bad_operand = false;
                    for_each_terminator_operand(&block.terminator, |operand| {
                        bad_operand |= !scalar_operand(*operand);
                    });
                    if bad_operand {
                        return Err(unit.origin().error(EvalErrorKind::UnsupportedOperation));
                    }
                }
                Ok(())
            })();
            if let Err(error) = check {
                return Err(admitted.error(index, error));
            }
            index += 1;
        }
        Ok(admitted)
    }

    pub(super) fn function(
        &self,
        key: &FunctionInstance,
    ) -> Result<Arc<CtfeFunction>, EvalErrorKind> {
        self.indices
            .get(key)
            .and_then(|&index| self.units.get(index))
            .map(|entry| Arc::clone(&entry.unit))
            .ok_or(EvalErrorKind::MissingFunction(key.definition))
    }
}
