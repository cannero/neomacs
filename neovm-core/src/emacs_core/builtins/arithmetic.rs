use super::*;
use std::sync::Mutex;

#[cfg(unix)]
unsafe extern "C" {
    fn random() -> libc::c_long;
    fn srandom(seed: libc::c_uint);
}

// ===========================================================================
// Arithmetic
// ===========================================================================

/// Eval-aware `+` that reads live marker positions from buffers.
pub(crate) fn builtin_add(eval: &mut super::super::eval::Context, args: Vec<Value>) -> EvalResult {
    if has_float(&args) {
        let mut sum = 0.0f64;
        for a in &args {
            sum += expect_number_or_marker_f64_eval(eval, a)?;
        }
        Ok(Value::make_float(sum))
    } else {
        let mut sum = 0i64;
        for a in &args {
            sum = sum.wrapping_add(expect_integer_or_marker_after_number_check_eval(eval, a)?);
        }
        Ok(Value::fixnum(sum))
    }
}

/// Eval-aware `-` that reads live marker positions from buffers.
pub(crate) fn builtin_sub(eval: &mut super::super::eval::Context, args: Vec<Value>) -> EvalResult {
    if args.is_empty() {
        return Ok(Value::fixnum(0));
    }
    if args.len() == 1 {
        if has_float(&args) {
            return Ok(Value::make_float(-expect_number_or_marker_f64_eval(
                eval, &args[0],
            )?));
        }
        let n = expect_integer_or_marker_after_number_check_eval(eval, &args[0])?;
        return Ok(Value::fixnum(n.wrapping_neg()));
    }
    if has_float(&args) {
        let mut acc = expect_number_or_marker_f64_eval(eval, &args[0])?;
        for a in &args[1..] {
            acc -= expect_number_or_marker_f64_eval(eval, a)?;
        }
        Ok(Value::make_float(acc))
    } else {
        let mut acc = expect_integer_or_marker_after_number_check_eval(eval, &args[0])?;
        for a in &args[1..] {
            acc = acc.wrapping_sub(expect_integer_or_marker_after_number_check_eval(eval, a)?);
        }
        Ok(Value::fixnum(acc))
    }
}

pub(crate) fn builtin_mul(args: Vec<Value>) -> EvalResult {
    if has_float(&args) {
        let mut prod = 1.0f64;
        for a in &args {
            prod *= expect_number_or_marker_f64(a)?;
        }
        Ok(Value::make_float(prod))
    } else {
        // Official Emacs uses wrapping arithmetic for integer * (no overflow error).
        let mut prod = 1i64;
        for a in &args {
            prod = prod.wrapping_mul(expect_integer_or_marker_after_number_check(a)?);
        }
        Ok(Value::fixnum(prod))
    }
}

pub(crate) fn builtin_div(args: Vec<Value>) -> EvalResult {
    expect_min_args("/", &args, 1)?;
    // Single argument: return 1 / arg (reciprocal), matching GNU Emacs.
    if args.len() == 1 {
        if has_float(&args) {
            let d = expect_number_or_marker_f64(&args[0])?;
            let result = 1.0 / d;
            return Ok(Value::make_float(result));
        } else {
            let d = expect_integer_or_marker_after_number_check(&args[0])?;
            if d == 0 {
                return Err(signal("arith-error", vec![]));
            }
            return Ok(Value::fixnum(1i64.checked_div(d).unwrap_or(0)));
        }
    }
    if has_float(&args) {
        let mut acc = expect_number_or_marker_f64(&args[0])?;
        for a in &args[1..] {
            let d = expect_number_or_marker_f64(a)?;
            acc /= d;
            if acc.is_nan() {
                // Emacs prints negative-NaN for float zero-divisor paths.
                acc = f64::from_bits(f64::NAN.to_bits() | (1_u64 << 63));
            }
        }
        Ok(Value::make_float(acc))
    } else {
        let mut acc = expect_integer_or_marker_after_number_check(&args[0])?;
        for a in &args[1..] {
            let d = expect_integer_or_marker_after_number_check(a)?;
            if d == 0 {
                return Err(signal("arith-error", vec![]));
            }
            acc = acc
                .checked_div(d)
                .ok_or_else(|| signal("overflow-error", vec![]))?;
        }
        Ok(Value::fixnum(acc))
    }
}

