//! Additional fns.c builtins for the Elisp interpreter.
//!
//! Implements: base64 encode/decode, md5, secure-hash, buffer-hash,
//! locale-info, eql, equal-including-properties, widget-get/put/apply,
//! identity, string-to-multibyte/unibyte, string-make-multibyte/unibyte,
//! compare-strings, string-version-lessp, string-collate-lessp/equalp.

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
// bytes_to_unibyte_storage_string and encode_nonunicode_char_for_storage
// imports removed — using emacs_char + LispString directly
use super::value::*;
use crate::buffer::BufferManager;
use sha1::Sha1;
use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};

// Sentinel constants removed — no longer needed with Vec<u8> LispString

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_range_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn require_string(_name: &str, val: &Value) -> Result<String, Flow> {
    match val.kind() {
        ValueKind::String => Ok(super::builtins::lisp_string_to_runtime_string(*val)),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *val],
        )),
    }
}

fn require_int(val: &Value) -> Result<i64, Flow> {
    match val.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *val],
        )),
    }
}

fn require_int_or_marker(val: &Value) -> Result<i64, Flow> {
    if val.is_marker() {
        return super::marker::marker_position_as_int(val);
    }
    match val.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *val],
        )),
    }
}

fn md5_known_coding_system(name: &str) -> bool {
    super::coding::CodingSystemManager::new().is_known(name)
}

