//! Shared immutable sources with independent typed destinations.
use super::CEmitter;
use std::fmt::Write;

impl CEmitter<'_> {
    pub(super) fn emit_static_sources(&mut self) {
        let mut sources = rustc_hash::FxHashMap::default();
        let mut blobs = Vec::new();
        for function in &self.program.funcs {
            if !self.live_functions.contains(&function.symbol) {
                continue;
            }
            let mut little = arandu_mir::static_data::static_initializers(
                function,
                self.interner,
                self.provider,
                &self.program.literal_pool,
                self.layout.data_layout,
                true,
            );
            let mut big = arandu_mir::static_data::static_initializers(
                function,
                self.interner,
                self.provider,
                &self.program.literal_pool,
                self.layout.data_layout,
                false,
            );
            for id in function.stmts.iter_ids() {
                let (Some(little), Some(big)) = (little.remove(&id), big.remove(&id)) else {
                    continue;
                };
                if little.alignment > 16 || !little.alignment.is_power_of_two() {
                    continue;
                }
                let key = (little.bytes, big.bytes);
                let index = match sources.get(&key) {
                    Some(&index) => index,
                    None => {
                        let index = blobs.len();
                        blobs.push(key.clone());
                        sources.insert(key, index);
                        index
                    }
                };
                self.static_sources.insert((function.symbol, id), index);
            }
        }
        for (index, (little, big)) in blobs.iter().enumerate() {
            // The C compiler chooses byte order; the Arandu driver owns sizes
            // and alignments. No source bytes depend on the compiler host.
            if little != big {
                let _ = writeln!(
                    self.output,
                    "#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__"
                );
                self.emit_static_bytes(index, big);
                let _ = writeln!(
                    self.output,
                    "#elif defined(_WIN32) || (defined(__BYTE_ORDER__) && defined(__ORDER_LITTLE_ENDIAN__) && __BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__)"
                );
            }
            self.emit_static_bytes(index, little);
            if little != big {
                let _ = writeln!(self.output, "#else");
                let _ = writeln!(
                    self.output,
                    "#error \"Arandu static data requires a known target byte order\""
                );
                let _ = writeln!(self.output, "#endif");
            }
        }
    }

    fn emit_static_bytes(&mut self, index: usize, bytes: &[u8]) {
        let _ = write!(
            self.output,
            "_Alignas(16) static const uint8_t __ar_static_{index}[{}] = {{",
            bytes.len()
        );
        for byte in bytes {
            let _ = write!(self.output, "{byte},");
        }
        let _ = writeln!(self.output, "}};");
    }
}
