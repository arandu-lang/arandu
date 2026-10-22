//! AMIR program, function, and CTFE hashes.

use arandu_semantics::amir::AmirProgram;
use blake3::Hasher;

use super::nodes::{
    hash_ar_type, hash_id, hash_optional_span, hash_optional_symbol_id, hash_span, hash_stmt,
    hash_terminator,
};
use super::symbols::{
    finish, hash_literal_entry, hash_return_borrow_summary, hash_str, hash_symbol_id, u32_le,
    u64_le, StableHash,
};

impl StableHash for AmirProgram {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"AmirProgram/v4");
        h.update(&u64_le(self.funcs.len() as u64));
        for f in &self.funcs {
            h.update(f.stable_hash().as_bytes());
        }
        h.update(&u64_le(self.literal_pool.entries.len() as u64));
        for entry in &self.literal_pool.entries {
            hash_literal_entry(&mut h, entry);
        }
        // HashMap iteration order must not influence the content hash.
        let mut externs: Vec<_> = self.extern_funcs.iter().collect();
        externs.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
        h.update(&u64_le(externs.len() as u64));
        for (symbol, (params, result)) in externs {
            hash_symbol_id(&mut h, *symbol);
            h.update(&u64_le(params.len() as u64));
            for ty in params {
                hash_ar_type(&mut h, ty);
            }
            hash_ar_type(&mut h, result);
        }
        h.update(&u64_le(self.debug_bindings.len() as u64));
        for binding in &self.debug_bindings {
            hash_symbol_id(&mut h, binding.function);
            hash_id(&mut h, binding.temp.as_usize());
            hash_id(&mut h, binding.local.as_usize());
        }
        h.update(&u64_le(self.debug_blocks.len() as u64));
        for block in &self.debug_blocks {
            hash_symbol_id(&mut h, block.function);
            hash_id(&mut h, block.block.as_usize());
            h.update(&u32_le(block.span.file_id));
            h.update(&u32_le(block.span.start));
            h.update(&u32_le(block.span.end));
        }
        finish(h)
    }
}

impl StableHash for crate::passes::LowerAmirArtifacts {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(self.amir.stable_hash().as_bytes());
        h.update(self.type_check.stable_hash().as_bytes());
        finish(h)
    }
}

impl StableHash for crate::passes::PreparedHir {
    fn stable_hash(&self) -> blake3::Hash {
        let mut hash = Hasher::new();
        hash.update(b"PreparedHir/v1");
        hash.update(self.source_fingerprint.as_bytes());
        hash.update(self.type_check.stable_hash().as_bytes());
        hash.update(self.diagnostics.stable_hash().as_bytes());
        hash.update(&[u8::from(self.hir.is_some())]);
        hash.finalize()
    }
}

impl StableHash for crate::ctfe::CtfeLowering {
    fn stable_hash(&self) -> blake3::Hash {
        use crate::ctfe::BuildFailure;
        let mut hash = Hasher::new();
        hash.update(b"CtfeLowering/v3");
        match &self.result {
            Ok(unit) => {
                hash.update(&[0]);
                hash.update(unit.identity().stable_hash().as_bytes());
                for (symbol, key) in unit.call_identities() {
                    hash_symbol_id(&mut hash, *symbol);
                    hash.update(key.stable_hash().as_bytes());
                }
                hash.update(unit.function().stable_hash().as_bytes());
                for bytes in unit.scalar_type_bytes() {
                    hash.update(&bytes);
                }
                // All literal spellings/kinds belong to this function's pool.
                for entry in &unit.literals().entries {
                    hash_literal_entry(&mut hash, entry);
                }
                // The whole explicit layout is part of the unit, even when no
                // pointer-sized integer happens to occur in this function.
                for class in [
                    unit.layout().pointer,
                    unit.layout().float,
                    unit.layout().i64,
                    unit.layout().f64,
                ] {
                    hash.update(&class.size.to_le_bytes());
                    hash.update(&class.abi_align.to_le_bytes());
                }
            }
            Err(BuildFailure::MissingFunction) => {
                hash.update(&[1]);
            }
            Err(BuildFailure::GenericFunction) => {
                hash.update(&[2]);
            }
            Err(BuildFailure::Diagnostics(diagnostics)) => {
                hash.update(&[3]);
                hash.update(diagnostics.stable_hash().as_bytes());
            }
            Err(BuildFailure::Evaluation(error)) => {
                hash.update(&[4]);
                hash_ctfe_error(&mut hash, error);
            }
            Err(BuildFailure::InvalidRoot) => {
                hash.update(&[5]);
            }
            Err(BuildFailure::RuntimeCapture(capture)) => {
                hash.update(&[6]);
                hash_symbol_id(&mut hash, capture.symbol);
                hash_str(&mut hash, &capture.name);
                for span in [capture.use_span, capture.declaration_span] {
                    hash.update(&span.file_id.to_le_bytes());
                    hash.update(&span.start.to_le_bytes());
                    hash.update(&span.end.to_le_bytes());
                }
            }
        }
        finish(hash)
    }
}

