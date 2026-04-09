//! Shared benchmark helpers for the neovm-gc criterion suite.
//!
//! Every bench uses these types and workload builders so
//! numbers across benches are apples-to-apples. Any new
//! bench should start from one of the workload primitives
//! defined here.

#![allow(dead_code)]

use neovm_gc::spaces::{LargeObjectSpaceConfig, NurseryConfig, OldGenConfig, PinnedSpaceConfig};
use neovm_gc::{EdgeCell, HeapConfig, MovePolicy, Relocator, Trace, Tracer};

// ---------- Test object types ----------

/// Small leaf: one word payload. Exercises the tiny-object
/// allocation path. Total record size (header + payload) is
/// the smallest meaningful nursery allocation.
#[derive(Debug)]
#[allow(dead_code)]
pub struct SmallLeaf(pub u64);

unsafe impl Trace for SmallLeaf {
    fn trace(&self, _: &mut dyn Tracer) {}
    fn relocate(&self, _: &mut dyn Relocator) {}
}

/// Medium leaf: 128 bytes of payload. Sits on the boundary
/// of cache-line-friendly small allocations.
#[derive(Debug)]
#[allow(dead_code)]
pub struct MediumLeaf(pub [u8; 128]);

unsafe impl Trace for MediumLeaf {
    fn trace(&self, _: &mut dyn Tracer) {}
    fn relocate(&self, _: &mut dyn Relocator) {}
}

/// Large leaf: 1 KiB of payload. Still nursery-sized with
/// the default `max_regular_object_bytes = 64 KiB` config,
/// so it goes through the nursery bump path (not large
/// object space).
#[derive(Debug)]
#[allow(dead_code)]
pub struct LargeLeaf(pub [u8; 1024]);

unsafe impl Trace for LargeLeaf {
    fn trace(&self, _: &mut dyn Tracer) {}
    fn relocate(&self, _: &mut dyn Relocator) {}
}

/// Pinned leaf: exercises the pinned-space allocation path.
#[derive(Debug)]
#[allow(dead_code)]
pub struct PinnedLeaf(pub u64);

unsafe impl Trace for PinnedLeaf {
    fn trace(&self, _: &mut dyn Tracer) {}
    fn relocate(&self, _: &mut dyn Relocator) {}

    fn move_policy() -> MovePolicy
    where
        Self: Sized,
    {
        MovePolicy::Pinned
    }
}

/// Linked-list node with a managed edge to another node.
/// Used by barrier and mutation benches.
#[derive(Debug)]
#[allow(dead_code)]
pub struct Node {
    pub label: u64,
    pub next: EdgeCell<Node>,
}

unsafe impl Trace for Node {
    fn trace(&self, tracer: &mut dyn Tracer) {
        self.next.trace(tracer);
    }

    fn relocate(&self, relocator: &mut dyn Relocator) {
        self.next.relocate(relocator);
    }
}

// ---------- Heap config presets ----------

/// Huge nursery, no pacer soft trigger. Benchmarks that
/// want to measure the raw allocation path without GC
/// firing mid-run use this.
pub fn fast_alloc_config() -> HeapConfig {
    HeapConfig {
        nursery: NurseryConfig {
            semispace_bytes: 256 * 1024 * 1024,
            ..NurseryConfig::default()
        },
        ..HeapConfig::default()
    }
}

/// Default config — realistic steady-state workloads go
/// through this path.
pub fn default_config() -> HeapConfig {
    HeapConfig::default()
}

/// Tight nursery config designed to force frequent minor
/// GCs during a benchmark run. Used by collection-latency
/// benches.
pub fn tight_nursery_config(semispace_bytes: usize) -> HeapConfig {
    HeapConfig {
        nursery: NurseryConfig {
            semispace_bytes,
            ..NurseryConfig::default()
        },
        ..HeapConfig::default()
    }
}

/// Large-object-space config with a low threshold so the
/// large-object allocation path is exercised by small
/// allocations.
pub fn small_threshold_config() -> HeapConfig {
    HeapConfig {
        large: LargeObjectSpaceConfig {
            threshold_bytes: 64,
            ..LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    }
}

/// Pinned space with a very small reservation. Used to
/// force pinned-space allocation pressure.
pub fn tight_pinned_config(reserved_bytes: usize) -> HeapConfig {
    HeapConfig {
        pinned: PinnedSpaceConfig { reserved_bytes },
        ..HeapConfig::default()
    }
}

/// Config with concurrent mark workers enabled (for the
/// concurrent marker bench).
pub fn concurrent_mark_config() -> HeapConfig {
    HeapConfig {
        old: OldGenConfig {
            concurrent_mark_workers: 2,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    }
}
