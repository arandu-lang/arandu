//! Model-based state machine for the JIT host ABI in `arandu_runtime`.
//!
//! Covers the owned-string append (`ar_string_push_str`, including self-views
//! across reallocation), the legacy vector handle table, raw/aligned buffers,
//! ToStr integer/float/char formatters and the fat-string path/string hosts.
//! Every operation is checked against a pure Rust oracle; any divergence or
//! memory error (under ASan) is a runtime bug.

use arandu_runtime::rt_runtime::{
    ar_path_file_name, ar_path_join, ar_path_join_owned, ar_str_concat, ar_str_split_last, ArFatStr,
};
use arandu_runtime::to_str_runtime::{
    ar_jit_char_to_str, ar_jit_f64_to_str, ar_jit_i64_to_str, ar_jit_u64_to_str,
    ar_rt_i64_write_digits, ar_rt_u64_write_digits, format_f64_v01,
};
use arandu_runtime::vec_runtime::{
    ar_rt_alloc_aligned, ar_rt_free_aligned, ar_rt_raw_free, ar_rt_raw_malloc, ar_string_push_str,
    ar_vec_buf_free, ar_vec_destroy, ar_vec_get, ar_vec_has, ar_vec_len, ar_vec_malloc, ar_vec_new,
    ar_vec_pop, ar_vec_push, ar_vec_put, ar_vec_realloc, ArOwnedString,
};

const PATH_ALPHABET: &[u8] = b"a/.b\xc3\xa9";

pub(crate) fn run(data: &[u8]) {
    let mut string = ArOwnedString {
        data: std::ptr::null_mut(),
        len: 0,
        capacity: 0,
    };
    let mut model: Vec<u8> = Vec::new();
    let mut vectors: Vec<(i64, Vec<i64>)> = Vec::new();

    for op in data.chunks(9).take(512) {
        let code = op[0] % 10;
        let arg = |index: usize| op.get(index).copied().unwrap_or(0);
        let word = u64::from_le_bytes(std::array::from_fn(|index| arg(index + 1)));
        match code {
            0 => push_external(&mut string, &mut model, op.get(1..).unwrap_or_default()),
            1 => push_self_view(&mut string, &mut model, arg(1), arg(2)),
            2 => push_overflowing(&mut string, &model, word),
            3 => vector_op(&mut vectors, arg(1), arg(2), word),
            4 => check_integers(word),
            5 => check_float(f64::from_bits(word)),
            6 => check_char(word as u32),
            7 => check_buffers(arg(1), arg(2), arg(3)),
            8 => check_paths(op.get(1..).unwrap_or_default()),
            _ => check_string_hosts(op.get(1..).unwrap_or_default()),
        }
    }

    // SAFETY: `string` is exclusively owned by this harness and matches the
    // model after every append.
    unsafe {
        assert_eq!(bytes_of(&string), model.as_slice());
        ar_vec_buf_free(string.data, string.capacity);
    }
    for (id, _) in vectors {
        // SAFETY: each id is live and destroyed exactly once.
        unsafe { ar_vec_destroy(id) };
    }
}

unsafe fn bytes_of(string: &ArOwnedString) -> &[u8] {
    if string.len == 0 {
        return &[];
    }
    // SAFETY: the runtime keeps `data` valid for `len` initialized bytes.
    unsafe { std::slice::from_raw_parts(string.data, string.len) }
}

fn push_external(string: &mut ArOwnedString, model: &mut Vec<u8>, bytes: &[u8]) {
    // SAFETY: `bytes` is a separate, readable slice.
    let status = unsafe { ar_string_push_str(string, bytes.as_ptr(), bytes.len()) };
    assert_eq!(status, 1, "bounded append must succeed");
    model.extend_from_slice(bytes);
    // SAFETY: see `bytes_of`.
    assert_eq!(unsafe { bytes_of(string) }, model.as_slice());
    assert!(string.len <= string.capacity);
}