pub(crate) fn builtin_percent(args: Vec<Value>) -> EvalResult {
    expect_args("%", &args, 2)?;
    let a = expect_integer_or_marker(&args[0])?;
    let b = expect_integer_or_marker(&args[1])?;
    if b == 0 {
        return Err(signal("arith-error", vec![]));
    }
    Ok(Value::fixnum(a % b))
}

pub(crate) fn builtin_mod(args: Vec<Value>) -> EvalResult {
    expect_args("mod", &args, 2)?;
    let a_raw = expect_number_or_marker(&args[0])?;
    let b_raw = expect_number_or_marker(&args[1])?;
    match (a_raw, b_raw) {
        (NumberOrMarker::Int(a), NumberOrMarker::Int(b)) => {
            if b == 0 {
                return Err(signal("arith-error", vec![]));
            }
            // Emacs mod: result has sign of divisor.
            let r = a % b;
            let r = if r != 0 && (r < 0) != (b < 0) {
                r + b
            } else {
                r
            };
            Ok(Value::fixnum(r))
        }
        (a, b) => {
            let a = match a {
                NumberOrMarker::Int(n) => n as f64,
                NumberOrMarker::Float(f) => f,
            };
            let b = match b {
                NumberOrMarker::Int(n) => n as f64,
                NumberOrMarker::Float(f) => f,
            };
            let r = a % b;
            let mut r = if r != 0.0 && (r < 0.0) != (b < 0.0) {
                r + b
            } else {
                r
            };
            if r.is_nan() {
                // Emacs prints negative-NaN for floating mod-by-zero payloads.
                r = f64::from_bits(f64::NAN.to_bits() | (1_u64 << 63));
            }
            Ok(Value::make_float(r)) // TODO(tagged): remove next_float_id()
        }
    }
}

pub(crate) fn builtin_add1(args: Vec<Value>) -> EvalResult {
    expect_args("1+", &args, 1)?;
    match args[0].kind() {
        // Official Emacs uses wrapping arithmetic for 1+ (no overflow error).
        ValueKind::Fixnum(n) => Ok(Value::fixnum(n.wrapping_add(1))),
        ValueKind::Float => Ok(Value::make_float(args[0].xfloat() + 1.0)),
        _ if args[0].is_marker() => Ok(Value::fixnum(
            super::marker::marker_position_as_int(&args[0])?.wrapping_add(1),
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), args[0]],
        )),
    }
}

pub(crate) fn builtin_sub1(args: Vec<Value>) -> EvalResult {
    expect_args("1-", &args, 1)?;
    match args[0].kind() {
        // Official Emacs uses wrapping arithmetic for 1- (no overflow error).
        ValueKind::Fixnum(n) => Ok(Value::fixnum(n.wrapping_sub(1))),
        ValueKind::Float => Ok(Value::make_float(args[0].xfloat() - 1.0)),
        _ if args[0].is_marker() => Ok(Value::fixnum(
            super::marker::marker_position_as_int(&args[0])?.wrapping_sub(1),
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), args[0]],
        )),
    }
}

pub(crate) fn builtin_max(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("max", &args, 1)?;
    let mut best_num = expect_number_or_marker_f64_eval(eval, &args[0])?;
    let mut best_value = args[0];
    for a in &args[1..] {
        let n = expect_number_or_marker_f64_eval(eval, a)?;
        if n > best_num {
            best_num = n;
            best_value = *a;
        }
    }
    match best_value.kind() {
        ValueKind::Fixnum(_) | ValueKind::Float => Ok(best_value),
        _ if best_value.is_marker() => Ok(Value::fixnum(
            super::marker::marker_position_as_int_eval(eval, &best_value)?,
        )),
        _ => unreachable!("max winner must be numeric"),
    }
}

