//! Structured, indentation-aware code writer for synthesized Arandu programs.

/// Structured code writer providing indentation tracking and safe literal emission.
#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct CodeWriter {
    buffer: String,
    indent_level: usize,
    at_line_start: bool,
}

#[allow(dead_code)]
impl CodeWriter {
    /// Create a new code writer with an empty buffer.
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            indent_level: 0,
            at_line_start: true,
        }
    }

    /// Create a new code writer with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: String::with_capacity(capacity),
            indent_level: 0,
            at_line_start: true,
        }
    }

    /// Increase indentation level by 1.
    pub fn indent(&mut self) {
        self.indent_level = self.indent_level.saturating_add(1);
    }

    /// Decrease indentation level by 1.
    pub fn dedent(&mut self) {
        self.indent_level = self.indent_level.saturating_sub(1);
    }

    /// Write current indentation if at start of line.
    fn ensure_indent(&mut self) {
        if self.at_line_start {
            for _ in 0..self.indent_level {
                self.buffer.push_str("    ");
            }
            self.at_line_start = false;
        }
    }

    /// Write raw text without appending a newline.
    pub fn write(&mut self, text: &str) {
        self.ensure_indent();
        self.buffer.push_str(text);
    }

    /// Write raw text followed by a newline.
    pub fn writeln(&mut self, text: &str) {
        self.ensure_indent();
        self.buffer.push_str(text);
        self.buffer.push('\n');
        self.at_line_start = true;
    }

    /// Write an empty line.
    pub fn newline(&mut self) {
        self.buffer.push('\n');
        self.at_line_start = true;
    }

    /// Write a character literal using the canonical scalar encoding from arandu_lexer.
    pub fn write_char_literal(&mut self, c: char) {
        let literal = arandu_lexer::char_literal(c);
        self.write(&literal);
    }

    /// Write a string literal with standard character escapes.
    pub fn write_string_literal(&mut self, s: &str) {
        self.ensure_indent();
        self.buffer.push('"');
        for c in s.chars() {
            match c {
                '"' => self.buffer.push_str("\\\""),
                '\\' => self.buffer.push_str("\\\\"),
                '\n' => self.buffer.push_str("\\n"),
                '\r' => self.buffer.push_str("\\r"),
                '\t' => self.buffer.push_str("\\t"),
                '\0' => self.buffer.push_str("\\0"),
                _ => self.buffer.push(c),
            }
        }
        self.buffer.push('"');
    }

    /// Consume the writer and return the completed source text.
    pub fn finish(self) -> String {
        self.buffer
    }

    /// View the written text as a string slice.
    pub fn as_str(&self) -> &str {
        &self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_handles_indentation_and_lines() {
        let mut w = CodeWriter::new();
        w.writeln("func main() {");
        w.indent();
        w.writeln("let x = 10;");
        w.dedent();
        w.writeln("}");
        assert_eq!(w.finish(), "func main() {\n    let x = 10;\n}\n");
    }

    #[test]
    fn writer_escapes_strings_and_chars_safely() {
        let mut w = CodeWriter::new();
        w.write_string_literal("hello \"world\"\n");
        w.write(" ");
        w.write_char_literal('🦀');
        assert_eq!(w.finish(), "\"hello \\\"world\\\"\\n\" '🦀'");
    }
}
