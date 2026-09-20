//! Visual representations of AMIR Control Flow Graphs (DOT & ASCII).

use super::program::{AmirFunc, AmirProgram};
use super::stmt::AmirTerminator;
use crate::SymbolTable;
use crate::literal_pool::AmirLiteralPool;
use crate::types::TypeInterner;

impl AmirProgram {
    /// Renders the Control Flow Graphs of all functions as Graphviz DOT.
    #[must_use]
    pub fn render_cfg_dot(&self, symbols: &SymbolTable, interner: &TypeInterner) -> String {
        let mut out = String::new();
        for (i, func) in self.funcs.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            func.render_cfg_dot_to(&mut out, symbols, &self.literal_pool, interner);
        }
        out
    }

    /// Renders the Control Flow Graphs of all functions as formatted ASCII boxes.
    #[must_use]
    pub fn render_cfg_ascii(&self, symbols: &SymbolTable, interner: &TypeInterner) -> String {
        let mut out = String::new();
        for (i, func) in self.funcs.iter().enumerate() {
            if i > 0 {
                out.push_str("\n\n");
            }
            func.render_cfg_ascii_to(&mut out, symbols, &self.literal_pool, interner);
        }
        out
    }
}

impl AmirFunc {
    /// Appends the Graphviz DOT representation of this function's CFG to `out`.
    pub fn render_cfg_dot_to(
        &self,
        out: &mut String,
        symbols: &SymbolTable,
        pool: &AmirLiteralPool,
        interner: &TypeInterner,
    ) {
        let func_name = symbols.get(self.symbol).name.as_str();
        out.push_str(&format!("digraph \"CFG_{func_name}\" {{\n"));
        out.push_str("    graph [rankdir=TB, bgcolor=\"transparent\"];\n");
        out.push_str("    node [shape=box, fontname=\"Courier\", style=\"rounded,filled\", fillcolor=\"#f8f9fa\", color=\"#333333\"];\n");
        out.push_str("    edge [fontname=\"Courier\", fontsize=10];\n\n");

        for block in &self.blocks {
            let bid = block.id;
            let mut label = String::new();

            // Block header
            label.push_str(&format!("bb{}", bid.0));
            let block_params = self.block_params(block.params);
            if !block_params.is_empty() {
                let p_strs: Vec<String> = block_params
                    .iter()
                    .map(|p| {
                        format!(
                            "_{}: {}",
                            p.id.0,
                            interner.resolve(p.ty).display(symbols, interner)
                        )
                    })
                    .collect();
                label.push_str(&format!("({})", p_strs.join(", ")));
            }

            let preds = self.predecessors(bid);
            if !preds.is_empty() {
                let pred_names: Vec<String> = preds.iter().map(|p| format!("bb{}", p.0)).collect();
                label.push_str(&format!(" [preds: {}]", pred_names.join(", ")));
            }
            label.push_str("\\l");

            // Statements
            for stmt in self.block_stmts(bid) {
                let mut stmt_str = String::new();
                stmt.pretty_print_to(&mut stmt_str, symbols, pool);
                label.push_str(&format!("  {}\\l", escape_dot(&stmt_str)));
            }

            // Terminator
            let mut term_str = String::new();
            block
                .terminator
                .pretty_print_to(&mut term_str, symbols, pool);
            label.push_str(&format!("  {}\\l", escape_dot(&term_str)));

            out.push_str(&format!("    bb{} [label=\"{}\"];\n", bid.0, label));
        }

        out.push('\n');

        // Edges
        for block in &self.blocks {
            let bid = block.id;
            match &block.terminator {
                AmirTerminator::Return | AmirTerminator::Unreachable => {}
                AmirTerminator::Goto { target, .. } => {
                    out.push_str(&format!("    bb{} -> bb{};\n", bid.0, target.0));
                }
                AmirTerminator::Branch {
                    if_true, if_false, ..
                } => {
                    out.push_str(&format!(
                        "    bb{} -> bb{} [label=\"true\", color=\"#2e7d32\"];\n",
                        bid.0, if_true.0
                    ));
                    out.push_str(&format!(
                        "    bb{} -> bb{} [label=\"false\", color=\"#c62828\"];\n",
                        bid.0, if_false.0
                    ));
                }
                AmirTerminator::SwitchInt {
                    targets, otherwise, ..
                } => {
                    for (val, target, _) in targets {
                        out.push_str(&format!(
                            "    bb{} -> bb{} [label=\"{}\"];\n",
                            bid.0, target.0, val
                        ));
                    }
                    out.push_str(&format!(
                        "    bb{} -> bb{} [label=\"otherwise\"];\n",
                        bid.0, otherwise.0.0
                    ));
                }
                AmirTerminator::Suspend { resume, .. } => {
                    out.push_str(&format!(
                        "    bb{} -> bb{} [label=\"resume\", style=dashed];\n",
                        bid.0, resume.0
                    ));
                }
            }
        }

        out.push_str("}\n");
    }

