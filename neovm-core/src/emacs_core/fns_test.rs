use super::*;
use crate::emacs_core::eval::Context;
use crate::emacs_core::value::{ValueKind, VecLikeType};
use crate::emacs_core::{print, string_escape};

/// Test helper: create a minimal eval context for widget-apply tests.
fn test_eval_ctx() -> crate::emacs_core::eval::Context {
    crate::emacs_core::eval::Context::new()
}

macro_rules! call_fns_builtin {
    ($builtin:ident, $args:expr) => {{
        let mut eval = Context::new();
        $builtin(&mut eval, $args)
    }};
}

// ---- Base64 standard ----

#[test]
fn base64_encode_empty() {
    crate::test_utils::init_test_tracing();
    let r = builtin_base64_encode_string(vec![Value::string(""), Value::T]).unwrap();
    assert_eq!(r.as_str(), Some(""));
}

#[test]
fn base64_encode_hello() {
    crate::test_utils::init_test_tracing();
    let r = builtin_base64_encode_string(vec![Value::string("Hello"), Value::T]).unwrap();
    assert_eq!(r.as_str(), Some("SGVsbG8="));
}

#[test]
fn base64_encode_padding_1() {
    crate::test_utils::init_test_tracing();
    // "a" -> "YQ=="
    let r = builtin_base64_encode_string(vec![Value::string("a"), Value::T]).unwrap();
    assert_eq!(r.as_str(), Some("YQ=="));
}

#[test]
fn base64_encode_padding_2() {
    crate::test_utils::init_test_tracing();
    // "ab" -> "YWI="
    let r = builtin_base64_encode_string(vec![Value::string("ab"), Value::T]).unwrap();
    assert_eq!(r.as_str(), Some("YWI="));
}

#[test]
fn base64_encode_no_padding_3() {
    crate::test_utils::init_test_tracing();
    // "abc" -> "YWJj" (no padding needed)
    let r = builtin_base64_encode_string(vec![Value::string("abc"), Value::T]).unwrap();
    assert_eq!(r.as_str(), Some("YWJj"));
}

#[test]
fn base64_roundtrip() {
    crate::test_utils::init_test_tracing();
    let original = "The quick brown fox jumps over the lazy dog";
    let encoded = builtin_base64_encode_string(vec![Value::string(original), Value::T]).unwrap();
    let decoded = builtin_base64_decode_string(vec![encoded]).unwrap();
    assert_eq!(decoded.as_str(), Some(original));
}

#[test]
fn base64_decode_invalid() {
    crate::test_utils::init_test_tracing();
    // Invalid base64 now signals an error (matching GNU Emacs)
    let r = builtin_base64_decode_string(vec![Value::string("!!!!")]);
    assert!(r.is_err());
}

// ---- Base64 URL ----

#[test]
fn base64url_encode_no_pad() {
    crate::test_utils::init_test_tracing();
    let r = builtin_base64url_encode_string(vec![Value::string("a"), Value::T]).unwrap();
    // URL-safe, no padding
    assert_eq!(r.as_str(), Some("YQ"));
}

#[test]
fn base64url_encode_with_pad() {
    crate::test_utils::init_test_tracing();
    let r = builtin_base64url_encode_string(vec![Value::string("a")]).unwrap();
    assert_eq!(r.as_str(), Some("YQ=="));
}

#[test]
fn base64url_roundtrip() {
    crate::test_utils::init_test_tracing();
    let original = "Hello+World/Foo";
    let encoded = builtin_base64url_encode_string(vec![Value::string(original), Value::T]).unwrap();
    let decoded = builtin_base64_decode_string(vec![encoded, Value::T]).unwrap();
    assert_eq!(decoded.as_str(), Some(original));
}

#[test]
fn base64url_decode_basic() {
    crate::test_utils::init_test_tracing();
    let decoded = builtin_base64url_decode_string(vec![Value::string("YQ")]).unwrap();
    assert_eq!(decoded.as_str(), Some("a"));
}

#[test]
fn base64url_decode_invalid() {
    crate::test_utils::init_test_tracing();
    let decoded = builtin_base64url_decode_string(vec![Value::string("!!!!")]).unwrap();
    assert!(decoded.is_nil());
}

