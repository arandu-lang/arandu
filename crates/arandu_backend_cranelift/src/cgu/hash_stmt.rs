use super::HashContext;
use arandu_middle::amir::{
    AmirConstant, AmirOperand, AmirPlace, AmirProjection, AmirStmt, AmirTerminator,
};
use arandu_middle::literal_pool::AmirLiteralEntry;
use arandu_middle::types::{BorrowKind, BorrowPath, BorrowPathSegment, ReturnBorrowSummary};

impl HashContext<'_> {
    pub(super) fn place(&mut self, place: &AmirPlace) {
        self.hash.usize(place.local.as_usize());
        self.hash.usize(place.projections.len());
        for projection in &place.projections {
            match projection {
                AmirProjection::Field(symbol) => {
                    self.hash.tag(0);
                    self.symbol(*symbol);
                }
                AmirProjection::Variant(tag) => {
                    self.hash.tag(3);
                    self.hash.usize(*tag);
                }
                AmirProjection::Payload {
                    variant_tag,
                    index,
                    field_ty,
                    tuple_ty,
                } => {
                    self.hash.tag(4);
                    self.hash.usize(*variant_tag);
                    self.hash.usize(*index);
                    self.type_id(*field_ty);
                    if let Some(tuple_ty) = tuple_ty {
                        self.hash.tag(1);
                        self.type_id(*tuple_ty);
                    } else {
                        self.hash.tag(0);
                    }
                }
                AmirProjection::TupleField(index) => {
                    self.hash.tag(5);
                    self.hash.usize(*index);
                }
                AmirProjection::Index(operand) => {
                    self.hash.tag(1);
                    self.operand(operand);
                }
                AmirProjection::IndexConstant(index) => {
                    self.hash.tag(6);
                    self.hash.usize(*index);
                }
                AmirProjection::Deref => self.hash.tag(2),
            }
        }
    }

    pub(super) fn operand(&mut self, operand: &AmirOperand) {
        match operand {
            AmirOperand::Copy(temp) => {
                self.hash.tag(0);
                self.hash.usize(temp.as_usize());
            }
            AmirOperand::Move(temp) => {
                self.hash.tag(1);
                self.hash.usize(temp.as_usize());
            }
            AmirOperand::Constant(constant) => {
                self.hash.tag(2);
                self.constant(constant);
            }
            AmirOperand::FunctionRef(symbol) => {
                self.hash.tag(3);
                self.symbol(*symbol);
            }
            AmirOperand::GlobalRef(symbol) => {
                self.hash.tag(4);
                self.symbol(*symbol);
            }
        }
    }

    pub(super) fn constant(&mut self, constant: &AmirConstant) {
        match constant {
            AmirConstant::Pool(id) => {
                self.hash.tag(0);
                match self.program.literal_pool.entries.get(id.0 as usize) {
                    Some(literal) => {
                        self.hash.tag(0);
                        self.literal(literal);
                    }
                    None => {
                        self.hash.tag(1);
                        self.hash.u32(id.0);
                    }
                }
            }
            AmirConstant::Bool(value) => {
                self.hash.tag(1);
                self.hash.bool(*value);
            }
            AmirConstant::Nil => self.hash.tag(2),
        }
    }

    pub(super) fn literal(&mut self, literal: &AmirLiteralEntry) {
        match literal {
            AmirLiteralEntry::Int(value) => {
                self.hash.tag(0);
                self.hash.str(value);
            }
            AmirLiteralEntry::Float(value) => {
                self.hash.tag(1);
                self.hash.str(value);
            }
            AmirLiteralEntry::FloatBits(value) => {
                self.hash.tag(4);
                self.hash.bytes(&value.canonical_bytes());
            }
            AmirLiteralEntry::Str(value) => {
                self.hash.tag(2);
                self.hash.str(value);
            }
            AmirLiteralEntry::Char(value) => {
                self.hash.tag(3);
                self.hash.str(value);
            }
        }
    }

    pub(super) fn operands(&mut self, operands: &[AmirOperand]) {
        self.hash.usize(operands.len());
        for operand in operands {
            self.operand(operand);
        }
    }

    pub(super) fn stmt(&mut self, stmt: &AmirStmt) {
        match stmt {
            AmirStmt::Assign { lhs, rhs } => {
                self.hash.tag(0);
                self.hash.usize(lhs.as_usize());
                self.rvalue(rhs);
            }
            AmirStmt::Store { lhs, rhs } => {
                self.hash.tag(1);
                self.place(lhs);
                self.operand(rhs);
            }
            AmirStmt::Call {
                lhs,
                callee,
                args,
                return_borrow,
            } => {
                self.hash.tag(2);
                if let Some(lhs) = lhs {
                    self.hash.tag(1);
                    self.hash.usize(lhs.as_usize());
                } else {
                    self.hash.tag(0);
                }
                self.operand(callee);
                self.operands(args);
                if let Some(summary) = return_borrow {
                    self.hash.tag(1);
                    self.borrow_summary(summary);
                } else {
                    self.hash.tag(0);
                }
            }
            AmirStmt::Free(operand) => {
                self.hash.tag(3);
                self.operand(operand);
            }
            AmirStmt::StorageLive(local) => {
                self.hash.tag(4);
                self.hash.usize(local.as_usize());
            }
            AmirStmt::StorageDead(local) => {
                self.hash.tag(5);
                self.hash.usize(local.as_usize());
            }
            AmirStmt::Destroy(place) => {
                self.hash.tag(6);
                self.place(place);
            }
            AmirStmt::Nop => self.hash.tag(7),
        }
    }

    pub(super) fn terminator(&mut self, terminator: &AmirTerminator) {
        match terminator {
            AmirTerminator::Return => self.hash.tag(0),
            AmirTerminator::Goto { target, args } => {
                self.hash.tag(1);
                self.hash.usize(target.as_usize());
                self.operands(args);
            }
            AmirTerminator::Branch {
                condition,
                if_true,
                true_args,
                if_false,
                false_args,
            } => {
                self.hash.tag(2);
                self.operand(condition);
                self.hash.usize(if_true.as_usize());
                self.operands(true_args);
                self.hash.usize(if_false.as_usize());
                self.operands(false_args);
            }
            AmirTerminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => {
                self.hash.tag(3);
                self.operand(discriminant);
                self.hash.usize(targets.len());
                for (value, target, args) in targets {
                    self.hash.i128(*value);
                    self.hash.usize(target.as_usize());
                    self.operands(args);
                }
                self.hash.usize(otherwise.0.as_usize());
                self.operands(&otherwise.1);
            }
            AmirTerminator::Suspend {
                future,
                resume,
                args,
            } => {
                self.hash.tag(4);
                self.operand(future);
                self.hash.usize(resume.as_usize());
                self.operands(args);
            }
            AmirTerminator::Unreachable => self.hash.tag(5),
        }
    }

    pub(super) fn borrow_summary(&mut self, summary: &ReturnBorrowSummary) {
        self.hash.usize(summary.dependencies.len());
        for dependency in &summary.dependencies {
            self.borrow_path(&dependency.result_path);
            self.hash.tag(match dependency.kind {
                BorrowKind::Shared => 0,
                BorrowKind::Exclusive => 1,
            });
            self.hash.usize(dependency.sources.len());
            for source in &dependency.sources {
                self.hash.u32(source.parameter_index);
                self.borrow_path(&source.parameter_path);
            }
        }
    }

    pub(super) fn borrow_path(&mut self, path: &BorrowPath) {
        self.hash.usize(path.len());
        for segment in path.iter() {
            match segment {
                BorrowPathSegment::Tuple(index) => {
                    self.hash.tag(0);
                    self.hash.u32(*index);
                }
                BorrowPathSegment::Field(name) => {
                    self.hash.tag(1);
                    self.hash.str(name);
                }
                BorrowPathSegment::Variant(index) => {
                    self.hash.tag(2);
                    self.hash.u32(*index);
                }
                BorrowPathSegment::Payload(index) => {
                    self.hash.tag(3);
                    self.hash.u32(*index);
                }
                BorrowPathSegment::OptionSome => self.hash.tag(4),
                BorrowPathSegment::ResultOk => self.hash.tag(5),
                BorrowPathSegment::ResultErr => self.hash.tag(6),
                BorrowPathSegment::ArrayElement => self.hash.tag(7),
                BorrowPathSegment::NullableValue => self.hash.tag(8),
                BorrowPathSegment::CoroutinePayload => self.hash.tag(9),
                BorrowPathSegment::PollReady => self.hash.tag(10),
                BorrowPathSegment::RangeElement => self.hash.tag(11),
            }
        }
    }
}