pub(crate) fn builtin_min(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("min", &args, 1)?;
    let mut best_num = expect_number_or_marker_f64_eval(eval, &args[0])?;
    let mut best_value = args[0];
    for a in &args[1..] {
        let n = expect_number_or_marker_f64_eval(eval, a)?;
        if n < best_num {
            best_num = n;
            best_value = *a;
        }
    }
    match best_value.kind() {
        ValueKind::Fixnum(_) | ValueKind::Float => Ok(best_value),
        _ if best_value.is_marker() => Ok(Value::fixnum(
            super::marker::marker_position_as_int_eval(eval, &best_value)?,
        )),
        _ => unreachable!("min winner must be numeric"),
    }
}

pub(crate) fn builtin_abs(args: Vec<Value>) -> EvalResult {
    expect_args("abs", &args, 1)?;
    match args[0].kind() {
        ValueKind::Fixnum(n) => {
            if n == Value::MOST_NEGATIVE_FIXNUM {
                return Err(signal("overflow-error", vec![]));
            }
            Ok(Value::fixnum(n.abs()))
        }
        ValueKind::Float => Ok(Value::make_float(args[0].xfloat().abs())),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), args[0]],
        )),
    }
}

// ===========================================================================
// Logical / bitwise
// ===========================================================================

pub(crate) fn builtin_logand(args: Vec<Value>) -> EvalResult {
    let mut acc = -1i64; // all bits set
    for a in &args {
        acc &= expect_integer_or_marker_after_number_check(a)?;
    }
    Ok(Value::fixnum(acc))
}

pub(crate) fn builtin_logior(args: Vec<Value>) -> EvalResult {
    let mut acc = 0i64;
    for a in &args {
        acc |= expect_integer_or_marker_after_number_check(a)?;
    }
    Ok(Value::fixnum(acc))
}

pub(crate) fn builtin_logxor(args: Vec<Value>) -> EvalResult {
    let mut acc = 0i64;
    for a in &args {
        acc ^= expect_integer_or_marker_after_number_check(a)?;
    }
    Ok(Value::fixnum(acc))
}

pub(crate) fn builtin_lognot(args: Vec<Value>) -> EvalResult {
    expect_args("lognot", &args, 1)?;
    Ok(Value::fixnum(!expect_int(&args[0])?))
}

pub(crate) fn builtin_ash(args: Vec<Value>) -> EvalResult {
    expect_args("ash", &args, 2)?;
    let n = expect_int(&args[0])?;
    let count = expect_int(&args[1])?;
    if count >= 0 {
        let shift = u32::try_from(count).unwrap_or(u32::MAX);
        Ok(Value::fixnum(n.checked_shl(shift).unwrap_or(0)))
    } else {
        let shift = count.unsigned_abs().min((i64::BITS - 1) as u64) as u32;
        Ok(Value::fixnum(n >> shift))
    }
}

// ===========================================================================
// Comparisons
// ===========================================================================

pub(crate) fn builtin_num_eq(
    eval: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("=", &args, 2)?;
    let first = expect_number_or_marker_f64_eval(eval, &args[0])?;
    for a in &args[1..] {
        if expect_number_or_marker_f64_eval(eval, a)? != first {
            return Ok(Value::NIL);
        }
    }
    Ok(Value::T)
}

pub(crate) fn builtin_num_lt(
    eval: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("<", &args, 2)?;
    for pair in args.windows(2) {
        if !(expect_number_or_marker_f64_eval(eval, &pair[0])?
            < expect_number_or_marker_f64_eval(eval, &pair[1])?)
        {
            return Ok(Value::NIL);
        }
    }
    Ok(Value::T)
}

pub(crate) fn builtin_num_le(
    eval: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("<=", &args, 2)?;
    for pair in args.windows(2) {
        if !(expect_number_or_marker_f64_eval(eval, &pair[0])?
            <= expect_number_or_marker_f64_eval(eval, &pair[1])?)
        {
            return Ok(Value::NIL);
        }
    }
    Ok(Value::T)
}

pub(crate) fn builtin_num_gt(
    eval: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args(">", &args, 2)?;
    for pair in args.windows(2) {
        if !(expect_number_or_marker_f64_eval(eval, &pair[0])?
            > expect_number_or_marker_f64_eval(eval, &pair[1])?)
        {
            return Ok(Value::NIL);
        }
    }
    Ok(Value::T)
}

