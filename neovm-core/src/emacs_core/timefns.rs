//! Time and date builtins for the Elisp interpreter.
//!
//! Implements `current-time`, `float-time`, `time-add`, `time-subtract`,
//! `time-less-p`, `time-equal-p`, `current-time-string`, `current-time-zone`,
//! `encode-time`, `decode-time`, `time-convert`, and `set-time-zone-rule`.
//!
//! Uses `std::time::SystemTime`/`UNIX_EPOCH` for time operations.

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::*;
use std::cell::RefCell;
use std::ffi::{CStr, OsString};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_max_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Internal time representation
// ---------------------------------------------------------------------------

/// Internal microsecond-precision time (seconds + microseconds since epoch).
/// Allows negative values for times before the epoch.
#[derive(Clone, Copy, Debug)]
struct TimeMicros {
    /// Total seconds (may be negative).
    secs: i64,
    /// Microseconds within the current second, always in [0, 999_999].
    usecs: i64,
}

impl TimeMicros {
    fn now() -> Self {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(dur) => TimeMicros {
                secs: dur.as_secs() as i64,
                usecs: dur.subsec_micros() as i64,
            },
            Err(e) => {
                let dur = e.duration();
                TimeMicros {
                    secs: -(dur.as_secs() as i64),
                    usecs: -(dur.subsec_micros() as i64),
                }
            }
        }
    }

    fn to_list(&self) -> Value {
        let high = (self.secs >> 16) & 0xFFFF_FFFF;
        let low = self.secs & 0xFFFF;
        Value::list(vec![
            Value::Int(high),
            Value::Int(low),
            Value::Int(self.usecs),
            Value::Int(0), // PSEC
        ])
    }

    fn to_float(&self) -> f64 {
        self.secs as f64 + self.usecs as f64 / 1_000_000.0
    }

    fn add(self, other: TimeMicros) -> TimeMicros {
        let mut usecs = self.usecs + other.usecs;
        let mut secs = self.secs + other.secs;
        if usecs >= 1_000_000 {
            usecs -= 1_000_000;
            secs += 1;
        } else if usecs < 0 {
            usecs += 1_000_000;
            secs -= 1;
        }
        TimeMicros { secs, usecs }
    }

    fn sub(self, other: TimeMicros) -> TimeMicros {
        let mut usecs = self.usecs - other.usecs;
        let mut secs = self.secs - other.secs;
        if usecs < 0 {
            usecs += 1_000_000;
            secs -= 1;
        } else if usecs >= 1_000_000 {
            usecs -= 1_000_000;
            secs += 1;
        }
        TimeMicros { secs, usecs }
    }

    fn less_than(self, other: TimeMicros) -> bool {
        if self.secs != other.secs {
            self.secs < other.secs
        } else {
            self.usecs < other.usecs
        }
    }

    fn equal(self, other: TimeMicros) -> bool {
        self.secs == other.secs && self.usecs == other.usecs
    }
}

/// Parse a time value from a Lisp argument.
///
/// Accepts:
///   - nil            -> current time
///   - integer        -> seconds since epoch
///   - float          -> seconds since epoch (with fractional part)
///   - (HIGH LOW)     -> high*65536 + low seconds, 0 usecs
///   - (HIGH LOW USEC)       -> with microseconds
///   - (HIGH LOW USEC PSEC)  -> with microseconds (PSEC ignored)
fn parse_time(val: &Value) -> Result<TimeMicros, Flow> {
    match val {
        Value::Nil => Ok(TimeMicros::now()),
        Value::Int(n) => Ok(TimeMicros { secs: *n, usecs: 0 }),
        Value::Float(f, _) => {
            let secs = f.floor() as i64;
            let usecs = ((f - f.floor()) * 1_000_000.0).round() as i64;
            Ok(TimeMicros { secs, usecs })
        }
        Value::Cons(_) => {
            let items = list_to_vec(val)
                .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), *val]))?;
            if items.len() < 2 {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), *val],
                ));
            }
            let high = items[0].as_int().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), items[0]],
                )
            })?;
            let low = items[1].as_int().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), items[1]],
                )
            })?;
            let usec = if items.len() > 2 {
                items[2].as_int().unwrap_or(0)
            } else {
                0
            };
            // PSEC (items[3]) is ignored
            let secs = high * 65536 + low;
            Ok(TimeMicros { secs, usecs: usec })
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), *other],
        )),
    }
}

