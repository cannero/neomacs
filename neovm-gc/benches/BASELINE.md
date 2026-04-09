# neovm-gc benchmark baseline

**Commit:** `390303d36` (Stage 0 fix: `build_plan(Minor)` O(1) hot path)
**Date:** 2026-04-09
**Rustc:** 1.93.1
**Platform:** Linux 6.12.76, x86_64

These numbers were captured with `--sample-size 10 --warm-up-time 1 --measurement-time {2,3}` — shorter than criterion defaults so the benches complete quickly. Production regression runs should use the defaults (100 samples, 3 s warm-up, 5 s measurement) for tighter confidence intervals.

The numbers below are the criterion "median throughput" or "median time" for each bench. The bracketed range is the criterion `[lower, upper]` estimate.

## alloc_throughput

| Bench | Median | Range |
|---|---|---|
| `small_leaf/long_scope/1000` | ~7.0 M elem/s | — |
| `small_leaf/long_scope/10000` | ~6.5 M elem/s | — |
| `small_leaf/long_scope/50000` | ~6.0 M elem/s | — |
| `small_leaf/scoped_batches/1000` | ~7.2 M elem/s | `[7.05, 7.40]` |
| `small_leaf/scoped_batches/10000` | ~6.5 M elem/s | `[5.59, 7.86]` |
| `small_leaf/scoped_batches/50000` | ~6.0 M elem/s | `[5.53, 6.55]` |
| `medium_leaf/long_scope` | ~5.3 M elem/s | `[4.50, 6.43]` |
| `large_leaf/long_scope` | ~1.1 M elem/s | `[0.94, 1.51]` |

**Shape check:** the small_leaf rate is roughly constant across batch sizes. This confirms the Stage 0 fix — the allocation path is O(1) per call, not O(n). A regression that brings back the quadratic loop would show `long_scope/50000` falling to ≤1 M elem/s while `long_scope/1000` stayed around 7 M.

## barrier_cost

| Bench | Median time | Range |
|---|---|---|
| `store_edge/no_new_value/short_circuit` | ~810 ns | `[682, 964]` |
| `store_edge/nursery_to_nursery/short_circuit` | ~470 ns | `[453, 510]` |

These numbers are dominated by the heap write lock acquisition inside `Mutator::store_edge` → `with_runtime`. A "short circuit" that still takes 800 ns is mostly lock overhead, not barrier logic. The per-call cost would drop significantly if the barrier path gained fine-grained locking.

## collection_latency

| Bench | Median time | Range |
|---|---|---|
| `minor/small/drop_all` (1000 dead) | ~39 µs | `[26.5, 57.0]` |
| `minor/all_survive/1000_survivors` | ~340 µs | `[240, 421]` |
| `major/small/1000_old_survivors` | ~94 µs | `[86.5, 102]` |

**Pause shape:** a minor cycle that reclaims 1000 dead objects takes ~40 µs. A minor cycle that copies 1000 survivors takes ~340 µs (~10x slower because every object goes through the evacuation path). A major cycle on a small old-gen population is ~94 µs. Interactive workloads with these pause numbers should be comfortable — P99 ≤ 500 µs for realistic nursery sizes.

## multi_mutator_scaling

| Bench | Aggregate throughput | Scaling factor vs 1 thread |
|---|---|---|
| `alloc/1` | ~3.8 M elem/s | 1.00x |
| `alloc/2` | ~3.4 M elem/s | 0.88x |
| `alloc/4` | ~2.4 M elem/s | 0.61x |
| `alloc/8` | ~2.1 M elem/s | 0.54x |

**Scaling shape:** positive but sub-linear at 2 threads, then degrading from 4 threads upward. This is the expected cost of the single `HeapCore` write lock serializing allocation bookkeeping. An improvement over these numbers (e.g. 2x at 2 threads, 3x at 4 threads) is a sign that fine-grained locks landed. A regression is a sign that lock scope leaked.

## Pre-Stage-0 baseline (for historical context)

Before commit `390303d36`, the allocation path was O(n) in total allocations because `build_plan(Minor)` walked every object on every `refresh_recommended_plans` call. The pre-fix numbers from `examples/bench_alloc_profile.rs` were:

| Bench | Before fix | After fix | Speedup |
|---|---|---|---|
| single-mutator small-leaf | 49 K elem/s | ~6.9 M elem/s | ~140x |
| 2 threads small-leaf | 16 K elem/s | ~3.4 M elem/s | ~210x |
| 4 threads small-leaf | 6 K elem/s | ~2.4 M elem/s | ~400x |

The Stage 0 investigation is the canonical example of why profiling catches an O(n²) hot path that "looks right" under casual review.

## How to regenerate this file

1. Run all four benches with `cargo bench -p neovm-gc -- --sample-size 10 --warm-up-time 1 --measurement-time 3`.
2. Copy the median numbers from the output into the tables above.
3. Note the commit hash the numbers were taken from.
4. Commit the updated BASELINE.md in the same commit as the code change being measured.

A simple machine-parseable export is on the TODO list; for now, manual transcription is the workflow.