fn validate_md5_coding_system_arg(args: &[Value]) -> Result<(), Flow> {
    let Some(coding_system) = args.get(3) else {
        return Ok(());
    };
    if coding_system.is_nil() {
        return Ok(());
    }

    let noerror = args.get(4).is_some_and(|v| v.is_truthy());
    let valid = match coding_system.kind() {
        ValueKind::Symbol(id) => md5_known_coding_system(resolve_sym(id)),
        _ => false,
    };

    if valid || noerror {
        Ok(())
    } else {
        Err(signal("coding-system-error", vec![*coding_system]))
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// Base64 alphabet tables
// ---------------------------------------------------------------------------

const B64_STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Build a decode table (256 entries, 0xFF = invalid) from an alphabet.
fn build_decode_table(alphabet: &[u8; 64]) -> [u8; 256] {
    let mut table = [0xFFu8; 256];
    for (i, &ch) in alphabet.iter().enumerate() {
        table[ch as usize] = i as u8;
    }
    table
}

// ---------------------------------------------------------------------------
// Base64 encode (manual implementation)
// ---------------------------------------------------------------------------

fn base64_encode(input: &[u8], alphabet: &[u8; 64], pad: bool, line_break: bool) -> String {
    let mut out = Vec::with_capacity(input.len().div_ceil(3) * 4 + input.len() / 57);
    let mut col = 0usize;

    let chunks = input.chunks(3);
    for chunk in chunks {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(alphabet[((triple >> 18) & 0x3F) as usize]);
        out.push(alphabet[((triple >> 12) & 0x3F) as usize]);

        if chunk.len() > 1 {
            out.push(alphabet[((triple >> 6) & 0x3F) as usize]);
        } else if pad {
            out.push(b'=');
        }

        if chunk.len() > 2 {
            out.push(alphabet[(triple & 0x3F) as usize]);
        } else if pad {
            out.push(b'=');
        }

        col += 4;
        if line_break && col >= 76 {
            out.push(b'\n');
            col = 0;
        }
    }

    // Safety: we only pushed ASCII bytes
    unsafe { String::from_utf8_unchecked(out) }
}

// ---------------------------------------------------------------------------
// Base64 decode (manual implementation)
// ---------------------------------------------------------------------------

fn base64_decode(input: &str, table: &[u8; 256]) -> Result<Vec<u8>, ()> {
    // Strip whitespace (CR, LF, space, tab) per Emacs behaviour
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'\n' && b != b'\r' && b != b' ' && b != b'\t')
        .collect();

    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;

    for &b in &bytes {
        if b == b'=' {
            // Padding — stop collecting
            break;
        }
        let val = table[b as usize];
        if val == 0xFF {
            return Err(());
        }
        buf = (buf << 6) | val as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Base64 builtins
// ---------------------------------------------------------------------------

/// (base64-encode-string STRING &optional NO-LINE-BREAK)
pub(crate) fn builtin_base64_encode_string(args: Vec<Value>) -> EvalResult {
    expect_range_args("base64-encode-string", &args, 1, 2)?;
    let s = require_string("base64-encode-string", &args[0])?;
    let no_line_break = args.get(1).is_some_and(|v| v.is_truthy());
    let encoded = base64_encode(s.as_bytes(), B64_STD, true, !no_line_break);
    Ok(Value::string(encoded))
}

/// (base64-decode-string STRING &optional BASE64URL)
pub(crate) fn builtin_base64_decode_string(args: Vec<Value>) -> EvalResult {
    expect_range_args("base64-decode-string", &args, 1, 2)?;
    let s = require_string("base64-decode-string", &args[0])?;
    let use_url = args.get(1).is_some_and(|v| v.is_truthy());
    let table = if use_url {
        build_decode_table(B64_URL)
    } else {
        build_decode_table(B64_STD)
    };
    match base64_decode(&s, &table) {
        Ok(bytes) => {
            let decoded = String::from_utf8_lossy(&bytes).into_owned();
            Ok(Value::string(decoded))
        }
        Err(()) => Err(signal("error", vec![Value::string("Invalid base64 data")])),
    }
}

/// (base64url-encode-string STRING &optional NO-PAD)
pub(crate) fn builtin_base64url_encode_string(args: Vec<Value>) -> EvalResult {
    expect_range_args("base64url-encode-string", &args, 1, 2)?;
    let s = require_string("base64url-encode-string", &args[0])?;
    let no_pad = args.get(1).is_some_and(|v| v.is_truthy());
    let encoded = base64_encode(s.as_bytes(), B64_URL, !no_pad, false);
    Ok(Value::string(encoded))
}

/// (base64url-decode-string STRING &optional IGNORE-INVALID)
#[cfg(test)]
pub(crate) fn builtin_base64url_decode_string(args: Vec<Value>) -> EvalResult {
    expect_range_args("base64url-decode-string", &args, 1, 2)?;
    let s = require_string("base64url-decode-string", &args[0])?;
    let table = build_decode_table(B64_URL);
    match base64_decode(&s, &table) {
        Ok(bytes) => {
            let decoded = String::from_utf8_lossy(&bytes).into_owned();
            Ok(Value::string(decoded))
        }
        Err(()) => Ok(Value::NIL),
    }
}

pub(crate) fn normalize_current_buffer_region_bounds_in_manager(
    buffers: &BufferManager,
    start_arg: &Value,
    end_arg: &Value,
) -> Result<(crate::buffer::BufferId, usize, usize), Flow> {
    let buffer_id = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
        .id;

    let buf = buffers
        .get(buffer_id)
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;

    let point_min_char = buf.point_min_char() as i64 + 1;
    let point_max_char = buf.point_max_char() as i64 + 1;
    let start_raw = require_int_or_marker(start_arg)?;
    let end_raw = require_int_or_marker(end_arg)?;
    if start_raw < point_min_char
        || start_raw > point_max_char
        || end_raw < point_min_char
        || end_raw > point_max_char
    {
        return Err(signal(
            "args-out-of-range",
            vec![Value::make_buffer(buffer_id), *start_arg, *end_arg],
        ));
    }

    let (lo, hi) = if start_raw <= end_raw {
        (start_raw, end_raw)
    } else {
        (end_raw, start_raw)
    };

    let start_byte = buf.text.char_to_byte((lo - 1) as usize);
    let end_byte = buf.text.char_to_byte((hi - 1) as usize);
    Ok((buffer_id, start_byte, end_byte))
}

fn normalize_current_buffer_region_bounds(
    eval: &super::eval::Context,
    start_arg: &Value,
    end_arg: &Value,
) -> Result<(crate::buffer::BufferId, usize, usize), Flow> {
    normalize_current_buffer_region_bounds_in_manager(&eval.buffers, start_arg, end_arg)
}

pub(crate) fn read_buffer_region_bytes_in_manager(
    buffers: &BufferManager,
    buffer_id: crate::buffer::BufferId,
    start_byte: usize,
    end_byte: usize,
) -> Result<Vec<u8>, Flow> {
    let buf = buffers
        .get(buffer_id)
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    Ok(buf.buffer_substring_bytes(start_byte, end_byte))
}

pub(crate) fn replace_buffer_region_lisp_string_in_manager(
    buffers: &mut BufferManager,
    buffer_id: crate::buffer::BufferId,
    start_byte: usize,
    end_byte: usize,
    replacement: &crate::heap_types::LispString,
) -> Result<(), Flow> {
    buffers
        .goto_buffer_byte(buffer_id, start_byte)
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    buffers
        .delete_buffer_region(buffer_id, start_byte, end_byte)
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    buffers
        .goto_buffer_byte(buffer_id, start_byte)
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    buffers
        .insert_lisp_string_into_buffer(buffer_id, replacement)
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    Ok(())
}

fn replace_buffer_region_lisp_string(
    eval: &mut super::eval::Context,
    buffer_id: crate::buffer::BufferId,
    start_byte: usize,
    end_byte: usize,
    replacement: &crate::heap_types::LispString,
) -> Result<(), Flow> {
    let old_len = super::editfns::current_buffer_byte_span_char_len(eval, start_byte, end_byte);
    let new_len = replacement.sbytes();
    super::editfns::signal_before_change(eval, start_byte, end_byte)?;
    replace_buffer_region_lisp_string_in_manager(
        &mut eval.buffers,
        buffer_id,
        start_byte,
        end_byte,
        replacement,
    )?;
    super::editfns::signal_after_change(eval, start_byte, start_byte + new_len, old_len)?;
    Ok(())
}

/// (base64-encode-region START END &optional NO-LINE-BREAK)
pub(crate) fn builtin_base64_encode_region(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("base64-encode-region", &args, 2, 3)?;
    let (buffer_id, start_byte, end_byte) =
        normalize_current_buffer_region_bounds_in_manager(&mut eval.buffers, &args[0], &args[1])?;
    let source =
        read_buffer_region_bytes_in_manager(&mut eval.buffers, buffer_id, start_byte, end_byte)?;
    let no_line_break = args.get(2).is_some_and(|v| v.is_truthy());
    let encoded = base64_encode(&source, B64_STD, true, !no_line_break);
    let encoded_len = encoded.len();
    let target_multibyte = eval
        .buffers
        .get(buffer_id)
        .map(|buf| buf.get_multibyte())
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    let replacement =
        super::builtins::lisp_string_from_buffer_bytes(encoded.into_bytes(), target_multibyte);
    replace_buffer_region_lisp_string(eval, buffer_id, start_byte, end_byte, &replacement)?;
    Ok(Value::fixnum(encoded_len as i64))
}

/// (base64url-encode-region START END &optional NO-PAD)
pub(crate) fn builtin_base64url_encode_region(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("base64url-encode-region", &args, 2, 3)?;
    let (buffer_id, start_byte, end_byte) =
        normalize_current_buffer_region_bounds_in_manager(&mut eval.buffers, &args[0], &args[1])?;
    let source =
        read_buffer_region_bytes_in_manager(&mut eval.buffers, buffer_id, start_byte, end_byte)?;
    let no_pad = args.get(2).is_some_and(|v| v.is_truthy());
    let encoded = base64_encode(&source, B64_URL, !no_pad, false);
    let encoded_len = encoded.len();
    let target_multibyte = eval
        .buffers
        .get(buffer_id)
        .map(|buf| buf.get_multibyte())
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    let replacement =
        super::builtins::lisp_string_from_buffer_bytes(encoded.into_bytes(), target_multibyte);
    replace_buffer_region_lisp_string(eval, buffer_id, start_byte, end_byte, &replacement)?;
    Ok(Value::fixnum(encoded_len as i64))
}

/// (base64-decode-region START END &optional BASE64URL NOERROR)
pub(crate) fn builtin_base64_decode_region(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("base64-decode-region", &args, 2, 4)?;
    let (buffer_id, start_byte, end_byte) =
        normalize_current_buffer_region_bounds_in_manager(&mut eval.buffers, &args[0], &args[1])?;
    let source =
        read_buffer_region_bytes_in_manager(&mut eval.buffers, buffer_id, start_byte, end_byte)?;
    let use_url = args.get(2).is_some_and(|v| v.is_truthy());
    let noerror = args.get(3).is_some_and(|v| v.is_truthy());
    let table = if use_url {
        build_decode_table(B64_URL)
    } else {
        build_decode_table(B64_STD)
    };
    let source = String::from_utf8_lossy(&source).into_owned();
    let target_multibyte = eval
        .buffers
        .get(buffer_id)
        .map(|buf| buf.get_multibyte())
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;

    match base64_decode(&source, &table) {
        Ok(bytes) => {
            let replacement =
                super::builtins::lisp_string_from_buffer_bytes(bytes.clone(), target_multibyte);
            replace_buffer_region_lisp_string(eval, buffer_id, start_byte, end_byte, &replacement)?;
            Ok(Value::fixnum(bytes.len() as i64))
        }
        Err(()) if noerror => {
            let replacement =
                super::builtins::lisp_string_from_buffer_bytes(Vec::new(), target_multibyte);
            replace_buffer_region_lisp_string(eval, buffer_id, start_byte, end_byte, &replacement)?;
            Ok(Value::fixnum(0))
        }
        Err(()) => Err(signal("error", vec![Value::string("Invalid base64 data")])),
    }
}

// ---------------------------------------------------------------------------
// Hash / digest builtins
// ---------------------------------------------------------------------------

/// (md5 OBJECT &optional START END CODING-SYSTEM NOERROR)
///
/// Context-aware implementation that also supports buffer objects.
pub(crate) fn builtin_md5(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("md5", &args, 1, 5)?;
    validate_md5_coding_system_arg(&args)?;
    let object = &args[0];
    match object.kind() {
        ValueKind::String => Ok(Value::string(md5_hex_for_string(
            object,
            args.get(1),
            args.get(2),
        )?)),
        ValueKind::Veclike(VecLikeType::Buffer) => {
            Ok(Value::string(md5_hex_for_buffer_in_manager(
                &eval.buffers,
                object.as_buffer_id().unwrap(),
                args.get(1),
                args.get(2),
            )?))
        }
        _ => Err(signal(
            "error",
            vec![
                Value::string("Invalid object argument"),
                invalid_object_payload(object),
            ],
        )),
    }
}

/// Minimal MD5 implementation (RFC 1321).
fn md5_digest(message: &[u8]) -> [u8; 16] {
    // Per-round shift amounts
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];

    // Pre-computed T[i] = floor(2^32 * abs(sin(i+1)))
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    // Pre-processing: add padding
    let orig_len_bits = (message.len() as u64).wrapping_mul(8);
    let mut msg = message.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0x00);
    }
    // Append original length in bits as 64-bit little-endian
    msg.extend_from_slice(&orig_len_bits.to_le_bytes());

    // Initialize hash values
    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    // Process each 512-bit (64-byte) block
    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for i in 0..16 {
            m[i] = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }

        let mut a = a0;
        let mut b = b0;
        let mut c = c0;
        let mut d = d0;

        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };

            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }

        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    [
        a0 as u8,
        (a0 >> 8) as u8,
        (a0 >> 16) as u8,
        (a0 >> 24) as u8,
        b0 as u8,
        (b0 >> 8) as u8,
        (b0 >> 16) as u8,
        (b0 >> 24) as u8,
        c0 as u8,
        (c0 >> 8) as u8,
        (c0 >> 16) as u8,
        (c0 >> 24) as u8,
        d0 as u8,
        (d0 >> 8) as u8,
        (d0 >> 16) as u8,
        (d0 >> 24) as u8,
    ]
}

