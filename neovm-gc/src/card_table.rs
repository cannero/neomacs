//! Card-table remembered-set primitive (Phase 4 foundation).
//!
//! A [`CardTable`] maps a contiguous address range to a dense byte
//! array, one byte per `CARD_SIZE`-aligned card. The intended use is
//! as the backing data structure for a fast write barrier: given an
//! owner address inside the covered range, compute the card index in
//! constant time and set the card byte to 1.
//!
//! This module is standalone infrastructure — Phase 4 will wire it
//! into the write barrier path and minor GC root scan once the
//! Immix old-generation has stable contiguous region addressing
//! (Phase 2 delivers a block-based old-gen allocator as its MVP but
//! blocks are allocated on demand; a per-block card table is a
//! natural follow-up).
//!
//! Design notes:
//! - Card size is 512 bytes by default (a common sweet spot: small
//!   enough to scan quickly, large enough to amortize card metadata).
//! - Clearing the table is O(N bytes / 64) using `write_bytes`, so
//!   minor GC can reset the dirty set in bulk after processing.
//! - `record_write` is `#[inline]` so write-barrier call sites
//!   compile into a handful of arithmetic ops and a single byte store.
//!
//! The card table is NOT a general-purpose hash set. Addresses
//! outside the covered range are silently dropped by `record_write`
//! via the fast-path range check; callers must ensure they use the
//! right table for each address space.

use core::sync::atomic::{AtomicU8, Ordering};

/// Default card size in bytes. 512B is the canonical choice from G1
/// and similar collectors: 8 cards cover one 4KB page.
pub(crate) const DEFAULT_CARD_SIZE_BYTES: usize = 512;

/// A dense card table covering one contiguous address range.
///
/// Each card is a single byte (`AtomicU8`), so the barrier can set
/// cards from multiple threads without any locking.
#[derive(Debug)]
pub(crate) struct CardTable {
    /// Base of the covered address range (inclusive).
    base: usize,
    /// End of the covered address range (exclusive).
    end: usize,
    /// Card size in bytes. Must be a power of two.
    card_size: usize,
    /// log2(card_size), cached for fast index computation.
    card_shift: u32,
    /// One entry per card. Each byte is either 0 (clean) or 1 (dirty).
    cards: Box<[AtomicU8]>,
}

/// Card state values. The table uses byte granularity so entries can
/// be extended in later phases to encode more detailed dirty reasons
/// (e.g. "contains an old-to-young edge" vs "modified since last GC").
pub(crate) const CARD_CLEAN: u8 = 0;
pub(crate) const CARD_DIRTY: u8 = 1;

impl CardTable {
    /// Build a card table covering the half-open address range
    /// `[base, base + length_bytes)`. Internally rounds the length
    /// up to a multiple of `card_size` so the final card covers the
    /// trailing remainder of the range.
    ///
    /// Panics if `card_size` is not a non-zero power of two.
    pub(crate) fn new(base: usize, length_bytes: usize, card_size: usize) -> Self {
        assert!(
            card_size.is_power_of_two() && card_size > 0,
            "card_size must be a non-zero power of two"
        );
        let card_shift = card_size.trailing_zeros();
        let card_count = length_bytes.div_ceil(card_size);
        let mut cards = Vec::with_capacity(card_count);
        for _ in 0..card_count {
            cards.push(AtomicU8::new(CARD_CLEAN));
        }
        Self {
            base,
            end: base.saturating_add(length_bytes),
            card_size,
            card_shift,
            cards: cards.into_boxed_slice(),
        }
    }

    /// Build a card table with the default 512-byte card size.
    pub(crate) fn with_default_card_size(base: usize, length_bytes: usize) -> Self {
        Self::new(base, length_bytes, DEFAULT_CARD_SIZE_BYTES)
    }

    /// Number of cards in this table.
    #[allow(dead_code)]
    pub(crate) fn card_count(&self) -> usize {
        self.cards.len()
    }

    /// Card size in bytes.
    #[allow(dead_code)]
    pub(crate) fn card_size(&self) -> usize {
        self.card_size
    }

    /// True if `addr` falls inside the table's covered range.
    #[inline]
    pub(crate) fn covers(&self, addr: usize) -> bool {
        addr >= self.base && addr < self.end
    }

    /// Compute the card index for `addr`. Returns `None` if the
    /// address is outside the covered range.
    #[inline]
    pub(crate) fn card_index_of(&self, addr: usize) -> Option<usize> {
        if !self.covers(addr) {
            return None;
        }
        Some((addr - self.base) >> self.card_shift)
    }

