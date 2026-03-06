//! Pure Rust composite character system.
//!
//! Implements character composition for displaying sequences of characters
//! as a single composite glyph. This is the Rust equivalent of Emacs's
//! `composite.c` / `composite.h`.
//!
//! # Composition Methods
//!
//! Characters can be composed in four ways:
//! - **Relative**: Components positioned relative to each other by metrics
//! - **WithRule**: Components positioned by explicit composition rules
//! - **WithAltChars**: Alternative character representation (relative)
//! - **WithRuleAltChars**: Alternative characters with explicit rules
//!
//! # Reference Points
//!
//! Rule-based composition uses a 3x4 grid of reference points on each glyph:
//! ```text
//!   0---1---2    (ascent)
//!   |       |
//!   |       |
//!   |       |
//!   9--10--11   (center)
//!   |       |
//!   3---4---5    (baseline)
//!   |       |
//!   6---7---8    (descent)
//! ```

use std::collections::HashMap;

/// Maximum number of components in a single composition.
pub const MAX_COMPOSITION_COMPONENTS: usize = 16;

/// Maximum number of characters to look back for auto-compositions.
pub const MAX_AUTO_COMPOSITION_LOOKBACK: usize = 3;

// ---------------------------------------------------------------------------
// 1. Composition Method
// ---------------------------------------------------------------------------

/// How a sequence of component characters is composed for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompositionMethod {
    /// Compose relatively without alternate characters.
    /// Components are stacked/overlaid according to their font metrics.
    Relative,

    /// Compose by specified composition rules.
    /// Each pair of adjacent components has an explicit positioning rule.
    /// (Legacy format from older Emacs versions.)
    WithRule,

    /// Compose relatively with alternate characters.
    /// The display uses alternate glyphs but positions them relatively.
    WithAltChars,

    /// Compose by specified composition rules with alternate characters.
    /// Both explicit rules and alternate glyphs are used.
    WithRuleAltChars,
}

// ---------------------------------------------------------------------------
// 2. Reference Points
// ---------------------------------------------------------------------------

/// A reference point on a glyph bounding box.
///
/// The 12 reference points form a 3-column x 4-row grid:
/// ```text
///   TopLeft(0)     TopCenter(1)     TopRight(2)       -- ascent line
///   BaseLeft(3)    BaseCenter(4)    BaseRight(5)       -- baseline
///   BottomLeft(6)  BottomCenter(7)  BottomRight(8)     -- descent line
///   MiddleLeft(9)  MiddleCenter(10) MiddleRight(11)    -- vertical center
/// ```
///
/// Emacs encodes these as integers 0..11.  The column is `index % 3`
/// and the row determines the vertical anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ReferencePoint {
    TopLeft = 0,
    TopCenter = 1,
    TopRight = 2,
    BaselineLeft = 3,
    BaselineCenter = 4,
    BaselineRight = 5,
    BottomLeft = 6,
    BottomCenter = 7,
    BottomRight = 8,
    MiddleLeft = 9,
    MiddleCenter = 10,
    MiddleRight = 11,
}

impl ReferencePoint {
    /// Decode a numeric reference point (0..11) into the enum.
    /// Values outside 0..11 are clamped to `MiddleRight` (11).
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => ReferencePoint::TopLeft,
            1 => ReferencePoint::TopCenter,
            2 => ReferencePoint::TopRight,
            3 => ReferencePoint::BaselineLeft,
            4 => ReferencePoint::BaselineCenter,
            5 => ReferencePoint::BaselineRight,
            6 => ReferencePoint::BottomLeft,
            7 => ReferencePoint::BottomCenter,
            8 => ReferencePoint::BottomRight,
            9 => ReferencePoint::MiddleLeft,
            10 => ReferencePoint::MiddleCenter,
            _ => ReferencePoint::MiddleRight,
        }
    }

    /// The column (0=left, 1=center, 2=right) of this reference point.
    #[inline]
    pub fn column(self) -> u8 {
        (self as u8) % 3
    }

    /// Compute the horizontal fraction (0.0 = left, 0.5 = center, 1.0 = right).
    #[inline]
    pub fn x_fraction(self) -> f64 {
        self.column() as f64 / 2.0
    }

    /// Return `true` if `gref` and `nref` are both valid reference points.
    pub fn is_valid_pair(gref: u8, nref: u8) -> bool {
        gref < 12 && nref < 12
    }
}

// ---------------------------------------------------------------------------
// 3. Composition Rule
// ---------------------------------------------------------------------------

/// A positioning rule that describes how to place a new component glyph
/// relative to the previously composed glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositionRule {
    /// Where on the previously composed glyph group to anchor.
    pub global_ref: ReferencePoint,
    /// Where on the new component glyph to anchor.
    pub new_ref: ReferencePoint,
    /// Horizontal pixel offset (from the extended encoding).
    pub x_offset: i16,
    /// Vertical pixel offset (from the extended encoding).
    pub y_offset: i16,
}

impl CompositionRule {
    /// Create a rule with only reference points (no pixel offsets).
    pub fn new(global_ref: ReferencePoint, new_ref: ReferencePoint) -> Self {
        Self {
            global_ref,
            new_ref,
            x_offset: 0,
            y_offset: 0,
        }
    }

    /// Create a rule with reference points and pixel offsets.
    pub fn with_offsets(
        global_ref: ReferencePoint,
        new_ref: ReferencePoint,
        x_offset: i16,
        y_offset: i16,
    ) -> Self {
        Self {
            global_ref,
            new_ref,
            x_offset,
            y_offset,
        }
    }

    /// Encode the rule as a single integer, matching the Emacs encoding:
    /// `(xoff << 16) | (yoff << 8) | (gref * 12 + nref)`.
    pub fn encode(&self) -> u32 {
        let refs = (self.global_ref as u32) * 12 + (self.new_ref as u32);
        let yoff = (self.y_offset as u8) as u32;
        let xoff = (self.x_offset as u16) as u32;
        (xoff << 16) | (yoff << 8) | refs
    }