fn md5_hash(message: &[u8]) -> String {
    bytes_to_hex(&md5_digest(message))
}

fn md5_hex_for_string(
    object: &Value,
    start_raw: Option<&Value>,
    end_raw: Option<&Value>,
) -> Result<String, Flow> {
    let string = object
        .as_lisp_string()
        .expect("md5_hex_for_string only accepts string object");
    let len = string.schars() as i64;
    let start_arg = start_raw.cloned().unwrap_or(Value::NIL);
    let end_arg = end_raw.cloned().unwrap_or(Value::NIL);
    let start =
        normalize_secure_hash_index(start_raw, 0, len, object, &start_arg, &end_arg)? as usize;
    let end =
        normalize_secure_hash_index(end_raw, len, len, object, &start_arg, &end_arg)? as usize;

    if start > end {
        return Err(signal(
            "args-out-of-range",
            vec![*object, start_arg, end_arg],
        ));
    }

    let bytes = string.as_bytes();
    let (byte_from, byte_to) = if string.is_multibyte() {
        (
            crate::emacs_core::emacs_char::char_to_byte_pos(bytes, start),
            crate::emacs_core::emacs_char::char_to_byte_pos(bytes, end),
        )
    } else {
        (start, end)
    };
    if byte_to > bytes.len() {
        return Err(signal(
            "args-out-of-range",
            vec![*object, start_arg, end_arg],
        ));
    }
    Ok(md5_hash(&bytes[byte_from..byte_to]))
}