pub(crate) fn builtin_num_ge(
    eval: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args(">=", &args, 2)?;
    for pair in args.windows(2) {
        if !(expect_number_or_marker_f64_eval(eval, &pair[0])?
            >= expect_number_or_marker_f64_eval(eval, &pair[1])?)
        {
            return Ok(Value::NIL);
        }
    }
    Ok(Value::T)
}

pub(crate) fn builtin_num_ne(
    eval: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("/=", &args, 2)?;
    let a = expect_number_or_marker_f64_eval(eval, &args[0])?;
    let b = expect_number_or_marker_f64_eval(eval, &args[1])?;
    Ok(Value::bool_val(a != b))
}

// ===========================================================================
// Conversion
// ===========================================================================

pub(crate) fn builtin_float(args: Vec<Value>) -> EvalResult {
    expect_args("float", &args, 1)?;
    match args[0].kind() {
        ValueKind::Fixnum(n) => Ok(Value::make_float(n as f64)),
        ValueKind::Float => Ok(args[0]),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), args[0]],
        )),
    }
}

/// Helper: extract a number as f64, signaling wrong-type-argument if not numeric.
fn value_to_f64(_name: &str, v: &Value) -> Result<f64, Flow> {
    match v.kind() {
        ValueKind::Fixnum(n) => Ok(n as f64),
        ValueKind::Float => Ok(v.xfloat()),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), *v],
        )),
    }
}

/// Helper for 1-or-2-arg rounding functions.
/// When called with 2 args, divides first by second, then applies the rounding op.
/// For int/int with no remainder, returns integer directly.
fn rounding_with_divisor(
    name: &str,
    args: &[Value],
    round_fn: fn(f64) -> f64,
    int_div: fn(i64, i64) -> i64,
) -> EvalResult {
    expect_range_args(name, args, 1, 2)?;
    if args.len() == 1 {
        match args[0].kind() {
            ValueKind::Fixnum(n) => return Ok(Value::fixnum(n)),
            ValueKind::Float => return Ok(Value::fixnum(round_fn(args[0].xfloat()) as i64)),
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("numberp"), args[0]],
                ));
            }
        }
    }
    // 2-arg form: (op ARG DIVISOR)
    let divisor = value_to_f64(name, &args[1])?;
    if divisor == 0.0 {
        return Err(signal("arith-error", vec![]));
    }
    // If both are integers and division is exact, use integer path
    if let (Some(a), Some(d)) = (&args[0].as_fixnum(), &args[1].as_fixnum()) {
        return Ok(Value::fixnum(int_div(*a, *d)));
    }
    let dividend = value_to_f64(name, &args[0])?;
    Ok(Value::fixnum(round_fn(dividend / divisor) as i64))
}

pub(crate) fn builtin_truncate(args: Vec<Value>) -> EvalResult {
    rounding_with_divisor(
        "truncate",
        &args,
        |f| f.trunc(),
        |a, d| {
            // Truncation: toward zero
            a / d
        },
    )
}

pub(crate) fn builtin_floor(args: Vec<Value>) -> EvalResult {
    rounding_with_divisor(
        "floor",
        &args,
        |f| f.floor(),
        |a, d| {
            // Floor division: toward negative infinity
            let q = a / d;
            let r = a % d;
            if (r != 0) && ((r ^ d) < 0) { q - 1 } else { q }
        },
    )
}

pub(crate) fn builtin_ceiling(args: Vec<Value>) -> EvalResult {
    rounding_with_divisor(
        "ceiling",
        &args,
        |f| f.ceil(),
        |a, d| {
            // Ceiling division: toward positive infinity
            let q = a / d;
            let r = a % d;
            if (r != 0) && ((r ^ d) >= 0) { q + 1 } else { q }
        },
    )
}