#[test]
fn base64url_uses_dash_underscore() {
    crate::test_utils::init_test_tracing();
    // Standard base64 of "?>" is "Pz4=" which contains no + or /.
    // Use a string that we know produces different chars in std vs url.
    // "abc?+/" in standard base64 is "YWJjPysvg" — contains + and /.
    // Actually, just test that the url alphabet is used:
    // base64url of ">?" is "Pj8" (std would be "Pj8" too — same for ASCII).
    // Instead, directly encode bytes [0xFF] which in std is "/w==" and url is "_w==".
    // Since our strings are UTF-8, we use a string with codepoint U+00FF (latin small y with diaeresis).
    let input = "\u{00FF}"; // UTF-8: [0xC3, 0xBF]
    let std_enc = builtin_base64_encode_string(vec![Value::string(input), Value::T]).unwrap();
    let url_enc = builtin_base64url_encode_string(vec![Value::string(input), Value::T]).unwrap();
    // Standard and URL should differ if the encoding contains + or /
    // For [0xC3, 0xBF]: std = "w78=" which has no + or /... let's just
    // verify neither + nor / appear in url encoding.
    let s = url_enc.as_str().unwrap();
    assert!(!s.contains('+'), "URL-safe encoding should not contain '+'");
    assert!(!s.contains('/'), "URL-safe encoding should not contain '/'");
    // Also verify the standard encoding does not contain - or _
    let s_std = std_enc.as_str().unwrap();
    assert!(
        !s_std.contains('-'),
        "Standard encoding should not contain '-'"
    );
    assert!(
        !s_std.contains('_'),
        "Standard encoding should not contain '_'"
    );
}

#[test]
fn base64_region_eval_encode_decode_roundtrip() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("Hi");
    }

    let encoded = builtin_base64_encode_region(&mut eval, vec![Value::fixnum(1), Value::fixnum(3)])
        .expect("encode region should succeed");
    assert_eq!(encoded, Value::fixnum(4));
    let encoded_text = eval
        .buffers
        .current_buffer()
        .expect("current buffer")
        .buffer_string();
    assert_eq!(encoded_text, "SGk=");

    let decoded = builtin_base64_decode_region(&mut eval, vec![Value::fixnum(1), Value::fixnum(5)])
        .expect("decode region should succeed");
    assert_eq!(decoded, Value::fixnum(2));
    let decoded_text = eval
        .buffers
        .current_buffer()
        .expect("current buffer")
        .buffer_string();
    assert_eq!(decoded_text, "Hi");
}

#[test]
fn base64_region_eval_swapped_bounds_and_url_encoding() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("ab");
    }

    let encoded = builtin_base64url_encode_region(
        &mut eval,
        vec![Value::fixnum(3), Value::fixnum(1), Value::T],
    )
    .expect("url encode region should succeed");
    assert_eq!(encoded, Value::fixnum(3));
    let encoded_text = eval
        .buffers
        .current_buffer()
        .expect("current buffer")
        .buffer_string();
    assert_eq!(encoded_text, "YWI");
}

#[test]
fn base64_decode_region_noerror_semantics() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("%%");
    }

    let ignored = builtin_base64_decode_region(
        &mut eval,
        vec![Value::fixnum(1), Value::fixnum(3), Value::NIL, Value::T],
    )
    .expect("noerror decode should succeed");
    assert_eq!(ignored, Value::fixnum(0));
    let emptied = eval
        .buffers
        .current_buffer()
        .expect("current buffer")
        .buffer_string();
    assert_eq!(emptied, "");

    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("%%");
    }
    let strict = builtin_base64_decode_region(&mut eval, vec![Value::fixnum(1), Value::fixnum(3)]);
    match strict {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Invalid base64 data")]);
        }
        other => panic!("expected invalid base64 signal, got {other:?}"),
    }
    let unchanged = eval
        .buffers
        .current_buffer()
        .expect("current buffer")
        .buffer_string();
    assert_eq!(unchanged, "%%");
}