    /// Decode a rule from the Emacs integer encoding.
    ///
    /// Format: `(xoff << 16) | (yoff << 8) | (gref * 12 + nref)`.
    pub fn decode(rule_code: u32) -> Self {
        let x_offset = (rule_code >> 16) as i16;
        let y_offset = ((rule_code >> 8) & 0xFF) as i16;
        let refs = (rule_code & 0xFF) as u8;
        let gref_val = refs / 12;
        let nref_val = refs % 12;
        // Clamp gref to valid range (Emacs does: if gref > 12, gref = 11)
        let gref_val = if gref_val > 11 { 11 } else { gref_val };
        Self {
            global_ref: ReferencePoint::from_code(gref_val),
            new_ref: ReferencePoint::from_code(nref_val),
            x_offset,
            y_offset,
        }
    }

    /// Decode only the reference points from a simple rule code (low 8 bits).
    pub fn decode_refs(rule_code: u8) -> (ReferencePoint, ReferencePoint) {
        let gref_val = rule_code / 12;
        let nref_val = rule_code % 12;
        let gref_val = if gref_val > 11 { 11 } else { gref_val };
        (
            ReferencePoint::from_code(gref_val),
            ReferencePoint::from_code(nref_val),
        )
    }

    /// Encode a pair of reference points into a single byte.
    pub fn encode_refs(gref: ReferencePoint, nref: ReferencePoint) -> u8 {
        (gref as u8) * 12 + (nref as u8)
    }

    /// Compute the horizontal position of the new component given the
    /// current composed extent `[leftmost, rightmost)` and the new
    /// component's `width`.
    ///
    /// Returns the left edge of the new component.
    pub fn compute_x(&self, leftmost: f64, rightmost: f64, new_width: f64) -> f64 {
        let gref_col = self.global_ref.column() as f64;
        let nref_col = self.new_ref.column() as f64;
        leftmost + gref_col * (rightmost - leftmost) / 2.0 - nref_col * new_width / 2.0
            + self.x_offset as f64
    }
}

// ---------------------------------------------------------------------------
// 4. Composition
// ---------------------------------------------------------------------------

/// A registered composition of characters.
///
/// Once registered in a [`CompositionTable`], a composition gets a unique
/// numeric ID and can be looked up cheaply.
#[derive(Debug, Clone)]
pub struct Composition {
    /// Unique identifier (index into the composition table).
    pub id: u32,
    /// How the components are composed.
    pub method: CompositionMethod,
    /// The component characters.
    pub components: Vec<char>,
    /// Positioning rules (one fewer than components for rule-based methods,
    /// empty for relative methods).
    pub rules: Vec<CompositionRule>,
    /// Number of characters this composition replaces in the buffer.
    pub char_count: usize,
    /// Display width in columns (computed from component widths and rules).
    pub width: usize,
    /// Per-glyph x/y offsets (in pixels), two entries per glyph: [x0, y0, x1, y1, ...].
    /// Filled in by the font/rendering layer.
    pub offsets: Vec<i16>,
}

impl Composition {
    /// Return the number of glyphs this composition produces.
    pub fn glyph_len(&self) -> usize {
        self.components.len()
    }

    /// Return the Nth component character, or `None` if out of range.
    pub fn glyph(&self, n: usize) -> Option<char> {
        self.components.get(n).copied()
    }

    /// Return the Nth composition rule (between component N and N+1).
    pub fn rule(&self, n: usize) -> Option<&CompositionRule> {
        self.rules.get(n)
    }
}

// ---------------------------------------------------------------------------
// 5. Composition Table
// ---------------------------------------------------------------------------

/// Registry of all known compositions, indexed by composition ID.
///
/// This is the Rust equivalent of Emacs's `composition_table` +
/// `composition_hash_table`.
#[derive(Debug, Clone)]
pub struct CompositionTable {
    /// All registered compositions, indexed by ID.
    compositions: Vec<Composition>,
    /// Reverse lookup: component chars -> composition ID.
    lookup: HashMap<Vec<char>, u32>,
}

impl CompositionTable {
    /// Create an empty composition table.
    pub fn new() -> Self {
        Self {
            compositions: Vec::new(),
            lookup: HashMap::new(),
        }
    }

    /// Register a new composition and return its ID.
    ///
    /// If an identical composition (same components) already exists, the
    /// existing ID is returned.
    pub fn register(
        &mut self,
        components: Vec<char>,
        method: CompositionMethod,
        rules: Vec<CompositionRule>,
        char_count: usize,
    ) -> u32 {
        // Check for existing identical composition.
        if let Some(&existing_id) = self.lookup.get(&components) {
            return existing_id;
        }

        let id = self.compositions.len() as u32;
        let width = Self::compute_width(&components, &rules, method);
        let glyph_len = components.len();

        let composition = Composition {
            id,
            method,
            components: components.clone(),
            rules,
            char_count,
            width,
            offsets: vec![0; glyph_len * 2],
        };

        self.compositions.push(composition);
        self.lookup.insert(components, id);
        id
    }

    /// Look up a composition by its ID.
    pub fn get(&self, id: u32) -> Option<&Composition> {
        self.compositions.get(id as usize)
    }

    /// Look up a composition by its ID (mutable).
    pub fn get_mut(&mut self, id: u32) -> Option<&mut Composition> {
        self.compositions.get_mut(id as usize)
    }

    /// Find a composition by its component characters.
    pub fn find(&self, components: &[char]) -> Option<u32> {
        self.lookup.get(components).copied()
    }

    /// Return the total number of registered compositions.
    pub fn len(&self) -> usize {
        self.compositions.len()
    }

    /// Return `true` if no compositions are registered.
    pub fn is_empty(&self) -> bool {
        self.compositions.is_empty()
    }

    /// Clear all registered compositions.
    pub fn clear(&mut self) {
        self.compositions.clear();
        self.lookup.clear();
    }

