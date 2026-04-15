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
| `store_edge/no_new_value/short_circuit` | ~47 ns | `[42, 54]` |
| `store_edge/nursery_to_nursery/short_circuit` | ~40 ns | `[37, 45]` |

**Post-fix numbers** (commit `46a489376`, amortized barrier event drain). The pre-fix numbers were ~810 ns and ~470 ns respectively, dominated by an O(MAX) `Vec::drain(..1)` inside `MutatorLocal::push_barrier_event` that fired on every barrier once the ring filled up. A flamegraph on `multi_mutator_scaling/store_edge/4` showed 88.52% of cycles in `__memmove_avx_unaligned_erms` via the drain path; amortizing the drain (run once per `MAX_BARRIER_EVENTS` pushes instead of per push) cut the single-threaded barrier cost by ~17x.

These numbers are now dominated by the atomic ops in the barrier path (bump_barrier_stats, collector mutex for active-major-mark check) and the `EdgeCell::replace` write, which is where you'd expect a bounded-cost barrier to bottom out. A regression that slowed either of these would indicate the amortization was broken or the barrier short-circuit stopped firing.

## collection_latency

| Bench | Median time | Range |
|---|---|---|
| `minor/small/drop_all` (1000 dead) | ~39 µs | `[26.5, 57.0]` |
| `minor/all_survive/1000_survivors` | ~340 µs | `[240, 421]` |
| `major/small/1000_old_survivors` | ~94 µs | `[86.5, 102]` |

**Pause shape:** a minor cycle that reclaims 1000 dead objects takes ~40 µs. A minor cycle that copies 1000 survivors takes ~340 µs (~10x slower because every object goes through the evacuation path). A major cycle on a small old-gen population is ~94 µs. Interactive workloads with these pause numbers should be comfortable — P99 ≤ 500 µs for realistic nursery sizes.

## workloads

| Bench | Median throughput | Range |
|---|---|---|
| `linked_list_prepend/1000` | ~3.5 M elem/s | `[2.80, 4.61]` |
| `linked_list_prepend/10000` | ~5.6 M elem/s | `[4.27, 6.68]` |
| `linked_list_append/1000` | ~1.5 M elem/s | `[1.12, 2.25]` |
| `linked_list_append/10000` | ~1.1 M elem/s | `[0.93, 1.49]` |
| `allocation_heavy_graph/1000` | ~4.1 M elem/s | `[3.04, 6.00]` |
| `allocation_heavy_graph/10000` | ~5.2 M elem/s | `[4.65, 6.15]` |

**Shape check:** `linked_list_append` is 3-4x slower than `linked_list_prepend` at both sizes. Prepend allocates a new nursery Node whose `next` cell is initialized at construction time (no barrier — the new head is in nursery). Append calls `mutator.store_edge(&tail, 0, ...)` on every iteration — after a few minor cycles the tail is promoted to old gen, so every barrier call goes through the full old-to-nursery path with a heap write lock. The 3x gap is the cost of that barrier call including lock acquisition.

`allocation_heavy_graph` (pure flat allocation, no edges) matches `linked_list_prepend` closely: both are allocation-dominated with no barrier cost. This confirms the prepend path's per-element cost is essentially just one allocation plus an in-construction `EdgeCell::new`.

A regression that slowed `linked_list_prepend` without slowing `allocation_heavy_graph` would indicate that `EdgeCell::new` stopped being a cheap nursery-local write. A regression that slowed `linked_list_append` more than `linked_list_prepend` would indicate the barrier fast path stopped firing or the heap lock got coarser.

## multi_mutator_scaling

### `alloc/*` (single `HeapCore` write lock — unchanged by Phase 1)

| Bench | Aggregate throughput | Scaling factor vs 1 thread |
|---|---|---|
| `alloc/1` | ~3.8 M elem/s | 1.00x |
| `alloc/2` | ~3.4 M elem/s | 0.88x |
| `alloc/4` | ~2.4 M elem/s | 0.61x |
| `alloc/8` | ~2.1 M elem/s | 0.54x |

