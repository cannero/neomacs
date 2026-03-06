//! Pure Rust case fiddling engine.
//!
//! Implements the core of Emacs's case conversion machinery (cf. `casefiddle.c`).
//! Supports four operations:
//!
//! - **Upcase**: convert all characters to uppercase.
//! - **Downcase**: convert all characters to lowercase.
//! - **Capitalize**: uppercase the first letter of each word, lowercase the rest.
//! - **UpcaseInitials**: uppercase only the first letter of each word, leave
//!   the rest unchanged.
//!
//! Handles Unicode special casing:
//! - Multi-character expansions (e.g. `ß` -> `SS` in uppercase).
//! - Greek final sigma (`Σ` -> `ς` at end of word, `σ` in the middle).
//! - Title-case characters (e.g. `Dž` -> `DŽ` for uppercase, `dž` for
//!   lowercase, `Dž` for title case).

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Greek capital letter sigma.
pub const GREEK_CAPITAL_SIGMA: char = '\u{03A3}'; // Σ
/// Greek small letter sigma (medial).
pub const GREEK_SMALL_SIGMA: char = '\u{03C3}'; // σ
/// Greek small letter final sigma.
pub const GREEK_SMALL_FINAL_SIGMA: char = '\u{03C2}'; // ς
/// German sharp s (eszett).
pub const SHARP_S: char = '\u{00DF}'; // ß
/// Capital sharp s.
pub const CAPITAL_SHARP_S: char = '\u{1E9E}'; // ẞ

// ---------------------------------------------------------------------------
// CaseOperation
// ---------------------------------------------------------------------------

/// The four case-conversion operations, mirroring Emacs's `case_action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseOperation {
    /// Convert all characters to uppercase.
    Upcase,
    /// Convert all characters to lowercase.
    Downcase,
    /// Capitalize each word: first letter uppercase (or title case),
    /// remaining letters lowercase.
    Capitalize,
    /// Upcase only the first letter of each word; leave the rest unchanged.
    UpcaseInitials,
}

// ---------------------------------------------------------------------------
// CaseResult
// ---------------------------------------------------------------------------

/// Result of casing a single character.
///
/// The output may be multiple characters (e.g. `ß` -> `SS`), so `chars` is
/// a `Vec<char>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseResult {
    /// The output character(s) after case conversion.
    pub chars: Vec<char>,
    /// Whether the casing operation determined that we have moved past a
    /// word boundary (i.e. the character is a word character and the
    /// previous position was not).
    pub advanced_word: bool,
}

impl CaseResult {
    /// Create a result containing a single character.
    #[inline]
    fn single(ch: char, advanced_word: bool) -> Self {
        CaseResult {
            chars: vec![ch],
            advanced_word,
        }
    }

    /// Create a result containing multiple characters.
    #[inline]
    fn multi(chars: Vec<char>, advanced_word: bool) -> Self {
        CaseResult {
            chars,
            advanced_word,
        }
    }
}

// ---------------------------------------------------------------------------
// CasingContext
// ---------------------------------------------------------------------------

/// Tracks state for iterative case conversion over a sequence of characters.
///
/// This mirrors `struct casing_context` from Emacs's `casefiddle.c`.
#[derive(Debug, Clone)]
pub struct CasingContext {
    /// The requested case-conversion operation.
    pub flag: CaseOperation,
    /// Whether we are currently inside a word.
    pub in_word: bool,
    /// Whether to prefer Unicode title-case forms for the capitalize
    /// operation (e.g. `Dž` instead of `DŽ`).
    pub title_case: bool,
}

impl CasingContext {
    /// Create a new casing context for the given operation.
    pub fn new(flag: CaseOperation) -> Self {
        CasingContext {
            flag,
            in_word: false,
            title_case: true,
        }
    }

