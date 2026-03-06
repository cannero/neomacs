//! Pure Rust char-table: a hierarchical, space-efficient map from
//! Unicode codepoints to values.
//!
//! This is a faithful reimplementation of the Emacs char-table data
//! structure defined in `src/chartab.c`.  The Emacs char-table uses a
//! 4-level tree whose branching factors and coverage come from the
//! `CHARTAB_SIZE_BITS` constants in `lisp.h`:
//!
//! | Level | Bits | Entries | Chars per entry |
//! |-------|------|---------|-----------------|
//! |   0   |  6   |   64   |     65 536      |
//! |   1   |  4   |   16   |      4 096      |
//! |   2   |  5   |   32   |        128      |
//! |   3   |  7   |  128   |          1      |
//!
//! The total addressable range is 64 * 65 536 = 4 194 304 (0x400000),
//! which covers Emacs's `MAX_CHAR` (0x3FFFFF).  For the purposes of
//! this Rust implementation we restrict input to valid `char` values
//! (0..=0x10FFFF).
//!
//! Sub-tables are allocated lazily: a top-level slot that has never
//! been individually set simply stores the table's default value
//! directly.  When a `set` touches a slot that is still "flat", the
//! required intermediate sub-tables are created on the fly.
//!
//! # ASCII fast-path
//!
//! The first 128 codepoints (ASCII) are stored in a separate inline
//! array so that lookups for the overwhelmingly common case can avoid
//! descending through three levels of indirection.

use std::fmt;

// ---------------------------------------------------------------------------
// Constants (mirroring lisp.h CHARTAB_SIZE_BITS)
// ---------------------------------------------------------------------------

/// Number of bits at each tree level.
const BITS: [u32; 4] = [6, 4, 5, 7];

/// Number of entries at each level: 64, 16, 32, 128.
const SIZE: [usize; 4] = [1 << BITS[0], 1 << BITS[1], 1 << BITS[2], 1 << BITS[3]];

/// Number of codepoints covered by one entry at each level.
const CHARS_PER_ENTRY: [u32; 4] = [
    1 << (BITS[1] + BITS[2] + BITS[3]), // 65536
    1 << (BITS[2] + BITS[3]),           // 4096
    1 << BITS[3],                       // 128
    1,                                  // 1
];

/// Bit-shift to compute the index at a given depth.
const SHIFT: [u32; 4] = [
    BITS[1] + BITS[2] + BITS[3], // 16
    BITS[2] + BITS[3],           // 12
    BITS[3],                     // 7
    0,                           // 0
];

/// Maximum codepoint we accept (inclusive).  This is the maximum
/// valid Unicode scalar value.
const MAX_CODEPOINT: u32 = 0x10FFFF;

/// Number of ASCII codepoints stored in the fast-path array.
const ASCII_SIZE: usize = 128;

// ---------------------------------------------------------------------------
// Index computation
// ---------------------------------------------------------------------------

/// Compute the index into a sub-table at `depth` for codepoint `c`,
/// where `min_char` is the first codepoint covered by that sub-table.
#[inline]
fn chartab_idx(c: u32, depth: usize, min_char: u32) -> usize {
    ((c - min_char) >> SHIFT[depth]) as usize
}

// ---------------------------------------------------------------------------
// Sub-table types (levels 1, 2, 3)
// ---------------------------------------------------------------------------

/// Bottom-level table (depth 3): 128 individual codepoint slots.
#[derive(Clone)]
struct SubTable3<V: Clone> {
    entries: Vec<Option<V>>, // length = SIZE[3] = 128
}

impl<V: Clone> SubTable3<V> {
    fn new() -> Self {
        Self {
            entries: vec![None; SIZE[3]],
        }
    }

    /// Returns true if all entries are `None`.
    fn all_none(&self) -> bool {
        self.entries.iter().all(Option::is_none)
    }
}

/// Mid-level table (depth 2): 32 slots, each either `None` (meaning
/// "use parent default") or a boxed `SubTable3`.
#[derive(Clone)]
struct SubTable2<V: Clone> {
    entries: Vec<Option<Box<SubTable3<V>>>>, // length = SIZE[2] = 32
}

impl<V: Clone> SubTable2<V> {
    fn new() -> Self {
        Self {
            entries: vec![None; SIZE[2]],
        }
    }

    fn all_none(&self) -> bool {
        self.entries.iter().all(Option::is_none)
    }
}

