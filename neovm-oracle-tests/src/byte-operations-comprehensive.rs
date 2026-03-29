//! Oracle parity tests for byte/unibyte string operations:
//! `unibyte-string`, `string-to-unibyte`, `string-to-multibyte`,
//! `string-as-unibyte`, `string-as-multibyte`, `string-bytes`,
//! `multibyte-string-p`, `unibyte-char-to-multibyte`,
//! `multibyte-char-to-unibyte`, byte-level substring operations,
//! encoding/decoding edge cases.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// unibyte-string: construct unibyte strings from byte values
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_byte_ops_unibyte_string_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Basic ASCII bytes
  (unibyte-string 72 101 108 108 111)
  ;; Single byte
  (unibyte-string 65)
  ;; Empty (zero args)
  (unibyte-string)
  ;; High bytes (128-255 range, raw bytes in unibyte)
  (unibyte-string 128 200 255)
  ;; All printable ASCII
  (unibyte-string 32 33 126 127)
  ;; Null byte
  (unibyte-string 0)
  ;; Mixed low and high bytes
  (unibyte-string 0 65 128 255 10 13)
  ;; Verify type is unibyte
  (multibyte-string-p (unibyte-string 72 101 108))
  ;; Verify length
  (length (unibyte-string 1 2 3 4 5))
  ;; string-bytes on unibyte string
  (string-bytes (unibyte-string 72 101 108 108 111)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// multibyte-string-p: predicate for multibyte strings
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_byte_ops_multibyte_string_p() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Regular string literal is multibyte
  (multibyte-string-p "hello")
  ;; Empty string
  (multibyte-string-p "")
  ;; String with non-ASCII multibyte chars
  (multibyte-string-p "\u00e9")
  ;; Unibyte string via unibyte-string
  (multibyte-string-p (unibyte-string 65 66 67))
  ;; String with only ASCII is still multibyte if created as multibyte
  (multibyte-string-p (string ?a ?b ?c))
  ;; make-string with multibyte char
  (multibyte-string-p (make-string 3 ?a))
  ;; concat of multibyte strings
  (multibyte-string-p (concat "abc" "def"))
  ;; Non-string arguments
  (condition-case err (multibyte-string-p 42)
    (wrong-type-argument (list 'error (car err))))
  (condition-case err (multibyte-string-p nil)
    (wrong-type-argument (list 'error (car err)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// string-bytes: byte length vs character length
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_byte_ops_string_bytes() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; ASCII: bytes = chars
  (string-bytes "hello")
  (length "hello")
  (= (string-bytes "hello") (length "hello"))
  ;; Empty string
  (string-bytes "")
  ;; Multibyte characters: bytes > chars
  (string-bytes "\u00e9")
  (length "\u00e9")
  ;; CJK character (3 bytes in UTF-8)
  (string-bytes "\u4e16")
  (length "\u4e16")
  ;; Mixed ASCII and multibyte
  (let ((s "a\u00e9b"))
    (list (string-bytes s) (length s)))
  ;; Unibyte string: bytes = chars always
  (let ((s (unibyte-string 128 200 255)))
    (list (string-bytes s) (length s)))
  ;; Emoji-like high codepoint (4 bytes in UTF-8)
  (string-bytes "\U0001f600")
  (length "\U0001f600"))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// string-to-multibyte / string-to-unibyte conversions
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_byte_ops_string_to_multibyte() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Convert ASCII unibyte to multibyte (identity for ASCII range)
  (let ((s (unibyte-string 72 101 108 108 111)))
    (let ((m (string-to-multibyte s)))
      (list (multibyte-string-p s)
            (multibyte-string-p m)
            (string= s m)
            (length m)
            (string-bytes m))))
  ;; Already multibyte: identity
  (let ((m (string-to-multibyte "hello")))
    (list (multibyte-string-p m) (string= m "hello")))
  ;; Empty string
  (string-to-multibyte "")
  (string-to-multibyte (unibyte-string))
  ;; Unibyte with raw bytes 128-159 -> multibyte raw bytes
  (let* ((s (unibyte-string 128))
         (m (string-to-multibyte s)))
    (list (length s) (length m)
          (string-bytes s) (string-bytes m)))
  ;; Unibyte high bytes 160-255 -> latin-1 multibyte
  (let* ((s (unibyte-string 192 224 255))
         (m (string-to-multibyte s)))
    (list (length s) (length m)
          (multibyte-string-p m))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// string-as-unibyte / string-as-multibyte (raw reinterpretation)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_byte_ops_string_as_unibyte() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; ASCII string: as-unibyte just changes the flag
  (let* ((s "hello")
         (u (string-as-unibyte s)))
    (list (multibyte-string-p s)
          (multibyte-string-p u)
          (string= s u)
          (length u)
          (string-bytes u)))
  ;; Already unibyte: identity
  (let* ((s (unibyte-string 65 66 67))
         (u (string-as-unibyte s)))
    (list (multibyte-string-p u)
          (length u)))
  ;; Empty string
  (string-as-unibyte "")
  ;; Round-trip: as-unibyte then as-multibyte on ASCII
  (let* ((s "abc")
         (u (string-as-unibyte s))
         (m (string-as-multibyte u)))
    (list (string= s m)
          (multibyte-string-p m))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// unibyte-char-to-multibyte / multibyte-char-to-unibyte
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_byte_ops_char_conversions() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; ASCII chars: identity mapping
  (unibyte-char-to-multibyte 65)
  (= (unibyte-char-to-multibyte 65) 65)
  (multibyte-char-to-unibyte 65)
  (= (multibyte-char-to-unibyte 65) 65)
  ;; Boundary: char 127
  (unibyte-char-to-multibyte 127)
  (multibyte-char-to-unibyte 127)
  ;; Char 0
  (unibyte-char-to-multibyte 0)
  (multibyte-char-to-unibyte 0)
  ;; High unibyte values -> multibyte raw bytes
  (let ((results nil))
    (dolist (b '(128 160 200 255))
      (setq results (cons (unibyte-char-to-multibyte b) results)))
    (nreverse results))
  ;; Round-trip: unibyte->multibyte->unibyte for ASCII
  (let ((results nil))
    (dolist (b '(0 32 65 97 126 127))
      (setq results
            (cons (= b (multibyte-char-to-unibyte
                        (unibyte-char-to-multibyte b)))
                  results)))
    (nreverse results)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Byte-level substring operations on unibyte strings
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_byte_ops_unibyte_substring() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Substring of unibyte string
  (let ((s (unibyte-string 65 66 67 68 69)))
    (list
      (substring s 0 3)
      (substring s 2)
      (substring s 1 4)
      (multibyte-string-p (substring s 0 3))
      (length (substring s 0 3))))
  ;; aref on unibyte string returns byte values
  (let ((s (unibyte-string 0 128 255)))
    (list (aref s 0) (aref s 1) (aref s 2)))
  ;; concat unibyte strings stays unibyte
  (let* ((a (unibyte-string 65 66))
         (b (unibyte-string 67 68))
         (c (concat a b)))
    (list (multibyte-string-p c)
          (length c)
          c))
  ;; Copying unibyte string
  (let* ((s (unibyte-string 72 73))
         (c (copy-sequence s)))
    (list (string= s c)
          (multibyte-string-p c)
          (eq s c))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Encoding edge cases: mixed byte operations
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_byte_ops_encoding_edge_cases() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; string-to-unibyte on pure ASCII multibyte -> unibyte
  (let* ((m "hello")
         (u (string-to-unibyte m)))
    (list (multibyte-string-p u)
          (string= m u)
          (length u)))
  ;; string-to-unibyte on string with non-ASCII -> error
  (condition-case err
      (string-to-unibyte "\u00e9")
    (error (list 'error (car err))))
  ;; make-string with unibyte flag
  (let ((s (make-string 5 ?x)))
    (list (multibyte-string-p s) (length s) (string-bytes s)))
  ;; string function creates multibyte
  (let ((s (string ?a ?b ?c)))
    (list (multibyte-string-p s) (length s)))
  ;; Comparison between unibyte and multibyte ASCII
  (let ((u (unibyte-string 65 66 67))
        (m "ABC"))
    (list (string= u m)
          (equal u m))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Comprehensive byte/char boundary analysis
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_byte_ops_boundary_analysis() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (fset 'neovm--byte-analyze
    (lambda (s)
      "Analyze byte/char properties of a string."
      (list
        (length s)
        (string-bytes s)
        (multibyte-string-p s)
        ;; Collect all char values
        (let ((chars nil) (i 0))
          (while (< i (length s))
            (setq chars (cons (aref s i) chars))
            (setq i (1+ i)))
          (nreverse chars)))))

  (unwind-protect
      (list
        ;; Pure ASCII
        (funcall 'neovm--byte-analyze "ABC")
        ;; Empty
        (funcall 'neovm--byte-analyze "")
        ;; Single multibyte char
        (funcall 'neovm--byte-analyze "\u00e9")
        ;; Unibyte bytes
        (funcall 'neovm--byte-analyze (unibyte-string 65 128 255))
        ;; Mixed ASCII + 2-byte + 3-byte chars
        (funcall 'neovm--byte-analyze "a\u00e9\u4e16")
        ;; Tab and newline
        (funcall 'neovm--byte-analyze "\t\n")
        ;; Long unibyte string
        (let ((s (make-string 100 ?x)))
          (list (string-bytes s) (length s) (= (string-bytes s) (length s)))))
    (fmakunbound 'neovm--byte-analyze)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Comprehensive pipeline: build, transform, and inspect byte strings
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_byte_ops_pipeline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (fset 'neovm--byte-xor-encrypt
    (lambda (data key)
      "XOR each byte of unibyte DATA with KEY byte. Returns unibyte string."
      (let ((result nil) (i 0) (len (length data)))
        (while (< i len)
          (setq result (cons (logand (logxor (aref data i) key) 255) result))
          (setq i (1+ i)))
        (apply 'unibyte-string (nreverse result)))))

  (fset 'neovm--byte-checksum
    (lambda (data)
      "Simple additive checksum mod 256 of unibyte string."
      (let ((sum 0) (i 0) (len (length data)))
        (while (< i len)
          (setq sum (logand (+ sum (aref data i)) 255))
          (setq i (1+ i)))
        sum)))

  (unwind-protect
      (let* ((plaintext (unibyte-string 72 101 108 108 111 32 87 111 114 108 100))
             (key 42)
             (encrypted (funcall 'neovm--byte-xor-encrypt plaintext key))
             (decrypted (funcall 'neovm--byte-xor-encrypt encrypted key)))
        (list
          ;; Round-trip: decrypt(encrypt(x)) = x
          (string= plaintext decrypted)
          ;; Encrypted differs from plaintext
          (not (string= plaintext encrypted))
          ;; Both are unibyte
          (multibyte-string-p plaintext)
          (multibyte-string-p encrypted)
          ;; Lengths match
          (= (length plaintext) (length encrypted))
          ;; Checksum of plaintext
          (funcall 'neovm--byte-checksum plaintext)
          ;; Checksum of empty
          (funcall 'neovm--byte-checksum (unibyte-string))
          ;; Checksum wraps at 256
          (funcall 'neovm--byte-checksum (unibyte-string 200 200))
          ;; Double encryption = identity (XOR property)
          (string= plaintext
                   (funcall 'neovm--byte-xor-encrypt
                            (funcall 'neovm--byte-xor-encrypt plaintext key)
                            key))))
    (fmakunbound 'neovm--byte-xor-encrypt)
    (fmakunbound 'neovm--byte-checksum)))"#;
    assert_oracle_parity_with_bootstrap(form);
}
