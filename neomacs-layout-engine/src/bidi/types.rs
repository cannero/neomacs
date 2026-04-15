//! Core types for the Unicode Bidirectional Algorithm (UAX#9).

/// Bidi character class as defined in Unicode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BidiClass {
    // Strong types
    L = 0,  // Left-to-right
    R = 1,  // Right-to-left
    AL = 2, // Arabic letter

    // Weak types
    EN = 3,  // European number
    ES = 4,  // European separator
    ET = 5,  // European terminator
    AN = 6,  // Arabic number
    CS = 7,  // Common separator
    NSM = 8, // Non-spacing mark
    BN = 9,  // Boundary neutral

    // Neutral types
    B = 10,  // Paragraph separator
    S = 11,  // Segment separator
    WS = 12, // Whitespace
    ON = 13, // Other neutral

    // Explicit formatting
    LRE = 14, // Left-to-right embedding
    LRO = 15, // Left-to-right override
    RLE = 16, // Right-to-left embedding
    RLO = 17, // Right-to-left override
    PDF = 18, // Pop directional format
    LRI = 19, // Left-to-right isolate
    RLI = 20, // Right-to-left isolate
    FSI = 21, // First strong isolate
    PDI = 22, // Pop directional isolate
}

impl BidiClass {
    /// Whether this is a strong type (L, R, AL).
    pub fn is_strong(self) -> bool {
        matches!(self, BidiClass::L | BidiClass::R | BidiClass::AL)
    }

    /// Whether this is a weak type.
    pub fn is_weak(self) -> bool {
        matches!(
            self,
            BidiClass::EN
                | BidiClass::ES
                | BidiClass::ET
                | BidiClass::AN
                | BidiClass::CS
                | BidiClass::NSM
                | BidiClass::BN
        )
    }

    /// Whether this is a neutral type.
    pub fn is_neutral(self) -> bool {
        matches!(
            self,
            BidiClass::B | BidiClass::S | BidiClass::WS | BidiClass::ON
        )
    }

    /// Whether this is an explicit formatting character.
    pub fn is_explicit(self) -> bool {
        matches!(
            self,
            BidiClass::LRE
                | BidiClass::LRO
                | BidiClass::RLE
                | BidiClass::RLO
                | BidiClass::PDF
                | BidiClass::LRI
                | BidiClass::RLI
                | BidiClass::FSI
                | BidiClass::PDI
        )
    }

    /// Whether this is an isolate initiator (LRI, RLI, FSI).
    pub fn is_isolate_initiator(self) -> bool {
        matches!(self, BidiClass::LRI | BidiClass::RLI | BidiClass::FSI)
    }

    /// Whether this is a removed-by-X9 type (LRE, RLE, LRO, RLO, PDF, BN).
    pub fn is_removed_by_x9(self) -> bool {
        matches!(
            self,
            BidiClass::LRE
                | BidiClass::RLE
                | BidiClass::LRO
                | BidiClass::RLO
                | BidiClass::PDF
                | BidiClass::BN
        )
    }

    /// Map to "strong" direction for neutral resolution.
    /// EN and AN are treated as R for N1/N2 rules.
    pub fn to_strong_for_neutral(self) -> BidiClass {
        match self {
            BidiClass::EN | BidiClass::AN => BidiClass::R,
            other => other,
        }
    }
}

/// Paragraph/base direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BidiDir {
    /// Left-to-right.
    LTR,
    /// Right-to-left.
    RTL,
    /// Auto-detect from first strong character.
    Auto,
}

impl BidiDir {
    /// Base embedding level for this direction.
    pub fn base_level(self) -> u8 {
        match self {
            BidiDir::LTR | BidiDir::Auto => 0,
            BidiDir::RTL => 1,
        }
    }
}

/// Bracket type for the Paired Bracket Algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BracketType {
    None,
    Open(char),  // Opening bracket, stores canonical closing
    Close(char), // Closing bracket, stores canonical opening
}

/// Override status for level stack entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Override {
    Neutral,
    LTR,
    RTL,
}

/// Entry on the directional status stack (X1-X8).
#[derive(Debug, Clone, Copy)]
pub struct DirectionalStatus {
    pub level: u8,
    pub override_status: Override,
    pub isolate_status: bool,
}

/// Result of resolving one character's bidi properties.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedChar {
    /// The character.
    pub ch: char,
    /// Original bidi class from Unicode data.
    pub original_class: BidiClass,
    /// Resolved embedding level (0-125).
    pub level: u8,
}

/// Maximum depth of explicit embedding/override/isolate nesting (UAX#9).
pub const MAX_DEPTH: u8 = 125;

/// Maximum size of the BPA stack.
pub const MAX_BPA_STACK: usize = 63;

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