    /// Mark the card containing `addr` as dirty. No-op if `addr` is
    /// outside the covered range. This is the write-barrier fast path.
    #[inline]
    pub(crate) fn record_write(&self, addr: usize) {
        let Some(index) = self.card_index_of(addr) else {
            return;
        };
        // Relaxed is sufficient: the barrier only needs to make the
        // dirty bit visible by the next stop-the-world remark, which
        // already contains a full memory fence.
        self.cards[index].store(CARD_DIRTY, Ordering::Relaxed);
    }

    /// Check whether the card containing `addr` is dirty. Returns
    /// `false` if `addr` is outside the covered range.
    #[allow(dead_code)]
    pub(crate) fn is_dirty(&self, addr: usize) -> bool {
        let Some(index) = self.card_index_of(addr) else {
            return false;
        };
        self.cards[index].load(Ordering::Acquire) == CARD_DIRTY
    }

    /// Clear every card back to `CARD_CLEAN`. Typically invoked at the
    /// end of a minor GC after dirty cards have been processed.
    pub(crate) fn clear_all(&self) {
        for card in self.cards.iter() {
            card.store(CARD_CLEAN, Ordering::Relaxed);
        }
    }

    /// Iterate the card indices that are currently dirty. Useful for
    /// the minor-GC root scan that walks dirty cards to find old-to-
    /// young references.
    pub(crate) fn dirty_card_indices(&self) -> Vec<usize> {
        self.cards
            .iter()
            .enumerate()
            .filter_map(|(index, card)| {
                if card.load(Ordering::Acquire) == CARD_DIRTY {
                    Some(index)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Return the address range covered by card `index`.
    pub(crate) fn card_range(&self, index: usize) -> (usize, usize) {
        let start = self.base + (index << self.card_shift);
        let end = start + self.card_size;
        (start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_index_of_inside_range_is_offset_divided_by_card_size() {
        let table = CardTable::new(0x1000, 0x2000, 512);
        assert_eq!(table.card_index_of(0x1000), Some(0));
        assert_eq!(table.card_index_of(0x11ff), Some(0));
        assert_eq!(table.card_index_of(0x1200), Some(1));
        // Last byte of the 0x2000-byte covered range → card 15.
        assert_eq!(table.card_index_of(0x2fff), Some(15));
    }

    #[test]
    fn card_index_of_outside_range_is_none() {
        let table = CardTable::new(0x1000, 0x1000, 512);
        assert_eq!(table.card_index_of(0x0fff), None);
        assert_eq!(table.card_index_of(0x2000), None);
        assert_eq!(table.card_index_of(0x5000), None);
    }

    #[test]
    fn record_write_sets_card_dirty_in_range() {
        let table = CardTable::new(0x1000, 0x1000, 512);
        assert!(!table.is_dirty(0x1000));
        table.record_write(0x1234);
        assert!(table.is_dirty(0x1234));
        // Neighboring card unaffected.
        assert!(!table.is_dirty(0x1400));
    }

    #[test]
    fn record_write_outside_range_is_a_no_op() {
        let table = CardTable::new(0x1000, 0x1000, 512);
        table.record_write(0x5000);
        // No panic, and the table still reports clean for every
        // in-range card.
        for index in 0..table.card_count() {
            let (start, _) = table.card_range(index);
            assert!(!table.is_dirty(start));
        }
    }

    #[test]
    fn clear_all_restores_every_card_to_clean() {
        let table = CardTable::new(0x1000, 0x1000, 512);
        table.record_write(0x1100);
        table.record_write(0x1600);
        table.record_write(0x1f00);
        assert_eq!(table.dirty_card_indices().len(), 3);
        table.clear_all();
        assert_eq!(table.dirty_card_indices().len(), 0);
    }

    #[test]
    fn dirty_card_indices_reports_indices_in_ascending_order() {
        let table = CardTable::new(0x0, 0x2000, 512);
        table.record_write(0x1000); // card 8
        table.record_write(0x200);  // card 1
        table.record_write(0x1800); // card 12
        let dirty = table.dirty_card_indices();
        assert_eq!(dirty, vec![1, 8, 12]);
    }

    #[test]
    fn card_range_matches_index() {
        let table = CardTable::new(0x1000, 0x1000, 512);
        assert_eq!(table.card_range(0), (0x1000, 0x1200));
        assert_eq!(table.card_range(7), (0x1e00, 0x2000));
    }

    #[test]
    fn default_card_size_is_512_bytes() {
        let table = CardTable::with_default_card_size(0, 4096);
        assert_eq!(table.card_size(), DEFAULT_CARD_SIZE_BYTES);
        assert_eq!(table.card_count(), 4096 / DEFAULT_CARD_SIZE_BYTES);
    }
}