fn normalize_md5_buffer_position(
    val: Option<&Value>,
    default: i64,
    point_min: i64,
    point_max: i64,
    start_arg: &Value,
    end_arg: &Value,
) -> Result<i64, Flow> {
    let raw = match val {
        None => default,
        Some(v) if v.is_nil() => default,
        Some(v) => require_int_or_marker(v)?,
    };
    if raw < point_min || raw > point_max {
        return Err(signal("args-out-of-range", vec![*start_arg, *end_arg]));
    }
    Ok(raw)
}

fn hash_slice_for_buffer_in_manager(
    buffers: &BufferManager,
    buffer_id: crate::buffer::BufferId,
    start_raw: Option<&Value>,
    end_raw: Option<&Value>,
) -> Result<Vec<u8>, Flow> {
    let buf = buffers
        .get(buffer_id)
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;

    let point_min = buf.point_min_char() as i64 + 1;
    let point_max = buf.point_max_char() as i64 + 1;

    let start_arg = start_raw.cloned().unwrap_or(Value::NIL);
    let end_arg = end_raw.cloned().unwrap_or(Value::NIL);
    let start = normalize_md5_buffer_position(
        start_raw, point_min, point_min, point_max, &start_arg, &end_arg,
    )?;
    let end = normalize_md5_buffer_position(
        end_raw, point_max, point_min, point_max, &start_arg, &end_arg,
    )?;

    let (lo, hi) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let byte_lo = buf.lisp_pos_to_accessible_byte(lo);
    let byte_hi = buf.lisp_pos_to_accessible_byte(hi);
    Ok(buf.buffer_substring_bytes(byte_lo, byte_hi))
}

