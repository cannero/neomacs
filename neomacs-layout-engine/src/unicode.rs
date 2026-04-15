//! Unicode utility functions for the Rust layout engine.
//!
//! Pure functions for UTF-8 decoding, character width classification,
//! grapheme cluster collection, and glyphless character detection.

/// Decode one UTF-8 character from a byte slice.
/// Returns (char, bytes_consumed).
pub(crate) fn decode_utf8(bytes: &[u8]) -> (char, usize) {
    if bytes.is_empty() {
        return ('\0', 0);
    }

    let b0 = bytes[0];
    if b0 < 0x80 {
        (b0 as char, 1)
    } else if b0 < 0xC0 {
        // Invalid continuation byte — treat as replacement
        ('\u{FFFD}', 1)
    } else if b0 < 0xE0 {
        if bytes.len() < 2 {
            return ('\u{FFFD}', 1);
        }
        let cp = ((b0 as u32 & 0x1F) << 6) | (bytes[1] as u32 & 0x3F);
        (char::from_u32(cp).unwrap_or('\u{FFFD}'), 2)
    } else if b0 < 0xF0 {
        if bytes.len() < 3 {
            return ('\u{FFFD}', 1);
        }
        let cp =
            ((b0 as u32 & 0x0F) << 12) | ((bytes[1] as u32 & 0x3F) << 6) | (bytes[2] as u32 & 0x3F);
        (char::from_u32(cp).unwrap_or('\u{FFFD}'), 3)
    } else {
        if bytes.len() < 4 {
            return ('\u{FFFD}', 1);
        }
        let cp = ((b0 as u32 & 0x07) << 18)
            | ((bytes[1] as u32 & 0x3F) << 12)
            | ((bytes[2] as u32 & 0x3F) << 6)
            | (bytes[3] as u32 & 0x3F);
        (char::from_u32(cp).unwrap_or('\u{FFFD}'), 4)
    }
}

/// Check if a character is a wide (CJK) character that occupies 2 columns.
// is_combining_char has been replaced by is_cluster_extender and
// collect_grapheme_cluster which properly handle multi-codepoint
// grapheme clusters (emoji ZWJ, combining marks, etc.)

pub(crate) fn is_wide_char(ch: char) -> bool {
    let cp = ch as u32;
    // CJK Unified Ideographs
    (0x4E00..=0x9FFF).contains(&cp)
    // CJK Extension A
    || (0x3400..=0x4DBF).contains(&cp)
    // CJK Extension B
    || (0x20000..=0x2A6DF).contains(&cp)
    // CJK Compatibility Ideographs
    || (0xF900..=0xFAFF).contains(&cp)
    // Fullwidth Forms
    || (0xFF01..=0xFF60).contains(&cp)
    || (0xFFE0..=0xFFE6).contains(&cp)
    // Hangul Syllables
    || (0xAC00..=0xD7AF).contains(&cp)
    // CJK Radicals
    || (0x2E80..=0x2FDF).contains(&cp)
    // Katakana/Hiragana
    || (0x3000..=0x303F).contains(&cp)
    || (0x3040..=0x309F).contains(&cp)
    || (0x30A0..=0x30FF).contains(&cp)
    || (0x31F0..=0x31FF).contains(&cp)
    // Emoji (East Asian Width = W in emoji presentation)
    || is_emoji_presentation(cp)
}

/// Check if a codepoint is an emoji that should have wide (2-column) presentation.
pub(crate) fn is_emoji_presentation(cp: u32) -> bool {
    // Emoticons
    (0x1F600..=0x1F64F).contains(&cp)
    // Miscellaneous Symbols and Pictographs
    || (0x1F300..=0x1F5FF).contains(&cp)
    // Transport and Map Symbols
    || (0x1F680..=0x1F6FF).contains(&cp)
    // Supplemental Symbols and Pictographs
    || (0x1F900..=0x1F9FF).contains(&cp)
    // Symbols and Pictographs Extended-A
    || (0x1FA00..=0x1FA6F).contains(&cp)
    || (0x1FA70..=0x1FAFF).contains(&cp)
    // Dingbats (selected emoji)
    || (0x2702..=0x27B0).contains(&cp)
    // Regional Indicator Symbols
    || (0x1F1E0..=0x1F1FF).contains(&cp)
    // Playing cards, mahjong, dominos
    || cp == 0x1F004  // mahjong red dragon
    || cp == 0x1F0CF // playing card black joker
    // Skin tone modifiers (display-width 0 when following emoji, but 2 when standalone)
    // We'll treat them as part of clusters, so this is just for standalone
}