pub(crate) fn builtin_round(args: Vec<Value>) -> EvalResult {
    rounding_with_divisor(
        "round",
        &args,
        |f| f.round_ties_even(),
        |a, d| {
            // Banker's rounding (round half to even)
            let q = a / d;
            let r = a % d;
            let abs_r2 = (r * 2).abs();
            let abs_d = d.abs();
            if abs_r2 > abs_d {
                if (r ^ d) >= 0 { q + 1 } else { q - 1 }
            } else if abs_r2 == abs_d {
                // Tie: round to even
                if q % 2 != 0 {
                    if (r ^ d) >= 0 { q + 1 } else { q - 1 }
                } else {
                    q
                }
            } else {
                q
            }
        },
    )
}

// ===========================================================================
// Math functions (pure)
// ===========================================================================

pub(crate) fn builtin_sqrt(args: Vec<Value>) -> EvalResult {
    expect_args("sqrt", &args, 1)?;
    Ok(Value::make_float(expect_number(&args[0])?.sqrt()))
}

pub(crate) fn builtin_sin(args: Vec<Value>) -> EvalResult {
    expect_args("sin", &args, 1)?;
    Ok(Value::make_float(expect_number(&args[0])?.sin()))
}

pub(crate) fn builtin_cos(args: Vec<Value>) -> EvalResult {
    expect_args("cos", &args, 1)?;
    Ok(Value::make_float(expect_number(&args[0])?.cos()))
}

pub(crate) fn builtin_tan(args: Vec<Value>) -> EvalResult {
    expect_args("tan", &args, 1)?;
    Ok(Value::make_float(expect_number(&args[0])?.tan()))
}

pub(crate) fn builtin_asin(args: Vec<Value>) -> EvalResult {
    expect_args("asin", &args, 1)?;
    Ok(Value::make_float(expect_number(&args[0])?.asin()))
}

pub(crate) fn builtin_acos(args: Vec<Value>) -> EvalResult {
    expect_args("acos", &args, 1)?;
    Ok(Value::make_float(expect_number(&args[0])?.acos()))
}

pub(crate) fn builtin_atan(args: Vec<Value>) -> EvalResult {
    expect_min_args("atan", &args, 1)?;
    if args.len() == 2 {
        let y = expect_number(&args[0])?;
        let x = expect_number(&args[1])?;
        Ok(Value::make_float(y.atan2(x)))
    } else {
        Ok(Value::make_float(expect_number(&args[0])?.atan()))
    }
}

pub(crate) fn builtin_exp(args: Vec<Value>) -> EvalResult {
    expect_args("exp", &args, 1)?;
    Ok(Value::make_float(expect_number(&args[0])?.exp()))
}

pub(crate) fn builtin_log(args: Vec<Value>) -> EvalResult {
    expect_min_args("log", &args, 1)?;
    let val = expect_number(&args[0])?;
    if args.len() == 2 {
        let base = expect_number(&args[1])?;
        Ok(Value::make_float(val.ln() / base.ln()))
    } else {
        Ok(Value::make_float(val.ln()))
    }
}

pub(crate) fn builtin_expt(args: Vec<Value>) -> EvalResult {
    expect_args("expt", &args, 2)?;
    if has_float(&args) {
        let base = expect_number(&args[0])?;
        let exp = expect_number(&args[1])?;
        Ok(Value::make_float(base.powf(exp)))
    } else {
        let base = expect_number(&args[0])? as i64;
        let exp = expect_number(&args[1])? as i64;
        if exp < 0 {
            Ok(Value::make_float((base as f64).powf(exp as f64)))
        } else {
            Ok(Value::fixnum(base.wrapping_pow(exp as u32)))
        }
    }
}

pub(crate) fn builtin_random(args: Vec<Value>) -> EvalResult {
    expect_max_args("random", &args, 1)?;

    if let Some(limit) = args.first() {
        match limit.kind() {
            ValueKind::T => emacs_init_random(),
            ValueKind::String => {
                let bytes = limit.as_str().unwrap().as_bytes().to_vec();
                emacs_seed_random(&bytes);
            }
            ValueKind::Fixnum(lim) => {
                if lim <= 0 {
                    return Err(signal("args-out-of-range", vec![*limit]));
                }
                return Ok(Value::fixnum(emacs_get_random_fixnum(lim)));
            }
            _ => {}
        }
    }

    Ok(Value::fixnum(emacs_get_random()))
}