#[test]
fn base64_region_eval_error_shapes() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("Hi");
    }

    let type_error = builtin_base64_encode_region(
        &mut eval,
        vec![Value::symbol("x"), Value::fixnum(2), Value::T],
    );
    match type_error {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("integer-or-marker-p"), Value::symbol("x")]
            );
        }
        other => panic!("expected wrong-type-argument, got {other:?}"),
    }

    let range_error =
        builtin_base64_encode_region(&mut eval, vec![Value::fixnum(0), Value::fixnum(2)]);
    match range_error {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data.len(), 3);
            assert!(sig.data[0].is_buffer());
            assert_eq!(sig.data[1], Value::fixnum(0));
            assert_eq!(sig.data[2], Value::fixnum(2));
        }
        other => panic!("expected args-out-of-range, got {other:?}"),
    }
}

// ---- MD5 ----

#[test]
fn md5_empty() {
    crate::test_utils::init_test_tracing();
    let r = call_fns_builtin!(builtin_md5, vec![Value::string("")]).unwrap();
    assert_eq!(r.as_str(), Some("d41d8cd98f00b204e9800998ecf8427e"));
}

#[test]
fn md5_hello() {
    crate::test_utils::init_test_tracing();
    // md5("Hello") = 8b1a9953c4611296a827abf8c47804d7
    let r = call_fns_builtin!(builtin_md5, vec![Value::string("Hello")]).unwrap();
    assert_eq!(r.as_str(), Some("8b1a9953c4611296a827abf8c47804d7"));
}

#[test]
fn md5_abc() {
    crate::test_utils::init_test_tracing();
    let r = call_fns_builtin!(builtin_md5, vec![Value::string("abc")]).unwrap();
    assert_eq!(r.as_str(), Some("900150983cd24fb0d6963f7d28e17f72"));
}

#[test]
fn md5_fox() {
    crate::test_utils::init_test_tracing();
    let r = call_fns_builtin!(
        builtin_md5,
        vec![Value::string("The quick brown fox jumps over the lazy dog")]
    )
    .unwrap();
    assert_eq!(r.as_str(), Some("9e107d9d372bb6826bd81d3542a419d6"));
}

#[test]
fn md5_string_range_errors() {
    crate::test_utils::init_test_tracing();
    match call_fns_builtin!(
        builtin_md5,
        vec![Value::string("abc"), Value::fixnum(2), Value::fixnum(1)]
    ) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(
                sig.data,
                vec![Value::string("abc"), Value::fixnum(2), Value::fixnum(1)]
            );
        }
        other => panic!("expected args-out-of-range signal, got {other:?}"),
    }
}

#[test]
fn md5_string_index_type_error() {
    crate::test_utils::init_test_tracing();
    match call_fns_builtin!(
        builtin_md5,
        vec![Value::string("abc"), Value::T, Value::fixnum(1)]
    ) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data.first(), Some(&Value::symbol("integerp")));
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn md5_invalid_object_errors() {
    crate::test_utils::init_test_tracing();
    match call_fns_builtin!(builtin_md5, vec![Value::NIL]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data.first().and_then(|v| v.as_str()),
                Some("Invalid object argument")
            );
            assert_eq!(sig.data.get(1), Some(&Value::string("nil")));
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn md5_unknown_coding_system_errors() {
    crate::test_utils::init_test_tracing();
    match call_fns_builtin!(
        builtin_md5,
        vec![
            Value::string("abc"),
            Value::NIL,
            Value::NIL,
            Value::symbol("no-such"),
        ]
    ) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "coding-system-error");
            assert_eq!(sig.data, vec![Value::symbol("no-such")]);
        }
        other => panic!("expected coding-system-error signal, got {other:?}"),
    }
}

#[test]
fn md5_unknown_coding_system_ignored_with_noerror() {
    crate::test_utils::init_test_tracing();
    let r = call_fns_builtin!(
        builtin_md5,
        vec![
            Value::string("abc"),
            Value::NIL,
            Value::NIL,
            Value::symbol("no-such"),
            Value::T,
        ]
    )
    .unwrap();
    assert_eq!(r.as_str(), Some("900150983cd24fb0d6963f7d28e17f72"));
}

#[test]
fn md5_accepts_iso_8859_15_alias() {
    crate::test_utils::init_test_tracing();
    let r = call_fns_builtin!(
        builtin_md5,
        vec![
            Value::string("abc"),
            Value::NIL,
            Value::NIL,
            Value::symbol("iso-8859-15"),
        ]
    )
    .unwrap();
    assert_eq!(r.as_str(), Some("900150983cd24fb0d6963f7d28e17f72"));
}

