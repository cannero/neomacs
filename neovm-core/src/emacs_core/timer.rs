//! Timer system for the Elisp VM.
//!
//! Provides Emacs-compatible timer functionality:
//! - `run-at-time` / `run-with-timer` — schedule a callback after a delay
//! - `run-with-idle-timer` — schedule a callback during idle time
//! - `cancel-timer` — deactivate a timer
//! - `timerp` — type predicate
//! - `timer-activate` — reactivate a timer

use std::time::{Duration, Instant};

use super::error::{EvalResult, Flow, signal};
use super::value::{Value, ValueKind, VecLikeType};
use crate::gc::GcTrace;

// ---------------------------------------------------------------------------
// Timer types
// ---------------------------------------------------------------------------

/// Unique timer identifier.
pub type TimerId = u64;

/// A single timer entry.
#[derive(Clone, Debug)]
pub struct Timer {
    /// Unique identifier.
    pub id: TimerId,
    /// Absolute time when this timer should next fire (used for non-idle timers).
    pub fire_time: Instant,
    /// If Some, the timer repeats at this interval after firing.
    pub repeat_interval: Option<Duration>,
    /// The callback to invoke (a lambda, symbol name, or other callable).
    pub callback: Value,
    /// Arguments to pass to the callback.
    pub args: Vec<Value>,
    /// Whether this timer is currently active.
    pub active: bool,
    /// Whether this is an idle timer.
    pub idle: bool,
    /// For idle timers: the idle duration threshold required before firing.
    pub idle_delay: Option<Duration>,
}

// ---------------------------------------------------------------------------
// TimerManager
// ---------------------------------------------------------------------------

/// Central registry for all timers.
pub struct TimerManager {
    timers: Vec<Timer>,
    next_id: TimerId,
}

impl TimerManager {
    /// Create a new empty timer manager.
    pub fn new() -> Self {
        Self {
            timers: Vec::new(),
            next_id: 1,
        }
    }

    /// Add a new timer that fires after `delay_secs` seconds.
    ///
    /// If `repeat_secs` is > 0, the timer repeats at that interval.
    /// Returns the timer id.
    pub fn add_timer(
        &mut self,
        delay_secs: f64,
        repeat_secs: f64,
        callback: Value,
        args: Vec<Value>,
        idle: bool,
    ) -> TimerId {
        let id = self.next_id;
        self.next_id += 1;

        let delay = Duration::from_secs_f64(delay_secs.max(0.0));
        let fire_time = Instant::now() + delay;
        let repeat_interval = if repeat_secs > 0.0 {
            Some(Duration::from_secs_f64(repeat_secs))
        } else {
            None
        };

        let idle_delay = if idle { Some(delay) } else { None };

        self.timers.push(Timer {
            id,
            fire_time,
            repeat_interval,
            callback,
            args,
            active: true,
            idle,
            idle_delay,
        });

        id
    }

    /// Cancel a timer by id. Returns true if the timer was found and cancelled.
    pub fn cancel_timer(&mut self, id: TimerId) -> bool {
        for timer in &mut self.timers {
            if timer.id == id {
                timer.active = false;
                return true;
            }
        }
        false
    }

    /// Check if a timer is active.
    pub fn timer_active_p(&self, id: TimerId) -> bool {
        self.timers.iter().any(|t| t.id == id && t.active)
    }

    /// Update a timer's delay (reschedules from now).
    pub fn timer_set_time(&mut self, id: TimerId, new_delay: f64) {
        let delay = Duration::from_secs_f64(new_delay.max(0.0));
        for timer in &mut self.timers {
            if timer.id == id {
                timer.fire_time = Instant::now() + delay;
                timer.active = true;
                return;
            }
        }
    }

    /// Reactivate a cancelled timer (reschedules from now using its repeat interval or zero).
    pub fn timer_activate(&mut self, id: TimerId) -> bool {
        for timer in &mut self.timers {
            if timer.id == id {
                if !timer.active {
                    timer.active = true;
                    let delay = timer.repeat_interval.unwrap_or(Duration::ZERO);
                    if timer.idle {
                        // Reset idle delay threshold to the repeat interval (or zero).
                        timer.idle_delay = Some(delay);
                    } else {
                        // Reschedule from now using repeat interval or immediately.
                        timer.fire_time = Instant::now() + delay;
                    }
                }
                return true;
            }
        }
        false
    }

