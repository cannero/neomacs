//! Advanced oracle parity tests for decode-char and encode-char primitives.
//!
//! Tests encoding characters to charset representations, decoding back,
//! roundtrip verification across charsets, boundary conditions, and
//! a complex charset-aware character classification engine.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// encode-char converting characters to charset representation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_decode_encode_char_encode_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Encode a variety of characters into multiple charsets and verify
    // the returned code points
    let form = r#"(let ((results nil))
  ;; ASCII charset: chars 0-127 should encode to themselves
  (dolist (ch (list 0 ?A ?Z ?a ?z ?0 ?9 32 127))
    (let ((encoded (encode-char ch 'ascii)))
      (setq results (cons (list :ascii ch encoded (and encoded (= ch encoded)))
                          results))))
  ;; Unicode charset: all valid codepoints should encode to themselves
  (dolist (ch (list ?A #xC0 #x03B1 #x4E2D #x1F600))
    (let ((encoded (encode-char ch 'unicode)))
      (setq results (cons (list :unicode ch encoded (and encoded (= ch encoded)))
                          results))))
  ;; ASCII charset rejects non-ASCII
  (dolist (ch (list #x80 #xFF #x100 #x4E2D #x1F600))
    (let ((encoded (encode-char ch 'ascii)))
      (setq results (cons (list :ascii-reject ch encoded (null encoded))
                          results))))
  (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// decode-char converting charset codes back to characters
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_decode_encode_char_decode_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Decode code points from various charsets back to characters
    let form = r#"(let ((results nil))
  ;; ASCII decode: codes 0-127 should decode to themselves
  (dolist (code (list 0 32 48 65 90 97 122 126 127))
    (let ((decoded (decode-char 'ascii code)))
      (setq results (cons (list :ascii-decode code decoded
                                (and decoded (= code decoded)))
                          results))))
  ;; ASCII decode: codes >= 128 should return nil
  (dolist (code (list 128 255 256 1000 -1))
    (let ((decoded (decode-char 'ascii code)))
      (setq results (cons (list :ascii-oob code decoded (null decoded))
                          results))))
  ;; Unicode decode
  (dolist (code (list 0 65 #xC0 #x03B1 #x4E2D #xFFFD #x10000 #x1F600 #x10FFFF))
    (let ((decoded (decode-char 'unicode code)))
      (setq results (cons (list :unicode-decode code decoded
                                (and decoded (= code decoded)))
                          results))))
  (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Roundtrip verification: encode then decode
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_decode_encode_char_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // For each character, encode into a charset then decode back,
    // verifying we get the original character
    let form = r#"(let ((results nil))
  ;; Roundtrip through ASCII
  (dolist (ch (list ?A ?Z ?a ?z ?0 ?9 32 0 127))
    (let* ((encoded (encode-char ch 'ascii))
           (decoded (and encoded (decode-char 'ascii encoded)))
           (ok (and decoded (= ch decoded))))
      (setq results (cons (list :rt-ascii ch ok) results))))
  ;; Roundtrip through Unicode for BMP characters
  (dolist (ch (list ?A #xC0 #xE9 #x03B1 #x0414 #x4E00 #xAC00 #xFFFD))
    (let* ((encoded (encode-char ch 'unicode))
           (decoded (and encoded (decode-char 'unicode encoded)))
           (ok (and decoded (= ch decoded))))
      (setq results (cons (list :rt-unicode-bmp ch ok) results))))
  ;; Roundtrip through Unicode for supplementary plane characters
  (dolist (ch (list #x10000 #x1F600 #x1F4A9 #x10FFFF))
    (let* ((encoded (encode-char ch 'unicode))
           (decoded (and encoded (decode-char 'unicode encoded)))
           (ok (and decoded (= ch decoded))))
      (setq results (cons (list :rt-unicode-sup ch ok) results))))
  ;; Roundtrip through iso-8859-1 for Latin-1 range
  (dolist (ch (list ?A #xC0 #xE9 #xFF))
    (let* ((encoded (encode-char ch 'iso-8859-1))
           (decoded (and encoded (decode-char 'iso-8859-1 encoded)))
           (ok (and decoded (= ch decoded))))
      (setq results (cons (list :rt-latin1 ch ok) results))))
  (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Testing with different charsets: ascii, unicode, iso-8859-1
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_decode_encode_char_cross_charset() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Compare encode-char results across charsets for the same characters
    let form = r#"(let ((test-chars (list ?A ?Z ?0 ?9 32 #xC0 #xE9 #xFF #x100 #x4E00 #x1F600))
      (charsets '(ascii unicode iso-8859-1))
      (results nil))
  (dolist (ch test-chars)
    (let ((encodings nil))
      (dolist (cs charsets)
        (let ((enc (encode-char ch cs)))
          (setq encodings (cons (cons cs enc) encodings))))
      (setq encodings (nreverse encodings))
      ;; For ASCII range: all three charsets should agree
      (let ((ascii-enc (cdr (assq 'ascii encodings)))
            (unicode-enc (cdr (assq 'unicode encodings)))
            (latin1-enc (cdr (assq 'iso-8859-1 encodings))))
        (setq results
              (cons (list
                     :ch ch
                     :ascii ascii-enc
                     :unicode unicode-enc
                     :latin1 latin1-enc
                     ;; ASCII and Unicode should agree for ASCII range
                     :ascii-unicode-agree
                     (if (and ascii-enc unicode-enc)
                         (= ascii-enc unicode-enc)
                       (and (null ascii-enc) (not (null unicode-enc))))
                     ;; Latin-1 and Unicode should agree for 0-255
                     :latin1-unicode-agree
                     (if (and latin1-enc unicode-enc)
                         (= latin1-enc unicode-enc)
                       (and (null latin1-enc) (not (null unicode-enc)))))
                    results)))))
  (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Boundary conditions and edge cases
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_decode_encode_char_boundaries() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Test at charset boundaries and special code points
    let form = r#"(list
  ;; ASCII boundary: 127 is in, 128 is out
  (encode-char 127 'ascii)
  (encode-char 128 'ascii)
  ;; iso-8859-1 boundary: 0xFF is in, 0x100 is out
  (encode-char #xFF 'iso-8859-1)
  (encode-char #x100 'iso-8859-1)
  ;; Unicode boundary: #x10FFFF is in
  (integerp (encode-char #x10FFFF 'unicode))
  ;; Zero is valid in all charsets
  (encode-char 0 'ascii)
  (encode-char 0 'unicode)
  (encode-char 0 'iso-8859-1)
  ;; decode-char with code 0 in all charsets
  (decode-char 'ascii 0)
  (decode-char 'unicode 0)
  (decode-char 'iso-8859-1 0)
  ;; Surrogates: D800-DFFF are not valid Unicode characters
  ;; encode-char should return nil for surrogate code points
  (encode-char #xD800 'ascii)
  (encode-char #xDFFF 'ascii)
  ;; char-charset for various boundary chars
  (char-charset 0)
  (char-charset 127)
  (char-charset 128)
  (char-charset #xFF)
  (char-charset #x100))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: charset-aware character classification engine
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_decode_encode_char_classification_engine() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a comprehensive character classifier that uses encode-char
    // to determine charset membership, then categorize characters
    let form = r#"(progn
  (defvar neovm--deca-charset-map nil)

  (fset 'neovm--deca-classify
    (lambda (ch)
      "Classify a character by its charset memberships and properties."
      (let ((in-ascii (not (null (encode-char ch 'ascii))))
            (in-unicode (not (null (encode-char ch 'unicode))))
            (in-latin1 (not (null (encode-char ch 'iso-8859-1))))
            (charset (char-charset ch)))
        (list
         :char ch
         :in-ascii in-ascii
         :in-unicode in-unicode
         :in-latin1 in-latin1
         :primary-charset charset
         ;; Derive a category
         :category
         (cond
          ((and in-ascii (<= ch 31)) 'control)
          ((and in-ascii (= ch 32)) 'space)
          ((and in-ascii (<= ?0 ch) (<= ch ?9)) 'digit)
          ((and in-ascii (<= ?A ch) (<= ch ?Z)) 'upper-alpha)
          ((and in-ascii (<= ?a ch) (<= ch ?z)) 'lower-alpha)
          ((and in-ascii (<= 33 ch) (<= ch 126)) 'punctuation)
          ((and in-latin1 (not in-ascii)) 'latin-extended)
          ((and in-unicode (not in-latin1)) 'unicode-beyond-latin)
          (t 'unknown))))))

  (fset 'neovm--deca-build-report
    (lambda (char-list)
      "Build a classification report for a list of characters."
      (let ((report nil)
            (category-counts (make-hash-table :test 'eq)))
        (dolist (ch char-list)
          (let* ((entry (funcall 'neovm--deca-classify ch))
                 (cat (plist-get entry :category))
                 (count (or (gethash cat category-counts) 0)))
            (puthash cat (1+ count) category-counts)
            (setq report (cons entry report))))
        ;; Convert hash table to sorted alist
        (let ((cat-alist nil))
          (maphash (lambda (k v) (setq cat-alist (cons (cons k v) cat-alist)))
                   category-counts)
          (setq cat-alist
                (sort cat-alist
                      (lambda (a b) (string< (symbol-name (car a))
                                              (symbol-name (car b))))))
          (list :entries (nreverse report)
                :category-counts cat-alist
                :total (length char-list))))))

  (unwind-protect
      (let* ((test-chars
              (list 0 10 32 ?0 ?5 ?9 ?A ?M ?Z ?a ?m ?z ?! ?~ #xC0 #xE9 #xFF
                    #x100 #x03B1 #x4E2D #x1F600))
             (report (funcall 'neovm--deca-build-report test-chars))
             (entries (plist-get report :entries))
             (counts (plist-get report :category-counts))
             ;; Verify invariants
             (all-unicode
              (let ((ok t))
                (dolist (e entries)
                  (unless (plist-get e :in-unicode)
                    (setq ok nil)))
                ok))
             (ascii-subset-latin1
              (let ((ok t))
                (dolist (e entries)
                  (when (and (plist-get e :in-ascii)
                             (not (plist-get e :in-latin1)))
                    (setq ok nil)))
                ok))
             (latin1-subset-unicode
              (let ((ok t))
                (dolist (e entries)
                  (when (and (plist-get e :in-latin1)
                             (not (plist-get e :in-unicode)))
                    (setq ok nil)))
                ok)))
        (list
         :total (plist-get report :total)
         :category-counts counts
         :all-in-unicode all-unicode
         :ascii-implies-latin1 ascii-subset-latin1
         :latin1-implies-unicode latin1-subset-unicode
         ;; Spot-check a few specific entries
         :char-A (plist-get (nth 6 entries) :category)
         :char-e-acute (plist-get (nth 16 entries) :category)
         :char-cjk (plist-get (nth 19 entries) :category)
         :char-emoji (plist-get (nth 20 entries) :category)))
    (fmakunbound 'neovm--deca-classify)
    (fmakunbound 'neovm--deca-build-report)
    (makunbound 'neovm--deca-charset-map)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: charset encode-char consistency matrix
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_decode_encode_char_consistency_matrix() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a matrix of (character x charset) encode results and verify
    // consistency properties
    let form = r#"(progn
  (fset 'neovm--deca-matrix
    (lambda (chars charsets)
      "Build an encode-char matrix: for each char, try each charset."
      (mapcar
       (lambda (ch)
         (cons ch
               (mapcar
                (lambda (cs)
                  (let ((enc (encode-char ch cs)))
                    (cons cs enc)))
                charsets)))
       chars)))

  (fset 'neovm--deca-analyze-matrix
    (lambda (matrix)
      "Analyze the encode matrix for consistency."
      (let ((total-cells 0)
            (nil-cells 0)
            (identity-cells 0))
        (dolist (row matrix)
          (let ((ch (car row)))
            (dolist (cell (cdr row))
              (setq total-cells (1+ total-cells))
              (let ((enc (cdr cell)))
                (if (null enc)
                    (setq nil-cells (1+ nil-cells))
                  (when (= ch enc)
                    (setq identity-cells (1+ identity-cells))))))))
        (list :total total-cells
              :nil-count nil-cells
              :identity-count identity-cells
              :coverage-pct (if (> total-cells 0)
                                (/ (* 100 (- total-cells nil-cells)) total-cells)
                              0)))))

  (unwind-protect
      (let* ((chars (list ?A ?z ?0 #x80 #xC0 #xFF #x100 #x03B1 #x4E2D #x1F600))
             (charsets '(ascii unicode iso-8859-1))
             (matrix (funcall 'neovm--deca-matrix chars charsets))
             (analysis (funcall 'neovm--deca-analyze-matrix matrix)))
        (list
         :matrix matrix
         :analysis analysis
         ;; Verify: unicode column should have zero nils for valid chars
         :unicode-complete
         (let ((ok t))
           (dolist (row matrix)
             (unless (cdr (assq 'unicode (cdr row)))
               (setq ok nil)))
           ok)))
    (fmakunbound 'neovm--deca-matrix)
    (fmakunbound 'neovm--deca-analyze-matrix)))"#;
    assert_oracle_parity_with_bootstrap(form);
}
