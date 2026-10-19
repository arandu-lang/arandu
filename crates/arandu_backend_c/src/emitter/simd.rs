//! Native C vectors with an explicitly selectable aligned scalar fallback.
use super::CEmitter;
use arandu_middle::amir::{AmirFunc, AmirRvalue, VectorCompareOp, VectorOp};
use arandu_middle::types::{ArType, VectorType};
use std::fmt::Write;

impl CEmitter<'_> {
    pub(super) fn emit_vector_type(&mut self, vector: VectorType) {
        if vector != VectorType::U8x16 {
            self.ensure_type_emitted(&ArType::Vector(VectorType::U8x16));
        }
        let name = format!("ArVector_{}", vector.name());
        let lane = self.format_type(&ArType::Primitive(vector.lane()));
        let lanes = vector.lanes();
        // Unsigned native arithmetic gives wrapping semantics for integer lanes.
        let unsigned = match vector {
            VectorType::I16x8 => "uint16_t",
            VectorType::I32x4 => "uint32_t",
            VectorType::I64x2 => "uint64_t",
            VectorType::U8x16 => "uint8_t",
            VectorType::F32x4 => "float",
            VectorType::F64x2 => "double",
        };
        let _ = writeln!(
            self.output,
            "#if (defined(__GNUC__) || defined(__clang__)) && !defined(ARANDU_SCALAR_SIMD)\ntypedef {unsigned} {name}_native __attribute__((vector_size(16)));\ntypedef union {{ {name}_native native; _Alignas(16) {lane} lane[{lanes}]; }} {name};\n#else\ntypedef struct {{ _Alignas(16) {lane} lane[{lanes}]; }} {name};\n#endif\n_Static_assert(sizeof({name}) == 16 && _Alignof({name}) == 16, \"invalid SIMD layout\");"
        );
        let _ = writeln!(
            self.output,
            "static inline {name} {name}_splat({lane} value) {{ {name} r; for (unsigned i=0; i<{lanes}; ++i) r.lane[i]=value; return r; }}"
        );
        for (op, spelling, symbol) in [
            (VectorOp::Add, "add", "+"),
            (VectorOp::Sub, "sub", "-"),
            (VectorOp::Mul, "mul", "*"),
            (VectorOp::Div, "div", "/"),
            (VectorOp::BitAnd, "and", "&"),
            (VectorOp::BitOr, "or", "|"),
            (VectorOp::BitXor, "xor", "^"),
        ] {
            if !op.supports(vector) {
                continue;
            }
            let _ = writeln!(
                self.output,
                "static inline {name} {name}_{spelling}({name} a, {name} b) {{ {name} r;\n#if (defined(__GNUC__) || defined(__clang__)) && !defined(ARANDU_SCALAR_SIMD)\nr.native = a.native {symbol} b.native;\n#else\nfor (unsigned i=0; i<{lanes}; ++i) {{"
            );
            if vector.lane().is_float() {
                let _ = writeln!(self.output, "r.lane[i] = a.lane[i] {symbol} b.lane[i];");
            } else {
                // Promotion to uint64_t avoids signed integer-promotion overflow
                // for u16 products; copying bits back avoids signed casts/alias UB.
                let _ = writeln!(
                    self.output,
                    "{unsigned} x,y,z; memcpy(&x,&a.lane[i],sizeof x); memcpy(&y,&b.lane[i],sizeof y); z=({unsigned})((uint64_t)x {symbol} (uint64_t)y); memcpy(&r.lane[i],&z,sizeof z);"
                );
            }
            let _ = writeln!(self.output, "}}\n#endif\nreturn r; }}");
        }
        let _ = writeln!(
            self.output,
            "static inline {name} {name}_select(ArVector_U8x16 mask, {name} yes, {name} no) {{ {name} r; ArVector_U8x16 a,b,c; memcpy(&a,&yes,16); memcpy(&b,&no,16);\n#if (defined(__GNUC__) || defined(__clang__)) && !defined(ARANDU_SCALAR_SIMD)\nc.native = (mask.native & a.native) | (~mask.native & b.native);\n#else\nfor(unsigned i=0;i<16;++i) c.lane[i]=(uint8_t)((mask.lane[i]&a.lane[i])|((uint8_t)~mask.lane[i]&b.lane[i]));\n#endif\nmemcpy(&r,&c,16); return r; }}"
        );
        for (suffix, comparison) in [
            ("eq", "=="),
            ("ne", "!="),
            ("lt", "<"),
            ("le", "<="),
            ("gt", ">"),
            ("ge", ">="),
        ] {
            let _ = writeln!(
                self.output,
                "static inline ArVector_U8x16 {name}_{suffix}({name} a, {name} b) {{ ArVector_U8x16 r;\n#if (defined(__GNUC__) || defined(__clang__)) && !defined(ARANDU_SCALAR_SIMD)\ntypedef {lane} lane_vector __attribute__((vector_size(16))); lane_vector x,y; memcpy(&x,&a,16); memcpy(&y,&b,16); __typeof__(x {comparison} y) bits = x {comparison} y; memcpy(&r,&bits,16);\n#else\nfor(unsigned i=0;i<{lanes};++i) memset(&r.lane[i*(16/{lanes})],a.lane[i] {comparison} b.lane[i]?255:0,16/{lanes});\n#endif\nreturn r; }}"
            );
        }
        // Shuffle indices are canonical byte indices (Wasm little-endian).
        // Endian conversion keeps byte semantics identical on a big-endian C target.
        let width = 16 / lanes;
        let _ = writeln!(
            self.output,
            "static inline {name} {name}_shuffle({name} a, {name} b, const uint8_t mask[16]) {{ {name} r; uint8_t x[16],y[16],z[16]; memcpy(x,&a,16); memcpy(y,&b,16);\nfor (unsigned i=0; i<16; ++i) {{ unsigned j=mask[i]; const uint16_t endian=1; unsigned dst=i,src=j%16; if (((const uint8_t *)&endian)[0]==0) {{ dst=(i/{width})*{width}+({width}-1-i%{width}); src=((j%16)/{width})*{width}+({width}-1-j%{width}); }}\nz[dst]=j<16?x[src]:y[src]; }} memcpy(&r,z,16); return r; }}"
        );
        self.emitted_types.insert(name);
    }

    pub(super) fn emit_vector_rvalue(
        &mut self,
        rhs: &AmirRvalue,
        func: &AmirFunc,
        result: &ArType,
    ) {
        let name = self.format_type(result).into_owned();
        match rhs {
            AmirRvalue::VectorSelect {
                mask,
                if_true,
                if_false,
            } => {
                let mask = self.format_operand(mask, func);
                let yes = self.format_operand(if_true, func);
                let no = self.format_operand(if_false, func);
                let _ = write!(self.output, "{name}_select({mask}, {yes}, {no})");
            }
            AmirRvalue::VectorCompare { op, left, right } => {
                let ty = self.operand_ty(func, left);
                let vector_name = self.format_type(&ty).into_owned();
                let left = self.format_operand(left, func);
                let right = self.format_operand(right, func);
                let suffix = match op {
                    VectorCompareOp::Eq => "eq",
                    VectorCompareOp::Ne => "ne",
                    VectorCompareOp::Lt => "lt",
                    VectorCompareOp::Le => "le",
                    VectorCompareOp::Gt => "gt",
                    VectorCompareOp::Ge => "ge",
                };
                let _ = write!(self.output, "{vector_name}_{suffix}({left}, {right})");
            }
            AmirRvalue::VectorSplat { value } => {
                let value = self.format_operand(value, func);
                let _ = write!(self.output, "{name}_splat({value})");
            }
            AmirRvalue::VectorOp { op, left, right } => {
                let left = self.format_operand(left, func);
                let right = self.format_operand(right, func);
                let suffix = match op {
                    VectorOp::Add => "add",
                    VectorOp::Sub => "sub",
                    VectorOp::Mul => "mul",
                    VectorOp::Div => "div",
                    VectorOp::BitAnd => "and",
                    VectorOp::BitOr => "or",
                    VectorOp::BitXor => "xor",
                };
                let _ = write!(self.output, "{name}_{suffix}({left}, {right})");
            }
            AmirRvalue::VectorExtractLane { vector, lane } => {
                let vector = self.format_operand(vector, func);
                let _ = write!(self.output, "({vector}).lane[{lane}]");
            }
            AmirRvalue::VectorInsertLane {
                vector,
                lane,
                value,
            } => {
                let vector = self.format_operand(vector, func);
                let value = self.format_operand(value, func);
                let _ = write!(
                    self.output,
                    "({{ {name} r = {vector}; r.lane[{lane}] = {value}; r; }})"
                );
            }
            AmirRvalue::VectorShuffle { left, right, mask } => {
                let left = self.format_operand(left, func);
                let right = self.format_operand(right, func);
                let mask = mask.iter().map(u8::to_string).collect::<Vec<_>>().join(",");
                let _ = write!(
                    self.output,
                    "{name}_shuffle({left}, {right}, (const uint8_t[16]){{{mask}}})"
                );
            }
            _ => self.record_codegen_ice(func, "expected a vector operation"),
        }
    }
}