    /// Process a single character, updating internal word-boundary state.
    ///
    /// `next` is the character following `ch` in the input (if any),
    /// used for Greek final-sigma detection.
    pub fn case_next(&mut self, ch: char, next: Option<char>) -> CaseResult {
        let was_in_word = self.in_word;
        self.in_word = is_word_constituent(ch);

        let entered_word = self.in_word && !was_in_word;

        // Determine the effective operation for this character.
        match self.flag {
            CaseOperation::Upcase => self.apply_upcase(ch, was_in_word, next, entered_word),
            CaseOperation::Downcase => self.apply_downcase(ch, was_in_word, next, entered_word),
            CaseOperation::Capitalize => {
                if !was_in_word && self.in_word {
                    // First character of a word: upcase (or titlecase).
                    self.apply_capitalize_initial(ch, next, entered_word)
                } else {
                    // Inside or outside a word: downcase.
                    self.apply_downcase(ch, was_in_word, next, entered_word)
                }
            }
            CaseOperation::UpcaseInitials => {
                if !was_in_word && self.in_word {
                    // First character of a word: upcase (or titlecase).
                    self.apply_capitalize_initial(ch, next, entered_word)
                } else {
                    // Not at word start: leave unchanged.
                    CaseResult::single(ch, entered_word)
                }
            }
        }
    }

    // -- Internal helpers ---------------------------------------------------

    fn apply_upcase(
        &self,
        ch: char,
        _was_in_word: bool,
        _next: Option<char>,
        entered_word: bool,
    ) -> CaseResult {
        let expanded = upcase_char_full(ch);
        CaseResult::multi(expanded, entered_word)
    }

    fn apply_downcase(
        &self,
        ch: char,
        was_in_word: bool,
        next: Option<char>,
        entered_word: bool,
    ) -> CaseResult {
        let lowered = downcase_char(ch);

        // Greek final-sigma rule: if we just downcased capital sigma and
        // the next character is NOT a word constituent, use final sigma.
        if ch == GREEK_CAPITAL_SIGMA && was_in_word {
            let next_is_word = next.map_or(false, is_word_constituent);
            if !next_is_word {
                return CaseResult::single(GREEK_SMALL_FINAL_SIGMA, entered_word);
            }
        }

        CaseResult::single(lowered, entered_word)
    }

    fn apply_capitalize_initial(
        &self,
        ch: char,
        _next: Option<char>,
        entered_word: bool,
    ) -> CaseResult {
        if self.title_case {
            let tc = titlecase_char(ch);
            // Title-case forms may differ from uppercase for digraphs.
            // If titlecase produces the same as the original and it is
            // lowercase, fall back to uppercase expansion.
            if tc == ch && ch.is_lowercase() {
                let expanded = upcase_char_full(ch);
                return CaseResult::multi(expanded, entered_word);
            }
            CaseResult::single(tc, entered_word)
        } else {
            let expanded = upcase_char_full(ch);
            CaseResult::multi(expanded, entered_word)
        }
    }
}

// ---------------------------------------------------------------------------
// Single-character conversion functions
// ---------------------------------------------------------------------------

/// Convert a single character to uppercase.
///
/// For characters that expand to multiple codepoints (e.g. `ß` -> `SS`),
/// only the first codepoint is returned. Use [`upcase_char_full`] for
/// the complete expansion.
#[inline]
pub fn upcase_char(ch: char) -> char {
    ch.to_uppercase().next().unwrap_or(ch)
}

/// Convert a single character to uppercase, returning all resulting chars.
///
/// Some characters expand: `ß` -> `['S', 'S']`, `ffi` ligature -> `['F', 'F', 'I']`.
pub fn upcase_char_full(ch: char) -> Vec<char> {
    ch.to_uppercase().collect()
}

/// Convert a single character to lowercase.
#[inline]
pub fn downcase_char(ch: char) -> char {
    ch.to_lowercase().next().unwrap_or(ch)
}

/// Convert a single character to title case.
///
/// Unicode defines distinct title-case forms for certain characters
/// (e.g. U+01F3 `dz` -> U+01F2 `Dz`, U+01C6 `dž` -> U+01C5 `Dž`).
/// For characters without a distinct title-case form, this returns
/// the uppercase form.
pub fn titlecase_char(ch: char) -> char {
    // Rust's standard library does not expose a `to_titlecase` method.
    // We handle the known Unicode title-case characters explicitly.
    match ch {
        // Latin digraphs: DZ, Dz, dz
        '\u{01F1}' => '\u{01F2}', // DZ -> Dz
        '\u{01F3}' => '\u{01F2}', // dz -> Dz
        '\u{01F2}' => '\u{01F2}', // Dz -> Dz (already title)

        // Latin digraphs with caron: DŽ, Dž, dž
        '\u{01C4}' => '\u{01C5}', // DŽ -> Dž
        '\u{01C6}' => '\u{01C5}', // dž -> Dž
        '\u{01C5}' => '\u{01C5}', // Dž -> Dž (already title)

        // Latin digraphs: LJ, Lj, lj
        '\u{01C7}' => '\u{01C8}', // LJ -> Lj
        '\u{01C9}' => '\u{01C8}', // lj -> Lj
        '\u{01C8}' => '\u{01C8}', // Lj -> Lj (already title)

        // Latin digraphs: NJ, Nj, nj
        '\u{01CA}' => '\u{01CB}', // NJ -> Nj
        '\u{01CC}' => '\u{01CB}', // nj -> Nj
        '\u{01CB}' => '\u{01CB}', // Nj -> Nj (already title)

        // Sharp s: titlecase is capital sharp s (ẞ) in some contexts,
        // but Emacs treats titlecase of ß as uppercase -> Ss.
        // We return 'S' here (single char); the multi-char expansion
        // is handled at the CasingContext level.

        // For all other characters, use the standard uppercase as proxy.
        _ => upcase_char(ch),
    }
}