fn hash_ctfe_error(hash: &mut Hasher, error: &arandu_mir::ctfe::EvalErrorKind) {
    use arandu_middle::ctfe::ConstValueError as V;
    use arandu_mir::ctfe::{EvalErrorKind as E, ScalarEvalError as S};
    let tag = match error {
        E::Cancelled => 0,
        E::FuelExhausted => 1,
        E::FrameLimit => 2,
        E::ValueLimit => 3,
        E::AllocationFailed => 4,
        E::MissingFunction(_) => 5,
        E::UnavailableFunction(_) => 6,
        E::UnsupportedType(_) => 7,
        E::UnsupportedOperation => 8,
        E::InvalidIr => 9,
        E::Uninitialized => 10,
        E::TypeMismatch => 11,
        E::InvalidLiteral => 12,
        E::TargetMismatch => 13,
        E::Arithmetic(_) => 14,
        E::Value(_) => 15,
        E::InvalidLayout(_) => 16,
    };
    hash.update(&[tag]);
    match error {
        E::InvalidLayout(error) => {
            use arandu_middle::layout::DataLayoutError;
            hash.update(&[match error {
                DataLayoutError::PointerWidth => 0,
                DataLayoutError::ScalarWidth => 1,
                DataLayoutError::Alignment => 2,
                DataLayoutError::FloatLayout => 3,
            }]);
        }
        E::MissingFunction(symbol) | E::UnavailableFunction(symbol) => {
            hash_symbol_id(hash, *symbol)
        }
        E::UnsupportedType(id) => hash_id(hash, id.as_usize()),
        E::Arithmetic(error) => match error {
            S::TypeMismatch => {
                hash.update(&[0]);
            }
            S::Overflow(ty) => {
                hash.update(&[1]);
                hash.update(&ty.canonical_bytes());
            }
            S::DivisionByZero => {
                hash.update(&[2]);
            }
            S::InvalidShift { amount, bit_width } => {
                hash.update(&[3, *bit_width]);
                hash.update(&amount.to_le_bytes());
            }
            S::UnsupportedBinary(op) => {
                hash.update(&[4, op.stable_tag()]);
            }
            S::UnsupportedUnary(op) => {
                hash.update(&[5, op.stable_tag()]);
            }
        },
        E::Value(error) => match error {
            V::UnsupportedIntegerType(primitive) => {
                hash.update(&[0]);
                hash_ar_type(hash, &arandu_middle::types::ArType::Primitive(*primitive));
            }
            V::UnsupportedPointerWidth(width) => {
                hash.update(&[1]);
                hash.update(&width.to_le_bytes());
            }
            V::OutOfRange { ty, value } => {
                hash.update(&[2]);
                hash.update(&ty.canonical_bytes());
                hash.update(&value.to_le_bytes());
            }
            V::NegativeConstGeneric(value) => {
                hash.update(&[3]);
                hash.update(&value.to_le_bytes());
            }
            V::StructuralLimit => {
                hash.update(&[4]);
            }
            V::InvalidAggregateShape => {
                hash.update(&[5]);
            }
        },
        E::Cancelled
        | E::FuelExhausted
        | E::FrameLimit
        | E::ValueLimit
        | E::AllocationFailed
        | E::UnsupportedOperation
        | E::InvalidIr
        | E::Uninitialized
        | E::TypeMismatch
        | E::InvalidLiteral
        | E::TargetMismatch => {}
    }
}

impl StableHash for crate::passes::BorrowInterfaces {
    fn stable_hash(&self) -> blake3::Hash {
        let mut hasher = Hasher::new();
        hasher.update(&u64_le(self.entries.len() as u64));
        for (symbol, summary) in &self.entries {
            hash_symbol_id(&mut hasher, *symbol);
            hash_return_borrow_summary(&mut hasher, summary);
        }
        hasher.update(&u64_le(self.instances.len() as u64));
        for (key, summary) in &self.instances {
            hasher.update(key.stable_hash().as_bytes());
            hash_return_borrow_summary(&mut hasher, summary);
        }
        finish(hasher)
    }
}

