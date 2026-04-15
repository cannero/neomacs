//! Card-table remembered-set primitive.
//!
//! A [`CardTable`] maps a contiguous address range to a dense byte
//! array, one byte per `CARD_SIZE`-aligned card. The intended use is
//! as the backing data structure for a fast write barrier: given an
//! owner address inside the covered range, compute the card index in
//! constant time and set the card byte to 1.
//!
//! Each old-gen block owns its own card table. The write barrier
//! looks up the block via `OldGenState::find_block_for_addr` and
//! marks the appropriate card in O(1). The minor GC's dirty-card
//! scan walks every block's dirty cards to find the records that
//! need to be treated as additional roots.
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
    #[allow(dead_code)]
    pub(crate) fn card_range(&self, index: usize) -> (usize, usize) {
        let start = self.base + (index << self.card_shift);
        let end = start + self.card_size;
        (start, end)
    }
}

#[cfg(test)]
#[path = "card_table_test.rs"]
mod tests;
