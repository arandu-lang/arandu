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

    /// Writable `static mut` storage: a union of its initial bytes and its
    /// typed value, so the bytes initialize the object and code reads and
    /// writes the typed member (union punning is defined in C11).
    pub(super) fn emit_mutable_statics(&mut self) {
        let statics = self.program.statics.clone();
        for item in &statics {
            let ty = self.interner.resolve(item.ty);
            self.ensure_type_emitted(&ty);
            let c_type = self.format_type(&ty).into_owned();
            let encode = |little| {
                arandu_middle::ctfe::const_value_bytes(
                    &item.init,
                    item.ty,
                    self.interner,
                    self.provider,
                    self.layout.data_layout,
                    little,
                )
            };
            let (Some(little), Some(big)) = (encode(true), encode(false)) else {
                continue;
            };
            if little.bytes != big.bytes {
                let _ = writeln!(
                    self.output,
                    "#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__"
                );
                self.emit_mutable_static(&item.name, &c_type, &big.bytes);
                let _ = writeln!(self.output, "#else");
            }
            self.emit_mutable_static(&item.name, &c_type, &little.bytes);
            if little.bytes != big.bytes {
                let _ = writeln!(self.output, "#endif");
            }
        }
    }

    fn emit_mutable_static(&mut self, name: &str, c_type: &str, bytes: &[u8]) {
        let _ = write!(
            self.output,
            "static union {{ uint8_t bytes[{}]; {c_type} value; }} {name} = {{ {{",
            bytes.len().max(1)
        );
        for byte in bytes {
            let _ = write!(self.output, "{byte},");
        }
        if bytes.is_empty() {
            let _ = write!(self.output, "0");
        }
        let _ = writeln!(self.output, "}} }};");
    }
}