    /// Compute the display width of a composition from its components and rules.
    ///
    /// For relative compositions, the width is the maximum component width.
    /// For rule-based compositions, the width is computed from the rules.
    fn compute_width(
        components: &[char],
        rules: &[CompositionRule],
        method: CompositionMethod,
    ) -> usize {
        if components.is_empty() {
            return 0;
        }

        match method {
            CompositionMethod::WithRule | CompositionMethod::WithRuleAltChars => {
                Self::compute_rule_width(components, rules)
            }
            CompositionMethod::Relative | CompositionMethod::WithAltChars => {
                Self::compute_relative_width(components)
            }
        }
    }

    /// Compute width for relative composition: max width of all components.
    fn compute_relative_width(components: &[char]) -> usize {
        components
            .iter()
            .map(|&ch| {
                if ch == '\t' {
                    1
                } else {
                    char_display_width(ch)
                }
            })
            .max()
            .unwrap_or(0)
    }

    /// Compute width for rule-based composition using positioning rules.
    fn compute_rule_width(components: &[char], rules: &[CompositionRule]) -> usize {
        if components.is_empty() {
            return 0;
        }

        let first_width = if components[0] == '\t' {
            1.0
        } else {
            char_display_width(components[0]) as f64
        };

        let mut leftmost: f64 = 0.0;
        let mut rightmost: f64 = first_width;

        for (i, rule) in rules.iter().enumerate() {
            let comp_idx = i + 1;
            if comp_idx >= components.len() {
                break;
            }
            let ch = components[comp_idx];
            let this_width = if ch == '\t' {
                1.0
            } else {
                char_display_width(ch) as f64
            };

            let this_left = rule.compute_x(leftmost, rightmost, this_width);

            if this_left < leftmost {
                leftmost = this_left;
            }
            if this_left + this_width > rightmost {
                rightmost = this_left + this_width;
            }
        }

        let w = rightmost - leftmost;
        // Ceiling integer value.
        w.ceil() as usize
    }
}

impl Default for CompositionTable {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 6. Composition Cache
// ---------------------------------------------------------------------------

/// Per-buffer cache mapping buffer ranges to composition IDs.
///
/// This avoids repeated lookups of compositions that have already been
/// identified for a region of text.
#[derive(Debug, Clone)]
pub struct CompositionCache {
    /// Maps `(buffer_start, buffer_end)` to a composition ID.
    entries: HashMap<(usize, usize), u32>,
}

impl CompositionCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Insert a cache entry mapping the range `[start, end)` to `composition_id`.
    pub fn insert(&mut self, start: usize, end: usize, composition_id: u32) {
        self.entries.insert((start, end), composition_id);
    }

    /// Look up the composition ID for the range `[start, end)`.
    pub fn get(&self, start: usize, end: usize) -> Option<u32> {
        self.entries.get(&(start, end)).copied()
    }

    /// Invalidate all cache entries that overlap with `[start, end)`.
    pub fn invalidate_range(&mut self, start: usize, end: usize) {
        self.entries.retain(|&(s, e), _| e <= start || s >= end);
    }

    /// Clear the entire cache.
    pub fn invalidate_all(&mut self) {
        self.entries.clear();
    }

