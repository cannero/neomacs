use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(crate) enum HotpathOp {
    StringMatch,
    MatchBeginning,
    MatchEnd,
    RegexpQuote,
    Substring,
    Concat,
    Format,
    Assoc,
}

#[derive(Clone, Copy, Default)]
struct HotpathCounter {
    count: u64,
    total: Duration,
    max: Duration,
}

impl HotpathCounter {
    fn record(&mut self, elapsed: Duration) {
        self.count += 1;
        self.total += elapsed;
        self.max = self.max.max(elapsed);
    }
}

#[derive(Clone, Default)]
struct HotpathStats {
    string_match: HotpathCounter,
    match_beginning: HotpathCounter,
    match_end: HotpathCounter,
    regexp_quote: HotpathCounter,
    substring: HotpathCounter,
    concat: HotpathCounter,
    format: HotpathCounter,
    assoc: HotpathCounter,
}

impl HotpathStats {
    fn counter_mut(&mut self, op: HotpathOp) -> &mut HotpathCounter {
        match op {
            HotpathOp::StringMatch => &mut self.string_match,
            HotpathOp::MatchBeginning => &mut self.match_beginning,
            HotpathOp::MatchEnd => &mut self.match_end,
            HotpathOp::RegexpQuote => &mut self.regexp_quote,
            HotpathOp::Substring => &mut self.substring,
            HotpathOp::Concat => &mut self.concat,
            HotpathOp::Format => &mut self.format,
            HotpathOp::Assoc => &mut self.assoc,
        }
    }

    fn entries(&self) -> [(&'static str, HotpathCounter); 8] {
        [
            ("string-match", self.string_match),
            ("match-beginning", self.match_beginning),
            ("match-end", self.match_end),
            ("regexp-quote", self.regexp_quote),
            ("substring", self.substring),
            ("concat", self.concat),
            ("format", self.format),
            ("assoc", self.assoc),
        ]
    }
}

thread_local! {
    static HOTPATH_STATS: RefCell<HotpathStats> = RefCell::new(HotpathStats::default());
}

pub(crate) fn hotpath_timing_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("NEOVM_ORACLE_TIMING").is_some()
            || std::env::var_os("NEOVM_HOTPATH_TIMING").is_some()
    })
}

pub(crate) fn time_op<R>(op: HotpathOp, f: impl FnOnce() -> R) -> R {
    if !hotpath_timing_enabled() {
        return f();
    }

    let start = Instant::now();
    let result = f();
    HOTPATH_STATS.with(|stats| {
        stats.borrow_mut().counter_mut(op).record(start.elapsed());
    });
    result
}

pub(crate) fn reset_hotpath_stats() {
    if !hotpath_timing_enabled() {
        return;
    }
    HOTPATH_STATS.with(|stats| {
        *stats.borrow_mut() = HotpathStats::default();
    });
}

pub(crate) fn log_hotpath_stats(label: &str) {
    if !hotpath_timing_enabled() {
        return;
    }

    HOTPATH_STATS.with(|stats| {
        let snapshot = std::mem::take(&mut *stats.borrow_mut());
        for (name, counter) in snapshot.entries() {
            if counter.count == 0 {
                continue;
            }
            let total_ms = counter.total.as_secs_f64() * 1000.0;
            let avg_ms = total_ms / counter.count as f64;
            let max_ms = counter.max.as_secs_f64() * 1000.0;
            tracing::info!(
                "{label}: builtin={name} count={} total_ms={:.3} avg_ms={:.3} max_ms={:.3}",
                counter.count,
                total_ms,
                avg_ms,
                max_ms
            );
        }
    });
}