/// Upper-mid-level table (depth 1): 16 slots, each either `None` or
/// a boxed `SubTable2`.
#[derive(Clone)]
struct SubTable1<V: Clone> {
    entries: Vec<Option<Box<SubTable2<V>>>>, // length = SIZE[1] = 16
}

impl<V: Clone> SubTable1<V> {
    fn new() -> Self {
        Self {
            entries: vec![None; SIZE[1]],
        }
    }

    fn all_none(&self) -> bool {
        self.entries.iter().all(Option::is_none)
    }
}

// ---------------------------------------------------------------------------
// CharTable
// ---------------------------------------------------------------------------

/// A hierarchical char-table mapping codepoints to values of type `V`.
///
/// Lookups for unmapped codepoints return the `default_value`.
/// Sub-tables are only allocated when a `set` or `set_range` touches
/// individual codepoints within a block.
#[derive(Clone)]
pub struct CharTable<V: Clone> {
    /// Value returned for codepoints that have not been explicitly set.
    default_value: Option<V>,

    /// Fast-path storage for ASCII (codepoints 0..127).
    ascii: Vec<Option<V>>,

    /// Top-level table (depth 0): 64 slots.  Each slot is either
    /// `None` (meaning "use default") or a boxed `SubTable1`.
    top: Vec<Option<Box<SubTable1<V>>>>,
}

impl<V: Clone + fmt::Debug> fmt::Debug for CharTable<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CharTable")
            .field("default_value", &self.default_value)
            .field("num_entries", &self.iter().count())
            .finish()
    }
}

impl<V: Clone> CharTable<V> {
    // -- Construction -------------------------------------------------------

    /// Create a new, empty char-table with the given default value.
    pub fn new(default: Option<V>) -> Self {
        Self {
            default_value: default,
            ascii: vec![None; ASCII_SIZE],
            top: vec![None; SIZE[0]],
        }
    }

    /// Return the default value.
    pub fn default_value(&self) -> Option<&V> {
        self.default_value.as_ref()
    }

    /// Set the default value.
    pub fn set_default(&mut self, value: Option<V>) {
        self.default_value = value;
    }

    // -- Lookup -------------------------------------------------------------

    /// Look up the value for codepoint `ch`.
    ///
    /// If the codepoint has been explicitly set, the stored value is
    /// returned.  Otherwise the table's default value is returned.
    pub fn get(&self, ch: char) -> Option<&V> {
        let c = ch as u32;

        // ASCII fast-path.
        if c < ASCII_SIZE as u32 {
            if let Some(ref v) = self.ascii[c as usize] {
                return Some(v);
            }
            return self.default_value.as_ref();
        }

        // Walk the tree.
        let i0 = chartab_idx(c, 0, 0);
        if let Some(ref sub1) = self.top[i0] {
            let min1 = (i0 as u32) * CHARS_PER_ENTRY[0];
            let i1 = chartab_idx(c, 1, min1);
            if let Some(ref sub2) = sub1.entries[i1] {
                let min2 = min1 + (i1 as u32) * CHARS_PER_ENTRY[1];
                let i2 = chartab_idx(c, 2, min2);
                if let Some(ref sub3) = sub2.entries[i2] {
                    let min3 = min2 + (i2 as u32) * CHARS_PER_ENTRY[2];
                    let i3 = chartab_idx(c, 3, min3);
                    if let Some(ref v) = sub3.entries[i3] {
                        return Some(v);
                    }
                }
            }
        }

        self.default_value.as_ref()
    }

    // -- Mutation -----------------------------------------------------------

    /// Set the value for a single codepoint.
    pub fn set(&mut self, ch: char, value: V) {
        let c = ch as u32;

        // ASCII fast-path.
        if c < ASCII_SIZE as u32 {
            self.ascii[c as usize] = Some(value);
            return;
        }

        let i0 = chartab_idx(c, 0, 0);
        let min0 = (i0 as u32) * CHARS_PER_ENTRY[0];

        let sub1 = self.top[i0].get_or_insert_with(|| Box::new(SubTable1::new()));

        let i1 = chartab_idx(c, 1, min0);
        let min1 = min0 + (i1 as u32) * CHARS_PER_ENTRY[1];

        let sub2 = sub1.entries[i1].get_or_insert_with(|| Box::new(SubTable2::new()));

        let i2 = chartab_idx(c, 2, min1);
        let min2 = min1 + (i2 as u32) * CHARS_PER_ENTRY[2];

        let sub3 = sub2.entries[i2].get_or_insert_with(|| Box::new(SubTable3::new()));

        let i3 = chartab_idx(c, 3, min2);
        sub3.entries[i3] = Some(value);
    }

