//! Advanced oracle parity tests for charset and character set primitives.
//!
//! Tests charsetp, decode-char/encode-char roundtrips, char-charset
//! across character ranges, max-char, characterp vs integerp,
//! Unicode block classification, and multibyte string analysis.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// charsetp on various charset names (valid and invalid)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_charset_advanced_charsetp_various() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  (charsetp 'ascii)
  (charsetp 'unicode)
  (charsetp 'unicode-bmp)
  (charsetp 'emacs)
  (charsetp 'eight-bit)
  (charsetp 'iso-8859-1)
  (charsetp 'nonexistent-charset-xyz-999)
  (charsetp nil)
  (charsetp 42)
  (charsetp "ascii")
  ;; Verify the type returned by charsetp
  (eq (charsetp 'ascii) t)
  (eq (charsetp 'nonexistent-charset-xyz-999) nil))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// decode-char / encode-char roundtrip for ASCII and Unicode
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_charset_advanced_encode_decode_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Encode ASCII chars into UCS and decode back; verify full roundtrip
    let form = r#"(let ((results nil))
  ;; Test a range of ASCII characters
  (dolist (ch '(0 32 48 65 90 97 122 126 127))
    (let* ((encoded (encode-char ch 'unicode))
           (decoded (decode-char 'unicode encoded)))
      (setq results (cons (list ch encoded decoded (= ch decoded)) results))))
  ;; Test some high Unicode codepoints
  (dolist (cp '(#x00E9 #x03B1 #x4E2D #x1F600 #x1F4A9))
    (let* ((ch cp)
           (encoded (encode-char ch 'unicode))
           (decoded (decode-char 'unicode encoded)))
      (setq results (cons (list ch encoded decoded (= ch decoded)) results))))
  (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// char-charset for different character ranges
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_charset_advanced_char_charset_ranges() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; ASCII range
  (char-charset 0)
  (char-charset 32)
  (char-charset ?A)
  (char-charset ?z)
  (char-charset 127)
  ;; Latin-1 supplement
  (char-charset #x80)
  (char-charset #xC0)
  (char-charset #xFF)
  ;; BMP range
  (char-charset #x100)
  (char-charset #x0391)    ;; Greek Alpha
  (char-charset #x4E00)    ;; CJK Unified Ideograph
  (char-charset #xFFFD)    ;; Replacement Character
  ;; Supplementary planes
  (char-charset #x10000)   ;; Linear B Syllable
  (char-charset #x1F600)   ;; Emoji grinning face
  (char-charset #x1F4A9)   ;; Pile of poo
  ;; Emoji in supplementary plane vs BMP
  (eq (char-charset #x1F600) 'unicode)
  (eq (char-charset ?A) 'ascii))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// max-char value verification
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_charset_advanced_max_char() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; max-char should be a large integer
  (integerp (max-char))
  ;; It should be greater than the highest Unicode codepoint
  (> (max-char) #x10FFFF)
  ;; Characters at boundaries
  (characterp 0)
  (characterp (max-char))
  ;; max-char with unicode restriction
  (integerp (max-char t))
  (= (max-char t) #x10FFFF)
  ;; Verify max-char > max-char(unicode-only)
  (> (max-char) (max-char t))
  ;; encode-char at boundary values
  (encode-char 0 'unicode)
  (encode-char (max-char t) 'unicode))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// characterp vs integerp relationship
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_charset_advanced_characterp_integerp() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // In Emacs, characters ARE integers but integerp is not characterp.
    let form = r#"(list
  ;; Characters satisfy both predicates
  (characterp ?A)
  (integerp ?A)
  (characterp 65)
  (integerp 65)
  ;; Large integers are not characters
  (characterp (1+ (max-char)))
  (integerp (1+ (max-char)))
  ;; Negative integers are not characters
  (characterp -1)
  (integerp -1)
  ;; Zero is both
  (characterp 0)
  (integerp 0)
  ;; Floats are neither
  (characterp 65.0)
  (integerp 65.0)
  ;; Strings and symbols
  (characterp "A")
  (characterp 'A)
  ;; wholenump vs characterp
  (natnump 0)
  (characterp 0)
  (natnump (max-char))
  (characterp (max-char))
  (natnump (1+ (max-char)))
  (characterp (1+ (max-char))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: Unicode block classification
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_charset_advanced_unicode_block_classification() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a classifier that identifies Unicode blocks by codepoint range
    let form = r#"(progn
  (fset 'neovm--ca-classify-char
    (lambda (ch)
      "Classify a character into a broad Unicode category."
      (cond
        ((and (>= ch 0) (<= ch 127)) 'basic-ascii)
        ((and (>= ch #x80) (<= ch #xFF)) 'latin-supplement)
        ((and (>= ch #x0370) (<= ch #x03FF)) 'greek)
        ((and (>= ch #x0400) (<= ch #x04FF)) 'cyrillic)
        ((and (>= ch #x0590) (<= ch #x05FF)) 'hebrew)
        ((and (>= ch #x0600) (<= ch #x06FF)) 'arabic)
        ((and (>= ch #x3040) (<= ch #x309F)) 'hiragana)
        ((and (>= ch #x30A0) (<= ch #x30FF)) 'katakana)
        ((and (>= ch #x4E00) (<= ch #x9FFF)) 'cjk-unified)
        ((and (>= ch #xAC00) (<= ch #xD7AF)) 'hangul)
        ((and (>= ch #x1F600) (<= ch #x1F64F)) 'emoticons)
        ((and (>= ch #x1F300) (<= ch #x1F5FF)) 'misc-symbols)
        (t 'other))))

  (unwind-protect
      (let ((test-chars
             (list ?A ?z #xC0 #xE9 #x03B1 #x0414 #x05D0 #x0627
                   #x3042 #x30AB #x4E2D #xAC00 #x1F600 #x1F319)))
        ;; Classify each character and verify charset assignment
        (mapcar (lambda (ch)
                  (list ch
                        (funcall 'neovm--ca-classify-char ch)
                        (char-charset ch)
                        (encode-char ch 'unicode)))
                test-chars))
    (fmakunbound 'neovm--ca-classify-char)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: multibyte string analysis (byte count vs char count)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_charset_advanced_multibyte_string_analysis() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Analyze strings of various compositions: pure ASCII, mixed, all multibyte
    let form = r#"(progn
  (fset 'neovm--ca-analyze-string
    (lambda (s)
      "Analyze a string: char count, byte length, char-charset per char, multibyte-p."
      (let ((char-count (length s))
            (byte-len (string-bytes s))
            (charsets (make-hash-table))
            (max-ch 0)
            (min-ch (max-char)))
        (dotimes (i char-count)
          (let ((ch (aref s i)))
            (puthash (char-charset ch) t charsets)
            (when (> ch max-ch) (setq max-ch ch))
            (when (< ch min-ch) (setq min-ch ch))))
        (let ((charset-list nil))
          (maphash (lambda (k _v) (setq charset-list (cons k charset-list))) charsets)
          (list char-count byte-len
                (sort charset-list (lambda (a b) (string< (symbol-name a) (symbol-name b))))
                (multibyte-string-p s)
                min-ch max-ch
                ;; ratio of bytes per char (as integer percentage)
                (if (> char-count 0)
                    (/ (* 100 byte-len) char-count)
                  0))))))

  (unwind-protect
      (list
        ;; Pure ASCII
        (funcall 'neovm--ca-analyze-string "hello world")
        ;; Latin-1 supplement
        (funcall 'neovm--ca-analyze-string "caf\u00E9")
        ;; Greek
        (funcall 'neovm--ca-analyze-string "\u03B1\u03B2\u03B3")
        ;; CJK
        (funcall 'neovm--ca-analyze-string "\u4E2D\u6587\u5B57")
        ;; Emoji (4-byte UTF-8)
        (funcall 'neovm--ca-analyze-string "\U0001F600\U0001F601\U0001F602")
        ;; Mixed: ASCII + Latin + CJK + Emoji
        (funcall 'neovm--ca-analyze-string "A\u00E9\u4E2D\U0001F600")
        ;; Empty string
        (funcall 'neovm--ca-analyze-string "")
        ;; Single character
        (funcall 'neovm--ca-analyze-string "X"))
    (fmakunbound 'neovm--ca-analyze-string)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: charset membership with encode-char nil return
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_charset_advanced_encode_char_membership() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // encode-char returns nil when a character is not in the specified charset
    let form = r#"(list
  ;; ASCII chars are in ascii charset
  (encode-char ?A 'ascii)
  (encode-char ?z 'ascii)
  (encode-char 0 'ascii)
  (encode-char 127 'ascii)
  ;; Non-ASCII chars are NOT in ascii charset
  (encode-char #x80 'ascii)
  (encode-char #x100 'ascii)
  (encode-char #x4E00 'ascii)
  ;; All Unicode chars should be in unicode charset
  (integerp (encode-char ?A 'unicode))
  (integerp (encode-char #x4E00 'unicode))
  (integerp (encode-char #x1F600 'unicode))
  ;; decode-char with invalid code returns nil
  (decode-char 'ascii 128)
  (decode-char 'ascii 256)
  (decode-char 'ascii -1)
  ;; Verify bidirectional consistency
  (let ((ch ?A))
    (= ch (decode-char 'ascii (encode-char ch 'ascii))))
  (let ((ch #x1F600))
    (= ch (decode-char 'unicode (encode-char ch 'unicode)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}