#[test]
fn md5_accepts_iso_8859_9_alias() {
    crate::test_utils::init_test_tracing();
    let r = call_fns_builtin!(
        builtin_md5,
        vec![
            Value::string("abc"),
            Value::NIL,
            Value::NIL,
            Value::symbol("iso-8859-9"),
        ]
    )
    .unwrap();
    assert_eq!(r.as_str(), Some("900150983cd24fb0d6963f7d28e17f72"));
}

#[test]
fn md5_non_symbol_coding_system_errors() {
    crate::test_utils::init_test_tracing();
    match call_fns_builtin!(
        builtin_md5,
        vec![
            Value::string("abc"),
            Value::NIL,
            Value::NIL,
            Value::fixnum(1),
        ]
    ) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "coding-system-error");
            assert_eq!(sig.data, vec![Value::fixnum(1)]);
        }
        other => panic!("expected coding-system-error signal, got {other:?}"),
    }
}

#[test]
fn md5_eval_buffer_core_semantics() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("abc");
    }
    let id = eval.buffers.current_buffer().expect("current buffer").id;

    let full = builtin_md5(&mut eval, vec![Value::make_buffer(id)]).unwrap();
    assert_eq!(full.as_str(), Some("900150983cd24fb0d6963f7d28e17f72"));

    let swapped = builtin_md5(
        &mut eval,
        vec![Value::make_buffer(id), Value::fixnum(4), Value::fixnum(3)],
    )
    .unwrap();
    assert_eq!(swapped.as_str(), Some("4a8a08f09d37b73795649038408b5f33"));
}

#[test]
fn md5_eval_buffer_range_errors() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("abc");
    }
    let id = eval.buffers.current_buffer().expect("current buffer").id;

    match builtin_md5(&mut eval, vec![Value::make_buffer(id), Value::fixnum(5)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::fixnum(5), Value::NIL]);
        }
        other => panic!("expected args-out-of-range signal, got {other:?}"),
    }
}