    /// Set the value for all codepoints in the range `[from, to]`
    /// (inclusive on both ends).
    ///
    /// Like the C implementation, this tries to set at the highest
    /// possible tree level when an entire sub-block is covered, and
    /// only descends into sub-tables when a partial block must be
    /// handled.
    pub fn set_range(&mut self, from: char, to: char, value: V) {
        let from_u = from as u32;
        let to_u = to as u32;

        if from_u > to_u {
            return;
        }

        if from_u == to_u {
            self.set(from, value);
            return;
        }

        // Handle the ASCII portion directly.
        if from_u < ASCII_SIZE as u32 {
            let ascii_end = std::cmp::min(to_u, (ASCII_SIZE - 1) as u32);
            for c in from_u..=ascii_end {
                self.ascii[c as usize] = Some(value.clone());
            }
            if to_u < ASCII_SIZE as u32 {
                return;
            }
        }

        // Walk top-level slots that overlap [from_u, to_u].
        let i0_start = chartab_idx(from_u, 0, 0);
        let i0_end = chartab_idx(to_u, 0, 0);

        for i0 in i0_start..=i0_end {
            let block_start = (i0 as u32) * CHARS_PER_ENTRY[0];
            let block_end = block_start + CHARS_PER_ENTRY[0] - 1;

            if block_start > to_u {
                break;
            }

            // If the whole top-level block is covered AND the slot
            // has no existing sub-table, we can skip creating one.
            // But we always need to fill leaf entries, so we must
            // descend.  However, if the entire block is covered we
            // can fill every leaf uniformly (or we can just mark the
            // block as "all set to value" -- but to keep the tree
            // simple we only use sub-tables).  Like Emacs, when a
            // full sub-block is covered we set the slot to the value
            // at the highest possible level.  Since our tree uses
            // Option<Box<SubTableN>>, there is no way to store a
            // single value at a non-leaf level.  Instead, when the
            // full block at level 0 is covered, we drop any existing
            // sub-table and later `get` will fall through to default
            // (assuming the value equals default).  For the general
            // case, we descend.
            if from_u <= block_start && block_end <= to_u {
                // Entire top-level block covered -- set every leaf.
                self.set_range_full_block(i0, &value);
            } else {
                // Partial block -- descend into level 1.
                let sub1 = self.top[i0].get_or_insert_with(|| Box::new(SubTable1::new()));
                let eff_from = std::cmp::max(from_u, block_start);
                let eff_to = std::cmp::min(to_u, block_end);
                Self::set_range_level1(sub1, block_start, eff_from, eff_to, &value);
            }
        }
    }

    /// Fill an entire top-level block (all codepoints in its range)
    /// with `value`.
    fn set_range_full_block(&mut self, i0: usize, value: &V) {
        let sub1 = self.top[i0].get_or_insert_with(|| Box::new(SubTable1::new()));
        let min0 = (i0 as u32) * CHARS_PER_ENTRY[0];
        for i1 in 0..SIZE[1] {
            let sub2 = sub1.entries[i1].get_or_insert_with(|| Box::new(SubTable2::new()));
            let min1 = min0 + (i1 as u32) * CHARS_PER_ENTRY[1];
            for i2 in 0..SIZE[2] {
                let sub3 = sub2.entries[i2].get_or_insert_with(|| Box::new(SubTable3::new()));
                let _min2 = min1 + (i2 as u32) * CHARS_PER_ENTRY[2];
                for i3 in 0..SIZE[3] {
                    sub3.entries[i3] = Some(value.clone());
                }
            }
        }
    }

    /// Set a range within a level-1 sub-table.
    fn set_range_level1(sub1: &mut SubTable1<V>, min0: u32, from: u32, to: u32, value: &V) {
        let i1_start = chartab_idx(from, 1, min0);
        let i1_end = chartab_idx(to, 1, min0);

        for i1 in i1_start..=i1_end {
            let block_start = min0 + (i1 as u32) * CHARS_PER_ENTRY[1];
            let block_end = block_start + CHARS_PER_ENTRY[1] - 1;

            if block_start > to {
                break;
            }

            if from <= block_start && block_end <= to {
                // Entire level-1 block covered.
                let sub2 = sub1.entries[i1].get_or_insert_with(|| Box::new(SubTable2::new()));
                for i2 in 0..SIZE[2] {
                    let sub3 = sub2.entries[i2].get_or_insert_with(|| Box::new(SubTable3::new()));
                    for i3 in 0..SIZE[3] {
                        sub3.entries[i3] = Some(value.clone());
                    }
                }
            } else {
                let sub2 = sub1.entries[i1].get_or_insert_with(|| Box::new(SubTable2::new()));
                let eff_from = std::cmp::max(from, block_start);
                let eff_to = std::cmp::min(to, block_end);
                Self::set_range_level2(sub2, block_start, eff_from, eff_to, value);
            }
        }
    }

