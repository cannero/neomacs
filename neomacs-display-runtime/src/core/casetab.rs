//! Pure Rust case table engine.
//!
//! Implements the core of Emacs's case table machinery (cf. `casetab.c`).
//! A case table holds four interrelated mappings:
//!
//! - **Downcase**: maps each character to its lowercase equivalent.
//! - **Upcase**: maps each character to its uppercase equivalent.
//! - **Canonicalize**: maps each character to a single canonical
//!   representative of its equivalence class (for case-insensitive
//!   comparison).
//! - **Equivalence**: forms circular chains linking all characters in
//!   the same equivalence class. For example, if 'a' and 'A' are
//!   case-equivalent, then `equivalence['a'] == 'A'` and
//!   `equivalence['A'] == 'a'`.
//!
//! The standard table covers ASCII letters; custom mappings can be
//! added via [`CaseTable::set_mapping`].

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// CaseTable
// ---------------------------------------------------------------------------

/// A case conversion table holding downcase, upcase, canonicalize, and
/// equivalence mappings.
///
/// Characters without an explicit mapping are treated as their own
/// uppercase, lowercase, canonical form, and sole equivalence member
/// (i.e., identity).
#[derive(Debug, Clone)]
pub struct CaseTable {
    /// Maps a character to its uppercase equivalent.
    upcase: HashMap<char, char>,
    /// Maps a character to its lowercase equivalent.
    downcase: HashMap<char, char>,
    /// Maps a character to its canonical form (the representative of
    /// its equivalence class, used for case-insensitive comparison).
    canonicalize: HashMap<char, char>,
    /// Circular equivalence chains: following the chain from any
    /// character eventually returns to the starting character, visiting
    /// every case-equivalent character along the way.
    equivalence: HashMap<char, char>,
}

impl CaseTable {
    // -- Construction -------------------------------------------------------

    /// Create an empty case table with no mappings.
    ///
    /// Every character maps to itself for all four tables.
    pub fn empty() -> Self {
        CaseTable {
            upcase: HashMap::new(),
            downcase: HashMap::new(),
            canonicalize: HashMap::new(),
            equivalence: HashMap::new(),
        }
    }

    /// Create the standard ASCII case table.
    ///
    /// This mirrors `init_casetab_once()` in Emacs's `casetab.c`:
    /// - 'A'..'Z' downcase to 'a'..'z'; upcase is identity for uppercase.
    /// - 'a'..'z' upcase to 'A'..'Z'; downcase is identity for lowercase.
    /// - Canonicalize maps both cases to the lowercase letter.
    /// - Equivalence chains: 'a' -> 'A' -> 'a', 'b' -> 'B' -> 'b', etc.
    /// - All other ASCII characters (0..128) map to themselves.
    pub fn standard() -> Self {
        let mut table = CaseTable::empty();

        for i in 0u8..128 {
            let ch = i as char;

            // Downcase: uppercase letters map to lowercase; everything else
            // maps to itself.
            let lower = if ch.is_ascii_uppercase() {
                (i as u32 + (b'a' as u32) - (b'A' as u32)) as u8 as char
            } else {
                ch
            };

            // Upcase: lowercase letters map to uppercase; everything else
            // maps to itself.
            let upper = if ch.is_ascii_lowercase() {
                (i as u32 + (b'A' as u32) - (b'a' as u32)) as u8 as char
            } else {
                ch
            };

            // Only store non-identity mappings.
            if lower != ch {
                table.downcase.insert(ch, lower);
            }
            if upper != ch {
                table.upcase.insert(ch, upper);
            }

            // Canonicalize: map every character to its lowercase form
            // (for letters, both 'A' and 'a' canonicalize to 'a').
            let canon = if ch.is_ascii_uppercase() {
                (i as u32 + (b'a' as u32) - (b'A' as u32)) as u8 as char
            } else {
                ch
            };
            if canon != ch {
                table.canonicalize.insert(ch, canon);
            }

            // Equivalence: for letter pairs, create a 2-cycle.
            // 'a' -> 'A' -> 'a', etc.  Non-letters point to themselves
            // (identity, so not stored).
            if ch.is_ascii_uppercase() {
                let partner = (i as u32 + (b'a' as u32) - (b'A' as u32)) as u8 as char;
                table.equivalence.insert(ch, partner);
                table.equivalence.insert(partner, ch);
            }
        }

        table
    }