impl StableHash for petgraph::Graph<u32, ()> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&u64_le(self.node_count() as u64));
        h.update(&u64_le(self.edge_count() as u64));
        let mut nodes: Vec<u32> = self.node_weights().copied().collect();
        nodes.sort_unstable();
        for n in nodes {
            h.update(&u32_le(n));
        }
        let mut edges: Vec<(u32, u32)> = self
            .edge_indices()
            .filter_map(|e| {
                let (source, target) = self.edge_endpoints(e)?;
                let s_weight = *self.node_weight(source)?;
                let t_weight = *self.node_weight(target)?;
                Some((s_weight, t_weight))
            })
            .collect();
        edges.sort_unstable();
        for (s, t) in edges {
            h.update(&u32_le(s));
            h.update(&u32_le(t));
        }
        finish(h)
    }
}

impl StableHash for arandu_middle::amir::AmirFunc {
    fn stable_hash(&self) -> blake3::Hash {
        use arandu_middle::amir::AmirReceiver;
        use arandu_middle::hir::ReceiverKind;

        let mut h = Hasher::new();
        h.update(b"AmirFunc/v2");
        hash_symbol_id(&mut h, self.symbol);
        hash_id(&mut h, self.return_type.as_usize());
        match self.receiver {
            None => {
                h.update(&[0]);
            }
            Some(AmirReceiver { temp, kind }) => {
                h.update(&[
                    1,
                    match kind {
                        ReceiverKind::Shared => 0,
                        ReceiverKind::Mut => 1,
                        ReceiverKind::Own => 2,
                    },
                ]);
                hash_id(&mut h, temp.as_usize());
            }
        };
        h.update(&u64_le(self.params.len() as u64));
        for param in &self.params {
            hash_id(&mut h, param.as_usize());
        }
        h.update(&u64_le(self.blocks.len() as u64));
        h.update(&u64_le(self.locals.len() as u64));
        h.update(&u64_le(self.temps.len() as u64));
        h.update(&u64_le(self.stmts.kinds.len() as u64));
        for kind in self.stmts.kinds.raw.iter() {
            h.update(&[match kind {
                arandu_middle::amir::AmirStmtKind::Assign => 0,
                arandu_middle::amir::AmirStmtKind::Store => 1,
                arandu_middle::amir::AmirStmtKind::Call => 2,
                arandu_middle::amir::AmirStmtKind::Free => 3,
                arandu_middle::amir::AmirStmtKind::StorageLive => 4,
                arandu_middle::amir::AmirStmtKind::StorageDead => 5,
                arandu_middle::amir::AmirStmtKind::Destroy => 6,
                arandu_middle::amir::AmirStmtKind::Nop => 7,
            }]);
        }
        h.update(&u64_le(self.stmts.payloads.len() as u64));
        for local in &self.locals {
            hash_id(&mut h, local.id.as_usize());
            hash_id(&mut h, local.ty.as_usize());
            h.update(&[u8::from(local.is_memory)]);
            hash_optional_symbol_id(&mut h, local.symbol);
            hash_span(&mut h, local.span);
            hash_optional_span(&mut h, local.use_span);
        }
        for temp in &self.temps {
            hash_id(&mut h, temp.id.as_usize());
            hash_id(&mut h, temp.ty.as_usize());
            h.update(&[u8::from(temp.is_copy), u8::from(temp.is_nullable)]);
            hash_span(&mut h, temp.span);
        }
        h.update(&u64_le(self.block_params.len() as u64));
        for param in &self.block_params {
            hash_id(&mut h, param.id.as_usize());
            hash_id(&mut h, param.local.as_usize());
            hash_id(&mut h, param.ty.as_usize());
            match &param.from {
                Some(from) => {
                    h.update(&[1]);
                    hash_str(&mut h, from);
                }
                None => {
                    h.update(&[0]);
                }
            }
            h.update(&[u8::from(param.moved)]);
        }
        for b in &self.blocks {
            hash_id(&mut h, b.id.as_usize());
            hash_id(&mut h, b.statements.start as usize);
            hash_id(&mut h, b.statements.len as usize);
            // Block params live in the dense `block_params` pool; hash the
            // range so a change in block signature invalidates the hash.
            hash_id(&mut h, b.params.start as usize);
            hash_id(&mut h, b.params.len as usize);
            hash_terminator(&mut h, &b.terminator);
        }
        for id in self.stmts.iter_ids() {
            if let Some(stmt) = self.stmts.get(id) {
                hash_stmt(&mut h, stmt);
            }
        }
        hash_id(&mut h, self.cfg.successors.len());
        for successors in &self.cfg.successors {
            hash_id(&mut h, successors.len());
            for block in successors {
                hash_id(&mut h, block.as_usize());
            }
        }
        hash_id(&mut h, self.cfg.predecessors.len());
        for predecessors in &self.cfg.predecessors {
            hash_id(&mut h, predecessors.len());
            for block in predecessors {
                hash_id(&mut h, block.as_usize());
            }
        }
        finish(h)
    }
}
