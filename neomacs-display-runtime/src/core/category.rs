//! Pure Rust implementation of Emacs's category table system.
//!
//! This module implements the core of Emacs's category machinery from
//! `category.c` / `category.h`. It provides:
//!
//! - `CategorySet` — a bitset of up to 95 categories (one per printable
//!   ASCII character from space `' '` through tilde `'~'`)
//! - `CategoryTable` — maps Unicode characters to their category sets,
//!   with range-based modification and deep copy
//! - `CategoryDocstring` — stores the documentation string for each
//!   defined category
//! - A `standard()` constructor that sets up the default Emacs categories
//!   (ASCII, Latin, CJK, etc.)
//! - `word_boundary_p()` for detecting word boundaries between adjacent
//!   characters based on category differences
//!
//! # Categories
//!
//! A category is represented by a single printable ASCII mnemonic character
//! in the range `' '` (32) through `'~'` (126), giving 95 possible
//! categories. Unlike syntax classes, categories are *not* exclusive: a
//! single character can belong to many categories simultaneously (a
//! category set).
//!
//! # Category Table
//!
//! A category table maps each Unicode code point to a `CategorySet`. It
//! is buffer-local in Emacs: different buffers can have different category
//! tables.

use std::collections::HashMap;
use std::fmt;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Lowest valid category character (space).
const CATEGORY_MIN: u8 = b' '; // 32

/// Highest valid category character (tilde).
const CATEGORY_MAX: u8 = b'~'; // 126

/// Total number of valid categories.
const CATEGORY_COUNT: usize = (CATEGORY_MAX - CATEGORY_MIN + 1) as usize; // 95

// ---------------------------------------------------------------------------
// CategorySet
// ---------------------------------------------------------------------------

/// A set of categories, stored as a 128-bit bitset.
///
/// Bit positions 32 through 126 correspond to the printable ASCII
/// characters `' '` through `'~'`. The remaining bits are unused.
///
/// This mirrors the Emacs `bool-vector` of length 128 used for category
/// sets in `category.h`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CategorySet {
    bits: u128,
}

impl CategorySet {
    /// Create an empty category set (no categories).
    pub fn new() -> Self {
        CategorySet { bits: 0 }
    }

    /// Create a category set from a string of category mnemonic characters.
    ///
    /// Any characters outside the valid range `' '`..=`'~'` are silently
    /// ignored. This mirrors `Fmake_category_set` in Emacs.
    pub fn from_str(categories: &str) -> Self {
        let mut set = CategorySet::new();
        for ch in categories.chars() {
            if is_valid_category(ch) {
                set.set(ch);
            }
        }
        set
    }

    /// Add `category` to the set.
    ///
    /// # Panics
    /// Panics if `category` is not a valid category character (`' '`..=`'~'`).
    pub fn set(&mut self, category: char) {
        assert!(
            is_valid_category(category),
            "invalid category: {:?}",
            category
        );
        self.bits |= 1u128 << (category as u32);
    }

    /// Remove `category` from the set.
    ///
    /// # Panics
    /// Panics if `category` is not a valid category character.
    pub fn unset(&mut self, category: char) {
        assert!(
            is_valid_category(category),
            "invalid category: {:?}",
            category
        );
        self.bits &= !(1u128 << (category as u32));
    }

    /// Return `true` if the set contains `category`.
    ///
    /// # Panics
    /// Panics if `category` is not a valid category character.
    pub fn has(&self, category: char) -> bool {
        assert!(
            is_valid_category(category),
            "invalid category: {:?}",
            category
        );
        self.bits & (1u128 << (category as u32)) != 0
    }

    /// Return the union of `self` and `other`.
    pub fn union(&self, other: &CategorySet) -> CategorySet {
        CategorySet {
            bits: self.bits | other.bits,
        }
    }

    /// Return the intersection of `self` and `other`.
    pub fn intersection(&self, other: &CategorySet) -> CategorySet {
        CategorySet {
            bits: self.bits & other.bits,
        }
    }

    /// Return the set difference (`self` minus `other`).
    pub fn difference(&self, other: &CategorySet) -> CategorySet {
        CategorySet {
            bits: self.bits & !other.bits,
        }
    }

    /// Return `true` if the set contains no categories.
    pub fn is_empty(&self) -> bool {
        self.bits == 0
    }

    /// Return the number of categories in the set.
    pub fn count(&self) -> u32 {
        self.bits.count_ones()
    }

    /// Return a string of category mnemonic characters in the set,
    /// in ascending order.
    ///
    /// This mirrors `Fcategory_set_mnemonics` in Emacs.
    pub fn mnemonics(&self) -> String {
        let mut s = String::with_capacity(CATEGORY_COUNT);
        for code in CATEGORY_MIN..=CATEGORY_MAX {
            if self.bits & (1u128 << code) != 0 {
                s.push(code as char);
            }
        }
        s
    }