    // -- Mutation -----------------------------------------------------------

    /// Define a case pair: `lower` is the lowercase form and `upper` is
    /// the uppercase form.
    ///
    /// Updates all four tables (upcase, downcase, canonicalize,
    /// equivalence) so that the two characters are treated as
    /// case-variants of each other.
    ///
    /// The canonical form for both characters is set to `lower`.
    ///
    /// Equivalence chains are rebuilt: any prior equivalence-class
    /// members of `lower` or `upper` are merged into a single circular
    /// chain.
    pub fn set_mapping(&mut self, lower: char, upper: char) {
        // 0. Clean up any previous partners.
        //
        // If `lower` previously upcased to some old_upper (different from
        // `upper`), we must remove old_upper's downcase entry and detach
        // it from the equivalence class.  Similarly for `upper`.
        let old_upper = self.upcase_char(lower);
        if old_upper != lower && old_upper != upper {
            // old_upper used to downcase to `lower`; remove that.
            self.downcase.remove(&old_upper);
            self.canonicalize.remove(&old_upper);
            // Detach old_upper from the equivalence chain by removing
            // its entry (it becomes its own 1-cycle).
            self.remove_from_equivalence_chain(old_upper);
        }

        let old_lower = self.downcase_char(upper);
        if old_lower != upper && old_lower != lower {
            // old_lower used to upcase to `upper`; remove that.
            self.upcase.remove(&old_lower);
            self.canonicalize.remove(&old_lower);
            self.remove_from_equivalence_chain(old_lower);
        }

        // 1. Upcase / downcase.
        if lower != upper {
            self.upcase.insert(lower, upper);
            self.downcase.insert(upper, lower);
        }
        // lower downcases to itself (remove any prior mapping).
        self.downcase.remove(&lower);
        // upper upcases to itself (remove any prior mapping).
        self.upcase.remove(&upper);

        // 2. Canonicalize: both map to `lower`.
        self.canonicalize.remove(&lower); // lower is already canonical
        if lower != upper {
            self.canonicalize.insert(upper, lower);
        }

        // 3. Equivalence: build circular chain for the equivalence class.
        //
        // Gather all members that are currently equivalent to `lower` or
        // `upper`, plus both characters themselves.
        let mut members = Vec::new();
        members.push(lower);
        if upper != lower {
            members.push(upper);
        }

        // Walk existing chains to collect any other members linked to
        // `lower` or `upper`.
        for &seed in &[lower, upper] {
            let mut cur = self.equivalence_char(seed);
            while cur != seed {
                if !members.contains(&cur) {
                    members.push(cur);
                }
                cur = self.equivalence_char(cur);
            }
        }

        // Build a circular chain through all members.
        if members.len() == 1 {
            // Single member: identity (remove any stored mapping).
            self.equivalence.remove(&members[0]);
        } else {
            for i in 0..members.len() {
                let next = members[(i + 1) % members.len()];
                self.equivalence.insert(members[i], next);
            }
        }

        // Update canonicalize for all members in the equivalence class.
        for &m in &members {
            if m != lower {
                self.canonicalize.insert(m, lower);
            } else {
                self.canonicalize.remove(&m);
            }
        }
    }

