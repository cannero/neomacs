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
pub(crate) fn is_regional_indicator(cp: u32) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&cp)
}

/// Collect a grapheme cluster starting with the base character `base_ch`.
/// Peeks at subsequent bytes in `remaining` to find cluster extenders.
///
/// Returns (cluster_string, extra_bytes_consumed, extra_chars_consumed).
/// If there are no extenders, returns (None, 0, 0) — use single-char path.
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
            peek += next_len;
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
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // decode_utf8 tests
    // ---------------------------------------------------------------

    #[test]
    fn decode_utf8_empty_slice() {
        let (ch, len) = decode_utf8(&[]);
        assert_eq!(ch, '\0');
        assert_eq!(len, 0);
    }

    #[test]
    fn decode_utf8_ascii_a() {
        let (ch, len) = decode_utf8(b"A");
        assert_eq!(ch, 'A');
        assert_eq!(len, 1);
    }

    #[test]
    fn decode_utf8_ascii_nul() {
        let (ch, len) = decode_utf8(&[0x00]);
        assert_eq!(ch, '\0');
        assert_eq!(len, 1);
    }

    #[test]
    fn decode_utf8_ascii_del() {
        // 0x7F is the highest single-byte value (DEL)
        let (ch, len) = decode_utf8(&[0x7F]);
        assert_eq!(ch, '\x7F');
        assert_eq!(len, 1);
    }

    #[test]
    fn decode_utf8_two_byte_latin() {
        // U+00E9 LATIN SMALL LETTER E WITH ACUTE = 0xC3 0xA9
        let bytes = "é".as_bytes();
        let (ch, len) = decode_utf8(bytes);
        assert_eq!(ch, 'é');
        assert_eq!(len, 2);
    }

    #[test]
    fn decode_utf8_two_byte_boundary() {
        // U+0080 is the first 2-byte code point = 0xC2 0x80
        let bytes = "\u{0080}".as_bytes();
        let (ch, len) = decode_utf8(bytes);
        assert_eq!(ch, '\u{0080}');
        assert_eq!(len, 2);
    }

    #[test]
    fn decode_utf8_three_byte_cjk() {
        // U+4E2D (中) = 0xE4 0xB8 0xAD
        let bytes = "中".as_bytes();
        let (ch, len) = decode_utf8(bytes);
        assert_eq!(ch, '中');
        assert_eq!(len, 3);
    }

    #[test]
    fn decode_utf8_three_byte_euro_sign() {
        // U+20AC EURO SIGN = 0xE2 0x82 0xAC
        let bytes = "€".as_bytes();
        let (ch, len) = decode_utf8(bytes);
        assert_eq!(ch, '€');
        assert_eq!(len, 3);
    }

    #[test]
    fn decode_utf8_four_byte_emoji() {
        // U+1F600 GRINNING FACE = 0xF0 0x9F 0x98 0x80
        let bytes = "\u{1F600}".as_bytes();
        let (ch, len) = decode_utf8(bytes);
        assert_eq!(ch, '\u{1F600}');
        assert_eq!(len, 4);
    }

    #[test]
    fn decode_utf8_four_byte_max_codepoint() {
        // U+10FFFF is the maximum Unicode code point
        let bytes = "\u{10FFFF}".as_bytes();
        let (ch, len) = decode_utf8(bytes);
        assert_eq!(ch, '\u{10FFFF}');
        assert_eq!(len, 4);
    }

    #[test]
    fn decode_utf8_invalid_continuation_byte() {
        // 0x80 is a bare continuation byte — invalid as a start byte
        let (ch, len) = decode_utf8(&[0x80]);
        assert_eq!(ch, '\u{FFFD}');
        assert_eq!(len, 1);
    }

    #[test]
    fn decode_utf8_invalid_continuation_0xBF() {
        let (ch, len) = decode_utf8(&[0xBF]);
        assert_eq!(ch, '\u{FFFD}');
        assert_eq!(len, 1);
    }

    #[test]
    fn decode_utf8_truncated_two_byte() {
        // 0xC3 expects a second byte but slice is too short
        let (ch, len) = decode_utf8(&[0xC3]);
        assert_eq!(ch, '\u{FFFD}');
        assert_eq!(len, 1);
    }

    #[test]
    fn decode_utf8_truncated_three_byte() {
        // 0xE4 expects two more bytes but only one provided
        let (ch, len) = decode_utf8(&[0xE4, 0xB8]);
        assert_eq!(ch, '\u{FFFD}');
        assert_eq!(len, 1);
    }

    #[test]
    fn decode_utf8_truncated_four_byte() {
        // 0xF0 expects three more bytes but only two provided
        let (ch, len) = decode_utf8(&[0xF0, 0x9F, 0x98]);
        assert_eq!(ch, '\u{FFFD}');
        assert_eq!(len, 1);
    }

    #[test]
    fn decode_utf8_consumes_only_first_char() {
        // "AB" — should decode only 'A' and report 1 byte consumed
        let (ch, len) = decode_utf8(b"AB");
        assert_eq!(ch, 'A');
        assert_eq!(len, 1);
    }

    #[test]
    fn decode_utf8_multibyte_followed_by_ascii() {
        // "中A" — should decode only '中' (3 bytes)
        let bytes = "中A".as_bytes();
        let (ch, len) = decode_utf8(bytes);
        assert_eq!(ch, '中');
        assert_eq!(len, 3);
    }

    // ---------------------------------------------------------------
    // is_wide_char tests
    // ---------------------------------------------------------------

    #[test]
    fn wide_char_ascii_not_wide() {
        assert!(!is_wide_char('A'));
        assert!(!is_wide_char(' '));
        assert!(!is_wide_char('~'));
    }

    #[test]
    fn wide_char_cjk_unified() {
        // U+4E2D 中
        assert!(is_wide_char('中'));
        // U+9FFF last in CJK Unified Ideographs block
        assert!(is_wide_char('\u{9FFF}'));
        // U+4E00 first in CJK Unified Ideographs block
        assert!(is_wide_char('\u{4E00}'));
    }

    #[test]
    fn wide_char_cjk_extension_a() {
        assert!(is_wide_char('\u{3400}'));
        assert!(is_wide_char('\u{4DBF}'));
    }

    #[test]
    fn wide_char_cjk_extension_b() {
        assert!(is_wide_char('\u{20000}'));
        assert!(is_wide_char('\u{2A6DF}'));
    }

    #[test]
    fn wide_char_fullwidth_forms() {
        // U+FF01 FULLWIDTH EXCLAMATION MARK
        assert!(is_wide_char('\u{FF01}'));
        // U+FF5A FULLWIDTH LATIN SMALL LETTER Z
        assert!(is_wide_char('\u{FF5A}'));
        // U+FFE0 FULLWIDTH CENT SIGN
        assert!(is_wide_char('\u{FFE0}'));
        // U+FFE6 FULLWIDTH WON SIGN
        assert!(is_wide_char('\u{FFE6}'));
    }

    #[test]
    fn wide_char_hangul_syllable() {
        // U+AC00 first Hangul syllable (가)
        assert!(is_wide_char('\u{AC00}'));
        // U+D7AF last Hangul syllable
        assert!(is_wide_char('\u{D7AF}'));
    }

    #[test]
    fn wide_char_hiragana() {
        // U+3042 HIRAGANA LETTER A (あ)
        assert!(is_wide_char('\u{3042}'));
    }

    #[test]
    fn wide_char_katakana() {
        // U+30A2 KATAKANA LETTER A (ア)
        assert!(is_wide_char('\u{30A2}'));
    }

    #[test]
    fn wide_char_cjk_symbols_and_punctuation() {
        // U+3000 IDEOGRAPHIC SPACE
        assert!(is_wide_char('\u{3000}'));
        // U+3001 IDEOGRAPHIC COMMA
        assert!(is_wide_char('\u{3001}'));
    }

    #[test]
    fn wide_char_cjk_radicals() {
        assert!(is_wide_char('\u{2E80}'));
        assert!(is_wide_char('\u{2FDF}'));
    }

    #[test]
    fn wide_char_emoji_is_wide() {
        // U+1F600 GRINNING FACE
        assert!(is_wide_char('\u{1F600}'));
        // U+1F4A9 PILE OF POO
        assert!(is_wide_char('\u{1F4A9}'));
    }

    #[test]
    fn wide_char_latin_extended_not_wide() {
        // U+00E9 LATIN SMALL LETTER E WITH ACUTE
        assert!(!is_wide_char('é'));
        // U+00FC LATIN SMALL LETTER U WITH DIAERESIS
        assert!(!is_wide_char('ü'));
    }

    #[test]
    fn wide_char_cjk_compat_ideographs() {
        assert!(is_wide_char('\u{F900}'));
        assert!(is_wide_char('\u{FAFF}'));
    }

    #[test]
    fn wide_char_katakana_phonetic_extensions() {
        assert!(is_wide_char('\u{31F0}'));
        assert!(is_wide_char('\u{31FF}'));
    }

    // ---------------------------------------------------------------
    // is_emoji_presentation tests
    // ---------------------------------------------------------------

    #[test]
    fn emoji_presentation_emoticons() {
        assert!(is_emoji_presentation(0x1F600)); // grinning face
        assert!(is_emoji_presentation(0x1F64F)); // last emoticon
    }

    #[test]
    fn emoji_presentation_misc_symbols_pictographs() {
        assert!(is_emoji_presentation(0x1F300)); // cyclone
        assert!(is_emoji_presentation(0x1F5FF)); // moyai
    }

    #[test]
    fn emoji_presentation_transport_map() {
        assert!(is_emoji_presentation(0x1F680)); // rocket
        assert!(is_emoji_presentation(0x1F6FF));
    }

    #[test]
    fn emoji_presentation_supplemental_symbols() {
        assert!(is_emoji_presentation(0x1F900));
        assert!(is_emoji_presentation(0x1F9FF));
    }

    #[test]
    fn emoji_presentation_extended_a() {
        assert!(is_emoji_presentation(0x1FA00));
        assert!(is_emoji_presentation(0x1FA6F));
        assert!(is_emoji_presentation(0x1FA70));
        assert!(is_emoji_presentation(0x1FAFF));
    }

    #[test]
    fn emoji_presentation_dingbats() {
        assert!(is_emoji_presentation(0x2702)); // black scissors
        assert!(is_emoji_presentation(0x27B0)); // curly loop
    }

    #[test]
    fn emoji_presentation_regional_indicators() {
        assert!(is_emoji_presentation(0x1F1E0));
        assert!(is_emoji_presentation(0x1F1FF));
    }

    #[test]
    fn emoji_presentation_special_cards() {
        assert!(is_emoji_presentation(0x1F004)); // mahjong red dragon
        assert!(is_emoji_presentation(0x1F0CF)); // playing card black joker
    }

    #[test]
    fn emoji_presentation_non_emoji_ascii() {
        assert!(!is_emoji_presentation(0x41)); // 'A'
        assert!(!is_emoji_presentation(0x20)); // space
    }

    #[test]
    fn emoji_presentation_non_emoji_cjk() {
        assert!(!is_emoji_presentation(0x4E2D)); // 中
    }

    // ---------------------------------------------------------------
    // is_cluster_extender tests
    // ---------------------------------------------------------------

    #[test]
    fn cluster_extender_combining_diacritical() {
        // U+0300 COMBINING GRAVE ACCENT
        assert!(is_cluster_extender('\u{0300}'));
        // U+036F last in basic combining diacriticals
        assert!(is_cluster_extender('\u{036F}'));
    }

    #[test]
    fn cluster_extender_combining_diacritical_extended() {
        assert!(is_cluster_extender('\u{1AB0}'));
        assert!(is_cluster_extender('\u{1AFF}'));
    }

    #[test]
    fn cluster_extender_combining_diacritical_supplement() {
        assert!(is_cluster_extender('\u{1DC0}'));
        assert!(is_cluster_extender('\u{1DFF}'));
    }

    #[test]
    fn cluster_extender_variation_selectors() {
        // U+FE00..U+FE0F are variation selectors
        assert!(is_cluster_extender('\u{FE00}'));
        assert!(is_cluster_extender('\u{FE0F}')); // VS16 (emoji presentation)
    }

    #[test]
    fn cluster_extender_skin_tone_modifiers() {
        assert!(is_cluster_extender('\u{1F3FB}')); // light skin
        assert!(is_cluster_extender('\u{1F3FF}')); // dark skin
    }

    #[test]
    fn cluster_extender_zero_width_joiner() {
        assert!(is_cluster_extender('\u{200D}')); // ZWJ
    }

    #[test]
    fn cluster_extender_zero_width_chars() {
        assert!(is_cluster_extender('\u{200B}')); // ZWSP
        assert!(is_cluster_extender('\u{200C}')); // ZWNJ
        assert!(is_cluster_extender('\u{200E}')); // LRM
        assert!(is_cluster_extender('\u{200F}')); // RLM
        assert!(is_cluster_extender('\u{FEFF}')); // BOM
    }

    #[test]
    fn cluster_extender_combining_enclosing_keycap() {
        assert!(is_cluster_extender('\u{20E3}'));
    }

    #[test]
    fn cluster_extender_hebrew_marks() {
        assert!(is_cluster_extender('\u{0591}'));
        assert!(is_cluster_extender('\u{05BD}'));
        assert!(is_cluster_extender('\u{05BF}'));
        assert!(is_cluster_extender('\u{05C1}'));
        assert!(is_cluster_extender('\u{05C7}'));
    }

    #[test]
    fn cluster_extender_arabic_marks() {
        assert!(is_cluster_extender('\u{0610}'));
        assert!(is_cluster_extender('\u{064B}'));
        assert!(is_cluster_extender('\u{0670}'));
    }

    #[test]
    fn cluster_extender_devanagari_marks() {
        assert!(is_cluster_extender('\u{0901}'));
        assert!(is_cluster_extender('\u{093A}'));
    }

    #[test]
    fn cluster_extender_thai_marks() {
        assert!(is_cluster_extender('\u{0E31}'));
        assert!(is_cluster_extender('\u{0E34}'));
        assert!(is_cluster_extender('\u{0E47}'));
    }

    #[test]
    fn cluster_extender_emoji_tag_chars() {
        assert!(is_cluster_extender('\u{E0020}'));
        assert!(is_cluster_extender('\u{E007F}')); // CANCEL TAG
    }

    #[test]
    fn cluster_extender_supplementary_variation_selectors() {
        assert!(is_cluster_extender('\u{E0100}'));
        assert!(is_cluster_extender('\u{E01EF}'));
    }

    #[test]
    fn cluster_extender_ascii_not_extender() {
        assert!(!is_cluster_extender('A'));
        assert!(!is_cluster_extender(' '));
        assert!(!is_cluster_extender('0'));
    }

    #[test]
    fn cluster_extender_cjk_not_extender() {
        assert!(!is_cluster_extender('中'));
    }

    // ---------------------------------------------------------------
    // is_regional_indicator tests
    // ---------------------------------------------------------------

    #[test]
    fn regional_indicator_valid() {
        // U+1F1E6 Regional Indicator Symbol Letter A
        assert!(is_regional_indicator(0x1F1E6));
        // U+1F1FF Regional Indicator Symbol Letter Z
        assert!(is_regional_indicator(0x1F1FF));
        // U+1F1FA Regional Indicator Symbol Letter U
        assert!(is_regional_indicator(0x1F1FA));
    }

    #[test]
    fn regional_indicator_invalid_before_range() {
        assert!(!is_regional_indicator(0x1F1E5));
    }

    #[test]
    fn regional_indicator_invalid_after_range() {
        assert!(!is_regional_indicator(0x1F200));
    }

    #[test]
    fn regional_indicator_ascii() {
        assert!(!is_regional_indicator(0x41)); // 'A'
    }

    // ---------------------------------------------------------------
    // collect_grapheme_cluster tests
    // ---------------------------------------------------------------

    #[test]
    fn cluster_single_char_no_extenders() {
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster('A', b"BC");
        assert!(cluster.is_none());
        assert_eq!(extra_bytes, 0);
        assert_eq!(extra_chars, 0);
    }

    #[test]
    fn cluster_single_char_empty_remaining() {
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster('A', &[]);
        assert!(cluster.is_none());
        assert_eq!(extra_bytes, 0);
        assert_eq!(extra_chars, 0);
    }

    #[test]
    fn cluster_combining_acute_accent() {
        // 'e' followed by U+0301 COMBINING ACUTE ACCENT (0xCC 0x81)
        let remaining = "\u{0301}".as_bytes();
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster('e', remaining);
        assert_eq!(cluster, Some("e\u{0301}".to_string()));
        assert_eq!(extra_bytes, 2);
        assert_eq!(extra_chars, 1);
    }

    #[test]
    fn cluster_multiple_combining_marks() {
        // 'a' + U+0300 (combining grave) + U+0301 (combining acute)
        let remaining_str = "\u{0300}\u{0301}";
        let remaining = remaining_str.as_bytes();
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster('a', remaining);
        assert_eq!(cluster, Some("a\u{0300}\u{0301}".to_string()));
        assert_eq!(extra_bytes, 4); // 2 bytes for each combining mark
        assert_eq!(extra_chars, 2);
    }

    #[test]
    fn cluster_emoji_with_skin_tone() {
        // U+1F44D THUMBS UP followed by U+1F3FD MEDIUM SKIN TONE
        let remaining = "\u{1F3FD}".as_bytes();
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster('\u{1F44D}', remaining);
        assert_eq!(cluster, Some("\u{1F44D}\u{1F3FD}".to_string()));
        assert_eq!(extra_bytes, 4); // skin tone is a 4-byte character
        assert_eq!(extra_chars, 1);
    }

    #[test]
    fn cluster_zwj_sequence_two_emoji() {
        // Heart + ZWJ + Fire = "heart on fire" emoji
        // U+2764 (heart) + U+200D (ZWJ) + U+1F525 (fire)
        let remaining_str = "\u{200D}\u{1F525}";
        let remaining = remaining_str.as_bytes();
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster('\u{2764}', remaining);
        assert_eq!(cluster, Some("\u{2764}\u{200D}\u{1F525}".to_string()));
        // ZWJ is 3 bytes, fire emoji is 4 bytes
        assert_eq!(extra_bytes, 7);
        assert_eq!(extra_chars, 2);
    }

    #[test]
    fn cluster_variation_selector() {
        // U+2764 HEAVY BLACK HEART + U+FE0F VARIATION SELECTOR-16
        let remaining = "\u{FE0F}".as_bytes();
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster('\u{2764}', remaining);
        assert_eq!(cluster, Some("\u{2764}\u{FE0F}".to_string()));
        assert_eq!(extra_bytes, 3); // FE0F is 3 bytes in UTF-8
        assert_eq!(extra_chars, 1);
    }

    #[test]
    fn cluster_regional_indicator_flag_pair() {
        // U+1F1FA (Regional Indicator U) + U+1F1F8 (Regional Indicator S) = US flag
        let ri_u = '\u{1F1FA}';
        let remaining = "\u{1F1F8}".as_bytes();
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster(ri_u, remaining);
        assert_eq!(cluster, Some("\u{1F1FA}\u{1F1F8}".to_string()));
        assert_eq!(extra_bytes, 4); // each RI is 4 bytes
        assert_eq!(extra_chars, 1);
    }

    #[test]
    fn cluster_regional_indicator_stops_at_two() {
        // Three RIs: should only consume two (one pair), then stop
        let ri_u = '\u{1F1FA}';
        let remaining_str = "\u{1F1F8}\u{1F1E6}";
        let remaining = remaining_str.as_bytes();
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster(ri_u, remaining);
        assert_eq!(cluster, Some("\u{1F1FA}\u{1F1F8}".to_string()));
        assert_eq!(extra_bytes, 4); // only one extra RI consumed
        assert_eq!(extra_chars, 1);
    }

    #[test]
    fn cluster_zwj_at_end_of_remaining() {
        // ZWJ at end of buffer with nothing after it
        let remaining = "\u{200D}".as_bytes();
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster('\u{1F468}', remaining);
        // ZWJ is consumed but no char after it
        assert_eq!(cluster, Some("\u{1F468}\u{200D}".to_string()));
        assert_eq!(extra_bytes, 3); // ZWJ is 3 bytes
        assert_eq!(extra_chars, 1);
    }

    #[test]
    fn cluster_skin_tone_then_ascii() {
        // Emoji + skin tone + ASCII — should stop at ASCII
        let remaining_str = "\u{1F3FB}A";
        let remaining = remaining_str.as_bytes();
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster('\u{1F44D}', remaining);
        assert_eq!(cluster, Some("\u{1F44D}\u{1F3FB}".to_string()));
        assert_eq!(extra_bytes, 4);
        assert_eq!(extra_chars, 1);
    }

    #[test]
    fn cluster_non_ri_base_with_ri_in_remaining() {
        // Non-RI base followed by RI should NOT absorb the RI
        let remaining = "\u{1F1FA}".as_bytes();
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster('A', remaining);
        assert!(cluster.is_none());
        assert_eq!(extra_bytes, 0);
        assert_eq!(extra_chars, 0);
    }

    #[test]
    fn cluster_zwj_emoji_sequence_family() {
        // Man + ZWJ + Woman + ZWJ + Boy
        // U+1F468 + U+200D + U+1F469 + U+200D + U+1F466
        let remaining_str = "\u{200D}\u{1F469}\u{200D}\u{1F466}";
        let remaining = remaining_str.as_bytes();
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster('\u{1F468}', remaining);
        assert_eq!(
            cluster,
            Some("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F466}".to_string())
        );
        // ZWJ=3 + woman=4 + ZWJ=3 + boy=4 = 14
        assert_eq!(extra_bytes, 14);
        assert_eq!(extra_chars, 4);
    }

    #[test]
    fn cluster_combining_on_cjk_base() {
        // CJK character + combining mark
        let remaining = "\u{0300}".as_bytes();
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster('中', remaining);
        assert_eq!(cluster, Some("中\u{0300}".to_string()));
        assert_eq!(extra_bytes, 2);
        assert_eq!(extra_chars, 1);
    }

    #[test]
    fn cluster_keycap_sequence() {
        // '3' + U+FE0F (VS16) + U+20E3 (combining enclosing keycap)
        let remaining_str = "\u{FE0F}\u{20E3}";
        let remaining = remaining_str.as_bytes();
        let (cluster, extra_bytes, extra_chars) = collect_grapheme_cluster('3', remaining);
        assert_eq!(cluster, Some("3\u{FE0F}\u{20E3}".to_string()));
        // FE0F = 3 bytes, 20E3 = 3 bytes
        assert_eq!(extra_bytes, 6);
        assert_eq!(extra_chars, 2);
    }

    // ---------------------------------------------------------------
    // is_potentially_glyphless tests
    // ---------------------------------------------------------------

    #[test]
    fn glyphless_c1_control_chars() {
        assert!(is_potentially_glyphless('\u{0080}'));
        assert!(is_potentially_glyphless('\u{009F}'));
        assert!(is_potentially_glyphless('\u{0085}')); // NEL
    }

    #[test]
    fn glyphless_soft_hyphen() {
        assert!(is_potentially_glyphless('\u{00AD}'));
    }

    #[test]
    fn glyphless_zero_width_chars() {
        assert!(is_potentially_glyphless('\u{200B}')); // ZWSP
        assert!(is_potentially_glyphless('\u{200C}')); // ZWNJ
        assert!(is_potentially_glyphless('\u{200D}')); // ZWJ
        assert!(is_potentially_glyphless('\u{200E}')); // LRM
        assert!(is_potentially_glyphless('\u{200F}')); // RLM
    }

    #[test]
    fn glyphless_bidi_embedding() {
        assert!(is_potentially_glyphless('\u{202A}')); // LRE
        assert!(is_potentially_glyphless('\u{202E}')); // RLO
    }

    #[test]
    fn glyphless_word_joiner_and_invisible_separators() {
        assert!(is_potentially_glyphless('\u{2060}')); // word joiner
        assert!(is_potentially_glyphless('\u{2069}'));
    }

    #[test]
    fn glyphless_line_paragraph_separator() {
        assert!(is_potentially_glyphless('\u{2028}')); // line separator
        assert!(is_potentially_glyphless('\u{2029}')); // paragraph separator
    }

    #[test]
    fn glyphless_bom() {
        assert!(is_potentially_glyphless('\u{FEFF}'));
    }

    #[test]
    fn glyphless_specials_block() {
        assert!(is_potentially_glyphless('\u{FFF0}'));
        assert!(is_potentially_glyphless('\u{FFFD}')); // replacement char
    }

    #[test]
    fn glyphless_tags_block() {
        assert!(is_potentially_glyphless('\u{E0000}'));
        assert!(is_potentially_glyphless('\u{E007F}')); // CANCEL TAG
    }

    #[test]
    fn glyphless_ascii_not_glyphless() {
        // Normal ASCII characters should not be glyphless
        assert!(!is_potentially_glyphless('A'));
        assert!(!is_potentially_glyphless(' '));
        assert!(!is_potentially_glyphless('0'));
        assert!(!is_potentially_glyphless('\n'));
    }

    #[test]
    fn glyphless_c0_control_not_glyphless() {
        // C0 controls (0x00-0x1F) are NOT in the glyphless ranges
        // (they are handled differently by Emacs)
        assert!(!is_potentially_glyphless('\u{0001}')); // SOH
        assert!(!is_potentially_glyphless('\u{001F}')); // US
    }

    #[test]
    fn glyphless_normal_latin_not_glyphless() {
        assert!(!is_potentially_glyphless('é'));
        assert!(!is_potentially_glyphless('ü'));
    }

    #[test]
    fn glyphless_cjk_not_glyphless() {
        assert!(!is_potentially_glyphless('中'));
    }

    #[test]
    fn glyphless_emoji_not_glyphless() {
        assert!(!is_potentially_glyphless('\u{1F600}'));
    }

    #[test]
    fn glyphless_just_outside_c1_range() {
        // U+007F (DEL) is NOT in the C1 range
        assert!(!is_potentially_glyphless('\u{007F}'));
        // U+00A0 (NBSP) is NOT glyphless
        assert!(!is_potentially_glyphless('\u{00A0}'));
    }

    // ---------------------------------------------------------------
    // Integration / round-trip tests
    // ---------------------------------------------------------------

    #[test]
    fn decode_utf8_sequential_walk() {
        // Decode an entire multi-byte string character by character
        let input = "Aé中\u{1F600}";
        let bytes = input.as_bytes();
        let mut pos = 0;
        let mut chars = Vec::new();

        while pos < bytes.len() {
            let (ch, len) = decode_utf8(&bytes[pos..]);
            assert!(len > 0, "must advance at least 1 byte");
            chars.push(ch);
            pos += len;
        }

        assert_eq!(chars, vec!['A', 'é', '中', '\u{1F600}']);
    }

    #[test]
    fn wide_and_emoji_mutual_coverage() {
        // Every emoji presentation char should also be wide via is_wide_char
        let emoji_cps: Vec<u32> =
            vec![0x1F600, 0x1F4A9, 0x1F680, 0x1F1E6, 0x1F004, 0x1F0CF, 0x2702];
        for cp in emoji_cps {
            if let Some(ch) = char::from_u32(cp) {
                assert!(is_wide_char(ch), "emoji 0x{:X} should be wide", cp);
            }
        }
    }

    #[test]
    fn cluster_extender_and_glyphless_overlap() {
        // Some chars are both cluster extenders and potentially glyphless
        // ZWJ (U+200D) is both
        assert!(is_cluster_extender('\u{200D}'));
        assert!(is_potentially_glyphless('\u{200D}'));
        // ZWSP (U+200B) is both
        assert!(is_cluster_extender('\u{200B}'));
        assert!(is_potentially_glyphless('\u{200B}'));
        // BOM (U+FEFF) is both
        assert!(is_cluster_extender('\u{FEFF}'));
        assert!(is_potentially_glyphless('\u{FEFF}'));
    }
}