    /// Collect all pending callbacks whose fire_time has passed.
    ///
    /// `idle_duration` is the current idle duration (if the system is idle).
    /// Idle timers only fire when `idle_duration >= idle_delay`.
    /// Normal timers fire when `current_time >= fire_time`.
    ///
    /// Returns a vec of (callback, args) pairs to be executed by the evaluator.
    /// Repeating timers are rescheduled; one-shot timers are deactivated.
    pub fn fire_pending_timers(
        &mut self,
        current_time: Instant,
        idle_duration: Option<Duration>,
    ) -> Vec<(Value, Vec<Value>)> {
        let mut fired = Vec::new();

        for timer in &mut self.timers {
            if !timer.active {
                continue;
            }

            let should_fire = if timer.idle {
                // Idle timers: fire when the user has been idle long enough.
                match (idle_duration, timer.idle_delay) {
                    (Some(idle_dur), Some(idle_del)) => idle_dur >= idle_del,
                    _ => false,
                }
            } else {
                current_time >= timer.fire_time
            };

            if should_fire {
                fired.push((timer.callback, timer.args.clone()));

                if let Some(interval) = timer.repeat_interval {
                    if timer.idle {
                        // For repeating idle timers, increase the idle delay threshold
                        // so it fires again after another `interval` of idle time.
                        if let Some(ref mut idle_del) = timer.idle_delay {
                            *idle_del = idle_duration.unwrap_or(Duration::ZERO) + interval;
                        }
                    } else {
                        // Reschedule: advance fire_time by interval (catch up if needed)
                        timer.fire_time = current_time + interval;
                    }
                } else {
                    timer.active = false;
                }
            }
        }

        fired
    }

    /// Return the duration until the next timer fires, or None if no active timers.
    ///
    /// `idle_duration` is the current idle duration (if the system is idle).
    /// For idle timers, the remaining time is `idle_delay - idle_duration`.
    /// For normal timers, the remaining time is `fire_time - now`.
    pub fn next_fire_time(&self, idle_duration: Option<Duration>) -> Option<Duration> {
        let now = Instant::now();
        self.timers
            .iter()
            .filter(|t| t.active)
            .filter_map(|t| {
                if t.idle {
                    // Idle timer: compute remaining idle time needed.
                    let idle_del = t.idle_delay.unwrap_or(Duration::ZERO);
                    match idle_duration {
                        Some(idle_dur) if idle_dur >= idle_del => Some(Duration::ZERO),
                        Some(idle_dur) => Some(idle_del - idle_dur),
                        // Not idle: idle timers can't fire, don't include in timeout.
                        None => None,
                    }
                } else if t.fire_time > now {
                    Some(t.fire_time - now)
                } else {
                    Some(Duration::ZERO)
                }
            })
            .min()
    }

    /// Return a list of all timer ids (both active and inactive).
    pub fn list_timers(&self) -> Vec<TimerId> {
        self.timers.iter().map(|t| t.id).collect()
    }

    /// Return a list of active timer ids.
    pub fn list_active_timers(&self) -> Vec<TimerId> {
        self.timers
            .iter()
            .filter(|t| t.active)
            .map(|t| t.id)
            .collect()
    }

    /// Check if the given id refers to a known timer.
    pub fn is_timer(&self, id: TimerId) -> bool {
        self.timers.iter().any(|t| t.id == id)
    }
}

impl Default for TimerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl GcTrace for TimerManager {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for timer in &self.timers {
            roots.push(timer.callback);
            for arg in &timer.args {
                roots.push(*arg);
            }
        }
    }
}

// ===========================================================================
// Builtin helpers
// ===========================================================================

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_number(value: &Value) -> Result<f64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n as f64),
        ValueKind::Float => Ok(value.xfloat()),
        ValueKind::Char(c) => Ok(c as u32 as f64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), *value],
        )),
    }
}

fn expect_fixnum_like(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        ValueKind::Char(c) => Ok(c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), *value],
        )),
    }
}

fn parse_time_unit_factor(unit: &str) -> Option<f64> {
    let unit = unit.to_ascii_lowercase();
    match unit.as_str() {
        // Full-word and widely-used shorthand forms.
        "sec" | "secs" | "second" | "seconds" => Some(1.0),
        "min" | "mins" | "minute" | "minutes" => Some(60.0),
        "hour" | "hours" => Some(3600.0),
        "day" | "days" => Some(86_400.0),
        "week" | "weeks" => Some(604_800.0),
        "month" | "months" => Some(2_592_000.0),
        "year" | "years" => Some(31_104_000.0),
        "fortnight" | "fortnights" => Some(1_209_600.0),
        _ => None,
    }
}