fn push_self_view(string: &mut ArOwnedString, model: &mut Vec<u8>, offset: u8, len: u8) {
    if model.is_empty() {
        return;
    }
    let offset = usize::from(offset) % model.len();
    let len = usize::from(len) % (model.len() - offset + 1);
    if len == 0 {
        return;
    }
    // SAFETY: the view lies inside the initialized prefix; the runtime must
    // rebase it if the append reallocates.
    let view = unsafe { string.data.add(offset) };
    let status = unsafe { ar_string_push_str(string, view, len) };
    assert_eq!(status, 1, "self-append within bounds must succeed");
    let copy = model[offset..offset + len].to_vec();
    model.extend_from_slice(&copy);
    assert_eq!(unsafe { bytes_of(string) }, model.as_slice());
}

fn push_overflowing(string: &mut ArOwnedString, model: &[u8], word: u64) {
    let before = (string.data, string.len, string.capacity);
    // Lengths that can never be satisfied: beyond `i32::MAX` total, or a
    // view that reads past the initialized prefix of the buffer itself.
    let huge = (word as usize) | (1usize << 31);
    let source = [0u8; 1];
    let status = unsafe { ar_string_push_str(string, source.as_ptr(), huge) };
    assert_eq!(status, 0, "append of {huge} bytes must fail");
    assert_eq!((string.data, string.len, string.capacity), before);
    if !string.data.is_null() && string.capacity > string.len {
        // A view into the uninitialized tail must be rejected atomically.
        let tail = unsafe { string.data.add(string.len) };
        let status = unsafe { ar_string_push_str(string, tail, 1) };
        assert_eq!(status, 0, "view past initialized length must fail");
        assert_eq!((string.data, string.len, string.capacity), before);
    }
    let null_status = unsafe { ar_string_push_str(string, std::ptr::null(), 1) };
    assert_eq!(null_status, 0);
    assert_eq!(
        unsafe { ar_string_push_str(std::ptr::null_mut(), source.as_ptr(), 1) },
        0
    );
    assert_eq!(unsafe { bytes_of(string) }, model);
}

fn vector_op(vectors: &mut Vec<(i64, Vec<i64>)>, selector: u8, kind: u8, word: u64) {
    if vectors.is_empty() || kind % 8 == 0 {
        if vectors.len() < 8 {
            // SAFETY: allocates a fresh handle.
            let id = unsafe { ar_vec_new() };
            assert!(id >= 0);
            vectors.push((id, Vec::new()));
        }
        return;
    }
    let slot = usize::from(selector) % vectors.len();
    let id = vectors[slot].0;
    let value = word as i64;
    // Indices include negatives, i64::MIN/MAX and values just past the end.
    let index = match selector % 4 {
        0 => value,
        1 => vectors[slot].1.len() as i64,
        2 => -1,
        _ => (word % 8) as i64,
    };
    let model = &mut vectors[slot].1;
    // SAFETY: `id` is a live handle owned by this harness.
    unsafe {
        match kind % 8 {
            1 => {
                ar_vec_push(id, value);
                model.push(value);
            }
            2 => {
                let in_range = usize::try_from(index).is_ok_and(|i| i < model.len());
                assert_eq!(ar_vec_has(id, index), i64::from(in_range));
                let expected = usize::try_from(index)
                    .ok()
                    .and_then(|i| model.get(i).copied())
                    .unwrap_or(0);
                assert_eq!(ar_vec_get(id, index), expected);
            }
            3 => {
                let target = usize::try_from(index).ok().and_then(|i| model.get_mut(i));
                let expected = i64::from(target.is_some());
                if let Some(cell) = target {
                    *cell = value;
                }
                assert_eq!(ar_vec_put(id, index, value), expected);
            }
            4 => assert_eq!(ar_vec_pop(id), model.pop().unwrap_or(0)),
            5 => {
                arandu_runtime::vec_runtime::ar_vec_clear(id);
                model.clear();
            }
            6 => {
                ar_vec_destroy(id);
                // Destroyed handles report the invalid sentinel and reject
                // every non-aborting operation; destroying twice is a no-op.
                assert_eq!(ar_vec_len(id), usize::MAX);
                assert_eq!(ar_vec_has(id, 0), 0);
                assert_eq!(ar_vec_get(id, 0), 0);
                assert_eq!(ar_vec_put(id, 0, 1), 0);
                assert_eq!(ar_vec_pop(id), 0);
                ar_vec_destroy(id);
                vectors.swap_remove(slot);
                return;
            }
            _ => {}
        }
        assert_eq!(ar_vec_len(id), model.len());
    }
    // Handles that were never allocated must be rejected without aborting.
    unsafe {
        assert_eq!(ar_vec_len(i64::MIN), usize::MAX);
        assert_eq!(ar_vec_get(-1, 0), 0);
    }
}

