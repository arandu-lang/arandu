//! C prelude detection, standard I/O, and string converter helpers.

use std::fmt::Write;

use arandu_middle::amir::{AmirOperand, AmirStmt};
use arandu_middle::literal_pool::AmirLiteralEntry;
use arandu_middle::types::{ArType, Primitive};

use super::super::CEmitter;

impl<'a> CEmitter<'a> {
    /// True if any call targets prelude `io.println` (symbol name or C sanitization).
    pub(in crate::emitter) fn program_uses_println(&self) -> bool {
        for func in &self.program.funcs {
            for stmt in func.stmts.payloads.iter() {
                if let AmirStmt::Call { callee, .. } = stmt
                    && let AmirOperand::FunctionRef(id) = callee
                {
                    let name = self.symbols.get(*id).name.as_str();
                    if name == "io.println" || name.ends_with(".println") || name == "println" {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// True if the program calls the prelude `io.eprint` function.
    pub(in crate::emitter) fn program_uses_eprint(&self) -> bool {
        self.program.funcs.iter().any(|func| {
            func.stmts.payloads.iter().any(|stmt| {
                if let AmirStmt::Call {
                    callee: AmirOperand::FunctionRef(symbol),
                    ..
                } = stmt
                {
                    self.symbols
                        .try_get(*symbol)
                        .is_some_and(|definition| definition.name == "io.eprint")
                } else {
                    false
                }
            })
        })
    }

    /// Emit `io.eprint` matching `sanitize_c_ident("io.eprint")`.
    pub(in crate::emitter) fn emit_prelude_eprint(&mut self) {
        let _ = writeln!(&mut self.output, "static void io__eprint(ArStr s) {{");
        let _ = writeln!(
            &mut self.output,
            "    if (s.len > 0 && s.ptr) {{ fwrite(s.ptr, 1, (size_t)s.len, stderr); }}"
        );
        let _ = writeln!(&mut self.output, "    fflush(stderr);");
        let _ = writeln!(&mut self.output, "}}\n");
    }

    /// Emit `io__println` matching sanitize_c_ident("io.println").
    pub(in crate::emitter) fn emit_prelude_println(&mut self) {
        let _ = writeln!(&mut self.output, "static void io__println(ArStr s) {{");
        let _ = writeln!(
            &mut self.output,
            "    if (s.len > 0 && s.ptr) {{ fwrite(s.ptr, 1, (size_t)s.len, stdout); }}"
        );
        let _ = writeln!(&mut self.output, "    fputc('\\n', stdout);");
        let _ = writeln!(&mut self.output, "    fflush(stdout);");
        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(&mut self.output);
    }

    /// Whether any local/temp/return or pool entry needs the ArStr runtime.
    pub(in crate::emitter) fn program_uses_str(&self) -> bool {
        if self
            .program
            .literal_pool
            .entries
            .iter()
            .any(|e| matches!(e, AmirLiteralEntry::Str(_)))
        {
            return true;
        }
        for func in &self.program.funcs {
            let ret = self.interner.resolve(func.return_type);
            if matches!(ret, ArType::Primitive(Primitive::Str)) {
                return true;
            }
            for local in &func.locals {
                if matches!(
                    self.interner.resolve(local.ty),
                    ArType::Primitive(Primitive::Str)
                ) {
                    return true;
                }
            }
            for temp in &func.temps {
                if matches!(
                    self.interner.resolve(temp.ty),
                    ArType::Primitive(Primitive::Str)
                ) {
                    return true;
                }
            }
        }
        false
    }

    /// String conversion helpers (ar_str_concat_n, ToStr v0.1 formatters).
    pub(super) fn emit_str_converters(&mut self, len_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            "static ArStr ar_str_concat_n(int n, ...) {{"
        );
        let _ = writeln!(
            &mut self.output,
            "    if (n <= 0) return ar_str_pack((const uint8_t*)\"\", 0);"
        );
        let _ = writeln!(&mut self.output, "    va_list ap;");
        let _ = writeln!(&mut self.output, "    va_start(ap, n);");
        let _ = writeln!(
            &mut self.output,
            "    ArStr *parts = (ArStr*)malloc((size_t)n * sizeof(ArStr));"
        );
        let _ = writeln!(
            &mut self.output,
            "    if (!parts) {{ va_end(ap); abort(); }}"
        );
        let _ = writeln!(&mut self.output, "    {len_c_ty} total = 0;");
        let _ = writeln!(&mut self.output, "    for (int i = 0; i < n; i++) {{");
        let _ = writeln!(&mut self.output, "        parts[i] = va_arg(ap, ArStr);");
        let _ = writeln!(&mut self.output, "        const uint8_t *p; {len_c_ty} l;");
        let _ = writeln!(&mut self.output, "        ar_str_unpack(parts[i], &p, &l);");
        let _ = writeln!(&mut self.output, "        if (l > 0) total += l;");
        let _ = writeln!(&mut self.output, "    }}");
        let _ = writeln!(&mut self.output, "    va_end(ap);");
        let _ = writeln!(
            &mut self.output,
            "    uint8_t *buf = (uint8_t*)malloc((size_t)total + 1);"
        );
        let _ = writeln!(
            &mut self.output,
            "    if (!buf) {{ free(parts); abort(); }}"
        );
        let _ = writeln!(&mut self.output, "    {len_c_ty} off = 0;");
        let _ = writeln!(&mut self.output, "    for (int i = 0; i < n; i++) {{");
        let _ = writeln!(&mut self.output, "        const uint8_t *p; {len_c_ty} l;");
        let _ = writeln!(&mut self.output, "        ar_str_unpack(parts[i], &p, &l);");
        let _ = writeln!(
            &mut self.output,
            "        if (l > 0 && p) {{ memcpy(buf + off, p, (size_t)l); off += l; }}"
        );
        let _ = writeln!(&mut self.output, "    }}");
        let _ = writeln!(&mut self.output, "    buf[total] = 0;");
        let _ = writeln!(&mut self.output, "    free(parts);");
        let _ = writeln!(&mut self.output, "    return ar_str_pack(buf, total);");
        let _ = writeln!(&mut self.output, "}}");
        // ToStr v0.1 helpers (malloc + snprintf; process-lifetime leak OK for debug).
        let _ = writeln!(&mut self.output, "static ArStr ar_i64_to_str(int64_t v) {{");
        let _ = writeln!(&mut self.output, "    char tmp[32];");
        let _ = writeln!(
            &mut self.output,
            "    int n = snprintf(tmp, sizeof(tmp), \"%lld\", (long long)v);"
        );
        let _ = writeln!(&mut self.output, "    if (n < 0) abort();");
        let _ = writeln!(
            &mut self.output,
            "    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);"
        );
        let _ = writeln!(&mut self.output, "    if (!buf) abort();");
        let _ = writeln!(&mut self.output, "    memcpy(buf, tmp, (size_t)n);");
        let _ = writeln!(&mut self.output, "    buf[n] = 0;");
        let _ = writeln!(
            &mut self.output,
            "    return ar_str_pack(buf, ({len_c_ty})n);"
        );
        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(
            &mut self.output,
            "static ArStr ar_u64_to_str(uint64_t v) {{"
        );
        let _ = writeln!(&mut self.output, "    char tmp[32];");
        let _ = writeln!(
            &mut self.output,
            "    int n = snprintf(tmp, sizeof(tmp), \"%llu\", (unsigned long long)v);"
        );
        let _ = writeln!(&mut self.output, "    if (n < 0) abort();");
        let _ = writeln!(
            &mut self.output,
            "    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);"
        );
        let _ = writeln!(&mut self.output, "    if (!buf) abort();");
        let _ = writeln!(&mut self.output, "    memcpy(buf, tmp, (size_t)n);");
        let _ = writeln!(&mut self.output, "    buf[n] = 0;");
        let _ = writeln!(
            &mut self.output,
            "    return ar_str_pack(buf, ({len_c_ty})n);"
        );
        let _ = writeln!(&mut self.output, "}}");
        // Keep in sync with arandu_runtime::to_str_runtime::format_f64_v01
        // (specials + integer-looking values + %.15g for the rest).
        let _ = writeln!(&mut self.output, "static ArStr ar_f64_to_str(double v) {{");
        let _ = writeln!(&mut self.output, "    char tmp[64];");
        let _ = writeln!(&mut self.output, "    int n;");
        let _ = writeln!(
            &mut self.output,
            "    if (isnan(v)) {{ n = snprintf(tmp, sizeof(tmp), \"nan\"); }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    else if (isinf(v)) {{ n = snprintf(tmp, sizeof(tmp), \"%s\", (v < 0) ? \"-inf\" : \"inf\"); }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    else if (v == (double)(long long)v && v < 1e15 && v > -1e15) {{ n = snprintf(tmp, sizeof(tmp), \"%lld\", (long long)v); }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    else {{ n = snprintf(tmp, sizeof(tmp), \"%.15g\", v); }}"
        );
        let _ = writeln!(&mut self.output, "    if (n < 0) abort();");
        let _ = writeln!(
            &mut self.output,
            "    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);"
        );
        let _ = writeln!(&mut self.output, "    if (!buf) abort();");
        let _ = writeln!(&mut self.output, "    memcpy(buf, tmp, (size_t)n);");
        let _ = writeln!(&mut self.output, "    buf[n] = 0;");
        let _ = writeln!(
            &mut self.output,
            "    return ar_str_pack(buf, ({len_c_ty})n);"
        );
        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(&mut self.output, "static ArStr ar_bool_to_str(bool v) {{");
        let _ = writeln!(
            &mut self.output,
            "    const char *s = v ? \"true\" : \"false\";"
        );
        let _ = writeln!(&mut self.output, "    {len_c_ty} n = v ? 4 : 5;");
        let _ = writeln!(
            &mut self.output,
            "    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);"
        );
        let _ = writeln!(&mut self.output, "    if (!buf) abort();");
        let _ = writeln!(&mut self.output, "    memcpy(buf, s, (size_t)n);");
        let _ = writeln!(&mut self.output, "    buf[n] = 0;");
        let _ = writeln!(&mut self.output, "    return ar_str_pack(buf, n);");
        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(
            &mut self.output,
            "static ArStr ar_char_to_str(uint32_t cp) {{"
        );
        let _ = writeln!(&mut self.output, "    uint8_t tmp[4];");
        let _ = writeln!(&mut self.output, "    int n = 0;");
        let _ = writeln!(
            &mut self.output,
            "    if (cp <= 0x7F) {{ tmp[0] = (uint8_t)cp; n = 1; }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    else if (cp <= 0x7FF) {{ tmp[0] = (uint8_t)(0xC0 | (cp >> 6)); tmp[1] = (uint8_t)(0x80 | (cp & 0x3F)); n = 2; }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    else if (cp <= 0xFFFF) {{ tmp[0] = (uint8_t)(0xE0 | (cp >> 12)); tmp[1] = (uint8_t)(0x80 | ((cp >> 6) & 0x3F)); tmp[2] = (uint8_t)(0x80 | (cp & 0x3F)); n = 3; }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    else {{ tmp[0] = (uint8_t)(0xF0 | (cp >> 18)); tmp[1] = (uint8_t)(0x80 | ((cp >> 12) & 0x3F)); tmp[2] = (uint8_t)(0x80 | ((cp >> 6) & 0x3F)); tmp[3] = (uint8_t)(0x80 | (cp & 0x3F)); n = 4; }}"
        );
        let _ = writeln!(
            &mut self.output,
            "    uint8_t *buf = (uint8_t*)malloc((size_t)n + 1);"
        );
        let _ = writeln!(&mut self.output, "    if (!buf) abort();");
        let _ = writeln!(&mut self.output, "    memcpy(buf, tmp, (size_t)n);");
        let _ = writeln!(&mut self.output, "    buf[n] = 0;");
        let _ = writeln!(
            &mut self.output,
            "    return ar_str_pack(buf, ({len_c_ty})n);"
        );
        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(&mut self.output);
    }
}