    /// Remove `ch` from its equivalence chain, splicing the chain
    /// around it so the remaining members stay connected.
    fn remove_from_equivalence_chain(&mut self, ch: char) {
        // Find the predecessor: the character whose equivalence points to `ch`.
        let mut pred = ch;
        let mut cur = self.equivalence_char(ch);
        if cur == ch {
            // Already a 1-cycle; nothing to do.
            return;
        }

        // Walk the chain to find the predecessor of `ch`.
        let max_steps = self.equivalence.len() + 1;
        for _ in 0..max_steps {
            let next = self.equivalence_char(cur);
            if next == ch {
                pred = cur;
                break;
            }
            cur = next;
        }

        // Splice `ch` out: predecessor now points to ch's successor.
        let successor = self.equivalence_char(ch);
        if pred == successor {
            // Only one other member remains; it becomes a 1-cycle.
            self.equivalence.remove(&pred);
        } else {
            self.equivalence.insert(pred, successor);
        }

        // `ch` is now its own 1-cycle.
        self.equivalence.remove(&ch);
    }

    // -- Single-character queries -------------------------------------------

    /// Return the uppercase form of `ch`.
    ///
    /// If no mapping exists, returns `ch` unchanged.
    #[inline]
    pub fn upcase_char(&self, ch: char) -> char {
        self.upcase.get(&ch).copied().unwrap_or(ch)
    }

    /// Return the lowercase form of `ch`.
    ///
    /// If no mapping exists, returns `ch` unchanged.
    #[inline]
    pub fn downcase_char(&self, ch: char) -> char {
        self.downcase.get(&ch).copied().unwrap_or(ch)
    }

    /// Return the canonical form of `ch` for case-insensitive comparison.
    ///
    /// All characters in the same equivalence class share the same
    /// canonical form.
    #[inline]
    pub fn canonicalize_char(&self, ch: char) -> char {
        self.canonicalize.get(&ch).copied().unwrap_or(ch)
    }

    /// Return the next character in the equivalence chain.
    ///
    /// Following the chain from any character eventually returns to the
    /// starting character.  For characters with no case partner, this
    /// returns the character itself (a 1-cycle).
    #[inline]
    pub fn equivalence_char(&self, ch: char) -> char {
        self.equivalence.get(&ch).copied().unwrap_or(ch)
    }

    /// Return `true` if `a` and `b` are case-equivalent (i.e., they
    /// belong to the same equivalence class).
    ///
    /// Two characters are case-equivalent if they share the same
    /// canonical form.
    pub fn case_equivalent_p(&self, a: char, b: char) -> bool {
        self.canonicalize_char(a) == self.canonicalize_char(b)
    }

    // -- String operations --------------------------------------------------

    /// Convert every character in `s` to uppercase using this table.
    pub fn upcase_string(&self, s: &str) -> String {
        s.chars().map(|ch| self.upcase_char(ch)).collect()
    }

    /// Convert every character in `s` to lowercase using this table.
    pub fn downcase_string(&self, s: &str) -> String {
        s.chars().map(|ch| self.downcase_char(ch)).collect()
    }

    // -- Validation ---------------------------------------------------------

    /// Check consistency of the four tables.
    ///
    /// Returns `true` if all of the following hold:
    ///
    /// 1. For every `(ch, upper)` in upcase, `downcase[upper] == ch`.
    /// 2. For every `(ch, lower)` in downcase, `upcase[lower] == ch`.
    /// 3. For every character with a canonicalize mapping, the canonical
    ///    form is idempotent: `canonicalize(canonicalize(ch)) == canonicalize(ch)`.
    /// 4. Every equivalence chain is a valid cycle (following the chain
    ///    returns to the start within a bounded number of steps).
    /// 5. All members of an equivalence chain share the same canonical form.
    pub fn is_valid(&self) -> bool {
        // Check 1: upcase/downcase inverse relationship.
        for (&ch, &upper) in &self.upcase {
            if self.downcase_char(upper) != ch {
                return false;
            }
        }

        // Check 2: downcase/upcase inverse relationship.
        for (&ch, &lower) in &self.downcase {
            if self.upcase_char(lower) != ch {
                return false;
            }
        }

        // Check 3: canonicalize is idempotent.
        for (&ch, &canon) in &self.canonicalize {
            let _ = ch; // suppress unused warning
            if self.canonicalize_char(canon) != canon {
                return false;
            }
        }

        // Check 4 & 5: equivalence chains are valid cycles with
        // consistent canonical form.
        // We need to verify every chain, but avoid re-checking already
        // visited characters.
        let mut visited = std::collections::HashSet::new();
        for &start in self.equivalence.keys() {
            if visited.contains(&start) {
                continue;
            }

            let expected_canon = self.canonicalize_char(start);
            let mut cur = start;
            let max_steps = self.equivalence.len() + 1;

            for step in 0..max_steps {
                visited.insert(cur);

                // Check 5: all members share the same canonical form.
                if self.canonicalize_char(cur) != expected_canon {
                    return false;
                }

                cur = self.equivalence_char(cur);

                if cur == start {
                    // Valid cycle found.
                    break;
                }

                // Check 4: if we exceed max_steps, the chain is broken.
                if step == max_steps - 1 {
                    return false;
                }
            }
        }

        true
    }