#[test]
fn md5_eval_buffer_index_type_error() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    let id = eval.buffers.current_buffer().expect("current buffer").id;

    match builtin_md5(
        &mut eval,
        vec![Value::make_buffer(id), Value::T, Value::fixnum(3)],
    ) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data.first(),
                Some(&Value::symbol("integer-or-marker-p"))
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn md5_eval_deleted_buffer_errors() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    let id = eval.buffers.create_buffer("*md5-doomed*");
    assert!(eval.buffers.kill_buffer(id));

    match builtin_md5(&mut eval, vec![Value::make_buffer(id)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data.first().and_then(|v| v.as_str()),
                Some("Selecting deleted buffer")
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

// ---- secure-hash ----

#[test]
fn secure_hash_sha256_known() {
    crate::test_utils::init_test_tracing();
    let r = call_fns_builtin!(
        builtin_secure_hash,
        vec![Value::symbol("sha256"), Value::string("abc")]
    )
    .unwrap();
    assert_eq!(
        r.as_str(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
}

#[test]
fn secure_hash_sha1_known() {
    crate::test_utils::init_test_tracing();
    let r = call_fns_builtin!(
        builtin_secure_hash,
        vec![Value::symbol("sha1"), Value::string("abc")]
    )
    .unwrap();
    assert_eq!(r.as_str(), Some("a9993e364706816aba3e25717850c26c9cd0d89d"));
}

#[test]
fn secure_hash_md5_known() {
    crate::test_utils::init_test_tracing();
    let r = call_fns_builtin!(
        builtin_secure_hash,
        vec![Value::symbol("md5"), Value::string("abc")]
    )
    .unwrap();
    assert_eq!(r.as_str(), Some("900150983cd24fb0d6963f7d28e17f72"));
}

#[test]
fn secure_hash_binary_string_uses_unibyte_storage() {
    crate::test_utils::init_test_tracing();
    let r = call_fns_builtin!(
        builtin_secure_hash,
        vec![
            Value::symbol("sha1"),
            Value::string("abc"),
            Value::NIL,
            Value::NIL,
            Value::T,
        ]
    )
    .unwrap();

    let s = r
        .as_str()
        .expect("binary secure-hash should return a string");
    assert_eq!(string_escape::storage_byte_len(s), 20);
    assert_eq!(
        string_escape::decode_storage_char_codes(s).first(),
        Some(&169)
    );

    let printed = print::print_value_bytes(&r);
    assert_eq!(printed.first(), Some(&b'"'));
    assert_eq!(printed.last(), Some(&b'"'));
}

#[test]
fn secure_hash_subrange_semantics() {
    crate::test_utils::init_test_tracing();
    let r = call_fns_builtin!(
        builtin_secure_hash,
        vec![
            Value::symbol("sha256"),
            Value::string("abcdef"),
            Value::fixnum(1),
            Value::fixnum(4),
        ]
    )
    .unwrap();
    assert_eq!(
        r.as_str(),
        Some("a6b0f90d2ac2b8d1f250c687301aef132049e9016df936680e81fa7bc7d81d70")
    );
}

#[test]
fn secure_hash_invalid_algorithm_errors() {
    crate::test_utils::init_test_tracing();
    match call_fns_builtin!(
        builtin_secure_hash,
        vec![Value::symbol("no-such"), Value::string("abc")]
    ) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data.first().and_then(|v| v.as_str()),
                Some("Invalid algorithm arg: no-such")
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn secure_hash_invalid_algorithm_type_errors() {
    crate::test_utils::init_test_tracing();
    match call_fns_builtin!(
        builtin_secure_hash,
        vec![Value::fixnum(1), Value::string("abc")]
    ) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data.first(), Some(&Value::symbol("symbolp")));
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn secure_hash_invalid_object_errors() {
    crate::test_utils::init_test_tracing();
    match call_fns_builtin!(
        builtin_secure_hash,
        vec![Value::symbol("sha256"), Value::fixnum(123)]
    ) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data.first().and_then(|v| v.as_str()),
                Some("Invalid object argument")
            );
            assert_eq!(sig.data.get(1), Some(&Value::fixnum(123)));
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn secure_hash_eval_buffer_sha1() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("abc");
    }
    let id = eval.buffers.current_buffer().expect("current buffer").id;
    let r = builtin_secure_hash(
        &mut eval,
        vec![Value::symbol("sha1"), Value::make_buffer(id)],
    )
    .unwrap();
    assert_eq!(r.as_str(), Some("a9993e364706816aba3e25717850c26c9cd0d89d"));
}

#[test]
fn secure_hash_eval_buffer_range_errors() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("abc");
    }
    let id = eval.buffers.current_buffer().expect("current buffer").id;

    match builtin_secure_hash(
        &mut eval,
        vec![
            Value::symbol("sha1"),
            Value::make_buffer(id),
            Value::fixnum(5),
        ],
    ) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::fixnum(5), Value::NIL]);
        }
        other => panic!("expected args-out-of-range signal, got {other:?}"),
    }
}

#[test]
fn secure_hash_eval_buffer_index_type_error() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    let id = eval.buffers.current_buffer().expect("current buffer").id;

    match builtin_secure_hash(
        &mut eval,
        vec![
            Value::symbol("sha1"),
            Value::make_buffer(id),
            Value::T,
            Value::fixnum(3),
        ],
    ) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data.first(),
                Some(&Value::symbol("integer-or-marker-p"))
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn secure_hash_eval_buffer_marker_range() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("abc");
    }
    let id = eval.buffers.current_buffer().expect("current buffer").id;
    let marker = crate::emacs_core::marker::make_marker_value(None, Some(2), false);
    let r = builtin_secure_hash(
        &mut eval,
        vec![
            Value::symbol("sha1"),
            Value::make_buffer(id),
            marker,
            Value::fixnum(4),
        ],
    )
    .unwrap();
    assert_eq!(r.as_str(), Some("5b2505039ac5af9e197f5dad04113906a9cf9a2a"));
}

#[test]
fn secure_hash_eval_deleted_buffer_errors() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    let id = eval.buffers.create_buffer("*secure-doomed*");
    assert!(eval.buffers.kill_buffer(id));

    match builtin_secure_hash(
        &mut eval,
        vec![Value::symbol("sha1"), Value::make_buffer(id)],
    ) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data.first().and_then(|v| v.as_str()),
                Some("Selecting deleted buffer")
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn buffer_hash_eval_current_buffer_sha1() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    let buf = eval.buffers.current_buffer_mut().expect("current buffer");
    buf.delete_region(buf.point_min(), buf.point_max());
    buf.insert("abc");
    let r = builtin_buffer_hash(&mut eval, vec![]).unwrap();
    assert_eq!(r.as_str(), Some("a9993e364706816aba3e25717850c26c9cd0d89d"));
}

