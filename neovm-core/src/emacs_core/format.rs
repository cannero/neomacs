//! Advanced string formatting builtins.
//!
//! Pure builtins (`Vec<Value> -> EvalResult`):
//! - `format-time-string` — format time like strftime
//! - `string-chop-newline` — remove trailing newline
//! - `string-clean-whitespace` — collapse whitespace and trim
//! - `string-pixel-width` — batch-compatible display-column width

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::*;

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

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

fn require_string(_name: &str, val: &Value) -> Result<String, Flow> {
    match val.kind() {
        ValueKind::String => Ok(val.as_str().unwrap().to_owned()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *val],
        )),
    }
}

fn require_char(val: &Value) -> Result<char, Flow> {
    match val.kind() {
        ValueKind::Char(c) => Ok(c),
        ValueKind::Fixnum(n) => char::from_u32(n as u32).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *val],
            )
        }),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *val],
        )),
    }
}

// ---------------------------------------------------------------------------
// format-time-string
// ---------------------------------------------------------------------------

/// Broken-down time fields computed from a Unix timestamp.
struct BrokenDownTime {
    year: i64,
    month: u32,   // 1..=12
    day: u32,     // 1..=31
    hour: u32,    // 0..=23
    minute: u32,  // 0..=59
    second: u32,  // 0..=60 (leap second)
    weekday: u32, // 0=Sunday .. 6=Saturday
    yearday: u32, // 0..=365
}