fn md5_hex_for_buffer_in_manager(
    buffers: &BufferManager,
    buffer_id: crate::buffer::BufferId,
    start_raw: Option<&Value>,
    end_raw: Option<&Value>,
) -> Result<String, Flow> {
    let slice = hash_slice_for_buffer_in_manager(buffers, buffer_id, start_raw, end_raw)?;
    Ok(md5_hash(&slice))
}

fn secure_hash_algorithm_name(val: &Value) -> Result<String, Flow> {
    match val.kind() {
        ValueKind::Symbol(id) => Ok(resolve_sym(id).to_owned()),
        ValueKind::Nil => Ok("nil".to_string()),
        ValueKind::T => Ok("t".to_string()),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *val],
        )),
    }
}

fn normalize_secure_hash_index(
    val: Option<&Value>,
    default: i64,
    len: i64,
    object: &Value,
    start_arg: &Value,
    end_arg: &Value,
) -> Result<i64, Flow> {
    let raw = match val {
        None => default,
        Some(v) if v.is_nil() => default,
        Some(v) => require_int(v)?,
    };
    let idx = if raw < 0 { len + raw } else { raw };
    if idx < 0 || idx > len {
        return Err(signal(
            "args-out-of-range",
            vec![*object, *start_arg, *end_arg],
        ));
    }
    Ok(idx)
}

fn invalid_object_payload(val: &Value) -> Value {
    if val.is_nil() {
        Value::string("nil")
    } else {
        *val
    }
}

fn bytes_to_lisp_binary_value(bytes: &[u8]) -> Value {
    Value::heap_string(crate::heap_types::LispString::from_unibyte(bytes.to_vec()))
}

fn hash_slice_for_string(
    object: &Value,
    start_raw: Option<&Value>,
    end_raw: Option<&Value>,
) -> Result<Vec<u8>, Flow> {
    let string = object
        .as_lisp_string()
        .expect("hash_slice_for_string only accepts string object");
    let len = string.schars() as i64;
    let start_arg = start_raw.cloned().unwrap_or(Value::NIL);
    let end_arg = end_raw.cloned().unwrap_or(Value::NIL);
    let start =
        normalize_secure_hash_index(start_raw, 0, len, object, &start_arg, &end_arg)? as usize;
    let end =
        normalize_secure_hash_index(end_raw, len, len, object, &start_arg, &end_arg)? as usize;

    if start > end {
        return Err(signal(
            "args-out-of-range",
            vec![*object, start_arg, end_arg],
        ));
    }

    let bytes = string.as_bytes();
    let (byte_from, byte_to) = if string.is_multibyte() {
        (
            crate::emacs_core::emacs_char::char_to_byte_pos(bytes, start),
            crate::emacs_core::emacs_char::char_to_byte_pos(bytes, end),
        )
    } else {
        (start, end)
    };
    if byte_to > bytes.len() {
        return Err(signal(
            "args-out-of-range",
            vec![*object, start_arg, end_arg],
        ));
    }
    Ok(bytes[byte_from..byte_to].to_vec())
}

fn secure_hash_digest_bytes(algo_name: &str, input: &[u8]) -> Result<Vec<u8>, Flow> {
    let digest = match algo_name {
        "md5" => md5_digest(input).to_vec(),
        "sha1" => {
            let mut h = Sha1::new();
            h.update(input);
            h.finalize().to_vec()
        }
        "sha224" => {
            let mut h = Sha224::new();
            h.update(input);
            h.finalize().to_vec()
        }
        "sha256" => {
            let mut h = Sha256::new();
            h.update(input);
            h.finalize().to_vec()
        }
        "sha384" => {
            let mut h = Sha384::new();
            h.update(input);
            h.finalize().to_vec()
        }
        "sha512" => {
            let mut h = Sha512::new();
            h.update(input);
            h.finalize().to_vec()
        }
        _ => {
            return Err(signal(
                "error",
                vec![Value::string(format!("Invalid algorithm arg: {algo_name}"))],
            ));
        }
    };
    Ok(digest)
}

/// (secure-hash ALGORITHM OBJECT &optional START END BINARY)
///
/// Context-aware implementation that also supports buffer objects.
pub(crate) fn builtin_secure_hash(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("secure-hash", &args, 2, 5)?;
    let algo_name = secure_hash_algorithm_name(&args[0])?;

    let object = &args[1];
    let input = match object.kind() {
        ValueKind::String => hash_slice_for_string(object, args.get(2), args.get(3))?,
        ValueKind::Veclike(VecLikeType::Buffer) => hash_slice_for_buffer_in_manager(
            &eval.buffers,
            object.as_buffer_id().unwrap(),
            args.get(2),
            args.get(3),
        )?,
        _ => {
            return Err(signal(
                "error",
                vec![
                    Value::string("Invalid object argument"),
                    invalid_object_payload(object),
                ],
            ));
        }
    };

    let digest = secure_hash_digest_bytes(&algo_name, &input)?;
    let binary = args.get(4).is_some_and(|v| v.is_truthy());
    if binary {
        Ok(bytes_to_lisp_binary_value(&digest))
    } else {
        Ok(Value::string(bytes_to_hex(&digest)))
    }
}