    /// Set a range within a level-2 sub-table.
    fn set_range_level2(sub2: &mut SubTable2<V>, min1: u32, from: u32, to: u32, value: &V) {
        let i2_start = chartab_idx(from, 2, min1);
        let i2_end = chartab_idx(to, 2, min1);

        for i2 in i2_start..=i2_end {
            let block_start = min1 + (i2 as u32) * CHARS_PER_ENTRY[2];
            let block_end = block_start + CHARS_PER_ENTRY[2] - 1;

            if block_start > to {
                break;
            }

            if from <= block_start && block_end <= to {
                // Entire level-2 block covered.
                let sub3 = sub2.entries[i2].get_or_insert_with(|| Box::new(SubTable3::new()));
                for i3 in 0..SIZE[3] {
                    sub3.entries[i3] = Some(value.clone());
                }
            } else {
                let sub3 = sub2.entries[i2].get_or_insert_with(|| Box::new(SubTable3::new()));
                let eff_from = std::cmp::max(from, block_start);
                let eff_to = std::cmp::min(to, block_end);
                for c in eff_from..=eff_to {
                    let min3 = block_start;
                    let i3 = chartab_idx(c, 3, min3);
                    sub3.entries[i3] = Some(value.clone());
                }
            }
        }
    }

    /// Remove the mapping for a single codepoint, reverting it to the
    /// default value.
    pub fn remove(&mut self, ch: char) {
        let c = ch as u32;

        if c < ASCII_SIZE as u32 {
            self.ascii[c as usize] = None;
            return;
        }

        let i0 = chartab_idx(c, 0, 0);
        if let Some(ref mut sub1) = self.top[i0] {
            let min0 = (i0 as u32) * CHARS_PER_ENTRY[0];
            let i1 = chartab_idx(c, 1, min0);
            if let Some(ref mut sub2) = sub1.entries[i1] {
                let min1 = min0 + (i1 as u32) * CHARS_PER_ENTRY[1];
                let i2 = chartab_idx(c, 2, min1);
                if let Some(ref mut sub3) = sub2.entries[i2] {
                    let min2 = min1 + (i2 as u32) * CHARS_PER_ENTRY[2];
                    let i3 = chartab_idx(c, 3, min2);
                    sub3.entries[i3] = None;
                }
            }
        }
    }

    // -- Iteration ----------------------------------------------------------