/// Whether a year is a leap year (Gregorian).
fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Days in each month for a given year.
fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 => 31,
        2 => {
            if is_leap_year(y) {
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

/// Convert a Unix timestamp (seconds since 1970-01-01 00:00:00 UTC) into
/// broken-down UTC time fields.  No external crate needed.
fn unix_to_broken_down(timestamp: i64) -> BrokenDownTime {
    // Handle negative timestamps (before epoch).
    let remaining = timestamp;
    let second_of_day;
    let mut day_count; // days since epoch (can be negative)

    if remaining >= 0 {
        day_count = remaining / 86400;
        second_of_day = (remaining % 86400) as u32;
    } else {
        // For negative timestamps, adjust so second_of_day is non-negative.
        day_count = (remaining - 86399) / 86400; // floor division
        let rem = remaining - day_count * 86400;
        second_of_day = rem as u32;
    }

    let hour = second_of_day / 3600;
    let minute = (second_of_day % 3600) / 60;
    let second = second_of_day % 60;

    // Weekday: 1970-01-01 was a Thursday (4).
    let weekday = ((day_count % 7 + 4 + 7) % 7) as u32; // 0=Sunday

    // Convert day_count to year/month/day.
    // day_count is days since 1970-01-01.
    let mut year: i64 = 1970;

    if day_count >= 0 {
        loop {
            let days_in_year = if is_leap_year(year) { 366 } else { 365 };
            if day_count < days_in_year {
                break;
            }
            day_count -= days_in_year;
            year += 1;
        }
    } else {
        loop {
            year -= 1;
            let days_in_year = if is_leap_year(year) { 366 } else { 365 };
            day_count += days_in_year;
            if day_count >= 0 {
                break;
            }
        }
    }

    let yearday = day_count as u32;

    // Now day_count is the 0-based day within `year`.
    let mut month = 1u32;
    let mut remaining_days = day_count as u32;
    loop {
        let dim = days_in_month(year, month);
        if remaining_days < dim {
            break;
        }
        remaining_days -= dim;
        month += 1;
        if month > 12 {
            break;
        }
    }
    let day = remaining_days + 1;

    BrokenDownTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
        weekday,
        yearday,
    }
}

const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

const DAY_ABBREVS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

const MONTH_ABBREVS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `(format-time-string FORMAT-STRING &optional TIME ZONE)` -- format time
/// like C `strftime`.
///
/// Supported directives:
/// `%Y` year, `%m` month (01-12), `%d` day (01-31), `%H` hour (00-23),
/// `%M` minute (00-59), `%S` second (00-60), `%A` full day name,
/// `%a` abbreviated day name, `%B` full month name, `%b`/`%h` abbreviated
/// month name, `%Z` timezone name, `%z` numeric timezone offset,
/// `%j` day of year (001-366), `%e` day space-padded, `%k` hour space-padded,
/// `%l` 12-hour space-padded, `%I` 12-hour zero-padded, `%p` AM/PM,
/// `%P` am/pm, `%n` newline, `%t` tab, `%%` literal `%`.
///
/// If TIME is nil, uses current system time.  ZONE is currently ignored (UTC
/// assumed).
pub(crate) fn builtin_format_time_string(args: Vec<Value>) -> EvalResult {
    expect_min_args("format-time-string", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("format-time-string"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let format_str = require_string("format-time-string", &args[0])?;

    // Determine timestamp.
    let timestamp: i64 = if args.len() >= 2 && !args[1].is_nil() {
        match args[1].kind() {
            ValueKind::Fixnum(n) => n,
            ValueKind::Float => args[1].xfloat() as i64,
            ValueKind::Cons => {
                // Emacs time value: (HIGH LOW) or (HIGH LOW USEC) or (HIGH LOW USEC PSEC).
                // Decode as HIGH * 65536 + LOW.
                let items = list_to_vec(&args[1]).unwrap_or_default();
                if items.len() >= 2 {
                    let high = items[0].as_int().unwrap_or(0);
                    let low = items[1].as_int().unwrap_or(0);
                    high * 65536 + low
                } else {
                    current_unix_timestamp()
                }
            }
            _ => current_unix_timestamp(),
        }
    } else {
        current_unix_timestamp()
    };

    let tm = unix_to_broken_down(timestamp);
    let formatted = format_time(&format_str, &tm);
    Ok(Value::string(formatted))
}

/// Get current Unix timestamp using `std::time::SystemTime`.
fn current_unix_timestamp() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
use crate::emacs_core::value::{ValueKind};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Format a broken-down time according to a strftime-like format string.
fn format_time(fmt: &str, tm: &BrokenDownTime) -> String {
    let mut result = String::new();
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '%' {
            i += 1;
            if i >= chars.len() {
                result.push('%');
                break;
            }

            // Handle optional '-' flag to suppress padding.
            let suppress_pad = if chars[i] == '-' {
                i += 1;
                true
            } else {
                false
            };

            if i >= chars.len() {
                result.push('%');
                if suppress_pad {
                    result.push('-');
                }
                break;
            }

            match chars[i] {
                '%' => result.push('%'),
                'Y' => result.push_str(&format!("{:04}", tm.year)),
                'y' => result.push_str(&format!("{:02}", tm.year % 100)),
                'C' => result.push_str(&format!("{:02}", tm.year / 100)),
                'm' => {
                    if suppress_pad {
                        result.push_str(&tm.month.to_string());
                    } else {
                        result.push_str(&format!("{:02}", tm.month));
                    }
                }
                'd' => {
                    if suppress_pad {
                        result.push_str(&tm.day.to_string());
                    } else {
                        result.push_str(&format!("{:02}", tm.day));
                    }
                }
                'e' => result.push_str(&format!("{:2}", tm.day)),
                'H' => {
                    if suppress_pad {
                        result.push_str(&tm.hour.to_string());
                    } else {
                        result.push_str(&format!("{:02}", tm.hour));
                    }
                }
                'k' => result.push_str(&format!("{:2}", tm.hour)),
                'I' => {
                    let h12 = if tm.hour == 0 {
                        12
                    } else if tm.hour > 12 {
                        tm.hour - 12
                    } else {
                        tm.hour
                    };
                    if suppress_pad {
                        result.push_str(&h12.to_string());
                    } else {
                        result.push_str(&format!("{:02}", h12));
                    }
                }
                'l' => {
                    let h12 = if tm.hour == 0 {
                        12
                    } else if tm.hour > 12 {
                        tm.hour - 12
                    } else {
                        tm.hour
                    };
                    result.push_str(&format!("{:2}", h12));
                }
                'M' => {
                    if suppress_pad {
                        result.push_str(&tm.minute.to_string());
                    } else {
                        result.push_str(&format!("{:02}", tm.minute));
                    }
                }
                'S' => {
                    if suppress_pad {
                        result.push_str(&tm.second.to_string());
                    } else {
                        result.push_str(&format!("{:02}", tm.second));
                    }
                }
                'A' => result.push_str(DAY_NAMES[tm.weekday as usize % 7]),
                'a' => result.push_str(DAY_ABBREVS[tm.weekday as usize % 7]),
                'B' => result.push_str(MONTH_NAMES[(tm.month as usize).saturating_sub(1) % 12]),
                'b' | 'h' => {
                    result.push_str(MONTH_ABBREVS[(tm.month as usize).saturating_sub(1) % 12])
                }
                'p' => result.push_str(if tm.hour < 12 { "AM" } else { "PM" }),
                'P' => result.push_str(if tm.hour < 12 { "am" } else { "pm" }),
                'Z' => result.push_str("UTC"),
                'z' => result.push_str("+0000"),
                'j' => {
                    if suppress_pad {
                        result.push_str(&(tm.yearday + 1).to_string());
                    } else {
                        result.push_str(&format!("{:03}", tm.yearday + 1));
                    }
                }
                'u' => {
                    // ISO weekday: 1=Monday .. 7=Sunday
                    let iso_wd = if tm.weekday == 0 { 7 } else { tm.weekday };
                    result.push_str(&iso_wd.to_string());
                }
                'w' => result.push_str(&tm.weekday.to_string()),
                'n' => result.push('\n'),
                't' => result.push('\t'),
                'R' => result.push_str(&format!("{:02}:{:02}", tm.hour, tm.minute)),
                'T' => {
                    result.push_str(&format!("{:02}:{:02}:{:02}", tm.hour, tm.minute, tm.second))
                }
                'F' => result.push_str(&format!("{:04}-{:02}-{:02}", tm.year, tm.month, tm.day)),
                'D' => result.push_str(&format!(
                    "{:02}/{:02}/{:02}",
                    tm.month,
                    tm.day,
                    tm.year % 100
                )),
                'U' => {
                    // Week number of the year (Sunday as first day), 00-53
                    let wnum = (tm.yearday + 7 - tm.weekday) / 7;
                    if suppress_pad {
                        result.push_str(&wnum.to_string());
                    } else {
                        result.push_str(&format!("{:02}", wnum));
                    }
                }
                'W' => {
                    // Week number of the year (Monday as first day), 00-53
                    let monday_weekday = if tm.weekday == 0 { 6 } else { tm.weekday - 1 };
                    let wnum = (tm.yearday + 7 - monday_weekday) / 7;
                    if suppress_pad {
                        result.push_str(&wnum.to_string());
                    } else {
                        result.push_str(&format!("{:02}", wnum));
                    }
                }
                'c' => {
                    // Preferred date and time representation (C locale):
                    // equivalent to "%a %b %e %H:%M:%S %Y"
                    result.push_str(DAY_ABBREVS[tm.weekday as usize % 7]);
                    result.push(' ');
                    result.push_str(MONTH_ABBREVS[(tm.month as usize).saturating_sub(1) % 12]);
                    result.push_str(&format!(
                        " {:2} {:02}:{:02}:{:02} {:04}",
                        tm.day, tm.hour, tm.minute, tm.second, tm.year
                    ));
                }
                'x' => {
                    // Preferred date representation (C locale): "%m/%d/%y"
                    result.push_str(&format!(
                        "{:02}/{:02}/{:02}",
                        tm.month,
                        tm.day,
                        tm.year % 100
                    ));
                }
                'X' => {
                    // Preferred time representation (C locale): "%H:%M:%S"
                    result.push_str(&format!("{:02}:{:02}:{:02}", tm.hour, tm.minute, tm.second));
                }
                other => {
                    // Unknown directive -- emit as-is.
                    result.push('%');
                    if suppress_pad {
                        result.push('-');
                    }
                    result.push(other);
                }
            }
            i += 1;
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

// ---------------------------------------------------------------------------
// string-chop-newline
// ---------------------------------------------------------------------------

/// `(string-chop-newline STRING)` -- remove trailing CR/LF run from STRING.
pub(crate) fn builtin_string_chop_newline(args: Vec<Value>) -> EvalResult {
    expect_args("string-chop-newline", &args, 1)?;
    let s = require_string("string-chop-newline", &args[0])?;
    let trimmed = s.trim_end_matches(['\n', '\r']).to_string();
    Ok(Value::string(trimmed))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "format_test.rs"]
mod tests;