    /// Iterate over all category characters in the set, in ascending order.
    pub fn iter(&self) -> CategorySetIter {
        CategorySetIter {
            bits: self.bits,
            pos: CATEGORY_MIN,
        }
    }
}

impl Default for CategorySet {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CategorySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CategorySet(\"{}\")", self.mnemonics())
    }
}

impl fmt::Display for CategorySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.mnemonics())
    }
}

/// Iterator over category characters in a `CategorySet`.
pub struct CategorySetIter {
    bits: u128,
    pos: u8,
}

impl Iterator for CategorySetIter {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        while self.pos <= CATEGORY_MAX {
            let p = self.pos;
            self.pos += 1;
            if self.bits & (1u128 << p) != 0 {
                return Some(p as char);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// CategoryDocstring
// ---------------------------------------------------------------------------

/// Stores documentation strings for defined categories.
///
/// In Emacs, this is a vector of 95 elements in the category table's
/// first extra slot. Here we use a `HashMap` for clarity.
#[derive(Debug, Clone)]
pub struct CategoryDocstring {
    docs: HashMap<char, String>,
}

impl CategoryDocstring {
    /// Create an empty docstring store.
    pub fn new() -> Self {
        CategoryDocstring {
            docs: HashMap::new(),
        }
    }

    /// Define a category with its documentation string.
    ///
    /// # Panics
    /// Panics if `category` is not a valid category character.
    pub fn define_category(&mut self, category: char, docstring: &str) {
        assert!(
            is_valid_category(category),
            "invalid category: {:?}",
            category
        );
        self.docs.insert(category, docstring.to_string());
    }

    /// Return the documentation string for `category`, or `None` if the
    /// category has not been defined.
    pub fn get_docstring(&self, category: char) -> Option<&str> {
        self.docs.get(&category).map(|s| s.as_str())
    }

    /// Return `true` if `category` has been defined (has a docstring).
    pub fn is_defined(&self, category: char) -> bool {
        self.docs.contains_key(&category)
    }

    /// Return the first unused (undefined) category character, or `None`
    /// if all 95 categories are in use.
    ///
    /// This mirrors `Fget_unused_category` in Emacs.
    pub fn get_unused_category(&self) -> Option<char> {
        for code in CATEGORY_MIN..=CATEGORY_MAX {
            let ch = code as char;
            if !self.docs.contains_key(&ch) {
                return Some(ch);
            }
        }
        None
    }

    /// Return the number of defined categories.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Return `true` if no categories have been defined.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
}

impl Default for CategoryDocstring {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// CategoryTable
// ---------------------------------------------------------------------------

/// A mapping from Unicode characters to category sets.
///
/// Characters not explicitly mapped return an empty `CategorySet` (no
/// categories). The table also stores docstrings for each defined
/// category.
///
/// This mirrors the Emacs `char-table` with purpose `category-table`.
#[derive(Debug, Clone)]
pub struct CategoryTable {
    /// Per-character category sets (sparse).
    entries: HashMap<char, CategorySet>,
    /// Default category set for unmapped characters.
    default_set: CategorySet,
    /// Documentation strings for categories.
    docstrings: CategoryDocstring,
}

impl CategoryTable {
    /// Create an empty category table. All characters have an empty
    /// category set by default.
    pub fn empty() -> Self {
        CategoryTable {
            entries: HashMap::new(),
            default_set: CategorySet::new(),
            docstrings: CategoryDocstring::new(),
        }
    }

    /// Create a new category table pre-populated with the standard Emacs
    /// categories and character assignments.
    ///
    /// This is equivalent to calling `standard()`.
    pub fn new() -> Self {
        standard()
    }

    /// Get the category set for a character.
    ///
    /// Returns the explicitly assigned set, or the default (empty) set
    /// if the character has no explicit mapping.
    pub fn get_categories(&self, ch: char) -> CategorySet {
        self.entries.get(&ch).copied().unwrap_or(self.default_set)
    }

    /// Set or unset a single category for a character.
    ///
    /// If `val` is `true`, the category is added; otherwise it is removed.
    ///
    /// # Panics
    /// Panics if `category` is not a valid category character.
    pub fn set_category(&mut self, ch: char, category: char, val: bool) {
        assert!(
            is_valid_category(category),
            "invalid category: {:?}",
            category
        );
        let set = self.entries.entry(ch).or_insert(self.default_set);
        if val {
            set.set(category);
        } else {
            set.unset(category);
        }
    }

    /// Set or unset a category for an inclusive range of characters.
    ///
    /// This mirrors `Fmodify_category_entry` with a cons range argument.
    ///
    /// # Panics
    /// Panics if `category` is not a valid category character, or if
    /// `from > to`.
    pub fn set_category_range(&mut self, from: char, to: char, category: char, val: bool) {
        assert!(from <= to, "invalid range: {:?}..={:?}", from, to);
        assert!(
            is_valid_category(category),
            "invalid category: {:?}",
            category
        );
        for code in (from as u32)..=(to as u32) {
            if let Some(ch) = char::from_u32(code) {
                let set = self.entries.entry(ch).or_insert(self.default_set);
                if val {
                    set.set(category);
                } else {
                    set.unset(category);
                }
            }
        }
    }

    /// Set the default category set returned for unmapped characters.
    pub fn set_default(&mut self, set: CategorySet) {
        self.default_set = set;
    }

    /// Return a reference to the docstring store.
    pub fn docstrings(&self) -> &CategoryDocstring {
        &self.docstrings
    }

    /// Return a mutable reference to the docstring store.
    pub fn docstrings_mut(&mut self) -> &mut CategoryDocstring {
        &mut self.docstrings
    }

    /// Define a category with its documentation string.
    ///
    /// Convenience method equivalent to `self.docstrings_mut().define_category(...)`.
    pub fn define_category(&mut self, category: char, docstring: &str) {
        self.docstrings.define_category(category, docstring);
    }

    /// Return the number of characters with explicit category mappings.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return `true` if no characters have explicit category mappings.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Create a deep copy of this category table.
    ///
    /// Both the character-to-category-set mapping and the docstrings
    /// are independently cloned.
    pub fn copy(&self) -> CategoryTable {
        self.clone()
    }
}

impl Default for CategoryTable {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Utility Functions
// ---------------------------------------------------------------------------

/// Return `true` if `ch` is a valid category mnemonic character
/// (printable ASCII, `' '` through `'~'`).
pub fn is_valid_category(ch: char) -> bool {
    let code = ch as u32;
    code >= CATEGORY_MIN as u32 && code <= CATEGORY_MAX as u32
}

/// Return `true` if the character `ch` belongs to `category` in the
/// given category table.
pub fn char_has_category(table: &CategoryTable, ch: char, category: char) -> bool {
    table.get_categories(ch).has(category)
}

/// Determine whether there is a word boundary between two word-constituent
/// characters `c1` and `c2` when they appear in this order.
///
/// The algorithm mirrors the logic in `word_boundary_p()` from Emacs's
/// `category.c`:
///
/// 1. If `c1` and `c2` belong to the same "script" (approximated here by
///    whether both are ASCII or both are non-ASCII), we check
///    `separating_categories` for a pair `(CAT1, CAT2)` where `c1` has
///    `CAT1` but not `CAT2`, and `c2` has `CAT2` but not `CAT1`. A match
///    means there IS a word boundary.
///
/// 2. If they belong to different scripts, we check `combining_categories`
///    for a pair `(CAT1, CAT2)` with the same condition. A match means
///    there is NOT a word boundary.
///
/// A `None` in either position of a pair acts as a wildcard: the condition
/// on that side is satisfied if the character does not have the *other*
/// category.
pub fn word_boundary_p(
    table: &CategoryTable,
    c1: char,
    c2: char,
    separating_categories: &[(Option<char>, Option<char>)],
    combining_categories: &[(Option<char>, Option<char>)],
) -> bool {
    let same_script = is_same_script(c1, c2);

    let (rules, default_result) = if same_script {
        (separating_categories, false)
    } else {
        (combining_categories, true)
    };

    let set1 = table.get_categories(c1);
    let set2 = table.get_categories(c2);

    if set1.is_empty() || set2.is_empty() {
        return default_result;
    }

    for &(cat1, cat2) in rules {
        let left_ok = match cat1 {
            None => {
                // Wildcard: c1 must not have cat2 (if cat2 is Some).
                match cat2 {
                    Some(c) => !set1.has(c),
                    None => true,
                }
            }
            Some(c) => {
                // c1 must have cat1 AND c1 must not have cat2 (if defined).
                set1.has(c) && !set2.has(c)
            }
        };

        let right_ok = match cat2 {
            None => {
                // Wildcard: c2 must not have cat1 (if cat1 is Some).
                match cat1 {
                    Some(c) => !set2.has(c),
                    None => true,
                }
            }
            Some(c) => {
                // c2 must have cat2 AND c2 must not have cat1 (if defined).
                set2.has(c) && !set1.has(c)
            }
        };

        if left_ok && right_ok {
            return !default_result;
        }
    }

    default_result
}

/// Simple heuristic for whether two characters belong to the same
/// "script". In full Emacs, this uses `char-script-table`; here we
/// approximate by checking whether both are single-byte (ASCII/Latin-1)
/// or both are multibyte.
fn is_same_script(c1: char, c2: char) -> bool {
    let single1 = (c1 as u32) < 0x100;
    let single2 = (c2 as u32) < 0x100;
    single1 == single2
}

// ---------------------------------------------------------------------------
// Standard Category Table
// ---------------------------------------------------------------------------

/// Build and return the standard Emacs category table.
///
/// This sets up the category definitions and character assignments that
/// Emacs performs in `init_category_once()` (C) and `characters.el` (Lisp).
///
/// The standard categories include:
/// - `'a'` — ASCII graphic characters (32-126)
/// - `'l'` — Latin
/// - `'c'` — Chinese
/// - `'j'` — Japanese
/// - `'k'` — Katakana
/// - `'h'` — Korean
/// - `'g'` — Greek
/// - `'y'` — Cyrillic
/// - `'t'` — Thai
/// - ... and many more (see `characters.el`)
///
/// At minimum, ASCII characters are assigned categories `'a'` and `'l'`.
pub fn standard() -> CategoryTable {
    let mut table = CategoryTable::empty();

    // -- Define categories (docstrings) --
    table.define_category(
        'a',
        "ASCII\nASCII graphic characters 32-126 (ISO646 IRV:1983[4/0])",
    );
    table.define_category('l', "Latin");
    table.define_category('t', "Thai");
    table.define_category('g', "Greek");
    table.define_category('b', "Arabic");
    table.define_category('w', "Hebrew");
    table.define_category('y', "Cyrillic");
    table.define_category('k', "Katakana\nJapanese katakana");
    table.define_category('r', "Roman\nJapanese roman");
    table.define_category('c', "Chinese");
    table.define_category('j', "Japanese");
    table.define_category('h', "Korean");
    table.define_category('e', "Ethiopic\nEthiopic (Ge'ez)");
    table.define_category('v', "Viet\nVietnamese");
    table.define_category('i', "Indian");
    table.define_category('o', "Lao");
    table.define_category('q', "Tibetan");

    table.define_category(
        'A',
        "2-byte alnum\nAlphanumeric characters of 2-byte character sets",
    );
    table.define_category(
        'C',
        "2-byte han\nChinese (Han) characters of 2-byte character sets",
    );
    table.define_category(
        'G',
        "2-byte Greek\nGreek characters of 2-byte character sets",
    );
    table.define_category(
        'H',
        "2-byte Hiragana\nJapanese Hiragana characters of 2-byte character sets",
    );
    table.define_category(
        'K',
        "2-byte Katakana\nJapanese Katakana characters of 2-byte character sets",
    );
    table.define_category(
        'N',
        "2-byte Korean\nKorean Hangul characters of 2-byte character sets",
    );
    table.define_category(
        'Y',
        "2-byte Cyrillic\nCyrillic characters of 2-byte character sets",
    );
    table.define_category('I', "Indian Glyphs");

    table.define_category('0', "consonant");
    table.define_category('1', "base vowel\nBase (independent) vowel");
    table.define_category(
        '2',
        "upper diacritic\nUpper diacritical mark (including upper vowel)",
    );
    table.define_category(
        '3',
        "lower diacritic\nLower diacritical mark (including lower vowel)",
    );
    table.define_category('4', "combining tone\nCombining tone mark");
    table.define_category('5', "symbol");
    table.define_category('6', "digit");
    table.define_category('7', "vowel diacritic\nVowel-modifying diacritical mark");
    table.define_category('8', "vowel-signs");
    table.define_category('9', "semivowel lower");

    table.define_category(
        '|',
        "line breakable\nWhile filling, we can break a line at this character.",
    );
    table.define_category(
        ' ',
        "space for indent\nThis character counts as a space for indentation purposes.",
    );
    table.define_category(
        '>',
        "Not at bol\nA character which can't be placed at beginning of line.",
    );
    table.define_category(
        '<',
        "Not at eol\nA character which can't be placed at end of line.",
    );
    table.define_category(
        '.',
        "Base\nBase characters (Unicode General Category L,N,P,S,Zs)",
    );
    table.define_category(
        '^',
        "Combining\nCombining diacritic or mark (Unicode General Category M)",
    );
    table.define_category(
        'R',
        "Strong R2L\nCharacters with strong right-to-left directionality.",
    );
    table.define_category(
        'L',
        "Strong L2R\nCharacters with strong left-to-right directionality.",
    );

    // -- Assign ASCII characters to categories 'a' and 'l' --
    // (modify-category-entry '(32 . 127) ?a)
    // (modify-category-entry '(32 . 127) ?l)
    // Note: Emacs uses 32..127 inclusive; char 127 (DEL) is included.
    table.set_category_range(' ', '\x7F', 'a', true);
    table.set_category_range(' ', '\x7F', 'l', true);

    // -- Assign CJK ranges to relevant categories --
    // Chinese characters (Unicode blocks).
    set_range_safe(&mut table, 0x2E80, 0x312F, '|');
    set_range_safe(&mut table, 0x3190, 0x33FF, '|');
    set_range_safe(&mut table, 0x3400, 0x4DBF, 'C');
    set_range_safe(&mut table, 0x4E00, 0x9FFF, 'C');
    set_range_safe(&mut table, 0x3400, 0x9FFF, 'c');
    set_range_safe(&mut table, 0x3400, 0x9FFF, '|');
    set_range_safe(&mut table, 0xF900, 0xFAFF, 'C');
    set_range_safe(&mut table, 0xF900, 0xFAFF, 'c');
    set_range_safe(&mut table, 0xF900, 0xFAFF, '|');

    // Japanese Hiragana.
    set_range_safe(&mut table, 0x3040, 0x309F, 'j');
    set_range_safe(&mut table, 0x3040, 0x309F, 'H');

    // Japanese Katakana.
    set_range_safe(&mut table, 0x30A0, 0x30FF, 'j');
    set_range_safe(&mut table, 0x30A0, 0x30FF, 'K');
    set_range_safe(&mut table, 0x30A0, 0x30FF, 'k');

    // Korean Hangul Syllables.
    set_range_safe(&mut table, 0xAC00, 0xD7AF, 'h');
    set_range_safe(&mut table, 0xAC00, 0xD7AF, 'N');

    // Greek.
    set_range_safe(&mut table, 0x0370, 0x03FF, 'g');
    set_range_safe(&mut table, 0x0370, 0x03FF, 'G');

    // Cyrillic.
    set_range_safe(&mut table, 0x0400, 0x04FF, 'y');
    set_range_safe(&mut table, 0x0400, 0x04FF, 'Y');

    // Thai.
    set_range_safe(&mut table, 0x0E00, 0x0E7F, 't');

    // Arabic.
    set_range_safe(&mut table, 0x0600, 0x06FF, 'b');

    // Hebrew.
    set_range_safe(&mut table, 0x0590, 0x05FF, 'w');

    // Latin Extended.
    set_range_safe(&mut table, 0x0080, 0x024F, 'l');

    table
}

/// Helper: set a category for a range of code points, skipping any
/// invalid `char` values (e.g. surrogates).
fn set_range_safe(table: &mut CategoryTable, from: u32, to: u32, category: char) {
    for code in from..=to {
        if let Some(ch) = char::from_u32(code) {
            let set = table.entries.entry(ch).or_insert(table.default_set);
            set.set(category);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- is_valid_category --

    #[test]
    fn test_valid_category_space() {
        assert!(is_valid_category(' '));
    }

    #[test]
    fn test_valid_category_tilde() {
        assert!(is_valid_category('~'));
    }

    #[test]
    fn test_valid_category_letters_and_digits() {
        assert!(is_valid_category('a'));
        assert!(is_valid_category('Z'));
        assert!(is_valid_category('0'));
        assert!(is_valid_category('9'));
    }

    #[test]
    fn test_invalid_category_control_char() {
        assert!(!is_valid_category('\x1F')); // below space
        assert!(!is_valid_category('\x7F')); // DEL
        assert!(!is_valid_category('\0'));
    }

    #[test]
    fn test_invalid_category_non_ascii() {
        assert!(!is_valid_category('\u{00E9}')); // e-acute
        assert!(!is_valid_category('\u{4E00}')); // CJK char
    }

    // -- CategorySet basics --

    #[test]
    fn test_category_set_empty() {
        let set = CategorySet::new();
        assert!(set.is_empty());
        assert_eq!(set.count(), 0);
        assert_eq!(set.mnemonics(), "");
    }

    #[test]
    fn test_category_set_set_and_has() {
        let mut set = CategorySet::new();
        set.set('a');
        assert!(set.has('a'));
        assert!(!set.has('b'));
        assert!(!set.is_empty());
        assert_eq!(set.count(), 1);
    }

    #[test]
    fn test_category_set_unset() {
        let mut set = CategorySet::new();
        set.set('a');
        set.set('b');
        assert!(set.has('a'));
        assert!(set.has('b'));

        set.unset('a');
        assert!(!set.has('a'));
        assert!(set.has('b'));
        assert_eq!(set.count(), 1);
    }

    #[test]
    fn test_category_set_multiple_categories() {
        let mut set = CategorySet::new();
        set.set('a');
        set.set('l');
        set.set('C');
        assert!(set.has('a'));
        assert!(set.has('l'));
        assert!(set.has('C'));
        assert!(!set.has('b'));
        assert_eq!(set.count(), 3);
    }

    #[test]
    fn test_category_set_from_str() {
        let set = CategorySet::from_str("alC");
        assert!(set.has('a'));
        assert!(set.has('l'));
        assert!(set.has('C'));
        assert!(!set.has('b'));
        assert_eq!(set.count(), 3);
    }

    #[test]
    fn test_category_set_mnemonics_order() {
        let mut set = CategorySet::new();
        set.set('z');
        set.set('a');
        set.set('m');
        // Mnemonics should be in ascending ASCII order.
        assert_eq!(set.mnemonics(), "amz");
    }

    #[test]
    fn test_category_set_union() {
        let mut s1 = CategorySet::new();
        s1.set('a');
        s1.set('b');

        let mut s2 = CategorySet::new();
        s2.set('b');
        s2.set('c');

        let u = s1.union(&s2);
        assert!(u.has('a'));
        assert!(u.has('b'));
        assert!(u.has('c'));
        assert_eq!(u.count(), 3);
    }

    #[test]
    fn test_category_set_intersection() {
        let mut s1 = CategorySet::new();
        s1.set('a');
        s1.set('b');

        let mut s2 = CategorySet::new();
        s2.set('b');
        s2.set('c');

        let i = s1.intersection(&s2);
        assert!(!i.has('a'));
        assert!(i.has('b'));
        assert!(!i.has('c'));
        assert_eq!(i.count(), 1);
    }

    #[test]
    fn test_category_set_difference() {
        let mut s1 = CategorySet::new();
        s1.set('a');
        s1.set('b');
        s1.set('c');

        let mut s2 = CategorySet::new();
        s2.set('b');

        let d = s1.difference(&s2);
        assert!(d.has('a'));
        assert!(!d.has('b'));
        assert!(d.has('c'));
        assert_eq!(d.count(), 2);
    }

    #[test]
    fn test_category_set_iter() {
        let mut set = CategorySet::new();
        set.set('c');
        set.set('a');
        set.set('b');
        let cats: Vec<char> = set.iter().collect();
        assert_eq!(cats, vec!['a', 'b', 'c']);
    }

    #[test]
    fn test_category_set_all_95_categories() {
        let mut set = CategorySet::new();
        for code in b' '..=b'~' {
            set.set(code as char);
        }
        assert_eq!(set.count(), 95);
        // Verify all are present.
        for code in b' '..=b'~' {
            assert!(set.has(code as char));
        }
    }

    #[test]
    fn test_category_set_display() {
        let set = CategorySet::from_str("abc");
        assert_eq!(format!("{}", set), "abc");
    }

    // -- CategoryDocstring --

    #[test]
    fn test_docstring_define_and_get() {
        let mut docs = CategoryDocstring::new();
        docs.define_category('a', "ASCII characters");
        assert_eq!(docs.get_docstring('a'), Some("ASCII characters"));
        assert_eq!(docs.get_docstring('b'), None);
    }

    #[test]
    fn test_docstring_is_defined() {
        let mut docs = CategoryDocstring::new();
        assert!(!docs.is_defined('a'));
        docs.define_category('a', "ASCII");
        assert!(docs.is_defined('a'));
    }

    #[test]
    fn test_docstring_get_unused_category() {
        let docs = CategoryDocstring::new();
        // First unused should be space (the lowest valid category).
        assert_eq!(docs.get_unused_category(), Some(' '));
    }

    #[test]
    fn test_docstring_get_unused_category_with_some_defined() {
        let mut docs = CategoryDocstring::new();
        docs.define_category(' ', "space");
        docs.define_category('!', "excl");
        // Next unused should be '"'.
        assert_eq!(docs.get_unused_category(), Some('"'));
    }

    #[test]
    fn test_docstring_len() {
        let mut docs = CategoryDocstring::new();
        assert_eq!(docs.len(), 0);
        assert!(docs.is_empty());
        docs.define_category('a', "ASCII");
        assert_eq!(docs.len(), 1);
        assert!(!docs.is_empty());
    }

    // -- CategoryTable --

    #[test]
    fn test_table_empty() {
        let table = CategoryTable::empty();
        assert!(table.is_empty());
        assert!(table.get_categories('a').is_empty());
    }

    #[test]
    fn test_table_set_and_get_category() {
        let mut table = CategoryTable::empty();
        table.set_category('A', 'a', true);
        assert!(table.get_categories('A').has('a'));
        assert!(!table.get_categories('A').has('b'));
        assert!(!table.get_categories('B').has('a'));
    }

    #[test]
    fn test_table_set_category_range() {
        let mut table = CategoryTable::empty();
        table.set_category_range('a', 'z', 'l', true);
        for ch in 'a'..='z' {
            assert!(
                table.get_categories(ch).has('l'),
                "expected 'l' for '{}'",
                ch
            );
        }
        // Characters outside range should not have the category.
        assert!(!table.get_categories('A').has('l'));
    }

    #[test]
    fn test_table_unset_category() {
        let mut table = CategoryTable::empty();
        table.set_category('x', 'a', true);
        table.set_category('x', 'l', true);
        assert!(table.get_categories('x').has('a'));
        assert!(table.get_categories('x').has('l'));

        table.set_category('x', 'a', false);
        assert!(!table.get_categories('x').has('a'));
        assert!(table.get_categories('x').has('l'));
    }

    #[test]
    fn test_table_copy() {
        let mut table = CategoryTable::empty();
        table.define_category('a', "ASCII");
        table.set_category('X', 'a', true);

        let copy = table.copy();
        assert!(copy.get_categories('X').has('a'));
        assert_eq!(copy.docstrings().get_docstring('a'), Some("ASCII"));

        // Modifying the original should not affect the copy.
        table.set_category('X', 'a', false);
        assert!(copy.get_categories('X').has('a'));
    }

    // -- Standard table --

    #[test]
    fn test_standard_ascii_categories() {
        let table = standard();
        // All ASCII printable characters should have 'a' and 'l'.
        for code in 32u8..=127 {
            let ch = code as char;
            let cats = table.get_categories(ch);
            assert!(cats.has('a'), "expected 'a' for char {}", code);
            assert!(cats.has('l'), "expected 'l' for char {}", code);
        }
    }

    #[test]
    fn test_standard_non_ascii_no_ascii_category() {
        let table = standard();
        // A non-ASCII character should NOT have 'a'.
        assert!(!table.get_categories('\u{4E00}').has('a'));
    }

    #[test]
    fn test_standard_cjk_categories() {
        let table = standard();
        // CJK Unified Ideograph should have 'C' and 'c'.
        let ch = '\u{4E00}'; // first CJK unified ideograph
        let cats = table.get_categories(ch);
        assert!(cats.has('C'), "expected 'C' for U+4E00");
        assert!(cats.has('c'), "expected 'c' for U+4E00");
    }

    #[test]
    fn test_standard_greek_categories() {
        let table = standard();
        // Greek capital letter alpha.
        let ch = '\u{0391}';
        let cats = table.get_categories(ch);
        assert!(cats.has('g'), "expected 'g' for U+0391");
        assert!(cats.has('G'), "expected 'G' for U+0391");
    }

    #[test]
    fn test_standard_docstrings_defined() {
        let table = standard();
        assert!(table.docstrings().is_defined('a'));
        assert!(table.docstrings().is_defined('l'));
        assert!(table.docstrings().is_defined('c'));
        assert!(table.docstrings().is_defined('j'));
        assert!(table.docstrings().is_defined('k'));
        assert!(table.docstrings().is_defined('h'));
    }

    // -- char_has_category utility --

    #[test]
    fn test_char_has_category_util() {
        let table = standard();
        assert!(char_has_category(&table, 'A', 'a'));
        assert!(char_has_category(&table, 'A', 'l'));
        assert!(!char_has_category(&table, 'A', 'c'));
    }

    // -- word_boundary_p --

    #[test]
    fn test_word_boundary_same_script_no_rules() {
        let table = standard();
        // Two ASCII chars, same script, no separating rules -> no boundary.
        let result = word_boundary_p(&table, 'a', 'b', &[], &[]);
        assert!(!result);
    }

    #[test]
    fn test_word_boundary_different_script_no_rules() {
        let table = standard();
        // ASCII vs CJK, different script, no combining rules -> boundary.
        let result = word_boundary_p(&table, 'a', '\u{4E00}', &[], &[]);
        assert!(result);
    }

    #[test]
    fn test_word_boundary_separating_rule_match() {
        let mut table = CategoryTable::empty();
        table.define_category('H', "Hiragana");
        table.define_category('K', "Katakana");
        // Both in same script range (non-ASCII).
        table.set_category('\u{3042}', 'H', true); // Hiragana 'a'
        table.set_category('\u{30A2}', 'K', true); // Katakana 'a'

        // Separating rule: (?H . ?K) => boundary between Hiragana and Katakana.
        let sep = vec![(Some('H'), Some('K'))];
        let result = word_boundary_p(&table, '\u{3042}', '\u{30A2}', &sep, &[]);
        assert!(result);
    }

    #[test]
    fn test_word_boundary_combining_rule_match() {
        let mut table = CategoryTable::empty();
        table.define_category('C', "CJK");
        table.define_category('a', "ASCII");
        // c1 = ASCII char with 'a', c2 = non-ASCII with 'C'.
        table.set_category('x', 'a', true);
        table.set_category('\u{4E00}', 'C', true);

        // Without combining rule: different scripts -> boundary.
        assert!(word_boundary_p(&table, 'x', '\u{4E00}', &[], &[]));

        // With combining rule (nil . ?C): combine when c2 has 'C'.
        let comb = vec![(None, Some('C'))];
        let result = word_boundary_p(&table, 'x', '\u{4E00}', &[], &comb);
        assert!(!result);
    }

    #[test]
    fn test_word_boundary_empty_category_sets() {
        let table = CategoryTable::empty();
        // Both chars have empty category sets -> default result.
        // Same script (both ASCII) -> default false.
        assert!(!word_boundary_p(&table, 'a', 'b', &[], &[]));
    }

    // -- Edge cases --

    #[test]
    fn test_category_set_idempotent_set() {
        let mut set = CategorySet::new();
        set.set('a');
        set.set('a'); // setting again should be harmless
        assert!(set.has('a'));
        assert_eq!(set.count(), 1);
    }

    #[test]
    fn test_category_set_unset_absent() {
        let mut set = CategorySet::new();
        set.unset('a'); // unsetting something not there should be fine
        assert!(!set.has('a'));
        assert!(set.is_empty());
    }

    #[test]
    #[should_panic(expected = "invalid category")]
    fn test_category_set_set_invalid_panics() {
        let mut set = CategorySet::new();
        set.set('\x7F'); // DEL is not a valid category
    }

    #[test]
    #[should_panic(expected = "invalid category")]
    fn test_category_set_has_invalid_panics() {
        let set = CategorySet::new();
        set.has('\x7F');
    }

    #[test]
    fn test_table_set_category_range_unicode() {
        let mut table = CategoryTable::empty();
        // Set a category for a small Unicode range.
        table.set_category_range('\u{0370}', '\u{0377}', 'g', true);
        assert!(table.get_categories('\u{0370}').has('g'));
        assert!(table.get_categories('\u{0377}').has('g'));
        assert!(!table.get_categories('\u{0378}').has('g'));
    }

    #[test]
    #[should_panic(expected = "invalid range")]
    fn test_table_set_category_range_invalid_panics() {
        let mut table = CategoryTable::empty();
        table.set_category_range('z', 'a', 'l', true);
    }

    #[test]
    fn test_category_set_equality() {
        let s1 = CategorySet::from_str("abc");
        let s2 = CategorySet::from_str("cba");
        assert_eq!(s1, s2);
    }

    #[test]
    fn test_category_set_hash_consistency() {
        use std::collections::HashSet;
        let s1 = CategorySet::from_str("abc");
        let s2 = CategorySet::from_str("cba");
        let mut hs = HashSet::new();
        hs.insert(s1);
        assert!(hs.contains(&s2));
    }

    #[test]
    fn test_standard_table_latin_extended() {
        let table = standard();
        // Latin Extended-A character should have 'l'.
        assert!(table.get_categories('\u{0100}').has('l'));
    }

    #[test]
    fn test_standard_table_cyrillic() {
        let table = standard();
        let ch = '\u{0410}'; // Cyrillic capital A
        let cats = table.get_categories(ch);
        assert!(cats.has('y'), "expected 'y' for U+0410");
        assert!(cats.has('Y'), "expected 'Y' for U+0410");
    }

    #[test]
    fn test_standard_table_hangul() {
        let table = standard();
        let ch = '\u{AC00}'; // first Hangul syllable
        let cats = table.get_categories(ch);
        assert!(cats.has('h'), "expected 'h' for U+AC00");
        assert!(cats.has('N'), "expected 'N' for U+AC00");
    }

    #[test]
    fn test_default_set_propagation() {
        let mut table = CategoryTable::empty();
        let mut def = CategorySet::new();
        def.set('x');
        table.set_default(def);

        // Unmapped character should get the default.
        assert!(table.get_categories('Z').has('x'));

        // Explicitly mapped character should NOT inherit default
        // unless it was initialized from it.
        table.set_category('A', 'a', true);
        // 'A' was initialized from default (which has 'x'), then 'a' added.
        assert!(table.get_categories('A').has('x'));
        assert!(table.get_categories('A').has('a'));
    }
}