    // -- Queries ------------------------------------------------------------

    /// Return the number of explicit upcase mappings.
    pub fn upcase_count(&self) -> usize {
        self.upcase.len()
    }

    /// Return the number of explicit downcase mappings.
    pub fn downcase_count(&self) -> usize {
        self.downcase.len()
    }

    /// Return the number of explicit canonicalize mappings.
    pub fn canonicalize_count(&self) -> usize {
        self.canonicalize.len()
    }

    /// Return the number of explicit equivalence mappings.
    pub fn equivalence_count(&self) -> usize {
        self.equivalence.len()
    }
}

impl Default for CaseTable {
    fn default() -> Self {
        Self::standard()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- 1. Standard table creation -----------------------------------------

    #[test]
    fn test_standard_upcase_lowercase_letters() {
        let ct = CaseTable::standard();
        for ch in 'a'..='z' {
            let expected = (ch as u8 - b'a' + b'A') as char;
            assert_eq!(
                ct.upcase_char(ch),
                expected,
                "upcase('{}') should be '{}'",
                ch,
                expected
            );
        }
    }

    // -- 2. Standard downcase uppercase letters -----------------------------

    #[test]
    fn test_standard_downcase_uppercase_letters() {
        let ct = CaseTable::standard();
        for ch in 'A'..='Z' {
            let expected = (ch as u8 - b'A' + b'a') as char;
            assert_eq!(
                ct.downcase_char(ch),
                expected,
                "downcase('{}') should be '{}'",
                ch,
                expected
            );
        }
    }

    // -- 3. Non-letter characters are identity ------------------------------

    #[test]
    fn test_standard_non_letters_identity() {
        let ct = CaseTable::standard();
        let non_letters = ['0', '1', '9', ' ', '!', '@', '#', '.', '\n', '\t'];
        for &ch in &non_letters {
            assert_eq!(
                ct.upcase_char(ch),
                ch,
                "upcase('{}') should be identity",
                ch
            );
            assert_eq!(
                ct.downcase_char(ch),
                ch,
                "downcase('{}') should be identity",
                ch
            );
        }
    }

    // -- 4. Upcase of already-uppercase is identity -------------------------

    #[test]
    fn test_standard_upcase_already_uppercase() {
        let ct = CaseTable::standard();
        for ch in 'A'..='Z' {
            assert_eq!(ct.upcase_char(ch), ch);
        }
    }

    // -- 5. Downcase of already-lowercase is identity -----------------------

    #[test]
    fn test_standard_downcase_already_lowercase() {
        let ct = CaseTable::standard();
        for ch in 'a'..='z' {
            assert_eq!(ct.downcase_char(ch), ch);
        }
    }

    // -- 6. Canonicalize maps both cases to lowercase -----------------------

    #[test]
    fn test_standard_canonicalize() {
        let ct = CaseTable::standard();
        for i in 0u8..26 {
            let lower = (b'a' + i) as char;
            let upper = (b'A' + i) as char;
            assert_eq!(ct.canonicalize_char(lower), lower);
            assert_eq!(ct.canonicalize_char(upper), lower);
        }
    }

    // -- 7. Equivalence chains form 2-cycles for letters --------------------

    #[test]
    fn test_standard_equivalence_chains() {
        let ct = CaseTable::standard();
        for i in 0u8..26 {
            let lower = (b'a' + i) as char;
            let upper = (b'A' + i) as char;

            // Following the chain from lower should reach upper, and then
            // back to lower.
            let next = ct.equivalence_char(lower);
            assert_eq!(
                next, upper,
                "equivalence('{}') should be '{}'",
                lower, upper
            );
            let back = ct.equivalence_char(next);
            assert_eq!(
                back, lower,
                "equivalence('{}') should be '{}'",
                upper, lower
            );
        }
    }

    // -- 8. case_equivalent_p -----------------------------------------------

    #[test]
    fn test_case_equivalent_p() {
        let ct = CaseTable::standard();
        assert!(ct.case_equivalent_p('a', 'A'));
        assert!(ct.case_equivalent_p('A', 'a'));
        assert!(ct.case_equivalent_p('z', 'Z'));
        assert!(!ct.case_equivalent_p('a', 'b'));
        assert!(!ct.case_equivalent_p('a', 'B'));
        // Non-letter compared to itself.
        assert!(ct.case_equivalent_p('1', '1'));
        // Non-letters that differ.
        assert!(!ct.case_equivalent_p('1', '2'));
    }

    // -- 9. upcase_string ---------------------------------------------------

    #[test]
    fn test_upcase_string() {
        let ct = CaseTable::standard();
        assert_eq!(ct.upcase_string("hello"), "HELLO");
        assert_eq!(ct.upcase_string("Hello World!"), "HELLO WORLD!");
        assert_eq!(ct.upcase_string("123"), "123");
        assert_eq!(ct.upcase_string(""), "");
    }

    // -- 10. downcase_string ------------------------------------------------

    #[test]
    fn test_downcase_string() {
        let ct = CaseTable::standard();
        assert_eq!(ct.downcase_string("HELLO"), "hello");
        assert_eq!(ct.downcase_string("Hello World!"), "hello world!");
        assert_eq!(ct.downcase_string("123"), "123");
        assert_eq!(ct.downcase_string(""), "");
    }

    // -- 11. is_valid on standard table -------------------------------------

    #[test]
    fn test_standard_is_valid() {
        let ct = CaseTable::standard();
        assert!(ct.is_valid());
    }

    // -- 12. Empty table is valid -------------------------------------------

    #[test]
    fn test_empty_is_valid() {
        let ct = CaseTable::empty();
        assert!(ct.is_valid());
    }

    // -- 13. set_mapping basic pair -----------------------------------------

    #[test]
    fn test_set_mapping_basic() {
        let mut ct = CaseTable::empty();
        ct.set_mapping('x', 'X');

        assert_eq!(ct.upcase_char('x'), 'X');
        assert_eq!(ct.downcase_char('X'), 'x');
        assert_eq!(ct.canonicalize_char('x'), 'x');
        assert_eq!(ct.canonicalize_char('X'), 'x');
        assert!(ct.case_equivalent_p('x', 'X'));
        assert!(ct.is_valid());
    }

    // -- 14. set_mapping same character (lower == upper) --------------------

    #[test]
    fn test_set_mapping_same_char() {
        let mut ct = CaseTable::empty();
        ct.set_mapping('a', 'a');

        assert_eq!(ct.upcase_char('a'), 'a');
        assert_eq!(ct.downcase_char('a'), 'a');
        assert_eq!(ct.canonicalize_char('a'), 'a');
        assert!(ct.is_valid());
    }

    // -- 15. set_mapping overwrites previous mapping ------------------------

    #[test]
    fn test_set_mapping_overwrite() {
        let mut ct = CaseTable::empty();
        ct.set_mapping('a', 'A');
        ct.set_mapping('a', 'B');

        // Now 'a' upcases to 'B', 'B' downcases to 'a'.
        assert_eq!(ct.upcase_char('a'), 'B');
        assert_eq!(ct.downcase_char('B'), 'a');
        assert!(ct.case_equivalent_p('a', 'B'));
        assert!(ct.is_valid());
    }

    // -- 16. Overwrite detaches old partner ----------------------------------

    #[test]
    fn test_overwrite_detaches_old_partner() {
        let mut ct = CaseTable::empty();
        // First set 'a' <-> 'A'.
        ct.set_mapping('a', 'A');
        assert!(ct.case_equivalent_p('a', 'A'));

        // Overwrite: now 'a' <-> 'B'. 'A' is detached.
        ct.set_mapping('a', 'B');

        // 'a' and 'B' are equivalent.
        assert!(ct.case_equivalent_p('a', 'B'));
        // 'A' is no longer equivalent to 'a' or 'B'.
        assert!(!ct.case_equivalent_p('a', 'A'));
        assert!(!ct.case_equivalent_p('A', 'B'));
        // 'A' is now its own equivalence class.
        assert_eq!(ct.equivalence_char('A'), 'A');

        assert!(ct.is_valid());
    }

    // -- 17. Non-ASCII characters -------------------------------------------

    #[test]
    fn test_non_ascii_mapping() {
        let mut ct = CaseTable::empty();
        // Map e-acute lowercase to E-acute uppercase.
        ct.set_mapping('\u{00E9}', '\u{00C9}');

        assert_eq!(ct.upcase_char('\u{00E9}'), '\u{00C9}');
        assert_eq!(ct.downcase_char('\u{00C9}'), '\u{00E9}');
        assert!(ct.case_equivalent_p('\u{00E9}', '\u{00C9}'));
        assert!(ct.is_valid());
    }

    // -- 18. upcase_string with custom mapping ------------------------------

    #[test]
    fn test_upcase_string_custom_mapping() {
        let mut ct = CaseTable::empty();
        ct.set_mapping('a', 'A');
        ct.set_mapping('b', 'B');
        ct.set_mapping('c', 'C');

        assert_eq!(ct.upcase_string("abc"), "ABC");
        assert_eq!(ct.upcase_string("xyz"), "xyz"); // no mapping for x,y,z
    }

    // -- 19. downcase_string with custom mapping ----------------------------

    #[test]
    fn test_downcase_string_custom_mapping() {
        let mut ct = CaseTable::empty();
        ct.set_mapping('a', 'A');
        ct.set_mapping('b', 'B');

        assert_eq!(ct.downcase_string("AB"), "ab");
        assert_eq!(ct.downcase_string("XY"), "XY"); // no mapping
    }

    // -- 20. Canonicalize non-letter is identity ----------------------------

    #[test]
    fn test_canonicalize_non_letter_identity() {
        let ct = CaseTable::standard();
        for &ch in &['0', '!', ' ', '\n', '@'] {
            assert_eq!(
                ct.canonicalize_char(ch),
                ch,
                "canonicalize('{}') should be identity",
                ch
            );
        }
    }

    // -- 21. Equivalence of non-letter is self-cycle ------------------------

    #[test]
    fn test_equivalence_non_letter_self_cycle() {
        let ct = CaseTable::standard();
        for &ch in &['0', '!', ' ', '@'] {
            assert_eq!(
                ct.equivalence_char(ch),
                ch,
                "equivalence('{}') should be self for non-letters",
                ch
            );
        }
    }

    // -- 22. Default trait gives standard table -----------------------------

    #[test]
    fn test_default_is_standard() {
        let ct = CaseTable::default();
        assert_eq!(ct.upcase_char('a'), 'A');
        assert_eq!(ct.downcase_char('Z'), 'z');
        assert!(ct.is_valid());
    }

    // -- 23. Counts on standard table ---------------------------------------

    #[test]
    fn test_standard_counts() {
        let ct = CaseTable::standard();
        // 26 lowercase letters have upcase mappings.
        assert_eq!(ct.upcase_count(), 26);
        // 26 uppercase letters have downcase mappings.
        assert_eq!(ct.downcase_count(), 26);
        // 26 uppercase letters have canonicalize mappings (lowercase are identity).
        assert_eq!(ct.canonicalize_count(), 26);
        // 52 equivalence entries (26 pairs, each with 2 entries).
        assert_eq!(ct.equivalence_count(), 52);
    }

    // -- 24. Empty table counts are zero ------------------------------------

    #[test]
    fn test_empty_counts() {
        let ct = CaseTable::empty();
        assert_eq!(ct.upcase_count(), 0);
        assert_eq!(ct.downcase_count(), 0);
        assert_eq!(ct.canonicalize_count(), 0);
        assert_eq!(ct.equivalence_count(), 0);
    }

    // -- 25. Roundtrip: upcase then downcase --------------------------------

    #[test]
    fn test_roundtrip_upcase_downcase() {
        let ct = CaseTable::standard();
        for ch in 'a'..='z' {
            let upper = ct.upcase_char(ch);
            let lower = ct.downcase_char(upper);
            assert_eq!(lower, ch, "roundtrip failed for '{}'", ch);
        }
    }

    // -- 26. Roundtrip: downcase then upcase --------------------------------

    #[test]
    fn test_roundtrip_downcase_upcase() {
        let ct = CaseTable::standard();
        for ch in 'A'..='Z' {
            let lower = ct.downcase_char(ch);
            let upper = ct.upcase_char(lower);
            assert_eq!(upper, ch, "roundtrip failed for '{}'", ch);
        }
    }

    // -- 27. Mixed string operations ----------------------------------------

    #[test]
    fn test_mixed_string_operations() {
        let ct = CaseTable::standard();
        let input = "Hello, World! 123";
        assert_eq!(ct.upcase_string(input), "HELLO, WORLD! 123");
        assert_eq!(ct.downcase_string(input), "hello, world! 123");
    }

    // -- 28. set_mapping multiple independent pairs -------------------------

    #[test]
    fn test_multiple_independent_pairs() {
        let mut ct = CaseTable::empty();
        ct.set_mapping('a', 'A');
        ct.set_mapping('b', 'B');
        ct.set_mapping('c', 'C');

        // Each pair is independent.
        assert!(!ct.case_equivalent_p('a', 'b'));
        assert!(!ct.case_equivalent_p('A', 'B'));
        assert!(ct.case_equivalent_p('a', 'A'));
        assert!(ct.case_equivalent_p('b', 'B'));
        assert!(ct.case_equivalent_p('c', 'C'));

        assert!(ct.is_valid());
    }

    // -- 29. Validation detects broken canonicalize -------------------------

    #[test]
    fn test_validation_broken_canonicalize() {
        let mut ct = CaseTable::standard();
        // Manually corrupt: make canonicalize non-idempotent.
        // 'A' normally canonicalizes to 'a'. Set 'a' to canonicalize to 'b'.
        ct.canonicalize.insert('a', 'b');

        // Now canonicalize('A') = 'a', but canonicalize('a') = 'b',
        // so canonicalize(canonicalize('A')) = 'b' != 'a'. Not idempotent.
        assert!(!ct.is_valid());
    }

    // -- 30. Validation detects broken upcase/downcase pair -----------------

    #[test]
    fn test_validation_broken_upcase_downcase() {
        let mut ct = CaseTable::standard();
        // Corrupt: 'a' upcases to 'A', but set 'A' to downcase to 'b'.
        ct.downcase.insert('A', 'b');

        // Now upcase('a') = 'A', but downcase('A') = 'b' != 'a'.
        assert!(!ct.is_valid());
    }
}