unsafe fn take_malloc_string(ptr: *mut u8, len: i64) -> Vec<u8> {
    assert!(!ptr.is_null());
    let len = usize::try_from(len).expect("non-negative length");
    // SAFETY: ToStr helpers return `len` bytes plus a NUL terminator.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len + 1) }.to_vec();
    assert_eq!(
        bytes.last(),
        Some(&0),
        "ToStr buffer must be NUL-terminated"
    );
    // ToStr buffers come from the 16-aligned raw allocation pair.
    assert!(ptr.addr().is_multiple_of(16), "ToStr buffer is misaligned");
    unsafe { ar_rt_raw_free(ptr) };
    bytes[..len].to_vec()
}

fn check_integers(word: u64) {
    let signed = word as i64;
    let mut out_len = -1;
    let text = unsafe { take_malloc_string(ar_jit_i64_to_str(signed, &mut out_len), out_len) };
    assert_eq!(text, signed.to_string().into_bytes());
    let text = unsafe { take_malloc_string(ar_jit_u64_to_str(word, &mut out_len), out_len) };
    assert_eq!(text, word.to_string().into_bytes());

    let mut buffer = [0xAAu8; 24];
    let measured = unsafe { ar_rt_i64_write_digits(signed, std::ptr::null_mut()) };
    let written = unsafe { ar_rt_i64_write_digits(signed, buffer.as_mut_ptr()) };
    assert_eq!(measured, written);
    let written = usize::try_from(written).expect("digit count");
    assert_eq!(&buffer[..written], signed.to_string().as_bytes());
    assert!(
        buffer[written..].iter().all(|&byte| byte == 0xAA),
        "digit writer overran"
    );

    let mut buffer = [0xAAu8; 24];
    let written = unsafe { ar_rt_u64_write_digits(word, buffer.as_mut_ptr()) };
    let written = usize::try_from(written).expect("digit count");
    assert_eq!(&buffer[..written], word.to_string().as_bytes());
    assert!(
        buffer[written..].iter().all(|&byte| byte == 0xAA),
        "digit writer overran"
    );
}

fn check_float(value: f64) {
    let rendered = format_f64_v01(value);
    let mut out_len = -1;
    let host = unsafe { take_malloc_string(ar_jit_f64_to_str(value, &mut out_len), out_len) };
    assert_eq!(
        host,
        rendered.as_bytes(),
        "JIT host and format_f64_v01 disagree"
    );
    if value.is_nan() {
        assert_eq!(rendered, "nan");
    } else if value.is_infinite() {
        assert_eq!(rendered, if value < 0.0 { "-inf" } else { "inf" });
    } else {
        let parsed: f64 = rendered
            .parse()
            .unwrap_or_else(|_| panic!("{rendered:?} is not a float spelling"));
        // `-0.0` is deliberately rendered as `0`; every other finite value
        // must round-trip exactly.
        assert!(
            parsed == value && (parsed.to_bits() == value.to_bits() || value == 0.0),
            "{value:?} rendered as {rendered:?} does not round-trip"
        );
        assert!(!rendered.contains(['e', 'E']), "{rendered:?} uses exponent");
    }
}