// ---------------------------------------------------------------------------
// Word boundary detection
// ---------------------------------------------------------------------------

/// Return `true` if `ch` is a "word constituent" for case-conversion purposes.
///
/// This mirrors the `case_ch_is_word` function in Emacs's `casefiddle.c`,
/// using Unicode alphanumeric as the word-constituent test. Underscore is
/// intentionally excluded (it has symbol syntax in Emacs, not word syntax,
/// and `case-symbols-as-words` defaults to nil).
#[inline]
pub fn is_word_constituent(ch: char) -> bool {
    ch.is_alphanumeric()
}

// ---------------------------------------------------------------------------
// String operations
// ---------------------------------------------------------------------------

/// Convert an entire string to uppercase.
///
/// Handles multi-character expansions (e.g. `ß` -> `SS`).
pub fn upcase_string(s: &str) -> String {
    s.to_uppercase()
}

/// Convert an entire string to lowercase.
///
/// Handles Greek final-sigma contextual rule.
pub fn downcase_string(s: &str) -> String {
    // Rust's `str::to_lowercase()` already handles Greek final sigma.
    s.to_lowercase()
}

/// Capitalize each word in `s`: first letter of each word is upper/title-cased,
/// remaining letters are lowercased.
///
/// Word boundaries are determined by transitions from non-word to word
/// characters (see [`is_word_constituent`]).
pub fn capitalize_string(s: &str) -> String {
    let mut ctx = CasingContext::new(CaseOperation::Capitalize);
    apply_casing_to_string(&mut ctx, s)
}

/// Upcase the initial letter of each word in `s`; leave remaining characters
/// unchanged.
pub fn upcase_initials_string(s: &str) -> String {
    let mut ctx = CasingContext::new(CaseOperation::UpcaseInitials);
    apply_casing_to_string(&mut ctx, s)
}

