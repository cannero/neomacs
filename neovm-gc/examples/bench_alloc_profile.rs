//! Stage 0 investigation: measure the raw single-mutator
//! allocation throughput with several harness variants to
//! find where the quick bench's ~49 K alloc/sec number
//! comes from.
//!
//! Variants:
//!
//! A. **no-handle-scope**: one `HandleScope` for the entire
//!    loop, no periodic drop. Measures allocation + root
//!    install cost, no scope-tear-down cost.
//!
//! B. **scope_every_n**: drop and recreate the `HandleScope`
//!    every N allocations. Matches the shape of the existing
//!    bench. Varies N over {1000, 100, 10, 1} so the
//!    overhead contribution of scope drops is visible.
//!
//! C. **forget_root**: allocate, convert to Gc via
//!    `Root::as_gc`, then forget the root so the stack does
//!    not grow. Same as A in practice but confirms that the
//!    root stack growth is what dominates (or isn't).
//!
//! All variants use a 256 MiB nursery so no minor cycle
//! fires during the measurement window. The goal is to
//! isolate the allocation-path cost from GC cost entirely.
//!
//! Usage:
//!
//! ```text
//! cargo run --release --example bench_alloc_profile -p neovm-gc
//! ```

use neovm_gc::spaces::NurseryConfig;
use neovm_gc::{Heap, HeapConfig, Relocator, Trace, Tracer};
use std::time::Instant;

#[derive(Debug)]
#[allow(dead_code)]
struct Leaf(u64);

unsafe impl Trace for Leaf {
    fn trace(&self, _: &mut dyn Tracer) {}
    fn relocate(&self, _: &mut dyn Relocator) {}
}

fn bench_config() -> HeapConfig {
    HeapConfig {
        nursery: NurseryConfig {
            // 256 MiB semispace: big enough that no minor GC
            // fires during a 1M-alloc loop.
            semispace_bytes: 256 * 1024 * 1024,
            ..NurseryConfig::default()
        },
        ..HeapConfig::default()
    }
}

/// Allocation counts for the scaling study. We run the
/// variants at several sizes so a super-linear curve (e.g.
/// O(n²) from walking the root stack on every alloc) is
/// visible vs a flat per-alloc cost curve (linear O(n)).
const ALLOCATION_COUNTS: &[u64] = &[1_000, 10_000, 50_000];

fn format_rate(allocations: u64, elapsed_ns: u64) -> String {
    let per_alloc_ns = (elapsed_ns as f64) / (allocations as f64);
    let rate = (allocations as f64) / ((elapsed_ns as f64) / 1e9);
    if rate >= 1e6 {
        format!("{:>8.2} ns/alloc  ({:>7.2} M alloc/sec)", per_alloc_ns, rate / 1e6)
    } else if rate >= 1e3 {
        format!("{:>8.2} ns/alloc  ({:>7.2} K alloc/sec)", per_alloc_ns, rate / 1e3)
    } else {
        format!("{:>8.2} ns/alloc  ({:>7.2}   alloc/sec)", per_alloc_ns, rate)
    }
}

fn bench_variant_a_single_scope(allocations: u64) -> (u64, u64) {
    let heap = Heap::new(bench_config());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();

    let start = Instant::now();
    for i in 0..allocations {
        mutator.alloc(&mut scope, Leaf(i)).expect("alloc");
    }
    let elapsed_ns = start.elapsed().as_nanos() as u64;
    (allocations, elapsed_ns)
}

fn bench_variant_b_scope_every_n(allocations: u64, n: u64) -> (u64, u64) {
    let heap = Heap::new(bench_config());
    let mut mutator = heap.mutator();

    let start = Instant::now();
    let mut remaining = allocations;
    while remaining > 0 {
        let batch = remaining.min(n);
        let mut scope = mutator.handle_scope();
        for i in 0..batch {
            mutator.alloc(&mut scope, Leaf(i)).expect("alloc");
        }
        drop(scope);
        remaining -= batch;
    }
    let elapsed_ns = start.elapsed().as_nanos() as u64;
    (allocations, elapsed_ns)
}

fn main() {
    println!("neovm-gc stage 0 allocation profile");
    println!("====================================");
    println!("(256 MiB nursery, no GC fires)");
    println!();

    println!("Variant A: single HandleScope (root stack grows)");
    for &n in ALLOCATION_COUNTS {
        let (a, t) = bench_variant_a_single_scope(n);
        println!("  {:>6} allocs : {}", n, format_rate(a, t));
    }
    println!();

    println!("Variant B: scope every 1000 (root stack bounded)");
    for &n in ALLOCATION_COUNTS {
        let (a, t) = bench_variant_b_scope_every_n(n, 1_000);
        println!("  {:>6} allocs : {}", n, format_rate(a, t));
    }
    println!();

    println!("Variant B: scope every 100");
    for &n in ALLOCATION_COUNTS {
        let (a, t) = bench_variant_b_scope_every_n(n, 100);
        println!("  {:>6} allocs : {}", n, format_rate(a, t));
    }
    println!();

    println!("Variant B: scope every 10");
    for &n in ALLOCATION_COUNTS {
        let (a, t) = bench_variant_b_scope_every_n(n, 10);
        println!("  {:>6} allocs : {}", n, format_rate(a, t));
    }
    println!();

    println!("Analysis hint:");
    println!("  - If Variant A per-alloc cost rises with allocation count,");
    println!("    the allocation path is O(n) in the root stack size.");
    println!("  - If Variant B per-alloc cost is stable across sizes but");
    println!("    differs across scope_every_n values, the scope drop is");
    println!("    the dominant per-batch cost.");
    println!("  - If Variant B scope-every-1 is way slower than scope-every-1000,");
    println!("    HandleScope construction+drop is dominating over alloc.");
}