fn parse_concatenated_time_delay_spec(spec: &str) -> Option<f64> {
    if spec.is_empty() {
        return None;
    }

    for split in (1..=spec.len()).filter(|idx| spec.is_char_boundary(*idx)) {
        let (number_part, unit_part) = spec.split_at(split);
        if unit_part.is_empty() {
            continue;
        }

        if let Ok(delay) = number_part.parse::<f64>() {
            if let Some(multiplier) = parse_time_unit_factor(unit_part) {
                return Some(delay * multiplier);
            }
        }
    }

    None
}

fn parse_spaced_run_at_time_delay(tokens: &[&str]) -> Option<f64> {
    let (unit_index, multiplier) =
        tokens.iter().enumerate().rev().find_map(|(index, token)| {
            parse_time_unit_factor(token).map(|factor| (index, factor))
        })?;

    let number_tokens = &tokens[..unit_index];
    if number_tokens.is_empty() {
        return None;
    }

    let is_fragment = |token: &str| {
        token == "+"
            || token == "-"
            || token
                .chars()
                .all(|c| matches!(c, '0'..='9' | '.' | '+' | '-' | 'e' | 'E'))
    };

    let mut parsed_delay = None;

    for token in number_tokens.iter().rev() {
        if let Ok(delay) = token.parse::<f64>() {
            if parsed_delay.is_none() {
                parsed_delay = Some(delay);
            }
            continue;
        }

        if !is_fragment(token) {
            return None;
        }
    }

    parsed_delay.map(|delay| delay * multiplier)
}

fn parse_run_at_time_delay(value: &Value) -> Result<f64, Flow> {
    match value.kind() {
        ValueKind::Nil => Ok(0.0),
        ValueKind::Fixnum(_) | ValueKind::Float | ValueKind::Char(_) => expect_number(value),
        ValueKind::String => {
            let s_str = value.as_str().unwrap();
            let spec = s_str.trim();
            if spec.is_empty() {
                return Err(signal(
                    "error",
                    vec![Value::string("Invalid time specification")],
                ));
            }

            if let Ok(delay) = spec.parse::<f64>() {
                return Ok(delay);
            }

            if let Some(delay) = parse_concatenated_time_delay_spec(spec) {
                return Ok(delay);
            }

            let tokens: Vec<&str> = spec.split_whitespace().collect();
            if tokens.len() > 1 {
                let merged = tokens.join("");
                if let Some(delay) = parse_concatenated_time_delay_spec(&merged) {
                    return Ok(delay);
                }
            }

            if let Some(delay) = parse_spaced_run_at_time_delay(&tokens) {
                return Ok(delay);
            }

            Err(signal(
                "error",
                vec![Value::string("Invalid time specification")],
            ))
        }
        _ => Err(signal(
            "error",
            vec![Value::string("Invalid time specification")],
        )),
    }
}

fn parse_idle_timer_delay(value: &Value) -> Result<f64, Flow> {
    match value.kind() {
        ValueKind::Nil => Ok(0.0),
        ValueKind::Fixnum(_) | ValueKind::Float | ValueKind::Char(_) => expect_number(value),
        _ => Err(signal(
            "error",
            vec![Value::string("Invalid time specification")],
        )),
    }
}

fn expect_timer_id(value: &Value) -> Result<TimerId, Flow> {
    match value.kind() {
        ValueKind::Veclike(VecLikeType::Timer) => Ok(value.as_timer_id().unwrap()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("timerp"), *value],
        )),
    }
}

// ===========================================================================
// Builtins (evaluator-dependent)
// ===========================================================================

/// (run-at-time TIME REPEAT FUNCTION &rest ARGS) -> timer
///
/// TIME is seconds from now (float or int). REPEAT is nil or seconds.
pub(crate) fn builtin_run_at_time(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("run-at-time", &args, 3)?;
    let delay = parse_run_at_time_delay(&args[0])?;
    let repeat = if args[1].is_nil() {
        0.0
    } else {
        expect_number(&args[1])?
    };
    let callback = args[2];
    let timer_args: Vec<Value> = args[3..].to_vec();

    let id = eval
        .timers
        .add_timer(delay, repeat, callback, timer_args, false);
    Ok(Value::make_timer(id))
}