/// (buffer-hash &optional BUFFER-OR-NAME)
/// Context-aware implementation used at runtime.
pub(crate) fn builtin_buffer_hash(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("buffer-hash", &args, 0, 1)?;

    let buffer_id = if args.is_empty() || args[0].is_nil() {
        eval.buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
            .id
    } else {
        match args[0].kind() {
            ValueKind::Veclike(VecLikeType::Buffer) => args[0].as_buffer_id().unwrap(),
            ValueKind::String => {
                let name = super::builtins::lisp_string_to_runtime_string(args[0]);
                eval.buffers.find_buffer_by_name(&name).ok_or_else(|| {
                    signal(
                        "error",
                        vec![Value::string(format!("No buffer named {name}"))],
                    )
                })?
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), args[0]],
                ));
            }
        }
    };

    // GNU Emacs accepts killed buffer objects and hashes as empty content.
    let text = eval
        .buffers
        .get(buffer_id)
        .map(|buf| buf.buffer_substring_bytes(buf.point_min(), buf.point_max()))
        .unwrap_or_default();

    let mut hasher = Sha1::new();
    hasher.update(&text);
    Ok(Value::string(bytes_to_hex(&hasher.finalize())))
}

/// (equal-including-properties O1 O2)
/// Like `equal` but also checks text properties. Since our implementation
/// does not yet track text properties on strings, this behaves the same
/// as `equal` for now.
pub(crate) fn builtin_equal_including_properties(args: Vec<Value>) -> EvalResult {
    expect_args("equal-including-properties", &args, 2)?;
    Ok(Value::bool_val(equal_value(&args[0], &args[1], 0)))
}

// ---------------------------------------------------------------------------
// Widget helpers
// ---------------------------------------------------------------------------

/// (widget-get WIDGET PROPERTY)
/// WIDGET is a list (plist-like).  Extract PROPERTY from the widget's plist
/// tail (skip car which is the widget type).
pub(crate) fn builtin_widget_get(args: Vec<Value>) -> EvalResult {
    expect_args("widget-get", &args, 2)?;
    let widget = &args[0];
    let property = &args[1];

    // WIDGET is (TYPE :prop1 val1 :prop2 val2 ...)
    // Skip the first element (type), then search plist-style.
    if let Some(items) = list_to_vec(widget) {
        // Start from index 1 (skip type), search plist pairs
        let mut i = 1;
        while i + 1 < items.len() {
            if equal_value(&items[i], property, 0) {
                return Ok(items[i + 1]);
            }
            i += 2;
        }
    }
    Ok(Value::NIL)
}

