use super::super::symbols::{finish, hash_str, hash_symbol_id, u32_le, u64_le, StableHash};
use blake3::Hasher;
use std::sync::Arc;

impl StableHash for crate::dataflow::DataflowFacts {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"DataflowFacts/v1");
        h.update(&u32_le(self.block.as_usize() as u32));
        h.update(&u32_le(self.live_in_count));
        h.update(&u32_le(self.live_out_count));
        h.update(&u32_le(self.init_in_count));
        h.update(&u32_le(self.moved_in_count));
        h.update(&u32_le(self.stmt_count));
        finish(h)
    }
}

impl StableHash for crate::dataflow::BorrowFacts {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"BorrowFacts/v2");
        h.update(&u32_le(self.block.as_usize() as u32));
        h.update(&u32_le(self.shared_in_count));
        h.update(&u32_le(self.exclusive_in_count));
        h.update(&u32_le(self.borrow_sites));
        h.update(&u32_le(self.shared_out_count));
        h.update(&u32_le(self.exclusive_out_count));
        finish(h)
    }
}

impl StableHash for arandu_mir::borrow_facts::BlockBorrowSummary {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"BlockBorrowSummary/v2");
        h.update(&u32_le(self.shared_in));
        h.update(&u32_le(self.exclusive_in));
        h.update(&u32_le(self.borrow_sites));
        h.update(&u32_le(self.shared_out));
        h.update(&u32_le(self.exclusive_out));
        finish(h)
    }
}

impl StableHash for Vec<arandu_mir::borrow_facts::BlockBorrowSummary> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"Vec<BlockBorrowSummary>/v1");
        h.update(&u64_le(self.len() as u64));
        for s in self {
            h.update(s.stable_hash().as_bytes());
        }
        finish(h)
    }
}

impl StableHash for crate::dataflow::LivenessMap {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"LivenessMap/v1");
        h.update(&u64_le(self.live_in_counts.len() as u64));
        for &c in &self.live_in_counts {
            h.update(&u32_le(c));
        }
        for &c in &self.live_out_counts {
            h.update(&u32_le(c));
        }
        finish(h)
    }
}

impl StableHash for crate::dataflow::IdeDiagnostic {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(self.code.as_bytes());
        h.update(&[self.severity]);
        h.update(self.message.as_bytes());
        if let Some(primary_label) = &self.primary_label {
            h.update(&[1]);
            hash_str(&mut h, primary_label);
        } else {
            h.update(&[0]);
        }
        h.update(&u32_le(self.file_id));
        h.update(&u32_le(self.start));
        h.update(&u32_le(self.end));
        h.update(&u64_le(self.labels.len() as u64));
        for label in &self.labels {
            h.update(&u32_le(label.file_id));
            h.update(&u32_le(label.start));
            h.update(&u32_le(label.end));
            hash_str(&mut h, &label.message);
        }
        h.update(&u64_le(self.notes.len() as u64));
        for note in &self.notes {
            hash_str(&mut h, note);
        }
        h.update(&u64_le(self.hints.len() as u64));
        for hint in &self.hints {
            hash_str(&mut h, &hint.message);
            if let Some(replacement) = &hint.replacement {
                h.update(&[1]);
                h.update(&u32_le(replacement.file_id));
                h.update(&u32_le(replacement.start));
                h.update(&u32_le(replacement.end));
                hash_str(&mut h, &replacement.new_text);
            } else {
                h.update(&[0]);
            }
        }
        if let Some(f) = self.func {
            h.update(&[1]);
            hash_symbol_id(&mut h, f);
        } else {
            h.update(&[0]);
        }
        if let Some(b) = self.block {
            h.update(&[1]);
            h.update(&u32_le(b.as_usize() as u32));
        } else {
            h.update(&[0]);
        }
        finish(h)
    }
}

impl StableHash for Vec<crate::dataflow::IdeDiagnostic> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&u64_le(self.len() as u64));
        for d in self {
            h.update(d.stable_hash().as_bytes());
        }
        finish(h)
    }
}

impl StableHash for Vec<arandu_middle::SymbolId> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(&u64_le(self.len() as u64));
        for id in self {
            hash_symbol_id(&mut h, *id);
        }
        finish(h)
    }
}

impl StableHash for crate::passes::ItemSourceInput {
    fn stable_hash(&self) -> blake3::Hash {
        // Content-address only this item's source fingerprint — not the whole Program.
        let mut h = Hasher::new();
        h.update(b"ItemSourceInput/v3");
        hash_symbol_id(&mut h, self.item_sym);
        h.update(&u32_le(self.item_start));
        h.update(self.body_fp.as_bytes());
        finish(h)
    }
}

impl StableHash for Arc<[crate::highlight::HlToken]> {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"HlTokenSlice/v1");
        h.update(&u64_le(self.len() as u64));
        for t in self.iter() {
            h.update(&u32_le(t.start));
            h.update(&u32_le(t.end));
            h.update(&[t.kind as u8]);
            h.update(&u32_le(u32::from(t.mods)));
        }
        finish(h)
    }
}

impl StableHash for arandu_parser::SyntaxTree {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = Hasher::new();
        h.update(b"SyntaxTree/v2");
        h.update(self.text().as_bytes());
        // Hash ranges (no per-item String alloc).
        let ranges = self.item_ranges();
        h.update(&u64_le(ranges.len() as u64));
        let text = self.text();
        let bytes = text.as_bytes();
        for (s, e) in ranges {
            h.update(&u32_le(s));
            h.update(&u32_le(e));
            let s = (s as usize).min(bytes.len());
            let e = (e as usize).min(bytes.len()).max(s);
            h.update(&bytes[s..e]);
        }
        finish(h)
    }
}
