//! C prelude detection, standard I/O, and string converter helpers.

use std::fmt::Write;

use arandu_middle::amir::{AmirOperand, AmirStmt};
use arandu_middle::literal_pool::AmirLiteralEntry;
use arandu_middle::types::{ArType, Primitive};

use super::super::CEmitter;

impl<'a> CEmitter<'a> {
    pub(in crate::emitter) fn program_uses_print(&self) -> bool {
        self.program.funcs.iter().any(|func| {
            func.stmts.payloads.iter().any(|stmt| {
                matches!(stmt, AmirStmt::Call { callee: AmirOperand::FunctionRef(id), .. }
                    if self.symbols.try_get(*id).is_some_and(|symbol| symbol.name == "io.print"))
            })
        })
    }

    pub(in crate::emitter) fn emit_prelude_print(&mut self) {
        let _ = writeln!(&mut self.output, "static void io__print(ArStr s) {{");
        let _ = writeln!(
            &mut self.output,
            "    if (s.len > 0 && s.ptr) {{ fwrite(s.ptr, 1, (size_t)s.len, stdout); }}"
        );
        let _ = writeln!(&mut self.output, "    fflush(stdout);\n}}\n");
    }

    /// True if any call targets prelude `io.println` (symbol name or C sanitization).
    pub(in crate::emitter) fn program_uses_println(&self) -> bool {
        for func in &self.program.funcs {
            if !self.live_functions.contains(&func.symbol) {
                continue;
            }
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

    /// True if the program calls the prelude `io.eprint` function or its host alias.
    pub(in crate::emitter) fn program_uses_eprint(&self) -> bool {
        self.program.funcs.iter().any(|func| {
            func.stmts.payloads.iter().any(|stmt| {
                if let AmirStmt::Call {
                    callee: AmirOperand::FunctionRef(symbol),
                    ..
                } = stmt
                {
                    self.symbols.try_get(*symbol).is_some_and(|definition| {
                        definition.name == "eprint" || definition.name.ends_with(".eprint")
                    })
                } else {
                    false
                }
            })
        })
    }

    /// Emit the C name and the unqualified runtime alias used by the JIT ABI.
    pub(in crate::emitter) fn emit_prelude_eprint(&mut self) {
        let _ = writeln!(&mut self.output, "static void io__eprint(ArStr s) {{");
        let _ = writeln!(
            &mut self.output,
            "    if (s.len > 0 && s.ptr) {{ fwrite(s.ptr, 1, (size_t)s.len, stderr); }}"
        );
        let _ = writeln!(&mut self.output, "    fflush(stderr);");
        let _ = writeln!(&mut self.output, "}}\n");
        let _ = writeln!(
            &mut self.output,
            "static void eprint(ArStr s) {{ io__eprint(s); }}\n"
        );
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
            if !self.live_functions.contains(&func.symbol) {
                continue;
            }
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
}
