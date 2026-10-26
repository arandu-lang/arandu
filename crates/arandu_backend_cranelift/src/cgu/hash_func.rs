use super::HashContext;
use arandu_middle::amir::AmirFunc;

impl HashContext<'_> {
    pub(super) fn func_signature(&mut self, func: &AmirFunc) {
        self.symbol(func.symbol);
        self.type_id(func.return_type);
        self.hash.usize(func.params.len());
        for &param in &func.params {
            self.hash.usize(param.as_usize());
            if let Some(temp) = func.temps.get(param.as_usize()) {
                self.hash.tag(0);
                self.type_id(temp.ty);
            } else {
                self.hash.tag(1);
            }
        }
        if let Some(receiver) = func.receiver {
            self.hash.tag(1);
            self.hash.usize(receiver.temp.as_usize());
            self.hash.tag(match receiver.kind {
                arandu_middle::hir::ReceiverKind::Shared => 0,
                arandu_middle::hir::ReceiverKind::Mut => 1,
                arandu_middle::hir::ReceiverKind::Own => 2,
            });
        } else {
            self.hash.tag(0);
        }
        self.hash
            .u32(self.type_info.function_effects(func.symbol).0);
    }

    pub(super) fn func(&mut self, func: &AmirFunc) {
        self.func_signature(func);

        self.hash.usize(func.locals.len());
        for local in &func.locals {
            self.hash.usize(local.id.as_usize());
            self.type_id(local.ty);
            self.hash.bool(local.is_memory);
            // Source/local debug symbols are not used by CGU code emission.
        }

        self.hash.usize(func.temps.len());
        for temp in &func.temps {
            self.hash.usize(temp.id.as_usize());
            self.type_id(temp.ty);
            self.hash.bool(temp.is_copy);
            self.hash.bool(temp.is_nullable);
        }

        self.hash.usize(func.block_params.len());
        for param in &func.block_params {
            self.hash.usize(param.id.as_usize());
            self.hash.usize(param.local.as_usize());
            self.type_id(param.ty);
            self.hash.bool(param.moved);
        }

        self.hash.usize(func.blocks.len());
        for block in &func.blocks {
            self.hash.usize(block.id.as_usize());
            let params = func.block_params(block.params);
            self.hash.usize(params.len());
            for param in params {
                self.hash.usize(param.id.as_usize());
            }

            self.hash.u32(block.statements.len);
            for statement_id in block.statements.iter_ids() {
                if let Some(statement) = func.try_stmt(statement_id) {
                    self.hash.tag(0);
                    self.stmt(statement);
                } else {
                    self.hash.tag(1);
                    self.hash.usize(statement_id.as_usize());
                }
            }
            self.terminator(&block.terminator);
        }
    }

    pub(super) fn direct_callee_signatures(&mut self, func: &AmirFunc) {
        let Ok(callees) = super::dependencies::function_dependencies(
            func,
            self.program,
            self.symbols,
            self.type_info,
        ) else {
            // Emission rejects the same invalid closure; no executable object
            // can be cached for this diagnostic-only sentinel.
            self.hash.tag(u8::MAX);
            return;
        };
        self.hash.tag(0);
        self.hash.usize(callees.len());
        for symbol in callees {
            self.symbol(symbol);
            if let Some(callee) = self.program.funcs.iter().find(|func| func.symbol == symbol) {
                self.hash.tag(0);
                self.func_signature(callee);
            } else if let Some((params, result)) = self.program.extern_funcs.get(&symbol) {
                self.hash.tag(1);
                self.hash.usize(params.len());
                for param in params {
                    self.ar_type(param);
                }
                self.ar_type(result);
            } else if let Some(ty) = self.type_info.decl_type_id(symbol) {
                self.hash.tag(2);
                self.type_id(ty);
            } else {
                self.hash.tag(3);
            }
        }
    }
}