fn check_char(scalar: u32) {
    let mut out_len = -1;
    let text = unsafe { take_malloc_string(ar_jit_char_to_str(scalar, &mut out_len), out_len) };
    let expected = char::from_u32(scalar).unwrap_or('\u{FFFD}').to_string();
    assert_eq!(text, expected.into_bytes());
}

fn check_buffers(size: u8, align_shift: u8, grow: u8) {
    let size = usize::from(size);
    let align = 1usize << (align_shift % 13);
    unsafe {
        let buffer = ar_rt_alloc_aligned(size, align);
        if size == 0 {
            assert!(buffer.is_null());
        } else {
            assert!(!buffer.is_null());
            assert_eq!(buffer as usize % align, 0);
            std::ptr::write_bytes(buffer, 0x5A, size);
            ar_rt_free_aligned(buffer, size, align);
        }
        // Invalid layouts and impossible sizes must return null, not abort.
        assert!(ar_rt_alloc_aligned(8, 3).is_null());
        assert!(ar_rt_alloc_aligned(8, 0).is_null());
        assert!(ar_rt_alloc_aligned(usize::MAX, align).is_null());
        assert!(ar_vec_malloc(0).is_null());
        assert!(ar_vec_malloc(usize::MAX).is_null());
        assert!(ar_vec_malloc(isize::MAX as usize).is_null());
        assert!(ar_rt_raw_malloc(usize::MAX).is_null());
        assert!(ar_rt_raw_malloc(usize::MAX - 8).is_null());
        let raw = ar_rt_raw_malloc(size);
        assert!(!raw.is_null() && raw.addr().is_multiple_of(16));
        std::ptr::write_bytes(raw, 0xA5, size.max(1));
        ar_rt_raw_free(raw);
        ar_rt_raw_free(std::ptr::null_mut());

        // Realloc preserves the common prefix; a failing grow keeps the old
        // buffer valid so the caller can still free it.
        let first = ar_vec_malloc(size.max(1));
        assert!(!first.is_null() && first.addr().is_multiple_of(16));
        for offset in 0..size.max(1) {
            *first.add(offset) = offset as u8;
        }
        let failed = ar_vec_realloc(first, size.max(1), usize::MAX);
        assert!(failed.is_null());
        let new_size = usize::from(grow);
        let second = ar_vec_realloc(first, size.max(1), new_size);
        if new_size == 0 {
            assert!(second.is_null());
        } else {
            assert!(!second.is_null());
            for offset in 0..size.max(1).min(new_size) {
                assert_eq!(*second.add(offset), offset as u8);
            }
            ar_vec_buf_free(second, new_size);
        }
    }
}

fn path_text(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    for byte in bytes {
        out.push(PATH_ALPHABET[usize::from(*byte) % PATH_ALPHABET.len()]);
    }
    // Lossy keeps split UTF-8 sequences well-formed for std::path oracles.
    String::from_utf8_lossy(&out).into_owned()
}

unsafe fn take_fat(fat: ArFatStr) -> Vec<u8> {
    let len = usize::try_from(fat.len).expect("non-negative fat length");
    // SAFETY: host fat strings are boxed byte slices of exactly `len` bytes.
    let boxed = unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(fat.ptr, len)) };
    boxed.into_vec()
}

/// `ar_str_concat` returns a caller-freeable raw-allocation buffer.
unsafe fn take_raw_fat(fat: ArFatStr) -> Vec<u8> {
    let len = usize::try_from(fat.len).expect("non-negative fat length");
    assert!(!fat.ptr.is_null() && fat.ptr.addr().is_multiple_of(16));
    // SAFETY: the host wrote `len` bytes into a live raw allocation.
    let bytes = unsafe { std::slice::from_raw_parts(fat.ptr, len) }.to_vec();
    unsafe { ar_rt_raw_free(fat.ptr) };
    bytes
}