// ---------------------------------------------------------------------------
// Date/time breakdown helpers (UTC only, no chrono)
// ---------------------------------------------------------------------------

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn days_in_month(month: i64, year: i64) -> i64 {
    match month {
        1 => 31,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        3 => 31,
        4 => 30,
        5 => 31,
        6 => 30,
        7 => 31,
        8 => 31,
        9 => 30,
        10 => 31,
        11 => 30,
        12 => 31,
        _ => 30,
    }
}

fn days_in_year(year: i64) -> i64 {
    if is_leap_year(year) { 366 } else { 365 }
}

/// Decoded time in UTC: (sec min hour day month year dow dst utcoff).
struct DecodedTime {
    sec: i64,
    min: i64,
    hour: i64,
    day: i64,   // 1-based
    month: i64, // 1-based
    year: i64,
    dow: i64, // 0=Sunday, 1=Monday, ..., 6=Saturday
}

/// Break epoch seconds into UTC date/time components.
fn decode_epoch_secs(total_secs: i64) -> DecodedTime {
    // Handle the time-of-day part
    let mut days = total_secs.div_euclid(86400);
    let day_secs = total_secs.rem_euclid(86400);

    let sec = day_secs % 60;
    let min = (day_secs / 60) % 60;
    let hour = day_secs / 3600;

    // Day of week: epoch (1970-01-01) was Thursday (4).
    // dow: 0=Sunday
    let dow = ((days % 7) + 4).rem_euclid(7);

    // Compute year, month, day from days since epoch.
    let mut year: i64 = 1970;
    if days >= 0 {
        loop {
            let dy = days_in_year(year);
            if days < dy {
                break;
            }
            days -= dy;
            year += 1;
        }
    } else {
        loop {
            year -= 1;
            let dy = days_in_year(year);
            days += dy;
            if days >= 0 {
                break;
            }
        }
    }

    // Now `days` is day-of-year (0-based).
    let mut month: i64 = 1;
    loop {
        let dm = days_in_month(month, year);
        if days < dm {
            break;
        }
        days -= dm;
        month += 1;
        if month > 12 {
            break;
        }
    }
    let day = days + 1; // 1-based

    DecodedTime {
        sec,
        min,
        hour,
        day,
        month,
        year,
        dow,
    }
}

/// Encode date/time components to epoch seconds (UTC).
fn encode_to_epoch_secs(sec: i64, min: i64, hour: i64, day: i64, month: i64, year: i64) -> i64 {
    // Count days from epoch (1970-01-01) to the given date.
    let mut total_days: i64 = 0;

    if year >= 1970 {
        for y in 1970..year {
            total_days += days_in_year(y);
        }
    } else {
        for y in year..1970 {
            total_days -= days_in_year(y);
        }
    }

    // Add days for months in the target year.
    for m in 1..month {
        total_days += days_in_month(m, year);
    }

    // Add days within month (day is 1-based).
    total_days += day - 1;

    total_days * 86400 + hour * 3600 + min * 60 + sec
}

// ---------------------------------------------------------------------------
// Day/month name tables
// ---------------------------------------------------------------------------

const DAY_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

#[derive(Clone, Debug)]
enum ZoneRule {
    Local,
    Utc,
    FixedOffset(i64),
    FixedNamedOffset(i64, String),
    TzString(String),
}

thread_local! {
    static TIME_ZONE_RULE: RefCell<ZoneRule> = RefCell::new(ZoneRule::Local);
}

/// Reset timezone rule to default (called from Evaluator::new).
pub(crate) fn reset_timefns_thread_locals() {
    TIME_ZONE_RULE.with(|slot| *slot.borrow_mut() = ZoneRule::Local);
}

fn tz_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn invalid_time_zone_spec(spec: &Value) -> Flow {
    signal(
        "error",
        vec![Value::string("Invalid time zone specification"), *spec],
    )
}

fn format_fixed_offset_name(offset_secs: i64) -> String {
    if offset_secs == 0 {
        return "GMT".to_string();
    }
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let abs_secs = offset_secs.abs();
    if abs_secs % 3600 == 0 {
        format!("{}{abs_hours:02}", sign, abs_hours = abs_secs / 3600)
    } else if abs_secs % 60 == 0 {
        let total_minutes = abs_secs / 60;
        format!(
            "{}{hours:02}{mins:02}",
            sign,
            hours = total_minutes / 60,
            mins = total_minutes % 60
        )
    } else {
        format!(
            "{}{hours:02}{mins:02}{secs:02}",
            sign,
            hours = abs_secs / 3600,
            mins = (abs_secs % 3600) / 60,
            secs = abs_secs % 60
        )
    }
}