    /// Iterate over all codepoints that have an explicitly set (non-default)
    /// value, yielding `(char, &V)` pairs in codepoint order.
    pub fn iter(&self) -> CharTableIter<'_, V> {
        CharTableIter {
            table: self,
            next_codepoint: 0,
        }
    }

    /// Call `f` for each explicitly mapped codepoint in the range
    /// `[from, to]` (inclusive).
    pub fn map_over_range(&self, from: char, to: char, mut f: impl FnMut(char, &V)) {
        let from_u = from as u32;
        let to_u = to as u32;

        for c in from_u..=to_u {
            // Safety: we only iterate valid codepoints within the
            // char range, but some u32 values in the surrogate range
            // (0xD800..=0xDFFF) are not valid chars.  Skip them.
            if let Some(ch) = char::from_u32(c) {
                if let Some(v) = self.get_explicit(ch) {
                    f(ch, v);
                }
            }
        }
    }

    /// Return the explicitly set value (not falling back to default).
    fn get_explicit(&self, ch: char) -> Option<&V> {
        let c = ch as u32;

        if c < ASCII_SIZE as u32 {
            return self.ascii[c as usize].as_ref();
        }

        let i0 = chartab_idx(c, 0, 0);
        if let Some(ref sub1) = self.top[i0] {
            let min0 = (i0 as u32) * CHARS_PER_ENTRY[0];
            let i1 = chartab_idx(c, 1, min0);
            if let Some(ref sub2) = sub1.entries[i1] {
                let min1 = min0 + (i1 as u32) * CHARS_PER_ENTRY[1];
                let i2 = chartab_idx(c, 2, min1);
                if let Some(ref sub3) = sub2.entries[i2] {
                    let min2 = min1 + (i2 as u32) * CHARS_PER_ENTRY[2];
                    let i3 = chartab_idx(c, 3, min2);
                    return sub3.entries[i3].as_ref();
                }
            }
        }

        None
    }

    // -- Compaction ---------------------------------------------------------

    /// Remove sub-tables where every leaf entry is `None` (i.e. all
    /// entries equal the parent default).  This reclaims memory after
    /// large ranges have been `remove`d.
    pub fn compact(&mut self)
    where
        V: PartialEq,
    {
        // Compact ASCII: nothing to do structurally (flat array).

        for i0 in 0..SIZE[0] {
            let should_drop = if let Some(ref mut sub1) = self.top[i0] {
                Self::compact_level1(sub1);
                sub1.all_none()
            } else {
                false
            };
            if should_drop {
                self.top[i0] = None;
            }
        }
    }

    fn compact_level1(sub1: &mut SubTable1<V>)
    where
        V: PartialEq,
    {
        for i1 in 0..SIZE[1] {
            let should_drop = if let Some(ref mut sub2) = sub1.entries[i1] {
                Self::compact_level2(sub2);
                sub2.all_none()
            } else {
                false
            };
            if should_drop {
                sub1.entries[i1] = None;
            }
        }
    }

    fn compact_level2(sub2: &mut SubTable2<V>)
    where
        V: PartialEq,
    {
        for i2 in 0..SIZE[2] {
            let should_drop = if let Some(ref sub3) = sub2.entries[i2] {
                sub3.all_none()
            } else {
                false
            };
            if should_drop {
                sub2.entries[i2] = None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Iterator
// ---------------------------------------------------------------------------

/// An iterator over the explicitly mapped entries of a [`CharTable`].
pub struct CharTableIter<'a, V: Clone> {
    table: &'a CharTable<V>,
    next_codepoint: u32,
}

impl<'a, V: Clone> Iterator for CharTableIter<'a, V> {
    type Item = (char, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        while self.next_codepoint <= MAX_CODEPOINT {
            let c = self.next_codepoint;
            self.next_codepoint += 1;

            // Skip surrogate range.
            if (0xD800..=0xDFFF).contains(&c) {
                continue;
            }

            let ch = unsafe { char::from_u32_unchecked(c) };
            if let Some(v) = self.table.get_explicit(ch) {
                return Some((ch, v));
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// CharTableExtra
// ---------------------------------------------------------------------------

/// A char-table with additional metadata slots.
///
/// Emacs char-tables can have "extra slots" (0..10) used for storing
/// auxiliary information such as the table's purpose, parent, encoder/
/// decoder functions, etc.  This wrapper pairs a [`CharTable`] with a
/// `Vec` of extra slot values.
#[derive(Clone)]
pub struct CharTableExtra<V: Clone> {
    /// The underlying char-table.
    pub table: CharTable<V>,

    /// Extra metadata slots.
    extras: Vec<Option<V>>,
}

impl<V: Clone + fmt::Debug> fmt::Debug for CharTableExtra<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CharTableExtra")
            .field("table", &self.table)
            .field("num_extras", &self.extras.len())
            .finish()
    }
}

impl<V: Clone> CharTableExtra<V> {
    /// Create a new char-table with `n_extras` extra slots, all
    /// initialized to `None`.
    pub fn new(default: Option<V>, n_extras: usize) -> Self {
        Self {
            table: CharTable::new(default),
            extras: vec![None; n_extras],
        }
    }

    /// Number of extra slots.
    pub fn num_extras(&self) -> usize {
        self.extras.len()
    }

    /// Get the value in extra slot `idx`.
    ///
    /// # Panics
    ///
    /// Panics if `idx >= num_extras()`.
    pub fn get_extra(&self, idx: usize) -> Option<&V> {
        self.extras[idx].as_ref()
    }

    /// Set the value in extra slot `idx`.
    ///
    /// # Panics
    ///
    /// Panics if `idx >= num_extras()`.
    pub fn set_extra(&mut self, idx: usize, value: V) {
        self.extras[idx] = Some(value);
    }

    /// Clear extra slot `idx` back to `None`.
    ///
    /// # Panics
    ///
    /// Panics if `idx >= num_extras()`.
    pub fn clear_extra(&mut self, idx: usize) {
        self.extras[idx] = None;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helpers ---------------------------------------------------------------

    fn make_table() -> CharTable<u32> {
        CharTable::new(None)
    }

    fn make_table_with_default(val: u32) -> CharTable<u32> {
        CharTable::new(Some(val))
    }

    // 1. New table is empty -------------------------------------------------

    #[test]
    fn test_new_table_is_empty() {
        let t = make_table();
        assert!(t.iter().next().is_none());
    }

    // 2. Default value returned for unmapped codepoint ----------------------

    #[test]
    fn test_default_value() {
        let t = make_table_with_default(42);
        assert_eq!(t.get('A'), Some(&42));
        assert_eq!(t.get('\u{4e00}'), Some(&42)); // CJK
    }

    // 3. Set and get a single ASCII char ------------------------------------

    #[test]
    fn test_set_get_ascii() {
        let mut t = make_table();
        t.set('a', 1);
        assert_eq!(t.get('a'), Some(&1));
        assert_eq!(t.get('b'), None);
    }

    // 4. Set and get a BMP char ---------------------------------------------

    #[test]
    fn test_set_get_bmp() {
        let mut t = make_table();
        t.set('\u{4e00}', 100); // CJK Unified Ideograph
        assert_eq!(t.get('\u{4e00}'), Some(&100));
        assert_eq!(t.get('\u{4e01}'), None);
    }

    // 5. Set and get a supplementary plane char -----------------------------

    #[test]
    fn test_set_get_supplementary() {
        let mut t = make_table();
        t.set('\u{1F600}', 200); // Grinning Face emoji
        assert_eq!(t.get('\u{1F600}'), Some(&200));
        assert_eq!(t.get('\u{1F601}'), None);
    }

    // 6. Set overrides previous value ---------------------------------------

    #[test]
    fn test_set_overrides() {
        let mut t = make_table();
        t.set('x', 10);
        assert_eq!(t.get('x'), Some(&10));
        t.set('x', 20);
        assert_eq!(t.get('x'), Some(&20));
    }

    // 7. Remove resets to default -------------------------------------------

    #[test]
    fn test_remove() {
        let mut t = make_table_with_default(0);
        t.set('z', 99);
        assert_eq!(t.get('z'), Some(&99));
        t.remove('z');
        assert_eq!(t.get('z'), Some(&0)); // falls back to default
    }

    // 8. Remove on unmapped codepoint is a no-op ----------------------------

    #[test]
    fn test_remove_unmapped() {
        let mut t = make_table();
        t.remove('Q'); // should not panic
        assert_eq!(t.get('Q'), None);
    }

    // 9. Set range within ASCII ---------------------------------------------

    #[test]
    fn test_set_range_ascii() {
        let mut t = make_table();
        t.set_range('a', 'z', 1);
        for c in 'a'..='z' {
            assert_eq!(t.get(c), Some(&1), "failed for {:?}", c);
        }
        assert_eq!(t.get('A'), None);
        assert_eq!(t.get('`'), None);
        assert_eq!(t.get('{'), None);
    }

    // 10. Set range spanning ASCII and non-ASCII ----------------------------

    #[test]
    fn test_set_range_cross_ascii_boundary() {
        let mut t = make_table();
        // ASCII ends at 127; this range spans 120..=140.
        t.set_range('\u{78}', '\u{8C}', 7);
        for c in 0x78u32..=0x8C {
            let ch = char::from_u32(c).unwrap();
            assert_eq!(t.get(ch), Some(&7), "failed for U+{:04X}", c);
        }
        assert_eq!(t.get('\u{77}'), None);
        assert_eq!(t.get('\u{8D}'), None);
    }

    // 11. Set range covering a full top-level block -------------------------

    #[test]
    fn test_set_range_full_block() {
        let mut t = make_table();
        // Cover the entire second top-level block: U+10000..U+1FFFF.
        t.set_range('\u{10000}', '\u{1FFFF}', 5);
        assert_eq!(t.get('\u{10000}'), Some(&5));
        assert_eq!(t.get('\u{1ABCD}'), Some(&5));
        assert_eq!(t.get('\u{1FFFF}'), Some(&5));
        assert_eq!(t.get('\u{0FFFF}'), None);
        assert_eq!(t.get('\u{20000}'), None);
    }

    // 12. Set range single codepoint ----------------------------------------

    #[test]
    fn test_set_range_single() {
        let mut t = make_table();
        t.set_range('Q', 'Q', 42);
        assert_eq!(t.get('Q'), Some(&42));
        assert_eq!(t.get('P'), None);
        assert_eq!(t.get('R'), None);
    }

    // 13. Set range reversed is a no-op ------------------------------------

    #[test]
    fn test_set_range_reversed() {
        let mut t = make_table();
        t.set_range('z', 'a', 1); // from > to
        assert_eq!(t.get('a'), None);
        assert_eq!(t.get('z'), None);
    }

    // 14. Iteration returns all set codepoints in order ---------------------

    #[test]
    fn test_iter_order() {
        let mut t = make_table();
        t.set('\u{1F600}', 3);
        t.set('a', 1);
        t.set('\u{4e00}', 2);

        let entries: Vec<(char, u32)> = t.iter().map(|(c, v)| (c, *v)).collect();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], ('a', 1));
        assert_eq!(entries[1], ('\u{4e00}', 2));
        assert_eq!(entries[2], ('\u{1F600}', 3));
    }

    // 15. Iteration skips default-value entries -----------------------------

    #[test]
    fn test_iter_skips_defaults() {
        let mut t = make_table_with_default(0);
        t.set('a', 1);
        // 'b' is not explicitly set; iter should not yield it.
        let entries: Vec<_> = t.iter().collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], ('a', &1));
    }

    // 16. map_over_range covers the requested range -------------------------

    #[test]
    fn test_map_over_range() {
        let mut t = make_table();
        t.set('a', 1);
        t.set('c', 3);
        t.set('e', 5);
        t.set('g', 7);

        let mut seen = Vec::new();
        t.map_over_range('b', 'f', |ch, v| {
            seen.push((ch, *v));
        });

        assert_eq!(seen, vec![('c', 3), ('e', 5)]);
    }

    // 17. Compact removes empty sub-tables ----------------------------------

    #[test]
    fn test_compact_removes_empty() {
        let mut t = make_table();
        t.set('\u{4e00}', 1);
        t.remove('\u{4e00}');

        // The sub-table chain still exists but is all None.
        assert!(t.top[chartab_idx(0x4e00, 0, 0)].is_some());
        t.compact();
        assert!(t.top[chartab_idx(0x4e00, 0, 0)].is_none());
    }

    // 18. Compact preserves non-empty entries -------------------------------

    #[test]
    fn test_compact_preserves_entries() {
        let mut t = make_table();
        t.set('\u{4e00}', 1);
        t.set('\u{4e01}', 2);
        t.remove('\u{4e01}');
        t.compact();
        assert_eq!(t.get('\u{4e00}'), Some(&1));
    }

    // 19. CharTableExtra: basic extra slot operations -----------------------

    #[test]
    fn test_extra_slots() {
        let mut te = CharTableExtra::<u32>::new(None, 5);
        assert_eq!(te.num_extras(), 5);
        assert_eq!(te.get_extra(0), None);

        te.set_extra(2, 42);
        assert_eq!(te.get_extra(2), Some(&42));

        te.clear_extra(2);
        assert_eq!(te.get_extra(2), None);
    }

    // 20. CharTableExtra: table operations still work -----------------------

    #[test]
    fn test_extra_table_operations() {
        let mut te = CharTableExtra::<String>::new(Some("default".into()), 3);
        te.table.set('A', "alpha".into());
        assert_eq!(te.table.get('A'), Some(&"alpha".to_string()));
        assert_eq!(te.table.get('B'), Some(&"default".to_string()));
    }

    // 21. Set and get at Unicode maximum ------------------------------------

    #[test]
    fn test_max_unicode() {
        let mut t = make_table();
        t.set('\u{10FFFF}', 999);
        assert_eq!(t.get('\u{10FFFF}'), Some(&999));
    }

    // 22. Set range across multiple top-level blocks ------------------------

    #[test]
    fn test_set_range_multiple_top_blocks() {
        let mut t = make_table();
        // This range spans from mid-block-0 into block-1.
        t.set_range('\u{FF00}', '\u{10100}', 8);
        assert_eq!(t.get('\u{FF00}'), Some(&8));
        assert_eq!(t.get('\u{FFFF}'), Some(&8));
        assert_eq!(t.get('\u{10000}'), Some(&8));
        assert_eq!(t.get('\u{10100}'), Some(&8));
        assert_eq!(t.get('\u{10101}'), None);
        assert_eq!(t.get('\u{FEFF}'), None);
    }

    // 23. Iter count matches number of set codepoints -----------------------

    #[test]
    fn test_iter_count() {
        let mut t = make_table();
        for c in 0u32..256 {
            if let Some(ch) = char::from_u32(c) {
                t.set(ch, c);
            }
        }
        let count = t.iter().count();
        assert_eq!(count, 256);
    }

    // 24. Default value accessor --------------------------------------------

    #[test]
    fn test_default_value_accessor() {
        let t = make_table_with_default(7);
        assert_eq!(t.default_value(), Some(&7));

        let t2 = make_table();
        assert_eq!(t2.default_value(), None);
    }

    // 25. set_default changes default behavior ------------------------------

    #[test]
    fn test_set_default() {
        let mut t = make_table();
        assert_eq!(t.get('A'), None);

        t.set_default(Some(100));
        assert_eq!(t.get('A'), Some(&100));

        t.set('A', 200);
        assert_eq!(t.get('A'), Some(&200));

        t.remove('A');
        assert_eq!(t.get('A'), Some(&100));
    }

    // 26. String values work (V = String) -----------------------------------

    #[test]
    fn test_string_values() {
        let mut t: CharTable<String> = CharTable::new(None);
        t.set('a', "alpha".into());
        t.set('\u{3B1}', "greek alpha".into());

        assert_eq!(t.get('a'), Some(&"alpha".to_string()));
        assert_eq!(t.get('\u{3B1}'), Some(&"greek alpha".to_string()));
        assert_eq!(t.get('b'), None);
    }

    // 27. Large range set performance (should not panic) --------------------

    #[test]
    fn test_large_range() {
        let mut t = make_table();
        // Set all of BMP.
        t.set_range('\u{0}', '\u{FFFF}', 1);
        assert_eq!(t.get('\0'), Some(&1));
        assert_eq!(t.get('\u{FFFF}'), Some(&1));
        assert_eq!(t.get('\u{10000}'), None);
    }

    // 28. Overwrite range with different value ------------------------------

    #[test]
    fn test_overwrite_range() {
        let mut t = make_table();
        t.set_range('a', 'z', 1);
        t.set_range('m', 'z', 2);

        assert_eq!(t.get('a'), Some(&1));
        assert_eq!(t.get('l'), Some(&1));
        assert_eq!(t.get('m'), Some(&2));
        assert_eq!(t.get('z'), Some(&2));
    }

    // 29. Remove ASCII then compact -----------------------------------------

    #[test]
    fn test_remove_ascii() {
        let mut t = make_table();
        t.set('A', 10);
        assert_eq!(t.get('A'), Some(&10));
        t.remove('A');
        assert_eq!(t.get('A'), None);
    }

    // 30. Compact on already-compact table is idempotent --------------------

    #[test]
    fn test_compact_idempotent() {
        let mut t = make_table();
        t.set('A', 1);
        t.compact();
        t.compact();
        assert_eq!(t.get('A'), Some(&1));
    }

    // 31. Clone produces independent copy -----------------------------------

    #[test]
    fn test_clone_independence() {
        let mut t = make_table();
        t.set('A', 1);
        t.set('\u{4e00}', 2);

        let mut t2 = t.clone();
        t2.set('A', 99);
        t2.set('\u{4e00}', 88);

        // Original should be unchanged.
        assert_eq!(t.get('A'), Some(&1));
        assert_eq!(t.get('\u{4e00}'), Some(&2));

        // Clone should have new values.
        assert_eq!(t2.get('A'), Some(&99));
        assert_eq!(t2.get('\u{4e00}'), Some(&88));
    }

    // 32. map_over_range with no matches yields nothing ---------------------

    #[test]
    fn test_map_over_range_empty() {
        let t = make_table();
        let mut count = 0;
        t.map_over_range('a', 'z', |_, _| count += 1);
        assert_eq!(count, 0);
    }

    // 33. Extra slot out of bounds panics -----------------------------------

    #[test]
    #[should_panic]
    fn test_extra_slot_out_of_bounds() {
        let te = CharTableExtra::<u32>::new(None, 3);
        te.get_extra(3); // should panic
    }

    // 34. Null char (U+0000) works correctly --------------------------------

    #[test]
    fn test_null_char() {
        let mut t = make_table();
        t.set('\0', 42);
        assert_eq!(t.get('\0'), Some(&42));
        t.remove('\0');
        assert_eq!(t.get('\0'), None);
    }

    // 35. Set range then iterate collects correct set -----------------------

    #[test]
    fn test_set_range_then_iter() {
        let mut t = make_table();
        t.set_range('\u{100}', '\u{104}', 7);

        let entries: Vec<(char, u32)> = t.iter().map(|(c, v)| (c, *v)).collect();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].0, '\u{100}');
        assert_eq!(entries[4].0, '\u{104}');
        for (_, v) in &entries {
            assert_eq!(*v, 7);
        }
    }
}