    /// Appends the formatted ASCII representation of this function's CFG to `out`.
    pub fn render_cfg_ascii_to(
        &self,
        out: &mut String,
        symbols: &SymbolTable,
        pool: &AmirLiteralPool,
        interner: &TypeInterner,
    ) {
        let func_name = symbols.get(self.symbol).name.as_str();
        out.push_str(&format!("=== CFG: {} ===\n", func_name));

        for (i, block) in self.blocks.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let bid = block.id;

            // Header line
            let mut header = format!("bb{}", bid.0);
            let block_params = self.block_params(block.params);
            if !block_params.is_empty() {
                let p_strs: Vec<String> = block_params
                    .iter()
                    .map(|p| {
                        format!(
                            "_{}: {}",
                            p.id.0,
                            interner.resolve(p.ty).display(symbols, interner)
                        )
                    })
                    .collect();
                header.push_str(&format!("({})", p_strs.join(", ")));
            }

            let preds = self.predecessors(bid);
            if !preds.is_empty() {
                let pred_names: Vec<String> = preds.iter().map(|p| format!("bb{}", p.0)).collect();
                header.push_str(&format!(" (preds: {})", pred_names.join(", ")));
            }

            // Collect lines
            let mut lines = Vec::new();
            for stmt in self.block_stmts(bid) {
                let mut stmt_str = String::new();
                stmt.pretty_print_to(&mut stmt_str, symbols, pool);
                lines.push(format!("  {stmt_str}"));
            }
            let mut term_str = String::new();
            block
                .terminator
                .pretty_print_to(&mut term_str, symbols, pool);
            lines.push(format!("  {term_str}"));

            // Calculate width for box
            let mut max_width = header.len() + 4;
            for line in &lines {
                max_width = max_width.max(line.len() + 4);
            }
            max_width = max_width.clamp(34, 90);

            let inner_width = max_width - 4;
            let border = "-".repeat(max_width - 2);

            out.push_str(&format!("+{border}+\n"));
            out.push_str(&format!("| {:<inner_width$} |\n", header));
            out.push_str(&format!("+{border}+\n"));
            for line in lines {
                out.push_str(&format!("| {:<inner_width$} |\n", line));
            }
            out.push_str(&format!("+{border}+\n"));

            // Draw outgoing edge connectors
            match &block.terminator {
                AmirTerminator::Return => {
                    out.push_str("  └── (returns)\n");
                }
                AmirTerminator::Unreachable => {
                    out.push_str("  └── (unreachable)\n");
                }
                AmirTerminator::Goto { target, .. } => {
                    out.push_str(&format!("  │\n  └──▶ bb{}\n", target.0));
                }
                AmirTerminator::Branch {
                    if_true, if_false, ..
                } => {
                    out.push_str(&format!(
                        "  │\n  ├── [true]  ──▶ bb{}\n  └── [false] ──▶ bb{}\n",
                        if_true.0, if_false.0
                    ));
                }
                AmirTerminator::SwitchInt {
                    targets, otherwise, ..
                } => {
                    out.push_str("  │\n");
                    for (val, target, _) in targets {
                        out.push_str(&format!("  ├── [{val}] ──▶ bb{}\n", target.0));
                    }
                    out.push_str(&format!("  └── [otherwise] ──▶ bb{}\n", otherwise.0.0));
                }
                AmirTerminator::Suspend { resume, .. } => {
                    out.push_str(&format!("  │\n  └── [resume] ┄┄▶ bb{}\n", resume.0));
                }
            }
        }
    }
}

fn escape_dot(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('<', "\\<")
        .replace('>', "\\>")
}

#[cfg(test)]
mod tests {
    use crate::SymbolTable;
    use crate::amir::block::{AmirBasicBlock, BlockId};
    use crate::amir::program::{AmirFunc, AmirProgram};
    use crate::amir::stmt::{AmirStmtTable, AmirTerminator};
    use crate::cfg::compute_cfg_edges;
    use crate::layout::DenseRange;
    use crate::literal_pool::AmirLiteralPool;
    use crate::types::TypeInterner;

    #[test]
    fn test_cfg_dot_and_ascii_rendering() {
        let mut symbols = SymbolTable::new(0);
        let sym = symbols
            .define(
                crate::ScopeId(0),
                "test_fn",
                crate::SymbolKind::Func,
                crate::Span::new(0, 0, 0),
            )
            .unwrap();
        let interner = TypeInterner::new();
        let void_ty = interner.intern(crate::types::ArType::Void);

        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = compute_cfg_edges(&blocks);

        let func = AmirFunc {
            symbol: sym,
            return_type: void_ty,
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: Vec::new(),
            blocks,
            block_params: Vec::new(),
            stmts: AmirStmtTable::new(),
            cfg,
        };

        let program = AmirProgram {
            funcs: vec![func],
            literal_pool: AmirLiteralPool::default(),
            extern_funcs: rustc_hash::FxHashMap::default(),
            debug_bindings: Vec::new(),
        };

        let dot = program.render_cfg_dot(&symbols, &interner);
        assert!(dot.contains("digraph \"CFG_test_fn\""));
        assert!(dot.contains("bb0 -> bb1;"));

        let ascii = program.render_cfg_ascii(&symbols, &interner);
        assert!(ascii.contains("=== CFG: test_fn ==="));
        assert!(ascii.contains("bb0"));
        assert!(ascii.contains("└──▶ bb1"));
        assert!(ascii.contains("bb1 (preds: bb0)"));
        assert!(ascii.contains("└── (returns)"));
    }
}
