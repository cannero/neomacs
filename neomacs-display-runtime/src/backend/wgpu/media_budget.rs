//! Unified memory budget management for all media caches.
//!
//! Provides a shared memory budget across images, video frames, and WebKit surfaces.
//! Each cache reports its usage; eviction is coordinated centrally.

use std::collections::BTreeMap;

/// Media type for priority ordering
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MediaType {
    /// Static images (lowest priority - can be reloaded)
    Image,
    /// Video frames (medium priority - can be re-decoded)
    Video,
    /// WebKit surfaces (highest priority - expensive to recreate)
    WebKit,
}

/// Entry in the media budget tracker
#[derive(Debug)]
pub struct BudgetEntry {
    pub media_type: MediaType,
    pub id: u32,
    pub size_bytes: usize,
    pub last_access: u64,
}

/// Unified media budget manager
pub struct MediaBudget {
    /// Maximum total memory in bytes (default: 256MB)
    max_memory: usize,
    /// Current total memory usage
    current_memory: usize,
    /// All tracked entries, ordered by (media_type, last_access)
    entries: BTreeMap<(MediaType, u64, u32), BudgetEntry>,
    /// Global access counter
    access_counter: u64,
}

impl MediaBudget {
    /// Create with default 256MB budget
    pub fn new() -> Self {
        Self::with_limit(256 * 1024 * 1024)
    }

    /// Create with custom memory limit
    pub fn with_limit(max_memory: usize) -> Self {
        Self {
            max_memory,
            current_memory: 0,
            entries: BTreeMap::new(),
            access_counter: 0,
        }
    }

    /// Register a new media item
    pub fn register(&mut self, media_type: MediaType, id: u32, size_bytes: usize) {
        self.access_counter += 1;
        let entry = BudgetEntry {
            media_type,
            id,
            size_bytes,
            last_access: self.access_counter,
        };
        self.entries
            .insert((media_type, self.access_counter, id), entry);
        self.current_memory += size_bytes;

        tracing::trace!(
            "MediaBudget: registered {:?}:{} ({}KB), total={}MB/{}MB",
            media_type,
            id,
            size_bytes / 1024,
            self.current_memory / (1024 * 1024),
            self.max_memory / (1024 * 1024)
        );
    }

    /// Unregister a media item
    pub fn unregister(&mut self, media_type: MediaType, id: u32) {
        let key = self
            .entries
            .iter()
            .find(|(_, e)| e.media_type == media_type && e.id == id)
            .map(|(k, _)| *k);

        if let Some(key) = key {
            if let Some(entry) = self.entries.remove(&key) {
                self.current_memory = self.current_memory.saturating_sub(entry.size_bytes);
            }
        }
    }

    /// Touch an entry (update last access time)
    pub fn touch(&mut self, media_type: MediaType, id: u32) {
        let old_key = self
            .entries
            .iter()
            .find(|(_, e)| e.media_type == media_type && e.id == id)
            .map(|(k, _)| *k);

        if let Some(old_key) = old_key {
            if let Some(mut entry) = self.entries.remove(&old_key) {
                self.access_counter += 1;
                entry.last_access = self.access_counter;
                self.entries
                    .insert((media_type, self.access_counter, id), entry);
            }
        }
    }

    /// Get items to evict to make room for new_size bytes
    pub fn get_eviction_candidates(&self, new_size: usize) -> Vec<(MediaType, u32)> {
        let mut candidates = Vec::new();
        let target = self.current_memory + new_size;

        if target <= self.max_memory {
            return candidates;
        }

        let mut freed = 0usize;
        let need_to_free = target - self.max_memory;

        for ((media_type, _, id), entry) in &self.entries {
            if freed >= need_to_free {
                break;
            }
            candidates.push((*media_type, *id));
            freed += entry.size_bytes;
        }

        candidates
    }

    /// Check if we're over budget
    pub fn is_over_budget(&self) -> bool {
        self.current_memory > self.max_memory
    }

    /// Get current memory usage
    pub fn current_usage(&self) -> usize {
        self.current_memory
    }

    /// Get max memory limit
    pub fn max_limit(&self) -> usize {
        self.max_memory
    }
}

impl Default for MediaBudget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "media_budget_test.rs"]
mod tests;