/// (widget-put WIDGET PROPERTY VALUE)
/// Set PROPERTY to VALUE in the widget plist. Returns VALUE.
/// Since widgets are mutable lists, we modify in-place by walking cons cells.
pub(crate) fn builtin_widget_put(args: Vec<Value>) -> EvalResult {
    expect_args("widget-put", &args, 3)?;
    let widget = &args[0];
    let property = &args[1];
    let value = &args[2];

    // Walk the cdr of WIDGET (skip the type cons cell) looking for PROPERTY.
    if widget.is_cons() {
        let mut cursor = {
            let cell_car = widget.cons_car();
            let cell_cdr = widget.cons_cdr();
            cell_cdr
        };
        loop {
            match cursor.kind() {
                ValueKind::Cons => {
                    let key = {
                        let cell_car = cursor.cons_car();
                        let cell_cdr = cursor.cons_cdr();
                        cell_car
                    };
                    if equal_value(&key, property, 0) {
                        // Found the key cons. The *next* cons cell
                        // holds the value (plist layout: KEY VAL KEY
                        // VAL ...). Mutate that next cell's car, NOT
                        // the current key cell — overwriting the key
                        // would break the plist structure.
                        let next = cursor.cons_cdr();
                        if next.is_cons() {
                            next.set_car(*value);
                            return Ok(*value);
                        }
                        break;
                    }
                    // Skip value, move to next key
                    let after_key = cursor.cons_cdr();
                    if after_key.is_cons() {
                        cursor = after_key.cons_cdr();
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }

        // Property not found — append to end of widget plist (after type).
        // Prepend (PROPERTY VALUE ...) to the cdr of the first cons cell.
        let old_cdr = (*widget).cons_cdr();
        let new_tail = Value::cons(*property, Value::cons(*value, old_cdr));
        (*widget).set_cdr(new_tail);
    }

    Ok(*value)
}

/// (widget-apply WIDGET PROPERTY &rest ARGS)
/// Apply WIDGET's PROPERTY function to WIDGET and ARGS.
pub(crate) fn builtin_widget_apply(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("widget-apply", &args, 2)?;
    let widget = args[0];
    let property = args[1];

    let function = builtin_widget_get(vec![widget, property])?;
    if function.is_nil() {
        return Err(signal("void-function", vec![Value::NIL]));
    }

    let mut call_args = Vec::with_capacity(args.len().saturating_sub(1));
    call_args.push(widget);
    call_args.extend_from_slice(&args[2..]);

    match function.kind() {
        ValueKind::Symbol(id) => {
            let name = resolve_sym(id);
            if let Some(result) = eval.dispatch_subr(name, call_args) {
                result
            } else {
                Err(signal("void-function", vec![Value::symbol(name)]))
            }
        }
        ValueKind::Veclike(VecLikeType::Subr) => {
            let id = function.as_subr_id().unwrap();
            let name = resolve_sym(id);
            if let Some(result) = eval.dispatch_subr(name, call_args) {
                result
            } else {
                Err(signal("void-function", vec![Value::symbol(name)]))
            }
        }
        _ => Err(signal("invalid-function", vec![function])),
    }
}

/// (string-make-multibyte STRING) -- convert unibyte storage bytes to multibyte chars.
pub(crate) fn builtin_string_make_multibyte(args: Vec<Value>) -> EvalResult {
    use crate::emacs_core::emacs_char;
    expect_args("string-make-multibyte", &args, 1)?;
    let ls = args[0].as_lisp_string().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        )
    })?;
    if ls.is_multibyte() {
        return Ok(args[0]);
    }
    // Unibyte -> multibyte: each byte 0x80..0xFF becomes a raw-byte char.
    let src = ls.as_bytes();
    let mut out = Vec::with_capacity(src.len() * 2);
    for &b in src {
        let mut buf = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
        let c = emacs_char::byte8_to_char(b);
        let len = emacs_char::char_string(c, &mut buf);
        out.extend_from_slice(&buf[..len]);
    }
    Ok(Value::heap_string(
        crate::heap_types::LispString::from_emacs_bytes(out),
    ))
}

/// (string-make-unibyte STRING) -- convert each character code to a single byte.
pub(crate) fn builtin_string_make_unibyte(args: Vec<Value>) -> EvalResult {
    expect_args("string-make-unibyte", &args, 1)?;
    match args[0].kind() {
        ValueKind::String => {
            let string = args[0].as_lisp_string().expect("string");
            let src_bytes = string.as_bytes();
            let result_bytes: Vec<u8> = if string.is_multibyte() {
                let mut out = Vec::with_capacity(string.schars());
                let mut pos = 0;
                while pos < src_bytes.len() {
                    let (cp, len) = crate::emacs_core::emacs_char::string_char(&src_bytes[pos..]);
                    out.push((cp & 0xFF) as u8);
                    pos += len;
                }
                out
            } else {
                // Already unibyte
                src_bytes.to_vec()
            };
            Ok(Value::heap_string(
                crate::heap_types::LispString::from_unibyte(result_bytes),
            ))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        )),
    }
}

// ---------------------------------------------------------------------------
// String comparison
// ---------------------------------------------------------------------------