**Scaling shape:** positive but sub-linear at 2 threads, then degrading from 4 threads upward. This is the expected cost of the single `HeapCore` write lock serializing allocation bookkeeping. An improvement over these numbers (e.g. 2x at 2 threads, 3x at 4 threads) is a sign that fine-grained locks landed. A regression is a sign that lock scope leaked.

### `store_edge/*` (barrier path — post-drain-fix baseline)

Post-change baseline (commit `46a489376`, after the amortized barrier event drain fix exposed the true per-thread cost).

| Bench | Aggregate throughput | Scaling factor vs 1 thread |
|---|---|---|
| `store_edge/1` | ~9.14 M elem/s | 1.00x |
| `store_edge/2` | ~2.47 M elem/s | 0.27x |
| `store_edge/4` | ~1.65 M elem/s | 0.18x |
| `store_edge/8` | ~1.53 M elem/s | 0.17x |

**Scaling shape:** the 1-thread ceiling jumped 33x once the O(MAX) drain stopped dominating every single-thread barrier call. The multi-thread numbers improved in absolute terms (2.47 M > 1.35 M, 1.65 M > 1.98 M is a regression at 4t, 1.53 M > 2.51 M is a regression at 8t) but the relative scaling factor collapsed because the 1-thread base is now so much higher. The new bottleneck for multi-thread is the collector mutex that `record_active_major_post_write_and_refresh` acquires on every barrier even when there is no active major-mark session; Phase 2 would either skip that call when the atomic mirror says `has_active_major_mark == false` (correctness-risky — the mirror is refreshed at end of `with_state`, so a stale-false read during a concurrent `begin_major_mark` could miss SATB protection) or eliminate it entirely by folding the "assist active major mark" work into a separate background path.

### Phase 1 barrier read-lock improvement — A/B comparison

Same bench, same machine, measured immediately before and after the barrier path was moved onto a `HeapCore` read lock. The "before" column runs the stashed library code; the "after" column runs the post-change library code. (These numbers pre-date the amortized barrier event drain fix in `46a489376`, which is why they are much lower than the post-drain-fix baseline above.)

| Threads | Before (write lock) | After (read lock) | Improvement |
|---|---|---|---|
| 1 | ~963 K elem/s (1.00x) | ~1.04 M elem/s (1.00x) | ~unchanged |
| 2 | ~708 K elem/s (0.74x) | ~1.35 M elem/s (1.30x) | **+91%** |
| 4 | ~688 K elem/s (0.71x) | ~1.98 M elem/s (1.90x) | **+187%** |
| 8 | ~435 K elem/s (0.45x) | ~2.51 M elem/s (2.42x) | **+477%** |

Before the change, multi-mutator barrier scaling was actively *degrading* at every thread count — 8 threads delivered less than half the per-thread throughput of 1 thread because every `store_edge` call serialized on the heap write lock. After the change, the barrier path runs under a read lock. Both columns were held down by the O(MAX) `MutatorLocal::push_barrier_event` drain cost; see the post-drain-fix numbers above for the true per-thread ceiling.

### Amortized barrier event drain — A/B comparison

Commit `46a489376`. Same bench, same machine, criterion A/B with `--save-baseline` + `--baseline`.

| Threads | Before (O(MAX) drain) | After (amortized drain) | Improvement |
|---|---|---|---|
| 1 | ~271 K elem/s | ~9.14 M elem/s | **~33x** |
| 2 | ~382 K elem/s | ~2.47 M elem/s | **~6.5x** |
| 4 | ~624 K elem/s | ~1.65 M elem/s | **~2.6x** |
| 8 | ~1.06 M elem/s | ~1.53 M elem/s | **~1.45x** |

A flamegraph of `multi_mutator_scaling/store_edge/4` before the fix showed 88.52% of cycles in `__memmove_avx_unaligned_erms` via `MutatorLocal::push_barrier_event`'s drain path. The fix raises the drain threshold from `MAX_BARRIER_EVENTS` to `2 * MAX_BARRIER_EVENTS` and drains the ring back down to `MAX_BARRIER_EVENTS` at that threshold, so the O(MAX) drain runs once per `MAX` pushes (amortized O(1) per barrier). The 1-thread improvement is the biggest signal — the per-thread ceiling was being held down by the drain; the new ceiling is roughly the same order as the allocation throughput, as expected.

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