/// Check if a character is a grapheme cluster extender: it should be
/// bundled with the preceding base character for proper rendering.
#[cfg(test)]
pub(crate) fn is_cluster_extender(ch: char) -> bool {
    let cp = ch as u32;
    // Combining Diacritical Marks
    (0x0300..=0x036F).contains(&cp)
    // Combining Diacritical Marks Extended
    || (0x1AB0..=0x1AFF).contains(&cp)
    // Combining Diacritical Marks Supplement
    || (0x1DC0..=0x1DFF).contains(&cp)
    // Combining Diacritical Marks for Symbols
    || (0x20D0..=0x20FF).contains(&cp)
    // Combining Half Marks
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
    || (0x0901..=0x0903).contains(&cp)
    || (0x093A..=0x094F).contains(&cp)
    || (0x0951..=0x0957).contains(&cp)
    || (0x0962..=0x0963).contains(&cp)
    // Thai combining marks
    || (0x0E31..=0x0E31).contains(&cp)
    || (0x0E34..=0x0E3A).contains(&cp)
    || (0x0E47..=0x0E4E).contains(&cp)
    // Hangul Jamo combining vowels/final consonants
    || (0x1160..=0x11FF).contains(&cp)
    // Variation selectors
    || (0xFE00..=0xFE0F).contains(&cp)
    || (0xE0100..=0xE01EF).contains(&cp)
    // Emoji skin tone modifiers
    || (0x1F3FB..=0x1F3FF).contains(&cp)
    // Combining Enclosing Keycap
    || cp == 0x20E3
    // Emoji tag characters (U+E0020..U+E007F, used in flag tag sequences)
    || (0xE0020..=0xE007F).contains(&cp)
    // Zero-width joiner (handled specially in collect_grapheme_cluster)
    || cp == 0x200D
    // Zero-width non-joiner, zero-width space, directional marks
    || cp == 0x200C
    || cp == 0x200B
    || cp == 0x200E
    || cp == 0x200F
    || cp == 0xFEFF
}

/// Check if a codepoint is a Regional Indicator Symbol.
#[cfg(test)]
pub(crate) fn is_regional_indicator(cp: u32) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&cp)
}

/// Collect a grapheme cluster starting with the base character `base_ch`.
/// Peeks at subsequent bytes in `remaining` to find cluster extenders.
///
/// Returns (cluster_string, extra_bytes_consumed, extra_chars_consumed).
/// If there are no extenders, returns (None, 0, 0) — use single-char path.
#[cfg(test)]
pub(crate) fn collect_grapheme_cluster(
    base_ch: char,
    remaining: &[u8],
) -> (Option<String>, usize, usize) {
    let mut extra_bytes = 0usize;
    let mut extra_chars = 0usize;
    let mut cluster = String::new();
    cluster.push(base_ch);

    let mut peek = 0usize;
    let base_is_ri = is_regional_indicator(base_ch as u32);

    loop {
        if peek >= remaining.len() {
            break;
        }
        let (next_ch, next_len) = decode_utf8(&remaining[peek..]);

        if next_ch == '\u{200D}' {
            // ZWJ: consume it AND the next character (emoji ZWJ sequence)
            cluster.push(next_ch);
            peek += next_len;
            extra_bytes += next_len;
            extra_chars += 1;

            // Consume the character after ZWJ
            if peek < remaining.len() {
                let (zjoin_ch, zjoin_len) = decode_utf8(&remaining[peek..]);
                cluster.push(zjoin_ch);
                peek += zjoin_len;
                extra_bytes += zjoin_len;
                extra_chars += 1;
            }
        } else if is_cluster_extender(next_ch) && next_ch != '\u{200D}' {
            // Combining mark, variation selector, skin tone modifier, etc.
            cluster.push(next_ch);
            peek += next_len;
            extra_bytes += next_len;
            extra_chars += 1;
        } else if base_is_ri
            && is_regional_indicator(next_ch as u32)
            && cluster.chars().count() == 1
        {
            // Second regional indicator forms a flag pair
            cluster.push(next_ch);
            extra_bytes += next_len;
            extra_chars += 1;
            break; // Flags are exactly 2 regional indicators
        } else {
            break;
        }
    }

    if extra_chars > 0 {
        (Some(cluster), extra_bytes, extra_chars)
    } else {
        (None, 0, 0)
    }
}

/// Check if a character is potentially glyphless and should be looked up
/// in the glyphless-char-display char-table.
/// This is a fast pre-filter — only chars in these ranges trigger the FFI call.
#[cfg(test)]
pub(crate) fn is_potentially_glyphless(ch: char) -> bool {
    let cp = ch as u32;
    // C1 control characters (0x80-0x9F)
    (0x80..=0x9F).contains(&cp)
    // Soft hyphen (sometimes glyphless)
    || cp == 0xAD
    // Unicode format/control characters
    || (0x200B..=0x200F).contains(&cp)  // ZWSP, ZWNJ, ZWJ, LRM, RLM
    || (0x202A..=0x202E).contains(&cp)  // bidi embedding
    || (0x2060..=0x2069).contains(&cp)  // word joiner, invisible separators
    || (0x2028..=0x2029).contains(&cp)  // line/paragraph separator
    || cp == 0xFEFF                      // BOM / ZWNBSP
    || (0xFFF0..=0xFFFD).contains(&cp)  // specials (interlinear annotation, replacement)
    // Emacs raw bytes (BYTE8 encoding: 0x3FFF80..0x3FFFFF)
    || (0x3FFF80..=0x3FFFFF).contains(&cp)
    // Unassigned/private use — only very high ranges
    || (0xE0000..=0xE007F).contains(&cp) // tags block
}

#[cfg(test)]
#[path = "unicode_test.rs"]
mod tests;
