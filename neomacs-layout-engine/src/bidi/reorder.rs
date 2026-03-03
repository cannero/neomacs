//! Visual reordering of bidi-resolved text (UAX#9 L2-L4).

use super::tables::bidi_mirror;

/// Reorder characters into visual order based on resolved embedding levels.
///
/// Implements UAX#9 rule L2: reverse each maximal sequence of characters
/// at the same or higher level, starting from the highest level.
///
/// Returns a vector of indices into the original character array, in visual order.
pub fn reorder_visual(levels: &[u8]) -> Vec<usize> {
    let n = levels.len();
    if n == 0 {
        return Vec::new();
    }

    // Create initial logical order
    let mut order: Vec<usize> = (0..n).collect();

    if levels.iter().all(|&l| l == 0) {
        return order; // Fast path: all LTR
    }

    // Find the highest level
    let max_level = *levels.iter().max().unwrap();
    let min_odd_level = {
        let mut min = max_level;
        for &l in levels {
            if l > 0 && l % 2 == 1 && l < min {
                min = l;
            }
        }
        if min % 2 == 0 { min + 1 } else { min }
    };

    // L2: From the highest level down to the lowest odd level,
    // reverse any contiguous sequence of characters at that level or higher.
    let mut level = max_level;
    while level >= min_odd_level {
        let mut i = 0;
        while i < n {
            if levels[order[i]] >= level {
                // Find the end of this run
                let start = i;
                while i < n && levels[order[i]] >= level {
                    i += 1;
                }
                // Reverse the run
                order[start..i].reverse();
            } else {
                i += 1;
            }
        }
        if level == 0 {
            break;
        }
        level -= 1;
    }

    order
}

/// Apply character mirroring for RTL characters (L4).
///
/// Returns a new vector of characters with mirrored glyphs where appropriate.
/// Characters at odd embedding levels get their mirrored counterparts.
pub fn apply_mirroring(chars: &[char], levels: &[u8]) -> Vec<char> {
    chars
        .iter()
        .zip(levels.iter())
        .map(|(&ch, &level)| {
            if level % 2 == 1 {
                bidi_mirror(ch).unwrap_or(ch)
            } else {
                ch
            }
        })
        .collect()
}

/// Reorder a line of text into visual order, applying mirroring.
///
/// This is the high-level entry point for display: given text and resolved levels,
/// produce the visual character sequence.
pub fn reorder_line(chars: &[char], levels: &[u8]) -> Vec<char> {
    let mirrored = apply_mirroring(chars, levels);
    let visual_order = reorder_visual(levels);
    visual_order.iter().map(|&i| mirrored[i]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_ltr() {
        let levels = vec![0, 0, 0, 0, 0];
        let order = reorder_visual(&levels);
        assert_eq!(order, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn test_all_rtl() {
        let levels = vec![1, 1, 1, 1, 1];
        let order = reorder_visual(&levels);
        assert_eq!(order, vec![4, 3, 2, 1, 0]);
    }

    #[test]
    fn test_mixed_ltr_rtl() {
        // LTR LTR RTL RTL LTR
        let levels = vec![0, 0, 1, 1, 0];
        let order = reorder_visual(&levels);
        // RTL segment [2,3] is reversed
        assert_eq!(order, vec![0, 1, 3, 2, 4]);
    }

    #[test]
    fn test_nested_levels() {
        // Level 0, 1, 2, 1, 0
        let levels = vec![0, 1, 2, 1, 0];
        let order = reorder_visual(&levels);
        // Level 2: reverse [2] (single, no change from reversal alone)
        // Level 1: reverse [1,2,3] → [3,2,1]
        // Result: [0, 3, 2, 1, 4]
        assert_eq!(order, vec![0, 3, 2, 1, 4]);
    }

    #[test]
    fn test_empty() {
        let levels: Vec<u8> = vec![];
        let order = reorder_visual(&levels);
        assert!(order.is_empty());
    }

    #[test]
    fn test_mirroring() {
        let chars = vec!['(', 'A', ')'];
        let levels = vec![1, 1, 1];
        let mirrored = apply_mirroring(&chars, &levels);
        assert_eq!(mirrored, vec![')', 'A', '(']);
    }

    #[test]
    fn test_reorder_line() {
        // "Hello" + RTL "אב" + "World"
        let chars: Vec<char> = "Hello".chars().collect();
        let levels = vec![0, 0, 0, 0, 0];
        let result = reorder_line(&chars, &levels);
        assert_eq!(result, chars);
    }

    #[test]
    fn test_reorder_with_brackets() {
        let chars = vec!['(', 'A', ')'];
        let levels = vec![1, 1, 1]; // All RTL
        let result = reorder_line(&chars, &levels);
        // Visual order: reversed, with mirroring
        // Logical: ( A ) at levels 1,1,1
        // Mirror: ) A (
        // Reorder: reverse all → ( A )
        assert_eq!(result, vec!['(', 'A', ')']);
    }

    #[test]
    fn test_complex_reorder() {
        // LTR text with RTL embedded
        // Logical: A B [rtl]C D[/rtl] E F
        let levels = vec![0, 0, 1, 1, 0, 0];
        let order = reorder_visual(&levels);
        // RTL segment [2,3] reversed → [3,2]
        assert_eq!(order, vec![0, 1, 3, 2, 4, 5]);
    }
}
