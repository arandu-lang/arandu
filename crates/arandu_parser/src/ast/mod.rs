pub mod ast_pool;
mod decl;
mod dump;
mod expr;
mod pattern;
mod shift;
mod stmt;
mod types;

pub use ast_pool::*;
pub use decl::*;
pub use expr::*;
pub use pattern::*;
pub use shift::ItemBases;
pub use stmt::*;
pub use types::*;

impl Program {
    #[must_use]
    pub fn dump(&self, source: &str) -> String {
        let line_index = arandu_base::line_index::LineIndex::new(source);
        dump::dump_program(self, &line_index)
    }
}