fn check_paths(bytes: &[u8]) {
    let split = usize::from(bytes.first().copied().unwrap_or(0)) % (bytes.len().max(1));
    let a = path_text(bytes.get(1..=split.max(1)).unwrap_or_default());
    let b = path_text(bytes.get(split.max(1) + 1..).unwrap_or_default());
    let expected = std::path::Path::new(&a).join(&b);
    let expected = expected.to_string_lossy().into_owned().into_bytes();
    unsafe {
        let joined = take_fat(ar_path_join(
            a.as_ptr(),
            a.len() as isize,
            b.as_ptr(),
            b.len() as isize,
        ));
        assert_eq!(joined, expected);

        let mut out_buf = std::ptr::null_mut();
        let mut out_len = 0usize;
        let mut out_cap = 0usize;
        let status = ar_path_join_owned(
            a.as_ptr(),
            a.len() as isize,
            b.as_ptr(),
            b.len() as isize,
            &mut out_buf,
            &mut out_len,
            &mut out_cap,
        );
        assert_eq!(status, 1);
        let owned = if out_len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(out_buf, out_len).to_vec()
        };
        assert_eq!(owned, expected, "join and joinOwned disagree");
        ar_vec_buf_free(out_buf, out_cap);

        // Negative lengths and null pointers are rejected by the owned form.
        let status = ar_path_join_owned(
            a.as_ptr(),
            -1,
            b.as_ptr(),
            0,
            &mut out_buf,
            &mut out_len,
            &mut out_cap,
        );
        assert_eq!((status, out_len, out_cap), (0, 0, 0));
        assert!(out_buf.is_null());
        let status = ar_path_join_owned(
            std::ptr::null(),
            1,
            b.as_ptr(),
            0,
            &mut out_buf,
            &mut out_len,
            &mut out_cap,
        );
        assert_eq!(status, 0);

        let name = take_fat(ar_path_file_name(a.as_ptr(), a.len() as isize));
        let expected_name = std::path::Path::new(&a)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert_eq!(name, expected_name.into_bytes());
        assert!(take_fat(ar_path_file_name(std::ptr::null(), 4)).is_empty());
        assert!(take_fat(ar_path_file_name(a.as_ptr(), -3)).is_empty());
    }
}

fn check_string_hosts(bytes: &[u8]) {
    let split = usize::from(bytes.first().copied().unwrap_or(0)) % (bytes.len().max(1));
    let s = bytes.get(1..=split.max(1)).unwrap_or_default();
    let sep = bytes.get(split.max(1) + 1..).unwrap_or_default();
    let sep = &sep[..sep.len().min(3)];
    unsafe {
        let joined = take_raw_fat(ar_str_concat(
            s.as_ptr(),
            s.len() as isize,
            sep.as_ptr(),
            sep.len() as isize,
        ));
        assert_eq!(joined, [s, sep].concat());
        let joined = take_raw_fat(ar_str_concat(std::ptr::null(), 5, sep.as_ptr(), -1));
        assert!(joined.is_empty());

        let suffix = take_fat(ar_str_split_last(
            s.as_ptr(),
            s.len() as isize,
            sep.as_ptr(),
            sep.len() as isize,
        ));
        let expected = if sep.is_empty() {
            s
        } else {
            s.windows(sep.len())
                .rposition(|window| window == sep)
                .map_or(s, |position| &s[position + sep.len()..])
        };
        // The fat `str` contract is UTF-8; only well-formed operands have a
        // byte-exact oracle (the host repairs malformed input lossily).
        if std::str::from_utf8(s).is_ok() && std::str::from_utf8(sep).is_ok() {
            assert_eq!(suffix, expected);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_abi_state_machine_smoke() {
        let mut input = Vec::new();
        for code in 0u8..10 {
            input.extend_from_slice(&[code, 7, 3, 250, 1, 2, 3, 4, 5]);
            input.extend_from_slice(&[code + 10, 0, 1, 0x80, 0, 0, 0, 0, 0x80]);
        }
        super::run(&input);
    }
}