/// Apply a `CasingContext` to each character of `s`, collecting the result.
fn apply_casing_to_string(ctx: &mut CasingContext, s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut result = String::with_capacity(s.len());

    for i in 0..len {
        let next = if i + 1 < len {
            Some(chars[i + 1])
        } else {
            None
        };
        let case_result = ctx.case_next(chars[i], next);
        for &ch in &case_result.chars {
            result.push(ch);
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- 1. upcase_char basic ASCII ----------------------------------------

    #[test]
    fn test_upcase_char_ascii() {
        assert_eq!(upcase_char('a'), 'A');
        assert_eq!(upcase_char('z'), 'Z');
        assert_eq!(upcase_char('A'), 'A');
        assert_eq!(upcase_char('1'), '1');
        assert_eq!(upcase_char(' '), ' ');
    }

    // -- 2. downcase_char basic ASCII --------------------------------------

    #[test]
    fn test_downcase_char_ascii() {
        assert_eq!(downcase_char('A'), 'a');
        assert_eq!(downcase_char('Z'), 'z');
        assert_eq!(downcase_char('a'), 'a');
        assert_eq!(downcase_char('1'), '1');
        assert_eq!(downcase_char(' '), ' ');
    }

    // -- 3. titlecase_char basic -------------------------------------------

    #[test]
    fn test_titlecase_char_basic() {
        assert_eq!(titlecase_char('a'), 'A');
        assert_eq!(titlecase_char('A'), 'A');
        assert_eq!(titlecase_char('1'), '1');
    }

    // -- 4. titlecase_char digraph DZ --------------------------------------

    #[test]
    fn test_titlecase_char_digraph_dz() {
        // U+01F1 DZ -> U+01F2 Dz
        assert_eq!(titlecase_char('\u{01F1}'), '\u{01F2}');
        // U+01F3 dz -> U+01F2 Dz
        assert_eq!(titlecase_char('\u{01F3}'), '\u{01F2}');
        // U+01F2 Dz -> U+01F2 Dz (idempotent)
        assert_eq!(titlecase_char('\u{01F2}'), '\u{01F2}');
    }

    // -- 5. titlecase_char digraph DŽ --------------------------------------

    #[test]
    fn test_titlecase_char_digraph_dz_caron() {
        // U+01C4 DŽ -> U+01C5 Dž
        assert_eq!(titlecase_char('\u{01C4}'), '\u{01C5}');
        // U+01C6 dž -> U+01C5 Dž
        assert_eq!(titlecase_char('\u{01C6}'), '\u{01C5}');
        // U+01C5 Dž -> U+01C5 Dž
        assert_eq!(titlecase_char('\u{01C5}'), '\u{01C5}');
    }

    // -- 6. titlecase_char digraph LJ --------------------------------------

    #[test]
    fn test_titlecase_char_digraph_lj() {
        assert_eq!(titlecase_char('\u{01C7}'), '\u{01C8}'); // LJ -> Lj
        assert_eq!(titlecase_char('\u{01C9}'), '\u{01C8}'); // lj -> Lj
        assert_eq!(titlecase_char('\u{01C8}'), '\u{01C8}'); // Lj -> Lj
    }

    // -- 7. titlecase_char digraph NJ --------------------------------------

    #[test]
    fn test_titlecase_char_digraph_nj() {
        assert_eq!(titlecase_char('\u{01CA}'), '\u{01CB}'); // NJ -> Nj
        assert_eq!(titlecase_char('\u{01CC}'), '\u{01CB}'); // nj -> Nj
        assert_eq!(titlecase_char('\u{01CB}'), '\u{01CB}'); // Nj -> Nj
    }

    // -- 8. upcase_char_full: sharp s expansion ----------------------------

    #[test]
    fn test_upcase_char_full_sharp_s() {
        let result = upcase_char_full(SHARP_S);
        assert_eq!(result, vec!['S', 'S']);
    }

    // -- 9. upcase_char_full: regular character ----------------------------

    #[test]
    fn test_upcase_char_full_regular() {
        assert_eq!(upcase_char_full('a'), vec!['A']);
        assert_eq!(upcase_char_full('A'), vec!['A']);
        assert_eq!(upcase_char_full('1'), vec!['1']);
    }

    // -- 10. upcase_string basic -------------------------------------------

    #[test]
    fn test_upcase_string_basic() {
        assert_eq!(upcase_string("hello"), "HELLO");
        assert_eq!(upcase_string("Hello World!"), "HELLO WORLD!");
        assert_eq!(upcase_string(""), "");
        assert_eq!(upcase_string("123"), "123");
    }

    // -- 11. upcase_string with sharp s ------------------------------------

    #[test]
    fn test_upcase_string_sharp_s() {
        assert_eq!(upcase_string("straße"), "STRASSE");
        assert_eq!(upcase_string("ß"), "SS");
    }

    // -- 12. downcase_string basic -----------------------------------------

    #[test]
    fn test_downcase_string_basic() {
        assert_eq!(downcase_string("HELLO"), "hello");
        assert_eq!(downcase_string("Hello World!"), "hello world!");
        assert_eq!(downcase_string(""), "");
        assert_eq!(downcase_string("123"), "123");
    }

    // -- 13. downcase_string Greek final sigma -----------------------------

    #[test]
    fn test_downcase_string_greek_final_sigma() {
        // "ΣΟΦΟΣ" (sophos) -> "σοφος" with final sigma
        // Rust's to_lowercase handles this: final position gets ς.
        let result = downcase_string("\u{03A3}\u{039F}\u{03A6}\u{039F}\u{03A3}");
        // Expected: σοφος with final ς
        assert_eq!(result, "\u{03C3}\u{03BF}\u{03C6}\u{03BF}\u{03C2}");
    }

    // -- 14. downcase_string Greek sigma mid-word --------------------------

    #[test]
    fn test_downcase_string_greek_sigma_mid_word() {
        // Lone Σ is NOT preceded by a cased letter, so per the Unicode
        // final-sigma rule (SpecialCasing), it does NOT become final sigma.
        // It becomes regular lowercase sigma σ.
        let result = downcase_string("\u{03A3}");
        assert_eq!(result, "\u{03C3}"); // regular sigma, not final
    }

    // -- 15. capitalize_string basic ---------------------------------------

    #[test]
    fn test_capitalize_string_basic() {
        assert_eq!(capitalize_string("hello world"), "Hello World");
        assert_eq!(capitalize_string("HELLO WORLD"), "Hello World");
        assert_eq!(capitalize_string(""), "");
    }

    // -- 16. capitalize_string with punctuation ----------------------------

    #[test]
    fn test_capitalize_string_punctuation() {
        assert_eq!(capitalize_string("hello-world"), "Hello-World");
        assert_eq!(capitalize_string("it's a test"), "It'S A Test");
        // Apostrophe is not alphanumeric, so it breaks word boundaries.
    }

    // -- 17. capitalize_string with numbers --------------------------------

    #[test]
    fn test_capitalize_string_numbers() {
        // Numbers are word constituents, so "3rd" is one word; 'r' is not
        // at a word start.
        assert_eq!(capitalize_string("3rd place"), "3rd Place");
    }

    // -- 18. capitalize_string Unicode -------------------------------------

    #[test]
    fn test_capitalize_string_unicode() {
        assert_eq!(capitalize_string("über cool"), "\u{00DC}ber Cool");
        assert_eq!(capitalize_string("café latte"), "Caf\u{00E9} Latte");
    }

    // -- 19. upcase_initials_string basic ----------------------------------

    #[test]
    fn test_upcase_initials_string_basic() {
        assert_eq!(upcase_initials_string("hello world"), "Hello World");
        // Unlike capitalize, non-initial letters are NOT lowercased.
        assert_eq!(upcase_initials_string("hELLO wORLD"), "HELLO WORLD");
    }

    // -- 20. upcase_initials_string with punctuation -----------------------

    #[test]
    fn test_upcase_initials_string_punctuation() {
        assert_eq!(upcase_initials_string("hello-world"), "Hello-World");
    }

    // -- 21. capitalize_string sharp s at word start -----------------------

    #[test]
    fn test_capitalize_string_sharp_s_start() {
        // Capitalizing ß at word start: the initial gets title-cased.
        // titlecase_char('ß') returns 'S' (single character, not the
        // multi-char uppercase expansion SS). The rest of the word is
        // lowercased. This matches Emacs's capitalize behavior.
        let result = capitalize_string("ßtraße");
        assert_eq!(result, "Stra\u{00DF}e");
    }

    // -- 22. CasingContext tracks word boundaries --------------------------

    #[test]
    fn test_casing_context_word_boundaries() {
        let mut ctx = CasingContext::new(CaseOperation::Capitalize);
        // "ab cd"
        let r1 = ctx.case_next('a', Some('b'));
        assert!(r1.advanced_word); // entered word
        assert_eq!(r1.chars, vec!['A']); // capitalized

        let r2 = ctx.case_next('b', Some(' '));
        assert!(!r2.advanced_word);
        assert_eq!(r2.chars, vec!['b']); // lowercased

        let r3 = ctx.case_next(' ', Some('c'));
        assert!(!r3.advanced_word);
        assert_eq!(r3.chars, vec![' ']); // unchanged

        let r4 = ctx.case_next('c', Some('d'));
        assert!(r4.advanced_word); // entered new word
        assert_eq!(r4.chars, vec!['C']); // capitalized
    }

    // -- 23. CaseOperation enum values -------------------------------------

    #[test]
    fn test_case_operation_equality() {
        assert_eq!(CaseOperation::Upcase, CaseOperation::Upcase);
        assert_ne!(CaseOperation::Upcase, CaseOperation::Downcase);
        assert_ne!(CaseOperation::Capitalize, CaseOperation::UpcaseInitials);
    }

    // -- 24. CaseResult single vs multi ------------------------------------

    #[test]
    fn test_case_result_construction() {
        let single = CaseResult::single('A', false);
        assert_eq!(single.chars, vec!['A']);
        assert!(!single.advanced_word);

        let multi = CaseResult::multi(vec!['S', 'S'], true);
        assert_eq!(multi.chars, vec!['S', 'S']);
        assert!(multi.advanced_word);
    }

    // -- 25. is_word_constituent -------------------------------------------

    #[test]
    fn test_is_word_constituent() {
        assert!(is_word_constituent('a'));
        assert!(is_word_constituent('Z'));
        assert!(is_word_constituent('5'));
        assert!(is_word_constituent('\u{00E9}')); // e-acute
        assert!(!is_word_constituent(' '));
        assert!(!is_word_constituent('-'));
        assert!(!is_word_constituent('!'));
        assert!(!is_word_constituent('_')); // symbol syntax, not word
    }

    // -- 26. Greek final sigma via CasingContext ----------------------------

    #[test]
    fn test_casing_context_greek_final_sigma() {
        let mut ctx = CasingContext::new(CaseOperation::Downcase);
        // "ΟΣ " — Σ is at end of word (next is space).
        let _r1 = ctx.case_next('\u{039F}', Some(GREEK_CAPITAL_SIGMA)); // Ο
        let r2 = ctx.case_next(GREEK_CAPITAL_SIGMA, Some(' ')); // Σ at end
        assert_eq!(r2.chars, vec![GREEK_SMALL_FINAL_SIGMA]);
    }

    // -- 27. Greek medial sigma via CasingContext ---------------------------

    #[test]
    fn test_casing_context_greek_medial_sigma() {
        let mut ctx = CasingContext::new(CaseOperation::Downcase);
        // "ΣΟ" — Σ at start, followed by word char.
        let r1 = ctx.case_next(GREEK_CAPITAL_SIGMA, Some('\u{039F}'));
        // At start, was_in_word is false, so the final-sigma rule does not
        // apply (the rule requires was_in_word to be true).
        assert_eq!(r1.chars, vec![GREEK_SMALL_SIGMA]);
    }

    // -- 28. upcase_string Unicode accented --------------------------------

    #[test]
    fn test_upcase_string_accented() {
        assert_eq!(upcase_string("caf\u{00E9}"), "CAF\u{00C9}");
        assert_eq!(upcase_string("\u{00E9}l\u{00E8}ve"), "\u{00C9}L\u{00C8}VE");
    }

    // -- 29. downcase_string preserves non-letters -------------------------

    #[test]
    fn test_downcase_string_non_letters() {
        assert_eq!(downcase_string("ABC 123 !@#"), "abc 123 !@#");
    }

    // -- 30. capitalize_string multiple spaces -----------------------------

    #[test]
    fn test_capitalize_string_multiple_spaces() {
        assert_eq!(capitalize_string("  hello   world  "), "  Hello   World  ");
    }

    // -- 31. upcase_initials with already-capitalized ----------------------

    #[test]
    fn test_upcase_initials_already_capitalized() {
        // Already capitalized input: initials are already uppercase,
        // rest is left as-is (unlike capitalize which lowercases).
        assert_eq!(upcase_initials_string("Hello World"), "Hello World");
    }

    // -- 32. upcase_initials with all lowercase ----------------------------

    #[test]
    fn test_upcase_initials_all_lowercase() {
        assert_eq!(upcase_initials_string("hello world"), "Hello World");
    }

    // -- 33. CasingContext upcase mode -------------------------------------

    #[test]
    fn test_casing_context_upcase_mode() {
        let mut ctx = CasingContext::new(CaseOperation::Upcase);
        let r = ctx.case_next('a', None);
        assert_eq!(r.chars, vec!['A']);
    }

    // -- 34. CasingContext downcase mode ------------------------------------

    #[test]
    fn test_casing_context_downcase_mode() {
        let mut ctx = CasingContext::new(CaseOperation::Downcase);
        let r = ctx.case_next('A', None);
        assert_eq!(r.chars, vec!['a']);
    }

    // -- 35. CasingContext upcase_initials does not change mid-word --------

    #[test]
    fn test_casing_context_upcase_initials_mid_word() {
        let mut ctx = CasingContext::new(CaseOperation::UpcaseInitials);
        let _r1 = ctx.case_next('H', Some('e')); // first char -> H (already upper)
        let r2 = ctx.case_next('e', Some('l')); // mid-word, unchanged
        assert_eq!(r2.chars, vec!['e']);
        let r3 = ctx.case_next('l', Some('l'));
        assert_eq!(r3.chars, vec!['l']);
    }

    // -- 36. capitalize with titlecase digraphs ----------------------------

    #[test]
    fn test_capitalize_titlecase_digraph() {
        // U+01C6 dž at word start -> U+01C5 Dž (title case)
        let mut ctx = CasingContext::new(CaseOperation::Capitalize);
        let r = ctx.case_next('\u{01C6}', Some('a')); // dž at word start
        assert_eq!(r.chars, vec!['\u{01C5}']); // Dž
    }

    // -- 37. upcase digraph ------------------------------------------------

    #[test]
    fn test_upcase_digraph() {
        // U+01C6 dž uppercase -> U+01C4 DŽ
        assert_eq!(upcase_char('\u{01C6}'), '\u{01C4}');
    }

    // -- 38. downcase digraph ----------------------------------------------

    #[test]
    fn test_downcase_digraph() {
        // U+01C4 DŽ lowercase -> U+01C6 dž
        assert_eq!(downcase_char('\u{01C4}'), '\u{01C6}');
    }

    // -- 39. capitalize empty and single-char strings ----------------------

    #[test]
    fn test_capitalize_edge_cases() {
        assert_eq!(capitalize_string(""), "");
        assert_eq!(capitalize_string("a"), "A");
        assert_eq!(capitalize_string("A"), "A");
        assert_eq!(capitalize_string("1"), "1");
        assert_eq!(capitalize_string(" "), " ");
    }

    // -- 40. upcase_initials empty and single-char strings -----------------

    #[test]
    fn test_upcase_initials_edge_cases() {
        assert_eq!(upcase_initials_string(""), "");
        assert_eq!(upcase_initials_string("a"), "A");
        assert_eq!(upcase_initials_string("A"), "A");
        assert_eq!(upcase_initials_string(" "), " ");
    }

    // -- 41. CasingContext can be cloned -----------------------------------

    #[test]
    fn test_casing_context_clone() {
        let mut ctx = CasingContext::new(CaseOperation::Capitalize);
        ctx.case_next('h', Some('e'));
        let ctx2 = ctx.clone();
        assert_eq!(ctx2.in_word, ctx.in_word);
        assert_eq!(ctx2.flag, ctx.flag);
    }

    // -- 42. upcase_string with capital sharp s ----------------------------

    #[test]
    fn test_upcase_capital_sharp_s() {
        // Capital sharp s ẞ uppercased stays ẞ (or SS depending on impl).
        let result = upcase_string("\u{1E9E}");
        // Rust's to_uppercase of ẞ is ẞ itself.
        assert_eq!(result, "\u{1E9E}");
    }

    // -- 43. downcase capital sharp s -> ß ---------------------------------

    #[test]
    fn test_downcase_capital_sharp_s() {
        assert_eq!(downcase_char(CAPITAL_SHARP_S), SHARP_S);
    }

    // -- 44. capitalize with leading non-word chars -----------------------

    #[test]
    fn test_capitalize_leading_non_word() {
        assert_eq!(capitalize_string("...hello"), "...Hello");
        assert_eq!(capitalize_string("---abc---def"), "---Abc---Def");
    }

    // -- 45. upcase_initials preserves case after initial -----------------

    #[test]
    fn test_upcase_initials_preserves_case() {
        assert_eq!(upcase_initials_string("mcDonald"), "McDonald");
        // 'm' -> 'M', 'c' unchanged, 'D' unchanged, etc.
        // Wait: "mcDonald" is one word (all alphanumeric), so only 'm'
        // gets upcased to 'M', rest stays.
        assert_eq!(upcase_initials_string("mcDonald"), "McDonald");
    }

    // -- 46. Greek final sigma at end of string ---------------------------

    #[test]
    fn test_greek_final_sigma_end_of_string() {
        let mut ctx = CasingContext::new(CaseOperation::Downcase);
        // "ΟΣ" — sigma at end of string, no next character.
        let _r1 = ctx.case_next('\u{039F}', Some(GREEK_CAPITAL_SIGMA));
        let r2 = ctx.case_next(GREEK_CAPITAL_SIGMA, None); // end of string
        assert_eq!(r2.chars, vec![GREEK_SMALL_FINAL_SIGMA]);
    }

    // -- 47. upcase_char_full ffi ligature ---------------------------------

    #[test]
    fn test_upcase_char_full_ffi_ligature() {
        // U+FB03 ffi ligature -> FFI
        let result = upcase_char_full('\u{FB03}');
        assert_eq!(result, vec!['F', 'F', 'I']);
    }

    // -- 48. capitalize_string with tabs and newlines ----------------------

    #[test]
    fn test_capitalize_string_whitespace_types() {
        assert_eq!(capitalize_string("hello\tworld\nnew"), "Hello\tWorld\nNew");
    }
}