/// (compare-strings STR1 START1 END1 STR2 START2 END2 &optional IGNORE-CASE)
///
/// Compare substrings of STR1 and STR2.
/// Returns t if they are equal, or the 1-based index of the first differing
/// character (negative if STR1 is less, positive if STR1 is greater).
pub(crate) fn builtin_compare_strings(args: Vec<Value>) -> EvalResult {
    expect_range_args("compare-strings", &args, 6, 7)?;

    let s1 = require_string("compare-strings", &args[0])?;
    let s2 = require_string("compare-strings", &args[3])?;

    let chars1: Vec<char> = s1.chars().collect();
    let chars2: Vec<char> = s2.chars().collect();

    let start1 = match args[1].kind() {
        ValueKind::Nil => 0usize,
        ValueKind::Fixnum(n) => (n).max(0) as usize, // 0-based index
        _ => 0,
    };
    let end1 = match args[2].kind() {
        ValueKind::Nil => chars1.len(),
        ValueKind::Fixnum(n) => (n as usize).min(chars1.len()),
        _ => chars1.len(),
    };
    let start2 = match args[4].kind() {
        ValueKind::Nil => 0usize,
        ValueKind::Fixnum(n) => (n).max(0) as usize, // 0-based index
        _ => 0,
    };
    let end2 = match args[5].kind() {
        ValueKind::Nil => chars2.len(),
        ValueKind::Fixnum(n) => (n as usize).min(chars2.len()),
        _ => chars2.len(),
    };

    let ignore_case = args.get(6).is_some_and(|v| v.is_truthy());

    let sub1 = &chars1[start1.min(chars1.len())..end1.min(chars1.len())];
    let sub2 = &chars2[start2.min(chars2.len())..end2.min(chars2.len())];

    let len = sub1.len().min(sub2.len());
    for i in 0..len {
        let c1 = if ignore_case {
            sub1[i].to_lowercase().next().unwrap_or(sub1[i])
        } else {
            sub1[i]
        };
        let c2 = if ignore_case {
            sub2[i].to_lowercase().next().unwrap_or(sub2[i])
        } else {
            sub2[i]
        };
        if c1 != c2 {
            let pos = (i + 1) as i64; // 1-based
            if c1 < c2 {
                return Ok(Value::fixnum(-pos));
            } else {
                return Ok(Value::fixnum(pos));
            }
        }
    }

    if sub1.len() == sub2.len() {
        Ok(Value::T)
    } else if sub1.len() < sub2.len() {
        Ok(Value::fixnum(-((len + 1) as i64)))
    } else {
        Ok(Value::fixnum((len + 1) as i64))
    }
}

/// (string-version-lessp S1 S2) -- version-aware string comparison.
///
/// Compares strings character by character, but when both strings have a
/// run of digits at the same position, the digit runs are compared as
/// integers (so "foo2" < "foo10").
pub(crate) fn builtin_string_version_lessp(args: Vec<Value>) -> EvalResult {
    expect_args("string-version-lessp", &args, 2)?;
    // Symbols are allowed; their print names are used instead (like official Emacs).
    let s1 = if let Some(name) = args[0].as_symbol_name() {
        name.to_string()
    } else {
        require_string("string-version-lessp", &args[0])?
    };
    let s2 = if let Some(name) = args[1].as_symbol_name() {
        name.to_string()
    } else {
        require_string("string-version-lessp", &args[1])?
    };

    let c1: Vec<char> = s1.chars().collect();
    let c2: Vec<char> = s2.chars().collect();

    let mut i = 0;
    let mut j = 0;

    while i < c1.len() && j < c2.len() {
        if c1[i].is_ascii_digit() && c2[j].is_ascii_digit() {
            // Extract numeric runs and compare as integers
            let mut n1: u64 = 0;
            while i < c1.len() && c1[i].is_ascii_digit() {
                n1 = n1
                    .saturating_mul(10)
                    .saturating_add(c1[i] as u64 - '0' as u64);
                i += 1;
            }
            let mut n2: u64 = 0;
            while j < c2.len() && c2[j].is_ascii_digit() {
                n2 = n2
                    .saturating_mul(10)
                    .saturating_add(c2[j] as u64 - '0' as u64);
                j += 1;
            }
            if n1 != n2 {
                return Ok(Value::bool_val(n1 < n2));
            }
        } else {
            if c1[i] != c2[j] {
                return Ok(Value::bool_val(c1[i] < c2[j]));
            }
            i += 1;
            j += 1;
        }
    }

    Ok(Value::bool_val(c1.len() < c2.len()))
}

/// (string-collate-lessp S1 S2 &optional LOCALE IGNORE-CASE)
/// Simple lexicographic comparison (locale is ignored).
pub(crate) fn builtin_string_collate_lessp(args: Vec<Value>) -> EvalResult {
    expect_range_args("string-collate-lessp", &args, 2, 4)?;
    let s1 = require_string("string-collate-lessp", &args[0])?;
    let s2 = require_string("string-collate-lessp", &args[1])?;
    let ignore_case = args.get(3).is_some_and(|v| v.is_truthy());

    let result = if ignore_case {
        s1.to_lowercase() < s2.to_lowercase()
    } else {
        s1 < s2
    };
    Ok(Value::bool_val(result))
}

/// (string-collate-equalp S1 S2 &optional LOCALE IGNORE-CASE)
/// Simple lexicographic equality (locale is ignored).
pub(crate) fn builtin_string_collate_equalp(args: Vec<Value>) -> EvalResult {
    expect_range_args("string-collate-equalp", &args, 2, 4)?;
    let s1 = require_string("string-collate-equalp", &args[0])?;
    let s2 = require_string("string-collate-equalp", &args[1])?;
    let ignore_case = args.get(3).is_some_and(|v| v.is_truthy());

    let result = if ignore_case {
        s1.to_lowercase() == s2.to_lowercase()
    } else {
        s1 == s2
    };
    Ok(Value::bool_val(result))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "fns_test.rs"]
mod tests;