#[cfg(unix)]
fn emacs_random_lock() -> &'static Mutex<()> {
    static RANDOM_LOCK: Mutex<()> = Mutex::new(());
    &RANDOM_LOCK
}

#[cfg(unix)]
fn emacs_intmask() -> u64 {
    (1_u64 << emacs_random_fixnum_bits()) - 1
}

#[cfg(unix)]
fn emacs_random_fixnum_bits() -> u32 {
    // Match GNU Emacs get_random()/INTMASK behavior on current 64-bit builds:
    // FIXNUM_BITS is 62 even though most-positive-fixnum is 2^61 - 1.
    62
}

#[cfg(unix)]
fn emacs_get_random_unlocked() -> i64 {
    const RAND_BITS: u32 = 31;
    const EMACS_INT_WIDTH: u32 = 64;
    let fixnum_bits = emacs_random_fixnum_bits();
    let mut val: u64 = 0;
    for _ in 0..fixnum_bits.div_ceil(RAND_BITS) {
        let r = unsafe { random() as u64 };
        val = r ^ (val << RAND_BITS) ^ (val >> (EMACS_INT_WIDTH - RAND_BITS));
    }
    val ^= val >> (EMACS_INT_WIDTH - fixnum_bits);
    (val & emacs_intmask()) as i64
}

#[cfg(unix)]
fn emacs_get_random() -> i64 {
    let _guard = emacs_random_lock().lock().expect("random lock poisoned");
    emacs_get_random_unlocked()
}

#[cfg(unix)]
fn emacs_get_random_fixnum(limit: i64) -> i64 {
    let lim = limit as u64;
    let intmask = emacs_intmask();
    let difflim = intmask - lim + 1;
    let _guard = emacs_random_lock().lock().expect("random lock poisoned");
    loop {
        let r = emacs_get_random_unlocked() as u64;
        let remainder = r % lim;
        let diff = r - remainder;
        if difflim >= diff {
            return remainder as i64;
        }
    }
}

#[cfg(unix)]
fn emacs_seed_random(seed: &[u8]) {
    let _guard = emacs_random_lock().lock().expect("random lock poisoned");
    let mut arg = [0u8; std::mem::size_of::<u32>()];
    for (index, byte) in seed.iter().enumerate() {
        arg[index % arg.len()] ^= *byte;
    }
    let seed = u32::from_ne_bytes(arg);
    unsafe {
        srandom(seed);
    }
}

#[cfg(unix)]
fn emacs_init_random() {
    let seed = (std::process::id() as u64)
        ^ (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() ^ u64::from(d.subsec_nanos()))
            .unwrap_or(0));
    let bytes = seed.to_ne_bytes();
    emacs_seed_random(&bytes);
}

#[cfg(not(unix))]
fn emacs_get_random() -> i64 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = const { Cell::new(0x12345678_9abcdef0) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x as i64) & (i64::MAX >> 2)
    })
}

#[cfg(not(unix))]
fn emacs_get_random_fixnum(limit: i64) -> i64 {
    emacs_get_random().unsigned_abs() as i64 % limit
}

#[cfg(not(unix))]
fn emacs_seed_random(seed: &[u8]) {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = const { Cell::new(0x12345678_9abcdef0) };
    }
    let mut arg = 0u64;
    for (index, byte) in seed.iter().enumerate() {
        arg ^= u64::from(*byte) << ((index % 8) * 8);
    }
    STATE.with(|state| state.set(arg));
}

#[cfg(not(unix))]
fn emacs_init_random() {
    let seed = (std::process::id() as u64)
        ^ (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() ^ u64::from(d.subsec_nanos()))
            .unwrap_or(0));
    emacs_seed_random(&seed.to_ne_bytes());
}

pub(crate) fn builtin_isnan(args: Vec<Value>) -> EvalResult {
    expect_args("isnan", &args, 1)?;
    match args[0].kind() {
        ValueKind::Float => Ok(Value::bool_val(args[0].xfloat().is_nan())),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("floatp"), args[0]],
        )),
    }
}