#[cfg(unix)]
fn local_offset_name_at_epoch(epoch_secs: i64) -> (i64, String) {
    let mut time_val: libc::time_t = epoch_secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let tm_ptr = unsafe { libc::localtime_r(&mut time_val as *mut _, &mut tm as *mut _) };
    if tm_ptr.is_null() {
        return (0, "UTC".to_string());
    }
    let offset = tm.tm_gmtoff as i64;
    let name = if tm.tm_zone.is_null() {
        format_fixed_offset_name(offset)
    } else {
        unsafe { CStr::from_ptr(tm.tm_zone) }
            .to_string_lossy()
            .into_owned()
    };
    (offset, name)
}

#[cfg(not(unix))]
fn local_offset_name_at_epoch(_epoch_secs: i64) -> (i64, String) {
    (0, "UTC".to_string())
}

#[cfg(unix)]
fn refresh_tz_env() {
    unsafe extern "C" {
        fn tzset();
    }
    unsafe {
        tzset();
    }
}

#[cfg(not(unix))]
fn refresh_tz_env() {}

struct ScopedTzEnv {
    previous: Option<OsString>,
}

impl ScopedTzEnv {
    fn new(spec: Option<&str>) -> Self {
        let previous = std::env::var_os("TZ");
        match spec {
            Some(v) => unsafe { std::env::set_var("TZ", v) },
            None => unsafe { std::env::remove_var("TZ") },
        }
        refresh_tz_env();
        Self { previous }
    }
}

impl Drop for ScopedTzEnv {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => unsafe { std::env::set_var("TZ", v) },
            None => unsafe { std::env::remove_var("TZ") },
        }
        refresh_tz_env();
    }
}

fn with_tz_env<T>(spec: Option<&str>, f: impl FnOnce() -> T) -> T {
    let _lock = tz_env_lock().lock().expect("time zone env lock poisoned");
    let _guard = ScopedTzEnv::new(spec);
    f()
}

fn parse_zone_rule(zone: &Value) -> Result<ZoneRule, Flow> {
    match zone {
        Value::Nil => Ok(ZoneRule::Local),
        Value::True => Ok(ZoneRule::Utc),
        Value::Symbol(id) if resolve_sym(*id) == "wall" => Ok(ZoneRule::Local),
        Value::Int(n) => Ok(ZoneRule::FixedOffset(*n)),
        Value::Str(_) => Ok(ZoneRule::TzString(zone.as_str().unwrap().to_string())),
        Value::Cons(_) => {
            let items = list_to_vec(zone).ok_or_else(|| invalid_time_zone_spec(zone))?;
            if items.len() != 2 {
                return Err(invalid_time_zone_spec(zone));
            }
            let Some(offset) = items[0].as_int() else {
                return Err(invalid_time_zone_spec(zone));
            };
            let name = match &items[1] {
                Value::Str(_) => items[1].as_str().unwrap().to_string(),
                Value::Symbol(id) => resolve_sym(*id).to_owned(),
                _ => return Err(invalid_time_zone_spec(zone)),
            };
            Ok(ZoneRule::FixedNamedOffset(offset, name))
        }
        _ => Err(invalid_time_zone_spec(zone)),
    }
}

fn zone_rule_to_offset_name(rule: &ZoneRule, epoch_secs: i64) -> (i64, String) {
    match rule {
        ZoneRule::Local => local_offset_name_at_epoch(epoch_secs),
        ZoneRule::Utc => (0, "GMT".to_string()),
        ZoneRule::FixedOffset(offset) => (*offset, format_fixed_offset_name(*offset)),
        ZoneRule::FixedNamedOffset(offset, name) => (*offset, name.clone()),
        ZoneRule::TzString(spec) => {
            with_tz_env(Some(spec), || local_offset_name_at_epoch(epoch_secs))
        }
    }
}

fn require_integer_component(value: &Value) -> Result<i64, Flow> {
    value.as_int().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *value],
        )
    })
}

