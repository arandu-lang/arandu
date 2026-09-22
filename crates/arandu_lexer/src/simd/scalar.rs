use crate::ident::is_ident_continue;

#[must_use]
pub fn skip_whitespace(bytes: &[u8]) -> (usize, usize, Option<usize>) {
    let mut i = 0;
    let mut newlines = 0;
    let mut last_nl = None;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            newlines += 1;
            last_nl = Some(i);
            i += 1;
        } else if b == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            newlines += 1;
            last_nl = Some(i + 1);
            i += 2;
        } else if b == b' ' || b == b'\t' || b == b'\r' {
            i += 1;
        } else {
            break;
        }
    }
    (newlines, i, last_nl)
}

#[must_use]
pub fn scan_identifier(bytes: &[u8]) -> usize {
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 128 {
            if is_ident_continue(b as char) {
                i += 1;
            } else {
                break;
            }
        } else {
            // Non-ASCII: stop and let the Unicode/scalar fallback loop handle it
            break;
        }
    }
    i
}