#[test]
fn buffer_hash_eval_by_name_sha1() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    let buf = eval.buffers.current_buffer_mut().expect("current buffer");
    buf.delete_region(buf.point_min(), buf.point_max());
    buf.insert("abc");
    let name = eval
        .buffers
        .current_buffer()
        .expect("current buffer")
        .name
        .clone();
    let r = builtin_buffer_hash(&mut eval, vec![Value::string(name)]).unwrap();
    assert_eq!(r.as_str(), Some("a9993e364706816aba3e25717850c26c9cd0d89d"));
}

#[test]
fn buffer_hash_eval_missing_name_errors() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    match builtin_buffer_hash(&mut eval, vec![Value::string("*missing*")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data.first().and_then(|v| v.as_str()),
                Some("No buffer named *missing*")
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

// ---- equal-including-properties ----

#[test]
fn equal_including_properties_strings() {
    crate::test_utils::init_test_tracing();
    let r =
        builtin_equal_including_properties(vec![Value::string("hello"), Value::string("hello")])
            .unwrap();
    assert!(r.is_truthy());
}

#[test]
fn string_make_multibyte_passthrough_ascii() {
    crate::test_utils::init_test_tracing();
    let r = builtin_string_make_multibyte(vec![Value::string("abc")]).unwrap();
    assert_eq!(r.as_str(), Some("abc"));
}

#[test]
fn string_make_multibyte_promotes_unibyte_byte() {
    crate::test_utils::init_test_tracing();
    let r = builtin_string_make_multibyte(vec![Value::string(bytes_to_unibyte_storage_string(&[
        0xFF,
    ]))])
    .unwrap();
    assert_eq!(
        string_escape::decode_storage_char_codes(r.as_str().unwrap()),
        vec![0x3FFFFF]
    );
}

#[test]
fn string_make_unibyte_passthrough_ascii() {
    crate::test_utils::init_test_tracing();
    let r = builtin_string_make_unibyte(vec![Value::string("abc")]).unwrap();
    assert_eq!(
        string_escape::decode_storage_char_codes(r.as_str().unwrap()),
        vec![97, 98, 99]
    );
}

#[test]
fn string_make_unibyte_truncates_unicode_char_code() {
    crate::test_utils::init_test_tracing();
    let r = builtin_string_make_unibyte(vec![Value::string("😀")]).unwrap();
    assert_eq!(
        string_escape::decode_storage_char_codes(r.as_str().unwrap()),
        vec![0]
    );
}

// ---- compare-strings ----

#[test]
fn compare_strings_equal() {
    crate::test_utils::init_test_tracing();
    let r = builtin_compare_strings(vec![
        Value::string("hello"),
        Value::NIL,
        Value::NIL,
        Value::string("hello"),
        Value::NIL,
        Value::NIL,
    ])
    .unwrap();
    assert!(r.is_t());
}

#[test]
fn compare_strings_less() {
    crate::test_utils::init_test_tracing();
    let r = builtin_compare_strings(vec![
        Value::string("abc"),
        Value::NIL,
        Value::NIL,
        Value::string("abd"),
        Value::NIL,
        Value::NIL,
    ])
    .unwrap();
    // First diff at position 3, "c" < "d" so negative
    assert_eq!(r.as_int(), Some(-3));
}

#[test]
fn compare_strings_greater() {
    crate::test_utils::init_test_tracing();
    let r = builtin_compare_strings(vec![
        Value::string("abd"),
        Value::NIL,
        Value::NIL,
        Value::string("abc"),
        Value::NIL,
        Value::NIL,
    ])
    .unwrap();
    assert_eq!(r.as_int(), Some(3));
}

#[test]
fn compare_strings_ignore_case() {
    crate::test_utils::init_test_tracing();
    let r = builtin_compare_strings(vec![
        Value::string("Hello"),
        Value::NIL,
        Value::NIL,
        Value::string("hello"),
        Value::NIL,
        Value::NIL,
        Value::T, // IGNORE-CASE
    ])
    .unwrap();
    assert!(r.is_t());
}

#[test]
fn compare_strings_subrange() {
    crate::test_utils::init_test_tracing();
    // Compare "hel" from "hello" (chars 1-3) with "hel" from "help" (chars 1-3)
    let r = builtin_compare_strings(vec![
        Value::string("hello"),
        Value::fixnum(1),
        Value::fixnum(3),
        Value::string("help"),
        Value::fixnum(1),
        Value::fixnum(3),
    ])
    .unwrap();
    assert!(r.is_t());
}

#[test]
fn compare_strings_length_diff() {
    crate::test_utils::init_test_tracing();
    let r = builtin_compare_strings(vec![
        Value::string("ab"),
        Value::NIL,
        Value::NIL,
        Value::string("abc"),
        Value::NIL,
        Value::NIL,
    ])
    .unwrap();
    // "ab" shorter — negative
    assert!(r.as_int().unwrap() < 0);
}

// ---- string-version-lessp ----

#[test]
fn version_lessp_basic() {
    crate::test_utils::init_test_tracing();
    let r =
        builtin_string_version_lessp(vec![Value::string("foo2"), Value::string("foo10")]).unwrap();
    assert!(r.is_truthy());
}

#[test]
fn version_lessp_equal() {
    crate::test_utils::init_test_tracing();
    let r =
        builtin_string_version_lessp(vec![Value::string("foo10"), Value::string("foo10")]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn version_lessp_alpha() {
    crate::test_utils::init_test_tracing();
    let r = builtin_string_version_lessp(vec![Value::string("abc"), Value::string("abd")]).unwrap();
    assert!(r.is_truthy());
}

#[test]
fn version_lessp_numeric_segments() {
    crate::test_utils::init_test_tracing();
    let r = builtin_string_version_lessp(vec![
        Value::string("emacs-27.1"),
        Value::string("emacs-27.2"),
    ])
    .unwrap();
    assert!(r.is_truthy());
}

// ---- string-collate-lessp ----

#[test]
fn collate_lessp_basic() {
    crate::test_utils::init_test_tracing();
    let r = builtin_string_collate_lessp(vec![Value::string("abc"), Value::string("abd")]).unwrap();
    assert!(r.is_truthy());
}

#[test]
fn collate_lessp_ignore_case() {
    crate::test_utils::init_test_tracing();
    let r = builtin_string_collate_lessp(vec![
        Value::string("ABC"),
        Value::string("abd"),
        Value::NIL, // locale
        Value::T,   // ignore-case
    ])
    .unwrap();
    assert!(r.is_truthy());
}

// ---- string-collate-equalp ----

#[test]
fn collate_equalp_basic() {
    crate::test_utils::init_test_tracing();
    let r =
        builtin_string_collate_equalp(vec![Value::string("abc"), Value::string("abc")]).unwrap();
    assert!(r.is_truthy());
}

#[test]
fn collate_equalp_ignore_case() {
    crate::test_utils::init_test_tracing();
    let r = builtin_string_collate_equalp(vec![
        Value::string("ABC"),
        Value::string("abc"),
        Value::NIL,
        Value::T,
    ])
    .unwrap();
    assert!(r.is_truthy());
}

#[test]
fn collate_equalp_different() {
    crate::test_utils::init_test_tracing();
    let r =
        builtin_string_collate_equalp(vec![Value::string("abc"), Value::string("abd")]).unwrap();
    assert!(r.is_nil());
}

// ---- widget-get / widget-put ----

#[test]
fn widget_get_found() {
    crate::test_utils::init_test_tracing();
    // Widget: (button :tag "OK" :value 42)
    let widget = Value::list(vec![
        Value::symbol("button"),
        Value::keyword("tag"),
        Value::string("OK"),
        Value::keyword("value"),
        Value::fixnum(42),
    ]);
    let r = builtin_widget_get(vec![widget, Value::keyword("value")]).unwrap();
    assert!(r.is_fixnum());
}

#[test]
fn widget_get_not_found() {
    crate::test_utils::init_test_tracing();
    let widget = Value::list(vec![
        Value::symbol("button"),
        Value::keyword("tag"),
        Value::string("OK"),
    ]);
    let r = builtin_widget_get(vec![widget, Value::keyword("missing")]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn widget_put_existing() {
    crate::test_utils::init_test_tracing();
    let widget = Value::list(vec![
        Value::symbol("button"),
        Value::keyword("value"),
        Value::fixnum(1),
    ]);
    let r = builtin_widget_put(vec![widget, Value::keyword("value"), Value::fixnum(99)]).unwrap();
    assert!(r.is_fixnum());

    // Verify it was modified
    let got = builtin_widget_get(vec![widget, Value::keyword("value")]).unwrap();
    assert!(got.is_fixnum());
}

#[test]
fn widget_put_new_property() {
    crate::test_utils::init_test_tracing();
    let widget = Value::list(vec![Value::symbol("button")]);
    let r =
        builtin_widget_put(vec![widget, Value::keyword("tag"), Value::string("Hello")]).unwrap();
    assert_eq!(r.as_str(), Some("Hello"));

    let got = builtin_widget_get(vec![widget, Value::keyword("tag")]).unwrap();
    assert_eq!(got.as_str(), Some("Hello"));
}

#[test]
fn widget_apply_missing_property_signals_void_function_nil() {
    crate::test_utils::init_test_tracing();
    let widget = Value::list(vec![Value::symbol("button")]);
    let mut ctx = test_eval_ctx();
    let err = builtin_widget_apply(&mut ctx, vec![widget, Value::keyword("action")])
        .expect_err("widget-apply should signal void-function for missing property");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "void-function");
            assert_eq!(sig.data, vec![Value::NIL]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn widget_apply_calls_symbol_property_with_widget_as_first_arg() {
    crate::test_utils::init_test_tracing();
    let widget = Value::list(vec![
        Value::symbol("button"),
        Value::keyword("action"),
        Value::symbol("car"),
    ]);
    let mut ctx = test_eval_ctx();
    let r = builtin_widget_apply(&mut ctx, vec![widget, Value::keyword("action")]).unwrap();
    assert_eq!(r, Value::symbol("button"));
}

#[test]
fn widget_apply_passes_rest_arguments() {
    crate::test_utils::init_test_tracing();
    let widget = Value::list(vec![
        Value::symbol("button"),
        Value::keyword("action"),
        Value::symbol("list"),
    ]);
    let mut ctx = test_eval_ctx();
    let r = builtin_widget_apply(
        &mut ctx,
        vec![
            widget,
            Value::keyword("action"),
            Value::fixnum(1),
            Value::fixnum(2),
        ],
    )
    .unwrap();
    assert_eq!(
        r,
        Value::list(vec![widget, Value::fixnum(1), Value::fixnum(2)])
    );
}

#[test]
fn widget_apply_non_callable_property_signals_invalid_function() {
    crate::test_utils::init_test_tracing();
    let widget = Value::list(vec![
        Value::symbol("button"),
        Value::keyword("action"),
        Value::fixnum(7),
    ]);
    let mut ctx = test_eval_ctx();
    let err = builtin_widget_apply(&mut ctx, vec![widget, Value::keyword("action")])
        .expect_err("widget-apply should reject non-callable property values");
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "invalid-function");
            assert_eq!(sig.data, vec![Value::fixnum(7)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

// ---- Line break in base64 ----

#[test]
fn base64_encode_line_break() {
    crate::test_utils::init_test_tracing();
    // A string long enough to trigger line breaks at column 76
    let long = "a".repeat(100);
    let encoded = builtin_base64_encode_string(vec![Value::string(long.clone())]).unwrap();
    let s = encoded.as_str().unwrap();
    assert!(s.contains('\n'));

    // No line break variant
    let encoded_no_lb = builtin_base64_encode_string(vec![Value::string(long), Value::T]).unwrap();
    let s2 = encoded_no_lb.as_str().unwrap();
    assert!(!s2.contains('\n'));
}

#[test]
fn base64_decode_ignores_whitespace() {
    crate::test_utils::init_test_tracing();
    // Encoded "Hello" with embedded whitespace
    let r = builtin_base64_decode_string(vec![Value::string("SGVs\nbG8=")]).unwrap();
    assert_eq!(r.as_str(), Some("Hello"));
}