fn encode_time_zone_offset(zone: &Value, approx_epoch_secs: i64) -> Result<i64, Flow> {
    let rule = parse_zone_rule(zone)?;
    let initial = zone_rule_to_offset_name(&rule, approx_epoch_secs).0;
    Ok(match rule {
        ZoneRule::Local | ZoneRule::TzString(_) => {
            let adjusted_epoch = approx_epoch_secs - initial;
            zone_rule_to_offset_name(&rule, adjusted_epoch).0
        }
        _ => initial,
    })
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// `(current-time)` -> `(HIGH LOW USEC PSEC)`
#[cfg(test)]
pub(crate) fn builtin_current_time(args: Vec<Value>) -> EvalResult {
    expect_args("current-time", &args, 0)?;
    Ok(TimeMicros::now().to_list())
}

/// `(float-time &optional TIME)` -> float seconds since epoch.
#[cfg(test)]
pub(crate) fn builtin_float_time(args: Vec<Value>) -> EvalResult {
    expect_min_max_args("float-time", &args, 0, 1)?;
    let tm = if args.is_empty() || args[0].is_nil() {
        TimeMicros::now()
    } else {
        parse_time(&args[0])?
    };
    Ok(Value::Float(tm.to_float(), next_float_id()))
}

/// `(time-add A B)` -> `(HIGH LOW USEC PSEC)`
pub(crate) fn builtin_time_add(args: Vec<Value>) -> EvalResult {
    expect_args("time-add", &args, 2)?;
    let a = parse_time(&args[0])?;
    let b = parse_time(&args[1])?;
    Ok(a.add(b).to_list())
}

/// `(time-subtract A B)` -> `(HIGH LOW USEC PSEC)`
pub(crate) fn builtin_time_subtract(args: Vec<Value>) -> EvalResult {
    expect_args("time-subtract", &args, 2)?;
    let a = parse_time(&args[0])?;
    let b = parse_time(&args[1])?;
    Ok(a.sub(b).to_list())
}

/// `(time-less-p A B)` -> t or nil
pub(crate) fn builtin_time_less_p(args: Vec<Value>) -> EvalResult {
    expect_args("time-less-p", &args, 2)?;
    let a = parse_time(&args[0])?;
    let b = parse_time(&args[1])?;
    Ok(Value::bool(a.less_than(b)))
}

/// `(time-equal-p A B)` -> t or nil
pub(crate) fn builtin_time_equal_p(args: Vec<Value>) -> EvalResult {
    expect_args("time-equal-p", &args, 2)?;
    let a = parse_time(&args[0])?;
    let b = parse_time(&args[1])?;
    Ok(Value::bool(a.equal(b)))
}

/// `(current-time-string &optional TIME ZONE)` -> human-readable string.
///
/// Returns a string like `"Mon Jan  2 15:04:05 2006"`.
/// ZONE is ignored; UTC is always used.
pub(crate) fn builtin_current_time_string(args: Vec<Value>) -> EvalResult {
    expect_min_max_args("current-time-string", &args, 0, 2)?;
    let tm = if args.is_empty() || args[0].is_nil() {
        TimeMicros::now()
    } else {
        parse_time(&args[0])?
    };
    let dt = decode_epoch_secs(tm.secs);

    // Format: "Dow Mon DD HH:MM:SS YYYY"
    // Day of month is right-justified in a 2-char field (space-padded).
    let s = format!(
        "{} {} {:2} {:02}:{:02}:{:02} {}",
        DAY_NAMES[dt.dow as usize],
        MONTH_NAMES[(dt.month - 1) as usize],
        dt.day,
        dt.hour,
        dt.min,
        dt.sec,
        dt.year,
    );
    Ok(Value::string(s))
}

/// `(current-time-zone &optional TIME ZONE)` -> `(OFFSET NAME)`.
pub(crate) fn builtin_current_time_zone(args: Vec<Value>) -> EvalResult {
    expect_min_max_args("current-time-zone", &args, 0, 2)?;
    let tm = if args.is_empty() || args[0].is_nil() {
        TimeMicros::now()
    } else {
        parse_time(&args[0])?
    };

    let rule = if args.len() > 1 {
        parse_zone_rule(&args[1])?
    } else {
        TIME_ZONE_RULE.with(|slot| slot.borrow().clone())
    };

    let (offset, name) = zone_rule_to_offset_name(&rule, tm.secs);
    Ok(Value::list(vec![Value::Int(offset), Value::string(name)]))
}

/// `(encode-time TIME &rest OBSOLESCENT-ARGUMENTS)` -> `(HIGH LOW)`
pub(crate) fn builtin_encode_time(args: Vec<Value>) -> EvalResult {
    let (sec, min, hour, day, month, year, zone) = if args.len() == 1 {
        let items = list_to_vec(&args[0])
            .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), args[0]]))?;
        if items.len() < 6 {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), args[0]],
            ));
        }
        (
            require_integer_component(&items[0])?,
            require_integer_component(&items[1])?,
            require_integer_component(&items[2])?,
            require_integer_component(&items[3])?,
            require_integer_component(&items[4])?,
            require_integer_component(&items[5])?,
            items.get(8).copied().unwrap_or(Value::Nil),
        )
    } else if args.len() < 6 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("encode-time"), Value::Int(args.len() as i64)],
        ));
    } else {
        (
            require_integer_component(&args[0])?,
            require_integer_component(&args[1])?,
            require_integer_component(&args[2])?,
            require_integer_component(&args[3])?,
            require_integer_component(&args[4])?,
            require_integer_component(&args[5])?,
            if args.len() > 6 {
                args.last().copied().unwrap_or(Value::Nil)
            } else {
                Value::Nil
            },
        )
    };

    let local_secs = encode_to_epoch_secs(sec, min, hour, day, month, year);
    let zone_offset = encode_time_zone_offset(&zone, local_secs)?;
    let total_secs = local_secs - zone_offset;
    let high = (total_secs >> 16) & 0xFFFF_FFFF;
    let low = total_secs & 0xFFFF;
    Ok(Value::list(vec![Value::Int(high), Value::Int(low)]))
}

