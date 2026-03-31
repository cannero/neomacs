use super::*;
use crate::emacs_core::intern::intern;
use crate::emacs_core::load::{apply_runtime_startup_state, create_bootstrap_evaluator_cached};
use crate::emacs_core::{format_eval_result, parse_forms};
use std::sync::{Mutex, OnceLock};
use crate::emacs_core::value::{ValueKind};

fn tz_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .expect("tz test lock poisoned")
}

fn reset_tz_rule() {
    let _ = builtin_set_time_zone_rule(vec![Value::NIL]);
}

fn bootstrap_eval(src: &str) -> Vec<String> {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

// -----------------------------------------------------------------------
// Internal helpers
// -----------------------------------------------------------------------

#[test]
fn time_micros_roundtrip_to_list() {
    let tm = TimeMicros {
        secs: 1_700_000_000,
        usecs: 123_456,
        psecs: 0,
    };
    let list = tm.to_list();
    let items = list_to_vec(&list).unwrap();
    assert_eq!(items.len(), 4);
    let high = items[0].as_int().unwrap();
    let low = items[1].as_int().unwrap();
    let usec = items[2].as_int().unwrap();
    let psec = items[3].as_int().unwrap();
    assert_eq!(high * 65536 + low, 1_700_000_000);
    assert_eq!(usec, 123_456);
    assert_eq!(psec, 0);
}

#[test]
fn time_micros_to_float() {
    let tm = TimeMicros {
        secs: 1000,
        usecs: 500_000,
        psecs: 0,
    };
    let f = tm.to_float();
    assert!((f - 1000.5).abs() < 1e-6);
}

#[test]
fn time_micros_add() {
    let a = TimeMicros {
        secs: 10,
        usecs: 800_000,
        psecs: 0,
    };
    let b = TimeMicros {
        secs: 5,
        usecs: 400_000,
        psecs: 0,
    };
    let c = a.add(b);
    assert_eq!(c.secs, 16);
    assert_eq!(c.usecs, 200_000);
}

#[test]
fn time_micros_sub() {
    let a = TimeMicros {
        secs: 10,
        usecs: 200_000,
        psecs: 0,
    };
    let b = TimeMicros {
        secs: 5,
        usecs: 400_000,
        psecs: 0,
    };
    let c = a.sub(b);
    assert_eq!(c.secs, 4);
    assert_eq!(c.usecs, 800_000);
}

#[test]
fn time_micros_less_than() {
    let a = TimeMicros {
        secs: 10,
        usecs: 0,
        psecs: 0,
    };
    let b = TimeMicros {
        secs: 10,
        usecs: 1,
        psecs: 0,
    };
    assert!(a.less_than(b));
    assert!(!b.less_than(a));
    assert!(!a.less_than(a));
}

#[test]
fn time_micros_equal() {
    let a = TimeMicros {
        secs: 42,
        usecs: 123,
        psecs: 0,
    };
    let b = TimeMicros {
        secs: 42,
        usecs: 123,
        psecs: 0,
    };
    assert!(a.equal(b));
    let c = TimeMicros {
        secs: 42,
        usecs: 124,
        psecs: 0,
    };
    assert!(!a.equal(c));
}

// -----------------------------------------------------------------------
// parse_time
// -----------------------------------------------------------------------

#[test]
fn parse_time_nil() {
    let tm = parse_time(&Value::NIL).unwrap();
    // Just check it returns something reasonable (recent epoch).
    assert!(tm.secs > 1_000_000_000);
}

#[test]
fn parse_time_integer() {
    let tm = parse_time(&Value::fixnum(1_700_000_000)).unwrap();
    assert_eq!(tm.secs, 1_700_000_000);
    assert_eq!(tm.usecs, 0);
}

#[test]
fn parse_time_float() {
    let tm = parse_time(&Value::make_float(1000.5)).unwrap();
    assert_eq!(tm.secs, 1000);
    assert_eq!(tm.usecs, 500_000);
}

#[test]
fn parse_time_list_two() {
    // (HIGH LOW) format: 25939 * 65536 + 34304 = 1700000000
    let high = 1_700_000_000i64 >> 16;
    let low = 1_700_000_000i64 & 0xFFFF;
    let list = Value::list(vec![Value::fixnum(high), Value::fixnum(low)]);
    let tm = parse_time(&list).unwrap();
    assert_eq!(tm.secs, 1_700_000_000);
    assert_eq!(tm.usecs, 0);
}

#[test]
fn parse_time_list_four() {
    let high = 1_700_000_000i64 >> 16;
    let low = 1_700_000_000i64 & 0xFFFF;
    let list = Value::list(vec![
        Value::fixnum(high),
        Value::fixnum(low),
        Value::fixnum(42),
        Value::fixnum(0),
    ]);
    let tm = parse_time(&list).unwrap();
    assert_eq!(tm.secs, 1_700_000_000);
    assert_eq!(tm.usecs, 42);
}

#[test]
fn parse_time_bad_type() {
    let result = parse_time(&Value::string("not a time"));
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// Date computation helpers
// -----------------------------------------------------------------------

#[test]
fn leap_years() {
    assert!(is_leap_year(2000));
    assert!(!is_leap_year(1900));
    assert!(is_leap_year(2024));
    assert!(!is_leap_year(2023));
    assert!(is_leap_year(2400));
}

#[test]
fn decode_epoch_zero() {
    let dt = decode_epoch_secs(0);
    assert_eq!(dt.year, 1970);
    assert_eq!(dt.month, 1);
    assert_eq!(dt.day, 1);
    assert_eq!(dt.hour, 0);
    assert_eq!(dt.min, 0);
    assert_eq!(dt.sec, 0);
    assert_eq!(dt.dow, 4); // Thursday
}

#[test]
fn decode_known_date() {
    // 2024-01-15 12:30:45 UTC -> epoch = 1705318245
    let epoch = encode_to_epoch_secs(45, 30, 12, 15, 1, 2024);
    let dt = decode_epoch_secs(epoch);
    assert_eq!(dt.year, 2024);
    assert_eq!(dt.month, 1);
    assert_eq!(dt.day, 15);
    assert_eq!(dt.hour, 12);
    assert_eq!(dt.min, 30);
    assert_eq!(dt.sec, 45);
}

#[test]
fn encode_decode_roundtrip() {
    let epoch = encode_to_epoch_secs(30, 15, 10, 25, 6, 2023);
    let dt = decode_epoch_secs(epoch);
    assert_eq!(dt.sec, 30);
    assert_eq!(dt.min, 15);
    assert_eq!(dt.hour, 10);
    assert_eq!(dt.day, 25);
    assert_eq!(dt.month, 6);
    assert_eq!(dt.year, 2023);
}

#[test]
fn encode_decode_roundtrip_leap_day() {
    let epoch = encode_to_epoch_secs(0, 0, 0, 29, 2, 2024);
    let dt = decode_epoch_secs(epoch);
    assert_eq!(dt.day, 29);
    assert_eq!(dt.month, 2);
    assert_eq!(dt.year, 2024);
}

#[test]
fn decode_y2k() {
    // 2000-01-01 00:00:00 UTC = 946684800
    let dt = decode_epoch_secs(946_684_800);
    assert_eq!(dt.year, 2000);
    assert_eq!(dt.month, 1);
    assert_eq!(dt.day, 1);
    assert_eq!(dt.hour, 0);
    assert_eq!(dt.min, 0);
    assert_eq!(dt.sec, 0);
    assert_eq!(dt.dow, 6); // Saturday
}

// -----------------------------------------------------------------------
// Builtins
// -----------------------------------------------------------------------

#[test]
fn builtin_current_time_returns_four_element_list() {
    let result = builtin_current_time(vec![]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 4);
    // All should be integers.
    for item in &items {
        assert!(item.is_integer());
    }
    // Reconstruct and check sanity.
    let high = items[0].as_int().unwrap();
    let low = items[1].as_int().unwrap();
    let secs = high * 65536 + low;
    assert!(secs > 1_000_000_000);
}

#[test]
fn builtin_current_time_wrong_arity() {
    let result = builtin_current_time(vec![Value::fixnum(1)]);
    assert!(result.is_err());
}

#[test]
fn builtin_float_time_no_args() {
    let result = builtin_float_time(vec![]).unwrap();
    match result.kind() {
        ValueKind::Float => { let f = result.as_float().unwrap(); assert!(f > 1_000_000_000.0); }
        _ => panic!("expected float"),
    }
}

#[test]
fn builtin_float_time_from_list() {
    let high = 1_700_000_000i64 >> 16;
    let low = 1_700_000_000i64 & 0xFFFF;
    let list = Value::list(vec![
        Value::fixnum(high),
        Value::fixnum(low),
        Value::fixnum(500_000),
        Value::fixnum(0),
    ]);
    let result = builtin_float_time(vec![list]).unwrap();
    match result.kind() {
        ValueKind::Float => { let f = result.as_float().unwrap(); assert!((f - 1_700_000_000.5).abs() < 1e-3); }
        _ => panic!("expected float"),
    }
}

#[test]
fn builtin_float_time_from_integer() {
    let result = builtin_float_time(vec![Value::fixnum(42)]).unwrap();
    match result.kind() {
        ValueKind::Float => { let f = result.as_float().unwrap(); assert!((f - 42.0).abs() < 1e-9); }
        _ => panic!("expected float"),
    }
}

#[test]
fn builtin_time_add_basic() {
    let a = Value::fixnum(100);
    let b = Value::fixnum(200);
    let result = builtin_time_add(vec![a, b]).unwrap();
    let items = list_to_vec(&result).unwrap();
    let high = items[0].as_int().unwrap();
    let low = items[1].as_int().unwrap();
    assert_eq!(high * 65536 + low, 300);
}

#[test]
fn builtin_time_subtract_basic() {
    let a = Value::fixnum(300);
    let b = Value::fixnum(100);
    let result = builtin_time_subtract(vec![a, b]).unwrap();
    let items = list_to_vec(&result).unwrap();
    let high = items[0].as_int().unwrap();
    let low = items[1].as_int().unwrap();
    assert_eq!(high * 65536 + low, 200);
}

#[test]
fn builtin_time_less_p_true() {
    let result = builtin_time_less_p(vec![Value::fixnum(1), Value::fixnum(2)]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn builtin_time_less_p_false() {
    let result = builtin_time_less_p(vec![Value::fixnum(2), Value::fixnum(1)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn builtin_time_equal_p_true() {
    let result = builtin_time_equal_p(vec![Value::fixnum(42), Value::fixnum(42)]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn builtin_time_equal_p_false() {
    let result = builtin_time_equal_p(vec![Value::fixnum(42), Value::fixnum(43)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn builtin_current_time_string_known_time() {
    // 2024-01-15 12:30:45 UTC
    let epoch = encode_to_epoch_secs(45, 30, 12, 15, 1, 2024);
    let result = builtin_current_time_string(vec![Value::fixnum(epoch)]).unwrap();
    let s = result.as_str().unwrap();
    assert!(s.contains("Jan"));
    assert!(s.contains("12:30:45"));
    assert!(s.contains("2024"));
    assert!(s.contains("15"));
}

#[test]
fn builtin_current_time_string_no_args() {
    let result = builtin_current_time_string(vec![]).unwrap();
    assert!(result.is_string());
}

#[test]
fn builtin_current_time_zone_default() {
    let _guard = tz_test_lock();
    reset_tz_rule();
    let result = builtin_current_time_zone(vec![]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 2);
    assert!(items[0].is_integer());
    assert!(items[1].is_string());
}

#[test]
fn builtin_encode_time_known() {
    let result = builtin_encode_time(vec![
        Value::fixnum(0),
        Value::fixnum(0),
        Value::fixnum(0),
        Value::fixnum(1),
        Value::fixnum(1),
        Value::fixnum(1970),
        Value::T,
    ])
    .unwrap();
    let items = list_to_vec(&result).unwrap();
    let high = items[0].as_int().unwrap();
    let low = items[1].as_int().unwrap();
    assert_eq!(high * 65536 + low, 0);
}

#[test]
fn builtin_encode_time_y2k() {
    let result = builtin_encode_time(vec![
        Value::fixnum(0),
        Value::fixnum(0),
        Value::fixnum(0),
        Value::fixnum(1),
        Value::fixnum(1),
        Value::fixnum(2000),
        Value::T,
    ])
    .unwrap();
    let items = list_to_vec(&result).unwrap();
    let high = items[0].as_int().unwrap();
    let low = items[1].as_int().unwrap();
    assert_eq!(high * 65536 + low, 946_684_800);
}

#[test]
fn builtin_encode_time_wrong_arity() {
    let result = builtin_encode_time(vec![]);
    assert!(result.is_err());
}

#[test]
fn builtin_encode_time_decoded_time_list() {
    let result = builtin_encode_time(vec![Value::list(vec![
        Value::fixnum(0),
        Value::fixnum(0),
        Value::fixnum(0),
        Value::fixnum(1),
        Value::fixnum(1),
        Value::fixnum(1970),
        Value::NIL,
        Value::fixnum(-1),
        Value::T,
    ])])
    .unwrap();
    let items = list_to_vec(&result).unwrap();
    let high = items[0].as_int().unwrap();
    let low = items[1].as_int().unwrap();
    assert_eq!(high * 65536 + low, 0);
}

#[test]
fn builtin_encode_time_honors_zone_offset() {
    let result = builtin_encode_time(vec![Value::list(vec![
        Value::fixnum(0),
        Value::fixnum(0),
        Value::fixnum(0),
        Value::fixnum(1),
        Value::fixnum(1),
        Value::fixnum(1970),
        Value::NIL,
        Value::fixnum(-1),
        Value::fixnum(-3600),
    ])])
    .unwrap();
    let items = list_to_vec(&result).unwrap();
    let high = items[0].as_int().unwrap();
    let low = items[1].as_int().unwrap();
    assert_eq!(high * 65536 + low, 3600);
}

#[test]
fn builtin_decode_time_epoch_zero() {
    let result = builtin_decode_time(vec![Value::fixnum(0)]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 9);
    assert_eq!(items[0].as_int(), Some(0)); // sec
    assert_eq!(items[1].as_int(), Some(0)); // min
    assert_eq!(items[2].as_int(), Some(0)); // hour
    assert_eq!(items[3].as_int(), Some(1)); // day
    assert_eq!(items[4].as_int(), Some(1)); // month
    assert_eq!(items[5].as_int(), Some(1970)); // year
    assert_eq!(items[6].as_int(), Some(4)); // dow (Thursday)
    assert!(items[7].is_nil()); // DST
    assert_eq!(items[8].as_int(), Some(0)); // utcoff
}

#[test]
fn builtin_decode_time_no_args() {
    let result = builtin_decode_time(vec![]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 9);
}

#[test]
fn builtin_encode_decode_roundtrip() {
    // Encode a specific time.
    let encoded = builtin_encode_time(vec![
        Value::fixnum(30),
        Value::fixnum(45),
        Value::fixnum(14),
        Value::fixnum(20),
        Value::fixnum(3),
        Value::fixnum(2025),
        Value::T,
    ])
    .unwrap();

    // Decode it back.
    let decoded = builtin_decode_time(vec![encoded]).unwrap();
    let items = list_to_vec(&decoded).unwrap();
    assert_eq!(items[0].as_int(), Some(30)); // sec
    assert_eq!(items[1].as_int(), Some(45)); // min
    assert_eq!(items[2].as_int(), Some(14)); // hour
    assert_eq!(items[3].as_int(), Some(20)); // day
    assert_eq!(items[4].as_int(), Some(3)); // month
    assert_eq!(items[5].as_int(), Some(2025)); // year
}

#[test]
fn builtin_time_convert_to_list() {
    let result = builtin_time_convert(vec![Value::fixnum(1000)]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 4);
    let high = items[0].as_int().unwrap();
    let low = items[1].as_int().unwrap();
    assert_eq!(high * 65536 + low, 1000);
}

#[test]
fn builtin_time_convert_to_integer() {
    let result = builtin_time_convert(vec![Value::fixnum(1000), Value::symbol("integer")]).unwrap();
    assert_eq!(result.as_int(), Some(1000));
}

#[test]
fn builtin_time_convert_to_float() {
    let result = builtin_time_convert(vec![Value::fixnum(1000), Value::symbol("float")]).unwrap();
    match result.kind() {
        ValueKind::Float => { let f = result.as_float().unwrap(); assert!((f - 1000.0).abs() < 1e-9); }
        _ => panic!("expected float"),
    }
}

#[test]
fn builtin_time_convert_with_t() {
    // Emacs 29+: (time-convert 42 t) returns (TICKS . HZ) cons
    let result = builtin_time_convert(vec![Value::fixnum(42), Value::T]).unwrap();
    match result.kind() {
        ValueKind::Cons => {
            let ticks = result.cons_car().as_int().expect("expected int ticks");
            let hz = result.cons_cdr().as_int().expect("expected int hz");
            assert_eq!(hz, 1_000_000);
            assert_eq!(ticks, 42_000_000);
        }
        _ => panic!("expected cons, got {:?}", result),
    }
}

#[test]
fn builtin_set_time_zone_rule_t() {
    let _guard = tz_test_lock();
    reset_tz_rule();

    let result = builtin_set_time_zone_rule(vec![Value::T]).unwrap();
    assert!(result.is_nil());
    let tz = builtin_current_time_zone(vec![]).unwrap();
    assert_eq!(tz, Value::list(vec![Value::fixnum(0), Value::string("GMT")]));
    reset_tz_rule();
}

#[test]
fn builtin_set_time_zone_rule_fixed_offsets() {
    let _guard = tz_test_lock();
    reset_tz_rule();

    builtin_set_time_zone_rule(vec![Value::fixnum(3600)]).unwrap();
    let plus = builtin_current_time_zone(vec![]).unwrap();
    assert_eq!(
        plus,
        Value::list(vec![Value::fixnum(3600), Value::string("+01")])
    );

    builtin_set_time_zone_rule(vec![Value::fixnum(-3600)]).unwrap();
    let minus = builtin_current_time_zone(vec![]).unwrap();
    assert_eq!(
        minus,
        Value::list(vec![Value::fixnum(-3600), Value::string("-01")])
    );

    builtin_set_time_zone_rule(vec![Value::fixnum(1)]).unwrap();
    let one = builtin_current_time_zone(vec![]).unwrap();
    assert_eq!(
        one,
        Value::list(vec![Value::fixnum(1), Value::string("+000001")])
    );
    reset_tz_rule();
}

#[test]
fn builtin_set_time_zone_rule_string_specs() {
    let _guard = tz_test_lock();
    reset_tz_rule();

    builtin_set_time_zone_rule(vec![Value::string("UTC")]).unwrap();
    let utc = builtin_current_time_zone(vec![]).unwrap();
    assert_eq!(utc, Value::list(vec![Value::fixnum(0), Value::string("UTC")]));

    builtin_set_time_zone_rule(vec![Value::string("JST-9")]).unwrap();
    let jst = builtin_current_time_zone(vec![]).unwrap();
    assert_eq!(
        jst,
        Value::list(vec![Value::fixnum(32400), Value::string("JST")])
    );
    reset_tz_rule();
}

#[test]
fn builtin_set_time_zone_rule_invalid_spec() {
    let _guard = tz_test_lock();
    reset_tz_rule();

    match builtin_set_time_zone_rule(vec![Value::keyword(":x")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data.first().and_then(|v| v.as_str()),
                Some("Invalid time zone specification")
            );
        }
        other => panic!("expected invalid time zone specification error, got {other:?}"),
    }
    reset_tz_rule();
}

#[test]
fn builtin_current_time_zone_with_zone_arg() {
    let _guard = tz_test_lock();
    reset_tz_rule();

    let gmt = builtin_current_time_zone(vec![Value::NIL, Value::T]).unwrap();
    assert_eq!(gmt, Value::list(vec![Value::fixnum(0), Value::string("GMT")]));

    let plus = builtin_current_time_zone(vec![Value::NIL, Value::fixnum(3600)]).unwrap();
    assert_eq!(
        plus,
        Value::list(vec![Value::fixnum(3600), Value::string("+01")])
    );

    match builtin_current_time_zone(vec![Value::NIL, Value::keyword(":x")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data.first().and_then(|v| v.as_str()),
                Some("Invalid time zone specification")
            );
        }
        other => panic!("expected invalid time zone specification error, got {other:?}"),
    }
    reset_tz_rule();
}

#[test]
fn safe_date_to_time_bootstrap_matches_gnu_elisp() {
    let results = bootstrap_eval(
        r#"
        (safe-date-to-time "1970-01-01 00:00:00 +0000")
        (safe-date-to-time "Thu, 01 Jan 1970 00:00:00 +0000")
        (safe-date-to-time "1970-01-01 00:00:00 -0100")
        (safe-date-to-time "not a date")
        (safe-date-to-time nil)
        (condition-case err (safe-date-to-time) (error (car err)))
        "#,
    );
    assert_eq!(results[0], "OK (0 0)");
    assert_eq!(results[1], "OK (0 0)");
    assert_eq!(results[2], "OK (0 3600)");
    assert_eq!(results[3], "OK 0");
    assert_eq!(results[4], "OK 0");
    assert_eq!(results[5], "OK wrong-number-of-arguments");
}

// -----------------------------------------------------------------------
// Edge cases
// -----------------------------------------------------------------------

#[test]
fn time_add_with_usec_overflow() {
    let a = Value::list(vec![
        Value::fixnum(0),
        Value::fixnum(10),
        Value::fixnum(999_000),
        Value::fixnum(0),
    ]);
    let b = Value::list(vec![
        Value::fixnum(0),
        Value::fixnum(5),
        Value::fixnum(500_000),
        Value::fixnum(0),
    ]);
    let result = builtin_time_add(vec![a, b]).unwrap();
    let items = list_to_vec(&result).unwrap();
    let high = items[0].as_int().unwrap();
    let low = items[1].as_int().unwrap();
    let usec = items[2].as_int().unwrap();
    assert_eq!(high * 65536 + low, 16); // 10 + 5 + 1 carry
    assert_eq!(usec, 499_000); // 999000 + 500000 - 1000000
}

#[test]
fn time_subtract_with_usec_borrow() {
    let a = Value::list(vec![
        Value::fixnum(0),
        Value::fixnum(10),
        Value::fixnum(100_000),
        Value::fixnum(0),
    ]);
    let b = Value::list(vec![
        Value::fixnum(0),
        Value::fixnum(5),
        Value::fixnum(500_000),
        Value::fixnum(0),
    ]);
    let result = builtin_time_subtract(vec![a, b]).unwrap();
    let items = list_to_vec(&result).unwrap();
    let high = items[0].as_int().unwrap();
    let low = items[1].as_int().unwrap();
    let usec = items[2].as_int().unwrap();
    assert_eq!(high * 65536 + low, 4); // 10 - 5 - 1 borrow
    assert_eq!(usec, 600_000); // 100000 - 500000 + 1000000
}

#[test]
fn float_time_nil_arg() {
    let result = builtin_float_time(vec![Value::NIL]).unwrap();
    match result.kind() {
        ValueKind::Float => { let f = result.as_float().unwrap(); assert!(f > 1_000_000_000.0); }
        _ => panic!("expected float"),
    }
}

#[test]
fn time_operations_with_mixed_formats() {
    // Add an integer to a list-format time.
    let a = Value::fixnum(100);
    let b = Value::list(vec![
        Value::fixnum(0),
        Value::fixnum(50),
        Value::fixnum(250_000),
        Value::fixnum(0),
    ]);
    let result = builtin_time_add(vec![a, b]).unwrap();
    let items = list_to_vec(&result).unwrap();
    let high = items[0].as_int().unwrap();
    let low = items[1].as_int().unwrap();
    let usec = items[2].as_int().unwrap();
    assert_eq!(high * 65536 + low, 150);
    assert_eq!(usec, 250_000);
}

#[test]
fn current_time_string_epoch() {
    let result = builtin_current_time_string(vec![Value::fixnum(0)]).unwrap();
    let s = result.as_str().unwrap();
    // 1970-01-01 00:00:00 UTC, Thursday
    assert!(s.contains("Thu"));
    assert!(s.contains("Jan"));
    assert!(s.contains("1970"));
    assert!(s.contains("00:00:00"));
}