    /// Return the number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return `true` if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for CompositionCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 7. Auto-Composition
// ---------------------------------------------------------------------------

/// A rule for automatic composition detection.
///
/// Auto-composition rules specify patterns of characters that should be
/// automatically composed when encountered during display.
#[derive(Debug, Clone)]
pub struct AutoCompositionRule {
    /// Pattern string for matching (analogous to Emacs `composition-function-table` regex).
    pub pattern: String,
    /// Characters that trigger this auto-composition rule.
    pub chars: Vec<char>,
    /// How many characters to look back from the trigger character.
    pub lookback: usize,
}

impl AutoCompositionRule {
    /// Create a new auto-composition rule.
    pub fn new(pattern: String, chars: Vec<char>, lookback: usize) -> Self {
        Self {
            pattern,
            chars,
            lookback,
        }
    }
}

/// Check if any auto-composition rule matches at position `pos` in `text`.
///
/// Returns `Some((start, end, matched_chars))` if a match is found, where
/// `start` and `end` delimit the range of composed characters and
/// `matched_chars` are the characters in the composition.
///
/// Returns `None` if no auto-composition applies.
pub fn check_auto_composition(
    text: &[char],
    pos: usize,
    rules: &[AutoCompositionRule],
) -> Option<(usize, usize, Vec<char>)> {
    if pos >= text.len() || rules.is_empty() {
        return None;
    }

    let trigger_char = text[pos];

    for rule in rules {
        // Check if trigger character matches any of the rule's chars.
        if !rule.chars.contains(&trigger_char) {
            continue;
        }

        // Check if we have enough lookback.
        if pos < rule.lookback {
            continue;
        }

        let start = pos - rule.lookback;

        // Try to match the pattern starting at `start`.
        if !rule.pattern.is_empty() {
            // Simple prefix match: collect text from start and compare.
            let remaining: String = text[start..].iter().collect();
            if remaining.starts_with(&rule.pattern) {
                let match_len = rule.pattern.chars().count();
                let end = start + match_len;
                if end <= text.len() {
                    let matched: Vec<char> = text[start..end].to_vec();
                    return Some((start, end, matched));
                }
            }
        } else {
            // No pattern: compose just the single character.
            return Some((pos, pos + 1, vec![trigger_char]));
        }
    }

    None
}

// ---------------------------------------------------------------------------
// 8. Unicode Composition Helpers
// ---------------------------------------------------------------------------

/// Return `true` if `ch` is a Unicode combining mark.
///
/// Covers general categories Mn (non-spacing), Mc (spacing combining),
/// and Me (enclosing). This delegates to the range-based check in
/// `char_utils` but is re-exported here for convenience.
pub fn is_combining_mark(ch: char) -> bool {
    let cp = ch as u32;

    // Combining Diacritical Marks (Mn): U+0300..U+036F
    (0x0300..=0x036F).contains(&cp)
    // Combining Diacritical Marks Extended: U+1AB0..U+1AFF
    || (0x1AB0..=0x1AFF).contains(&cp)
    // Combining Diacritical Marks Supplement: U+1DC0..U+1DFF
    || (0x1DC0..=0x1DFF).contains(&cp)
    // Combining Diacritical Marks for Symbols: U+20D0..U+20FF
    || (0x20D0..=0x20FF).contains(&cp)
    // Combining Half Marks: U+FE20..U+FE2F
    || (0xFE20..=0xFE2F).contains(&cp)
    // Hebrew combining marks
    || (0x0591..=0x05BD).contains(&cp)
    || cp == 0x05BF
    || (0x05C1..=0x05C2).contains(&cp)
    || (0x05C4..=0x05C5).contains(&cp)
    || cp == 0x05C7
    // Arabic combining marks
    || (0x0610..=0x061A).contains(&cp)
    || (0x064B..=0x065F).contains(&cp)
    || cp == 0x0670
    || (0x06D6..=0x06DC).contains(&cp)
    || (0x06DF..=0x06E4).contains(&cp)
    || (0x06E7..=0x06E8).contains(&cp)
    || (0x06EA..=0x06ED).contains(&cp)
    // Devanagari combining marks
    || (0x0900..=0x0903).contains(&cp)
    || (0x093A..=0x094F).contains(&cp)
    || (0x0951..=0x0957).contains(&cp)
    || (0x0962..=0x0963).contains(&cp)
    // Thai combining marks
    || cp == 0x0E31
    || (0x0E34..=0x0E3A).contains(&cp)
    || (0x0E47..=0x0E4E).contains(&cp)
    // Hangul Jamo combining
    || (0x1160..=0x11FF).contains(&cp)
    // Variation Selectors
    || (0xFE00..=0xFE0F).contains(&cp)
    // Variation Selectors Supplement
    || (0xE0100..=0xE01EF).contains(&cp)
}

/// Return the Canonical Combining Class (CCC) of a character.
///
/// This is a simplified version covering the most common combining marks.
/// CCC 0 means "not reorderable" (base character or starter).
/// Non-zero values indicate combining marks that may be reordered.
pub fn canonical_combining_class(ch: char) -> u8 {
    let cp = ch as u32;

    // Non-combining characters have CCC 0.
    if !is_combining_mark(ch) {
        return 0;
    }

    // Common CCC values for frequently-used combining marks:
    match cp {
        // Hebrew: shin dot = 24, sin dot = 25, dagesh = 21, etc.
        0x05B0 => 10,          // SHEVA
        0x05B1 => 11,          // HATAF SEGOL
        0x05B2 => 12,          // HATAF PATAH
        0x05B3 => 13,          // HATAF QAMATS
        0x05B4 => 14,          // HIRIQ
        0x05B5 => 15,          // TSERE
        0x05B6 => 16,          // SEGOL
        0x05B7 => 17,          // PATAH
        0x05B8 => 18,          // QAMATS
        0x05B9..=0x05BA => 19, // HOLAM
        0x05BB => 20,          // QUBUTS
        0x05BC => 21,          // DAGESH
        0x05BD => 22,          // METEG
        0x05BF => 23,          // RAFE
        0x05C1 => 24,          // SHIN DOT
        0x05C2 => 25,          // SIN DOT

        // Arabic: fathah = 27, kasra = 29, damma = 28, shadda = 33, sukun = 34
        0x064B => 27, // FATHATAN
        0x064C => 28, // DAMMATAN
        0x064D => 29, // KASRATAN
        0x064E => 30, // FATHA
        0x064F => 31, // DAMMA
        0x0650 => 32, // KASRA
        0x0651 => 33, // SHADDA
        0x0652 => 34, // SUKUN
        0x0670 => 35, // SUPERSCRIPT ALEF

        // Latin combining diacriticals: common values
        0x0300 => 230,          // COMBINING GRAVE ACCENT (above)
        0x0301 => 230,          // COMBINING ACUTE ACCENT
        0x0302 => 230,          // COMBINING CIRCUMFLEX
        0x0303 => 230,          // COMBINING TILDE
        0x0304 => 230,          // COMBINING MACRON
        0x0305 => 230,          // COMBINING OVERLINE
        0x0306 => 230,          // COMBINING BREVE
        0x0307 => 230,          // COMBINING DOT ABOVE
        0x0308 => 230,          // COMBINING DIAERESIS
        0x0309 => 230,          // COMBINING HOOK ABOVE
        0x030A => 230,          // COMBINING RING ABOVE
        0x030B => 230,          // COMBINING DOUBLE ACUTE
        0x030C => 230,          // COMBINING CARON
        0x030D..=0x0314 => 230, // Various above marks

        // Below marks
        0x0316..=0x0319 => 220, // COMBINING GRAVE/ACUTE BELOW etc.
        0x031A => 232,          // COMBINING LEFT ANGLE ABOVE
        0x031B => 216,          // COMBINING HORN
        0x031C..=0x0320 => 220, // Below marks
        0x0321..=0x0322 => 202, // COMBINING PALATALIZED/RETROFLEX HOOK
        0x0323..=0x0326 => 220, // COMBINING DOT BELOW etc.
        0x0327..=0x0328 => 202, // COMBINING CEDILLA, OGONEK
        0x0329..=0x0333 => 220, // Below marks
        0x0334..=0x0338 => 1,   // Overlays (CCC 1)
        0x0339..=0x033C => 220, // Below marks
        0x033D..=0x0344 => 230, // Above marks
        0x0345 => 240,          // COMBINING GREEK YPOGEGRAMMENI (iota subscript)
        0x0346..=0x034E => 230, // Above marks (approximate)
        0x0350..=0x0357 => 230, // Above marks
        0x0358 => 232,          // Above right
        0x0359..=0x035B => 220, // Below marks
        0x035C => 233,          // COMBINING DOUBLE BREVE BELOW
        0x035D..=0x035E => 234, // Double above
        0x035F => 233,          // COMBINING DOUBLE MACRON BELOW
        0x0360..=0x0361 => 234, // Double above
        0x0362 => 233,          // COMBINING DOUBLE TILDE BELOW

        // Devanagari nukta
        0x093C => 7,

        // Thai
        0x0E38..=0x0E39 => 103, // Below vowels
        0x0E48..=0x0E4B => 107, // Tone marks

        // Default for other combining marks: 230 (canonical above).
        _ => 230,
    }
}

/// Find the boundaries of the grapheme cluster containing position `pos`.
///
/// Returns `(start, end)` as a half-open range `[start, end)`.
///
/// A grapheme cluster is a base character followed by zero or more
/// combining marks.  This is a simplified UAX #29 implementation
/// sufficient for composition purposes.
pub fn find_grapheme_cluster(text: &[char], pos: usize) -> (usize, usize) {
    if text.is_empty() || pos >= text.len() {
        return (pos, pos);
    }

    // Find the start: walk backwards past combining marks.
    let mut start = pos;
    while start > 0 && is_combining_mark(text[start]) {
        start -= 1;
    }

    // Find the end: walk forward past combining marks.
    let mut end = start + 1;
    while end < text.len() && is_combining_mark(text[end]) {
        end += 1;
    }

    (start, end)
}

/// Return `true` if the character is composable (eligible for auto-composition).
///
/// Characters of Unicode general category Z? or C? are not composable,
/// except for ZWNJ (U+200C), ZWJ (U+200D), and space-type separators (Zs).
/// Tag characters used in emoji sequences are also composable.
pub fn is_composable(ch: char) -> bool {
    let cp = ch as u32;

    if cp < 0x20 {
        return false;
    }

    // ZWNJ and ZWJ are always composable.
    if cp == 0x200C || cp == 0x200D {
        return true;
    }

    // Tag characters for emoji sequences (U+E0020..U+E007F).
    if (0xE0020..=0xE007F).contains(&cp) {
        return true;
    }

    // Newline, paragraph separator, line separator are not composable.
    if cp == 0x0A || cp == 0x0D || cp == 0x2028 || cp == 0x2029 {
        return false;
    }

    // C0/C1 control characters are not composable.
    if cp < 0x20 || cp == 0x7F || (0x80..=0x9F).contains(&cp) {
        return false;
    }

    // Format characters (Cf) other than ZWNJ/ZWJ: mostly not composable.
    // But we keep it simple and allow most visible characters.
    true
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Simple character display width (1 for normal, 2 for wide/CJK, 0 for combining).
fn char_display_width(ch: char) -> usize {
    let cp = ch as u32;

    if cp < 0x20 || cp == 0x7F {
        return 0;
    }

    if is_combining_mark(ch) {
        return 0;
    }

    // Wide/fullwidth CJK ranges (simplified).
    if is_wide_char(cp) {
        return 2;
    }

    1
}

/// Check if a codepoint is in a wide/fullwidth range.
fn is_wide_char(cp: u32) -> bool {
    // CJK Unified Ideographs
    (0x4E00..=0x9FFF).contains(&cp)
    // CJK Extension A
    || (0x3400..=0x4DBF).contains(&cp)
    // CJK Compatibility Ideographs
    || (0xF900..=0xFAFF).contains(&cp)
    // Fullwidth Forms
    || (0xFF01..=0xFF60).contains(&cp)
    || (0xFFE0..=0xFFE6).contains(&cp)
    // CJK Radicals Supplement, Kangxi Radicals
    || (0x2E80..=0x2FDF).contains(&cp)
    // CJK Symbols and Punctuation, Hiragana, Katakana
    || (0x3000..=0x303F).contains(&cp)
    || (0x3040..=0x309F).contains(&cp)
    || (0x30A0..=0x30FF).contains(&cp)
    // Hangul Syllables
    || (0xAC00..=0xD7AF).contains(&cp)
    // CJK Extension B+
    || (0x20000..=0x2FA1F).contains(&cp)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- CompositionMethod tests --

    #[test]
    fn test_composition_method_equality() {
        assert_eq!(CompositionMethod::Relative, CompositionMethod::Relative);
        assert_ne!(CompositionMethod::Relative, CompositionMethod::WithRule);
        assert_ne!(
            CompositionMethod::WithAltChars,
            CompositionMethod::WithRuleAltChars
        );
    }

    // -- ReferencePoint tests --

    #[test]
    fn test_reference_point_from_code() {
        assert_eq!(ReferencePoint::from_code(0), ReferencePoint::TopLeft);
        assert_eq!(ReferencePoint::from_code(4), ReferencePoint::BaselineCenter);
        assert_eq!(ReferencePoint::from_code(8), ReferencePoint::BottomRight);
        assert_eq!(ReferencePoint::from_code(10), ReferencePoint::MiddleCenter);
        assert_eq!(ReferencePoint::from_code(11), ReferencePoint::MiddleRight);
        // Out-of-range clamps to MiddleRight.
        assert_eq!(ReferencePoint::from_code(12), ReferencePoint::MiddleRight);
        assert_eq!(ReferencePoint::from_code(255), ReferencePoint::MiddleRight);
    }

    #[test]
    fn test_reference_point_column() {
        assert_eq!(ReferencePoint::TopLeft.column(), 0);
        assert_eq!(ReferencePoint::TopCenter.column(), 1);
        assert_eq!(ReferencePoint::TopRight.column(), 2);
        assert_eq!(ReferencePoint::BaselineLeft.column(), 0);
        assert_eq!(ReferencePoint::MiddleCenter.column(), 1);
        assert_eq!(ReferencePoint::BottomRight.column(), 2);
    }

    #[test]
    fn test_reference_point_x_fraction() {
        assert!((ReferencePoint::TopLeft.x_fraction() - 0.0).abs() < f64::EPSILON);
        assert!((ReferencePoint::TopCenter.x_fraction() - 0.5).abs() < f64::EPSILON);
        assert!((ReferencePoint::TopRight.x_fraction() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_reference_point_valid_pair() {
        assert!(ReferencePoint::is_valid_pair(0, 0));
        assert!(ReferencePoint::is_valid_pair(11, 11));
        assert!(!ReferencePoint::is_valid_pair(12, 0));
        assert!(!ReferencePoint::is_valid_pair(0, 12));
    }

    // -- CompositionRule tests --

    #[test]
    fn test_rule_encode_decode_roundtrip() {
        let rule = CompositionRule::new(ReferencePoint::TopLeft, ReferencePoint::BottomRight);
        let encoded = rule.encode();
        let decoded = CompositionRule::decode(encoded);
        assert_eq!(decoded.global_ref, ReferencePoint::TopLeft);
        assert_eq!(decoded.new_ref, ReferencePoint::BottomRight);
        assert_eq!(decoded.x_offset, 0);
        assert_eq!(decoded.y_offset, 0);
    }

    #[test]
    fn test_rule_encode_decode_with_offsets() {
        let rule = CompositionRule::with_offsets(
            ReferencePoint::MiddleCenter,
            ReferencePoint::BaselineCenter,
            5,
            10,
        );
        let encoded = rule.encode();
        let decoded = CompositionRule::decode(encoded);
        assert_eq!(decoded.global_ref, ReferencePoint::MiddleCenter);
        assert_eq!(decoded.new_ref, ReferencePoint::BaselineCenter);
        assert_eq!(decoded.x_offset, 5);
        assert_eq!(decoded.y_offset, 10);
    }

    #[test]
    fn test_rule_decode_refs() {
        // gref=0, nref=8 => code = 0*12 + 8 = 8
        let (gref, nref) = CompositionRule::decode_refs(8);
        assert_eq!(gref, ReferencePoint::TopLeft);
        assert_eq!(nref, ReferencePoint::BottomRight);

        // gref=4 (BaselineCenter mapped from code: 4*12+0=48 doesn't work)
        // Actually, BaselineCenter=4 => code / 12 = 4 => gref code should be 4.
        // But 4*12 = 48, so decode_refs(48) => gref=4, nref=0
        let (gref2, nref2) = CompositionRule::decode_refs(48);
        assert_eq!(gref2, ReferencePoint::BaselineCenter);
        assert_eq!(nref2, ReferencePoint::TopLeft);
    }

    #[test]
    fn test_rule_encode_refs() {
        let code =
            CompositionRule::encode_refs(ReferencePoint::TopLeft, ReferencePoint::BottomRight);
        // 0 * 12 + 8 = 8
        assert_eq!(code, 8);

        let code2 = CompositionRule::encode_refs(
            ReferencePoint::MiddleCenter,
            ReferencePoint::MiddleCenter,
        );
        // 10 * 12 + 10 = 130
        assert_eq!(code2, 130);
    }

    #[test]
    fn test_rule_compute_x_centered() {
        // Place new glyph at the center of the composed group.
        let rule = CompositionRule::new(
            ReferencePoint::TopCenter, // gref column=1 (center)
            ReferencePoint::TopCenter, // nref column=1 (center)
        );
        // Composed extent is [0, 10), new glyph width is 4.
        let x = rule.compute_x(0.0, 10.0, 4.0);
        // center of composed = 0 + 1*(10-0)/2 = 5
        // minus center of new = 1*4/2 = 2
        // result = 3.0
        assert!((x - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_rule_compute_x_left_aligned() {
        let rule = CompositionRule::new(ReferencePoint::TopLeft, ReferencePoint::TopLeft);
        let x = rule.compute_x(0.0, 10.0, 4.0);
        assert!((x - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_rule_compute_x_right_aligned() {
        let rule = CompositionRule::new(ReferencePoint::TopRight, ReferencePoint::TopRight);
        let x = rule.compute_x(0.0, 10.0, 4.0);
        // gref right => 0 + 2*(10-0)/2 = 10
        // nref right => 2*4/2 = 4
        // result = 6.0
        assert!((x - 6.0).abs() < f64::EPSILON);
    }

    // -- Composition tests --

    #[test]
    fn test_composition_glyph_access() {
        let comp = Composition {
            id: 0,
            method: CompositionMethod::Relative,
            components: vec!['a', 'b', 'c'],
            rules: vec![],
            char_count: 3,
            width: 1,
            offsets: vec![0; 6],
        };

        assert_eq!(comp.glyph_len(), 3);
        assert_eq!(comp.glyph(0), Some('a'));
        assert_eq!(comp.glyph(2), Some('c'));
        assert_eq!(comp.glyph(3), None);
    }

    // -- CompositionTable tests --

    #[test]
    fn test_table_register_and_get() {
        let mut table = CompositionTable::new();
        assert!(table.is_empty());

        let id = table.register(
            vec!['a', '\u{0300}'],
            CompositionMethod::Relative,
            vec![],
            2,
        );
        assert_eq!(id, 0);
        assert_eq!(table.len(), 1);

        let comp = table.get(id).unwrap();
        assert_eq!(comp.id, 0);
        assert_eq!(comp.method, CompositionMethod::Relative);
        assert_eq!(comp.components, vec!['a', '\u{0300}']);
        assert_eq!(comp.char_count, 2);
    }

    #[test]
    fn test_table_dedup() {
        let mut table = CompositionTable::new();
        let id1 = table.register(
            vec!['e', '\u{0301}'],
            CompositionMethod::Relative,
            vec![],
            2,
        );
        let id2 = table.register(
            vec!['e', '\u{0301}'],
            CompositionMethod::Relative,
            vec![],
            2,
        );
        assert_eq!(id1, id2);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn test_table_find() {
        let mut table = CompositionTable::new();
        let id = table.register(
            vec!['o', '\u{0308}'],
            CompositionMethod::Relative,
            vec![],
            2,
        );

        assert_eq!(table.find(&['o', '\u{0308}']), Some(id));
        assert_eq!(table.find(&['a', '\u{0308}']), None);
    }

    #[test]
    fn test_table_multiple_compositions() {
        let mut table = CompositionTable::new();
        let id1 = table.register(vec!['a'], CompositionMethod::Relative, vec![], 1);
        let id2 = table.register(vec!['b'], CompositionMethod::Relative, vec![], 1);
        let id3 = table.register(vec!['c'], CompositionMethod::WithAltChars, vec![], 1);

        assert_eq!(id1, 0);
        assert_eq!(id2, 1);
        assert_eq!(id3, 2);
        assert_eq!(table.len(), 3);
    }

    #[test]
    fn test_table_clear() {
        let mut table = CompositionTable::new();
        table.register(vec!['x'], CompositionMethod::Relative, vec![], 1);
        table.register(vec!['y'], CompositionMethod::Relative, vec![], 1);
        assert_eq!(table.len(), 2);

        table.clear();
        assert!(table.is_empty());
        assert_eq!(table.find(&['x']), None);
    }

    #[test]
    fn test_table_relative_width() {
        let mut table = CompositionTable::new();
        // CJK character is wide (width 2).
        let id = table.register(
            vec!['\u{4E00}', 'a'],
            CompositionMethod::Relative,
            vec![],
            2,
        );
        let comp = table.get(id).unwrap();
        // Relative width = max component width.  CJK = 2, 'a' = 1, so max = 2.
        assert_eq!(comp.width, 2);
    }

    #[test]
    fn test_table_rule_based_width() {
        let mut table = CompositionTable::new();
        // Two characters side by side (right-aligned new on right edge).
        let rule = CompositionRule::new(ReferencePoint::TopRight, ReferencePoint::TopLeft);
        let id = table.register(
            vec!['a', 'b'],
            CompositionMethod::WithRuleAltChars,
            vec![rule],
            2,
        );
        let comp = table.get(id).unwrap();
        // 'a' width=1, placed at [0,1). Rule puts 'b' at right edge of 'a':
        // x = 0 + 2*(1)/2 - 0*1/2 = 1.0, so composed [0, 2), width=2.
        assert_eq!(comp.width, 2);
    }

    // -- CompositionCache tests --

    #[test]
    fn test_cache_insert_get() {
        let mut cache = CompositionCache::new();
        cache.insert(10, 15, 42);

        assert_eq!(cache.get(10, 15), Some(42));
        assert_eq!(cache.get(10, 16), None);
        assert_eq!(cache.get(11, 15), None);
    }

    #[test]
    fn test_cache_invalidate_range() {
        let mut cache = CompositionCache::new();
        cache.insert(0, 5, 0);
        cache.insert(5, 10, 1);
        cache.insert(10, 15, 2);
        cache.insert(15, 20, 3);
        cache.insert(20, 25, 4);

        // Invalidate [4, 11) — overlaps entries [0,5), [5,10), [10,15).
        cache.invalidate_range(4, 11);
        assert_eq!(cache.get(0, 5), None); // [0,5) overlaps [4,11): end 5 > start 4
        assert_eq!(cache.get(5, 10), None); // [5,10) fully inside [4,11)
        assert_eq!(cache.get(10, 15), None); // [10,15) overlaps [4,11): start 10 < end 11
        assert_eq!(cache.get(15, 20), Some(3)); // [15,20) starts at 15 >= 11, no overlap
        assert_eq!(cache.get(20, 25), Some(4)); // no overlap
    }

    #[test]
    fn test_cache_invalidate_all() {
        let mut cache = CompositionCache::new();
        cache.insert(0, 5, 0);
        cache.insert(10, 20, 1);
        assert_eq!(cache.len(), 2);

        cache.invalidate_all();
        assert!(cache.is_empty());
    }

    // -- Auto-composition tests --

    #[test]
    fn test_auto_composition_no_rules() {
        let text: Vec<char> = "hello".chars().collect();
        let result = check_auto_composition(&text, 0, &[]);
        assert!(result.is_none());
    }

    #[test]
    fn test_auto_composition_no_match() {
        let text: Vec<char> = "hello".chars().collect();
        let rules = vec![AutoCompositionRule::new("xyz".to_string(), vec!['x'], 0)];
        let result = check_auto_composition(&text, 0, &rules);
        assert!(result.is_none());
    }

    #[test]
    fn test_auto_composition_simple_match() {
        let text: Vec<char> = "fi rest".chars().collect();
        let rules = vec![AutoCompositionRule::new("fi".to_string(), vec!['f'], 0)];
        let result = check_auto_composition(&text, 0, &rules);
        assert!(result.is_some());
        let (start, end, chars) = result.unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 2);
        assert_eq!(chars, vec!['f', 'i']);
    }

    #[test]
    fn test_auto_composition_with_lookback() {
        // Text: "affix", trigger at 'f' (pos=1), lookback=1 means start at pos=0.
        let text: Vec<char> = "affix".chars().collect();
        let rules = vec![AutoCompositionRule::new("aff".to_string(), vec!['f'], 1)];
        let result = check_auto_composition(&text, 1, &rules);
        assert!(result.is_some());
        let (start, end, chars) = result.unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 3);
        assert_eq!(chars, vec!['a', 'f', 'f']);
    }

    #[test]
    fn test_auto_composition_insufficient_lookback() {
        let text: Vec<char> = "fix".chars().collect();
        let rules = vec![AutoCompositionRule::new(
            "prefix".to_string(),
            vec!['f'],
            5, // Need 5 lookback but 'f' is at pos 0
        )];
        let result = check_auto_composition(&text, 0, &rules);
        assert!(result.is_none());
    }

    #[test]
    fn test_auto_composition_empty_pattern() {
        let text: Vec<char> = "abc".chars().collect();
        let rules = vec![AutoCompositionRule::new(String::new(), vec!['b'], 0)];
        let result = check_auto_composition(&text, 1, &rules);
        assert!(result.is_some());
        let (start, end, chars) = result.unwrap();
        assert_eq!(start, 1);
        assert_eq!(end, 2);
        assert_eq!(chars, vec!['b']);
    }

    #[test]
    fn test_auto_composition_out_of_bounds() {
        let text: Vec<char> = "abc".chars().collect();
        let rules = vec![AutoCompositionRule::new("a".to_string(), vec!['a'], 0)];
        let result = check_auto_composition(&text, 10, &rules);
        assert!(result.is_none());
    }

    // -- Unicode helpers tests --

    #[test]
    fn test_is_combining_mark_basic() {
        // Combining grave accent
        assert!(is_combining_mark('\u{0300}'));
        // Combining acute accent
        assert!(is_combining_mark('\u{0301}'));
        // Combining diaeresis
        assert!(is_combining_mark('\u{0308}'));
        // Not combining
        assert!(!is_combining_mark('a'));
        assert!(!is_combining_mark('Z'));
        assert!(!is_combining_mark(' '));
    }

    #[test]
    fn test_is_combining_mark_variation_selectors() {
        assert!(is_combining_mark('\u{FE00}'));
        assert!(is_combining_mark('\u{FE0F}'));
        assert!(is_combining_mark('\u{E0100}'));
    }

    #[test]
    fn test_canonical_combining_class_base() {
        assert_eq!(canonical_combining_class('a'), 0);
        assert_eq!(canonical_combining_class(' '), 0);
        assert_eq!(canonical_combining_class('0'), 0);
    }

    #[test]
    fn test_canonical_combining_class_marks() {
        // Combining grave accent: CCC 230
        assert_eq!(canonical_combining_class('\u{0300}'), 230);
        // Hebrew dagesh: CCC 21
        assert_eq!(canonical_combining_class('\u{05BC}'), 21);
        // Arabic shadda: CCC 33
        assert_eq!(canonical_combining_class('\u{0651}'), 33);
    }

    #[test]
    fn test_find_grapheme_cluster_simple() {
        let text: Vec<char> = "abc".chars().collect();
        assert_eq!(find_grapheme_cluster(&text, 0), (0, 1));
        assert_eq!(find_grapheme_cluster(&text, 1), (1, 2));
        assert_eq!(find_grapheme_cluster(&text, 2), (2, 3));
    }

    #[test]
    fn test_find_grapheme_cluster_with_combining() {
        // 'a' followed by combining acute accent and combining diaeresis
        let text: Vec<char> = vec!['a', '\u{0301}', '\u{0308}', 'b'];
        // Position 0 (base 'a'): cluster is [0, 3)
        assert_eq!(find_grapheme_cluster(&text, 0), (0, 3));
        // Position 1 (combining mark): cluster starts at 'a' (0)
        assert_eq!(find_grapheme_cluster(&text, 1), (0, 3));
        // Position 2 (combining mark): cluster starts at 'a' (0)
        assert_eq!(find_grapheme_cluster(&text, 2), (0, 3));
        // Position 3 ('b'): cluster is [3, 4)
        assert_eq!(find_grapheme_cluster(&text, 3), (3, 4));
    }

    #[test]
    fn test_find_grapheme_cluster_empty() {
        let text: Vec<char> = vec![];
        assert_eq!(find_grapheme_cluster(&text, 0), (0, 0));
    }

    #[test]
    fn test_find_grapheme_cluster_out_of_bounds() {
        let text: Vec<char> = "abc".chars().collect();
        assert_eq!(find_grapheme_cluster(&text, 10), (10, 10));
    }

    #[test]
    fn test_is_composable() {
        assert!(is_composable('a'));
        assert!(is_composable('\u{4E00}')); // CJK
        assert!(is_composable('\u{200D}')); // ZWJ
        assert!(is_composable('\u{200C}')); // ZWNJ
        assert!(!is_composable('\n')); // Newline
        assert!(!is_composable('\x01')); // Control
    }

    #[test]
    fn test_is_composable_tag_characters() {
        assert!(is_composable('\u{E0020}')); // TAG SPACE
        assert!(is_composable('\u{E007F}')); // CANCEL TAG
    }

    #[test]
    fn test_char_display_width() {
        assert_eq!(char_display_width('a'), 1);
        assert_eq!(char_display_width('\u{4E00}'), 2); // CJK
        assert_eq!(char_display_width('\u{0300}'), 0); // Combining mark
        assert_eq!(char_display_width('\t'), 0); // Control
    }

    // -- Integration test: register and use composition with rules --

    #[test]
    fn test_full_workflow_relative() {
        let mut table = CompositionTable::new();
        let mut cache = CompositionCache::new();

        // Register a composition for "e" + combining acute.
        let id = table.register(
            vec!['e', '\u{0301}'],
            CompositionMethod::Relative,
            vec![],
            2,
        );

        // Cache it at buffer position [10, 12).
        cache.insert(10, 12, id);

        // Look up from cache.
        let cached_id = cache.get(10, 12).unwrap();
        let comp = table.get(cached_id).unwrap();
        assert_eq!(comp.components, vec!['e', '\u{0301}']);
        assert_eq!(comp.method, CompositionMethod::Relative);
        assert_eq!(comp.width, 1); // 'e' is width 1, combining mark is 0, max = 1

        // Invalidate and verify.
        cache.invalidate_range(10, 12);
        assert!(cache.get(10, 12).is_none());
    }

    #[test]
    fn test_full_workflow_rule_based() {
        let mut table = CompositionTable::new();

        // Compose two characters with a rule placing the second centered above.
        let rule = CompositionRule::new(ReferencePoint::TopCenter, ReferencePoint::BottomCenter);

        let id = table.register(
            vec!['A', '\u{0302}'], // A + circumflex
            CompositionMethod::WithRuleAltChars,
            vec![rule],
            2,
        );

        let comp = table.get(id).unwrap();
        assert_eq!(comp.glyph_len(), 2);
        assert_eq!(comp.rule(0).unwrap().global_ref, ReferencePoint::TopCenter);
        assert_eq!(comp.rule(0).unwrap().new_ref, ReferencePoint::BottomCenter);
    }

    #[test]
    fn test_composition_table_get_mut() {
        let mut table = CompositionTable::new();
        let id = table.register(vec!['x'], CompositionMethod::Relative, vec![], 1);

        // Modify offsets through mutable access.
        {
            let comp = table.get_mut(id).unwrap();
            comp.offsets[0] = 5;
            comp.offsets[1] = -3;
        }

        let comp = table.get(id).unwrap();
        assert_eq!(comp.offsets[0], 5);
        assert_eq!(comp.offsets[1], -3);
    }
}
