use super::*;
use crate::emacs_core::autoload::is_autoload_value;
use crate::emacs_core::eval::Evaluator;
use crate::emacs_core::load::{
    apply_ldefs_boot_autoloads_for_names, apply_runtime_startup_state,
    create_bootstrap_evaluator_cached,
};
use crate::emacs_core::{format_eval_result, parse_forms};

fn bootstrap_eval(src: &str) -> Vec<String> {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn eval_with_ldefs_boot_autoloads(names: &[&str]) -> Evaluator {
    let mut eval = Evaluator::new();
    for name in names {
        eval.obarray_mut().fmakunbound(name);
    }
    apply_ldefs_boot_autoloads_for_names(&mut eval, names).expect("ldefs-boot autoload restore");
    eval
}

// ===================================================================
// format-spec tests
// ===================================================================

#[test]
fn format_spec_bootstrap_matches_gnu_elisp() {
    let results = bootstrap_eval(
        r#"
        (format-spec "%n is %a" '((?n . "Bob") (?a . "21")))
        (format-spec "100%% done" nil)
        (format-spec "[%10n]" '((?n . "hi")))
        (format-spec "[%-10n]" '((?n . "hi")))
        (format-spec "[%05n]" '((?n . "42")))
        (condition-case err (format-spec "hello %x world" nil) (error (car err)))
        (format-spec "hello %x world" nil 'ignore)
        (condition-case err (format-spec "hi") (error (car err)))
        "#,
    );
    assert_eq!(results[0], r#"OK "Bob is 21""#);
    assert_eq!(results[1], r#"OK "100% done""#);
    assert_eq!(results[2], r#"OK "[        hi]""#);
    assert_eq!(results[3], r#"OK "[hi        ]""#);
    assert_eq!(results[4], r#"OK "[00042]""#);
    assert_eq!(results[5], "OK error");
    assert_eq!(results[6], r#"OK "hello %x world""#);
    assert_eq!(results[7], "OK wrong-number-of-arguments");
}

#[test]
fn format_percent_s_uses_recursive_princ_semantics_for_lists() {
    let results = bootstrap_eval(
        r#"
        (format "%s" '("development" "testing" "production"))
        "#,
    );
    assert_eq!(results[0], r#"OK "(development testing production)""#);
}

// ===================================================================
// format-time-string tests
// ===================================================================

#[test]
fn format_time_string_epoch() {
    // Unix epoch: 1970-01-01 00:00:00 UTC (Thursday)
    let result =
        builtin_format_time_string(vec![Value::string("%Y-%m-%d %H:%M:%S"), Value::Int(0)]);
    assert_eq!(result.unwrap().as_str().unwrap(), "1970-01-01 00:00:00");
}

#[test]
fn format_time_string_day_name() {
    // 1970-01-01 is a Thursday.
    let result = builtin_format_time_string(vec![Value::string("%A"), Value::Int(0)]);
    assert_eq!(result.unwrap().as_str().unwrap(), "Thursday");
}

#[test]
fn format_time_string_month_name() {
    let result = builtin_format_time_string(vec![Value::string("%B"), Value::Int(0)]);
    assert_eq!(result.unwrap().as_str().unwrap(), "January");
}

#[test]
fn format_time_string_known_date() {
    // 2000-01-01 00:00:00 UTC = 946684800
    let result =
        builtin_format_time_string(vec![Value::string("%Y-%m-%d %A"), Value::Int(946684800)]);
    assert_eq!(result.unwrap().as_str().unwrap(), "2000-01-01 Saturday");
}

#[test]
fn format_time_string_literal_percent() {
    let result = builtin_format_time_string(vec![Value::string("100%%"), Value::Int(0)]);
    assert_eq!(result.unwrap().as_str().unwrap(), "100%");
}

#[test]
fn format_time_string_timezone() {
    let result = builtin_format_time_string(vec![Value::string("%Z"), Value::Int(0)]);
    assert_eq!(result.unwrap().as_str().unwrap(), "UTC");
}

#[test]
fn format_time_string_iso_format() {
    let result = builtin_format_time_string(vec![Value::string("%F %T"), Value::Int(946684800)]);
    assert_eq!(result.unwrap().as_str().unwrap(), "2000-01-01 00:00:00");
}

#[test]
fn format_time_string_ampm() {
    // 2000-01-01 15:30:00 UTC = 946684800 + 15*3600 + 30*60 = 946740600
    let result = builtin_format_time_string(vec![Value::string("%I:%M %p"), Value::Int(946740600)]);
    assert_eq!(result.unwrap().as_str().unwrap(), "03:30 PM");
}

#[test]
fn format_time_string_no_time_uses_current() {
    // Should not error when TIME is nil.
    let result = builtin_format_time_string(vec![Value::string("%Y"), Value::Nil]);
    assert!(result.is_ok());
    // Should return a 4-digit year.
    let year_str = result.unwrap();
    assert_eq!(year_str.as_str().unwrap().len(), 4);
}

// ===================================================================
// format-seconds tests
// ===================================================================

#[test]
fn format_seconds_bootstrap_matches_gnu_elisp() {
    let results = bootstrap_eval(
        r#"
        (format-seconds "%h:%m:%s" 3661)
        (format-seconds "%d days, %h:%m:%s" 90061)
        (format-seconds "%h:%m:%s" 0)
        (format-seconds "100%%" 0)
        "#,
    );
    assert_eq!(results[0], r#"OK "1:1:1""#);
    assert_eq!(results[1], r#"OK "1 days, 1:1:1""#);
    assert_eq!(results[2], r#"OK "0:0:0""#);
    assert_eq!(results[3], r#"OK "100%""#);
}

// ===================================================================
// subr-x string helper tests
// ===================================================================

#[test]
fn subr_x_string_helpers_bootstrap_match_gnu() {
    let results = bootstrap_eval(
        r#"
        (load "subr-x")
        (let ((pad (symbol-function 'string-pad))
              (limit (symbol-function 'string-limit))
              (glyph (symbol-function 'string-glyph-split)))
          (list (subrp pad)
                (subrp limit)
                (subrp glyph)
                (funcall pad "x" 4 ?0 t)
                (funcall limit "abcd" 3 t)
                (funcall glyph "abc")))
        (string-fill "x" 2)
        (string-fill "aa bb ccc d" 5)
        (string-fill "a b\n\nc d" 10)
        (condition-case err (string-fill 1 2) (error (car err)))
        "#,
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], r#"OK (nil nil nil "000x" "bcd" ("a" "b" "c"))"#);
    assert_eq!(results[2], r#"OK "x""#);
    assert_eq!(results[3], "OK \"aa bb\nccc d\"");
    assert_eq!(results[4], "OK \"a b\n\nc d\"");
    assert_eq!(results[5], "OK \"\u{1}\"");
}

#[test]
fn subr_x_string_helpers_autoload() {
    let results = bootstrap_eval(
        r#"
        (let ((before-pad (symbol-function 'string-pad))
              (before-limit (symbol-function 'string-limit))
              (before-glyph (symbol-function 'string-glyph-split)))
          (list (autoloadp before-pad)
                (autoloadp before-limit)
                (autoloadp before-glyph)
                (string-pad "x" 2)
                (string-limit "abcd" 2)
                (string-glyph-split "abc")
                (autoloadp (symbol-function 'string-pad))
                (autoloadp (symbol-function 'string-limit))
                (autoloadp (symbol-function 'string-glyph-split))
                (subrp (symbol-function 'string-pad))
                (subrp (symbol-function 'string-limit))
                (subrp (symbol-function 'string-glyph-split))))
        "#,
    );
    // NeoVM loads .el source (not .elc), so `eval-when-compile` during
    // bootstrap runs `(require 'subr-x)` (from prog-mode.el), which defines
    // string-pad, string-limit, and string-glyph-split as Elisp functions
    // before this test runs.  In GNU Emacs they stay as autoloads because
    // .elc files fold eval-when-compile to a constant.  So autoloadp returns
    // nil here instead of t.
    assert_eq!(
        results[0],
        r#"OK (nil nil nil "x " "ab" ("a" "b" "c") nil nil nil nil nil nil)"#
    );
}

// ===================================================================
// string-chop-newline tests
// ===================================================================

#[test]
fn string_chop_newline_no_newline() {
    let result = builtin_string_chop_newline(vec![Value::string("x")]).unwrap();
    assert_eq!(result.as_str().unwrap(), "x");
}

#[test]
fn string_chop_newline_lf() {
    let result = builtin_string_chop_newline(vec![Value::string("x\n")]).unwrap();
    assert_eq!(result.as_str().unwrap(), "x");
}

#[test]
fn string_chop_newline_crlf_run() {
    let result = builtin_string_chop_newline(vec![Value::string("x\r\n\n")]).unwrap();
    assert_eq!(result.as_str().unwrap(), "x");
}

#[test]
fn string_chop_newline_wrong_type() {
    assert!(builtin_string_chop_newline(vec![Value::Int(1)]).is_err());
}

// ===================================================================
// string-lines tests
// ===================================================================

#[test]
fn string_lines_bootstrap_matches_gnu_subr() {
    let results = bootstrap_eval(
        r#"
        (subrp (symbol-function 'string-lines))
        (string-lines "a\nb\nc")
        (string-lines "a\nb\n")
        (string-lines "a\n\nb\n" t)
        (string-lines "")
        (string-lines "" t)
        (string-lines "a\n\nb\n" nil t)
        "#,
    );
    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], r#"OK ("a" "b" "c")"#);
    assert_eq!(results[2], r#"OK ("a" "b")"#);
    assert_eq!(results[3], r#"OK ("a" "b")"#);
    assert_eq!(results[4], r#"OK ("")"#);
    assert_eq!(results[5], "OK nil");
    assert_eq!(results[6], "OK (\"a\n\" \"\n\" \"b\n\")");
}

// ===================================================================
// string-clean-whitespace tests
// ===================================================================

#[test]
fn string_clean_whitespace_bootstrap_matches_gnu_elisp() {
    let results = bootstrap_eval(
        r#"
        (string-clean-whitespace "  hello   world  ")
        (string-clean-whitespace "a\t\tb\n\nc")
        (string-clean-whitespace "hello world")
        (string-clean-whitespace "")
        (string-clean-whitespace "   ")
        (condition-case err (string-clean-whitespace 1) (error (car err)))
        "#,
    );
    assert_eq!(results[0], r#"OK "hello world""#);
    assert_eq!(results[1], r#"OK "a b c""#);
    assert_eq!(results[2], r#"OK "hello world""#);
    assert_eq!(results[3], "OK \"\"");
    assert_eq!(results[4], "OK \"\"");
    assert_eq!(results[5], "OK wrong-type-argument");
}

// ===================================================================
// string-pixel-width tests
// ===================================================================

#[test]
fn string_pixel_width_startup_is_autoloaded() {
    let eval = eval_with_ldefs_boot_autoloads(&["string-pixel-width"]);
    let function = eval
        .obarray
        .symbol_function("string-pixel-width")
        .expect("missing string-pixel-width startup function cell");
    assert!(is_autoload_value(&function));
}

#[test]
fn string_pixel_width_bootstrap_matches_gnu_subr_x() {
    let results = bootstrap_eval(
        r#"
        (string-pixel-width "hello")
        (string-pixel-width "")
        (string-pixel-width "\t")
        (string-pixel-width "a\t")
        (string-pixel-width "a\tb")
        (string-pixel-width "漢字")
        (string-pixel-width "é")
        (with-temp-buffer
          (insert "abc\ndef")
          (buffer-text-pixel-size nil nil t))
        (with-temp-buffer
          (insert "abcdef\n123")
          (buffer-text-pixel-size nil nil 4))
        (subrp (symbol-function 'string-pixel-width))
        "#,
    );
    assert_eq!(results[0], "OK 5");
    assert_eq!(results[1], "OK 0");
    assert_eq!(results[2], "OK 8");
    assert_eq!(results[3], "OK 8");
    assert_eq!(results[4], "OK 9");
    assert_eq!(results[5], "OK 4");
    assert_eq!(results[6], "OK 1");
    assert_eq!(results[7], "OK (3 . 2)");
    assert_eq!(results[8], "OK (4 . 2)");
    assert_eq!(results[9], "OK nil");
}

// unix_to_broken_down internal tests
// ===================================================================

#[test]
fn broken_down_epoch() {
    let tm = unix_to_broken_down(0);
    assert_eq!(tm.year, 1970);
    assert_eq!(tm.month, 1);
    assert_eq!(tm.day, 1);
    assert_eq!(tm.hour, 0);
    assert_eq!(tm.minute, 0);
    assert_eq!(tm.second, 0);
    assert_eq!(tm.weekday, 4); // Thursday
}

#[test]
fn broken_down_y2k() {
    // 2000-01-01 00:00:00 UTC = 946684800
    let tm = unix_to_broken_down(946684800);
    assert_eq!(tm.year, 2000);
    assert_eq!(tm.month, 1);
    assert_eq!(tm.day, 1);
    assert_eq!(tm.weekday, 6); // Saturday
}

#[test]
fn broken_down_leap_year() {
    // 2000-02-29 00:00:00 UTC = 946684800 + 59*86400 = 946684800 + 5097600 = 951782400
    let tm = unix_to_broken_down(951782400);
    assert_eq!(tm.year, 2000);
    assert_eq!(tm.month, 2);
    assert_eq!(tm.day, 29);
}

#[test]
fn broken_down_end_of_day() {
    // 1970-01-01 23:59:59 = 86399
    let tm = unix_to_broken_down(86399);
    assert_eq!(tm.year, 1970);
    assert_eq!(tm.month, 1);
    assert_eq!(tm.day, 1);
    assert_eq!(tm.hour, 23);
    assert_eq!(tm.minute, 59);
    assert_eq!(tm.second, 59);
}

#[test]
fn broken_down_2024() {
    // 2024-03-15 12:30:45 UTC
    // Compute: days from 1970 to 2024-03-15
    // Using known: 2024-01-01 = 1704067200
    // Jan has 31 days, Feb has 29 (2024 is leap), so Mar 15 = 31 + 29 + 14 = 74 days after Jan 1
    // 1704067200 + 74 * 86400 = 1704067200 + 6393600 = 1710460800
    // + 12*3600 + 30*60 + 45 = 43200 + 1800 + 45 = 45045
    // Total: 1710505845
    let tm = unix_to_broken_down(1710505845);
    assert_eq!(tm.year, 2024);
    assert_eq!(tm.month, 3);
    assert_eq!(tm.day, 15);
    assert_eq!(tm.hour, 12);
    assert_eq!(tm.minute, 30);
    assert_eq!(tm.second, 45);
}