/// `(decode-time &optional TIME ZONE)`
/// -> `(SECONDS MINUTES HOURS DAY MONTH YEAR DOW DST UTCOFF)`
///
/// DOW is 0=Sunday .. 6=Saturday.  DST is nil.  UTCOFF is 0 (UTC).
pub(crate) fn builtin_decode_time(args: Vec<Value>) -> EvalResult {
    expect_min_max_args("decode-time", &args, 0, 2)?;
    let tm = if args.is_empty() || args[0].is_nil() {
        TimeMicros::now()
    } else {
        parse_time(&args[0])?
    };
    let dt = decode_epoch_secs(tm.secs);
    Ok(Value::list(vec![
        Value::Int(dt.sec),
        Value::Int(dt.min),
        Value::Int(dt.hour),
        Value::Int(dt.day),
        Value::Int(dt.month),
        Value::Int(dt.year),
        Value::Int(dt.dow),
        Value::Nil,    // DST
        Value::Int(0), // UTCOFF
    ]))
}

/// `(time-convert TIME &optional FORM)`
///
/// FORM controls the output format:
///   - nil or `list`   -> `(HIGH LOW USEC PSEC)`
///   - `integer`       -> integer seconds
///   - `t`             -> `(TICKS . HZ)` (highest precision cons cell)
///   - `float`         -> float seconds
pub(crate) fn builtin_time_convert(args: Vec<Value>) -> EvalResult {
    expect_min_max_args("time-convert", &args, 1, 2)?;
    let tm = parse_time(&args[0])?;

    let form = if args.len() > 1 {
        &args[1]
    } else {
        &Value::Nil
    };

    match form {
        Value::Nil => Ok(tm.to_list()),
        Value::True => {
            // Emacs 29+: t means highest resolution → (TICKS . HZ)
            // Use microsecond resolution: TICKS = secs*1000000 + usecs, HZ = 1000000
            let hz: i64 = 1_000_000;
            let ticks = tm.secs * hz + tm.usecs;
            Ok(Value::cons(Value::Int(ticks), Value::Int(hz)))
        }
        Value::Symbol(id) => match resolve_sym(*id) {
            "list" => Ok(tm.to_list()),
            "integer" => Ok(Value::Int(tm.secs)),
            "float" => Ok(Value::Float(tm.to_float(), next_float_id())),
            _ => Ok(tm.to_list()),
        },
        Value::Int(_) => {
            // When FORM is an integer, Emacs returns a cons (TICKS . HZ).
            // We approximate by returning (TICKS . 1) where TICKS = seconds.
            Ok(Value::cons(Value::Int(tm.secs), Value::Int(1)))
        }
        _ => Ok(tm.to_list()),
    }
}

/// `(set-time-zone-rule ZONE)` -> nil.
pub(crate) fn builtin_set_time_zone_rule(args: Vec<Value>) -> EvalResult {
    expect_args("set-time-zone-rule", &args, 1)?;
    let rule = parse_zone_rule(&args[0])?;
    TIME_ZONE_RULE.with(|slot| *slot.borrow_mut() = rule);
    Ok(Value::Nil)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "timefns_test.rs"]
mod tests;
