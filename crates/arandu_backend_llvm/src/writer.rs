use std::fmt::{self, Write};

#[derive(Default)]
pub(super) struct IrWriter {
    output: String,
    indent: usize,
}

impl IrWriter {
    pub(super) fn line(&mut self, line: impl AsRef<str>) {
        for _ in 0..self.indent {
            self.output.push_str("  ");
        }
        self.output.push_str(line.as_ref());
        self.output.push('\n');
    }

    pub(super) fn blank(&mut self) {
        self.output.push('\n');
    }

    pub(super) fn finish(self) -> String {
        self.output
    }

    /// Append pre-rendered IR lines without additional indentation.
    pub(super) fn append_raw(&mut self, fragment: &str) {
        self.output.push_str(fragment);
    }
}

impl Write for IrWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.output.write_str(s)
    }
}