/// (add-timeout SECS REPEAT FUNCTION &optional OBJECT) -> timer
///
/// Legacy timeout helper used by some runtime paths. In batch mode oracle
/// accepts any non-nil REPEAT marker and signals an "Invalid or uninitialized
/// timer" error when REPEAT is nil.
pub(crate) fn builtin_add_timeout(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("add-timeout", &args, 3)?;
    if args.len() > 4 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("add-timeout"), Value::fixnum(args.len() as i64)],
        ));
    }

    let delay = parse_run_at_time_delay(&args[0])?;
    let repeat_marker = &args[1];
    if repeat_marker.is_nil() {
        return Err(signal(
            "error",
            vec![Value::string("Invalid or uninitialized timer")],
        ));
    }
    let repeat = expect_number(repeat_marker).unwrap_or(0.0);
    let callback = args[2];
    let timer_args = args.get(3).cloned().into_iter().collect();

    let id = eval
        .timers
        .add_timer(delay, repeat, callback, timer_args, false);
    Ok(Value::make_timer(id))
}

/// (run-with-timer SECS REPEAT FUNCTION &rest ARGS) -> timer
///
/// Alias for run-at-time.
pub(crate) fn builtin_run_with_timer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_run_at_time(eval, args)
}

/// (run-with-idle-timer SECS REPEAT FUNCTION &rest ARGS) -> timer
///
/// Like run-at-time, but marks the timer as idle.
pub(crate) fn builtin_run_with_idle_timer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("run-with-idle-timer", &args, 3)?;
    let delay = parse_idle_timer_delay(&args[0])?;
    let repeat = if args[1].is_nil() {
        0.0
    } else {
        expect_number(&args[1])?
    };
    let callback = args[2];
    let timer_args: Vec<Value> = args[3..].to_vec();

    let id = eval
        .timers
        .add_timer(delay, repeat, callback, timer_args, true);
    Ok(Value::make_timer(id))
}

/// (cancel-timer TIMER) -> nil
pub(crate) fn builtin_cancel_timer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("cancel-timer", &args, 1)?;
    let id = expect_timer_id(&args[0])?;
    eval.timers.cancel_timer(id);
    Ok(Value::NIL)
}

/// (timerp OBJECT) -> t or nil
pub(crate) fn builtin_timerp(args: Vec<Value>) -> EvalResult {
    expect_args("timerp", &args, 1)?;
    Ok(Value::bool_val(args[0].is_timer()))
}

/// (timer-activate TIMER) -> nil
pub(crate) fn builtin_timer_activate(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("timer-activate", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("timer-activate"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    if let Some(delay) = args.get(2) {
        if !delay.is_nil() && !delay.is_cons() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("consp"), *delay],
            ));
        }
    }

    let id = match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Timer) => args[0].as_timer_id().unwrap(),
        _ => return Err(signal("error", vec![Value::string("Invalid timer")])),
    };
    if !eval.timers.is_timer(id) {
        return Err(signal("error", vec![Value::string("Invalid timer")]));
    }
    if eval.timers.timer_active_p(id) {
        return Err(signal(
            "error",
            vec![Value::string("Timer is already active")],
        ));
    }
    eval.timers.timer_activate(id);
    Ok(Value::NIL)
}

/// (sleep-for SECONDS &optional MILLISECONDS) -> nil
///
/// Sleep for the given duration through the shared wait/service path so that
/// subprocess filters/sentinels and timers run with the same ownership as
/// other event-loop waits.
pub(crate) fn builtin_sleep_for(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("sleep-for", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("sleep-for"), Value::fixnum(args.len() as i64)],
        ));
    }

    let secs = expect_number(&args[0])?;
    let millis = if args.len() > 1 {
        if args[1].is_nil() {
            0.0
        } else {
            // GNU Emacs requires a fixnum for the MILLISECONDS argument.
            expect_fixnum_like(&args[1])? as f64
        }
    } else {
        0.0
    };

    let total_secs = secs + millis / 1000.0;
    if total_secs > 0.0 {
        let total = Duration::from_secs_f64(total_secs);
        let start = Instant::now();
        let deadline = start + total;
        loop {
            let _ = eval.service_wait_path_once(None, false, true, false)?;
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline.saturating_duration_since(now);
            let wait_time = eval.next_wait_path_timeout(remaining, true);
            if wait_time.is_zero() {
                continue;
            }
            let _ = eval.processes.wait_for_output(wait_time);
            let _ = eval.service_wait_path_special_input_events()?;
        }
    }

    Ok(Value::NIL)
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "timer_test.rs"]
mod tests;
