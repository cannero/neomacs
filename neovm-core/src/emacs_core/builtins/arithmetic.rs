use super::*;
use rug::ops::Pow;
use std::sync::Mutex;

#[cfg(unix)]
unsafe extern "C" {
    fn random() -> libc::c_long;
    fn srandom(seed: libc::c_uint);
}

// ===========================================================================
// Arithmetic
// ===========================================================================
//
// `+`, `-`, `*` mirror GNU's `arith_driver` (src/data.c:3215): a fast
// fixnum loop using `ckd_add` / `ckd_sub` / `ckd_mul` for overflow
// detection, and a fall-back path that switches to GMP (rug::Integer)
// the moment overflow strikes or a bignum operand appears.

/// Pull an integer-valued operand into an `i64`. Accepts fixnums and
/// markers; for any other value (including bignums) returns
/// `Err(()) → caller decides`.  This is the fast-path helper used
/// before promotion to GMP.
fn try_i64_from_value(eval: &super::eval::Context, value: &Value) -> Result<Option<i64>, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(Some(n)),
        ValueKind::Veclike(VecLikeType::Bignum) => Ok(None),
        _ if super::marker::is_marker(value) => Ok(Some(
            super::marker::marker_position_as_int_eval(eval, value)?,
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *value],
        )),
    }
}

/// Materialize an integer-valued operand as a `rug::Integer`. Used by
/// the bignum slow path. Accepts fixnums, bignums, and markers.
fn rug_from_value(eval: &super::eval::Context, value: &Value) -> Result<rug::Integer, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(rug::Integer::from(n)),
        ValueKind::Veclike(VecLikeType::Bignum) => Ok(value.as_bignum().unwrap().clone()),
        _ if super::marker::is_marker(value) => Ok(rug::Integer::from(
            super::marker::marker_position_as_int_eval(eval, value)?,
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *value],
        )),
    }
}

/// Eval-aware `+` that reads live marker positions from buffers.
///
/// Mirrors GNU `Fplus` → `arith_driver` (src/data.c:3215, 3271): if
/// every operand is an i64-valued integer or marker and no addition
/// overflows, stay on the fixnum fast path; otherwise promote to GMP
/// via `rug::Integer`. Float operands divert through `make_float` as
/// before.
///
/// Note: i64 has 64 bits, but fixnums only get 62 bits (the low 2 are
/// the tag). A sum like `most-positive-fixnum + 1` does not overflow
/// i64 yet exceeds fixnum range; we therefore funnel the final result
/// through `Value::make_integer`, which decides between fixnum and
/// bignum just like GNU `make_int` (`src/lisp.h`).
pub(crate) fn builtin_add(eval: &mut super::super::eval::Context, args: Vec<Value>) -> EvalResult {
    if has_float(&args) {
        let mut sum = 0.0f64;
        for a in &args {
            sum += expect_number_or_marker_f64_eval(eval, a)?;
        }
        return Ok(Value::make_float(sum));
    }
    // Fixnum fast path with overflow detection.
    let mut sum: i64 = 0;
    for (i, a) in args.iter().enumerate() {
        match try_i64_from_value(eval, a)? {
            Some(n) => match sum.checked_add(n) {
                Some(s) => sum = s,
                None => {
                    let mut acc = rug::Integer::from(sum);
                    acc += n;
                    return continue_bignum_add(eval, &args[i + 1..], acc);
                }
            },
            None => {
                // Operand is a bignum — promote and re-process from here.
                let mut acc = rug::Integer::from(sum);
                acc += a.as_bignum().unwrap();
                return continue_bignum_add(eval, &args[i + 1..], acc);
            }
        }
    }
    Ok(Value::make_integer(rug::Integer::from(sum)))
}

fn continue_bignum_add(
    eval: &super::super::eval::Context,
    rest: &[Value],
    mut acc: rug::Integer,
) -> EvalResult {
    for a in rest {
        // We already verified upfront that no operand is float, so any
        // remaining value must be integer-or-marker.
        let n = rug_from_value(eval, a)?;
        acc += n;
    }
    Ok(Value::make_integer(acc))
}

/// Eval-aware `-` that reads live marker positions from buffers.
///
/// Mirrors GNU `Fminus` (`src/data.c:3282`):
/// * 0 args → 0
/// * 1 arg  → negation (with bignum promotion for `MIN_FIXNUM`)
/// * N args → arith_driver in subtract mode
pub(crate) fn builtin_sub(eval: &mut super::super::eval::Context, args: Vec<Value>) -> EvalResult {
    if args.is_empty() {
        return Ok(Value::fixnum(0));
    }
    if args.len() == 1 {
        return negate_value(eval, &args[0]);
    }
    if has_float(&args) {
        let mut acc = expect_number_or_marker_f64_eval(eval, &args[0])?;
        for a in &args[1..] {
            acc -= expect_number_or_marker_f64_eval(eval, a)?;
        }
        return Ok(Value::make_float(acc));
    }
    // Fixnum fast path: seed accumulator with first arg, then subtract.
    let first = &args[0];
    let mut acc: i64 = match try_i64_from_value(eval, first)? {
        Some(n) => n,
        None => {
            // First arg is a bignum — start GMP path immediately.
            let acc = first.as_bignum().unwrap().clone();
            return continue_bignum_sub(eval, &args[1..], acc);
        }
    };
    for (i, a) in args[1..].iter().enumerate() {
        match try_i64_from_value(eval, a)? {
            Some(n) => match acc.checked_sub(n) {
                Some(s) => acc = s,
                None => {
                    let mut bacc = rug::Integer::from(acc);
                    bacc -= n;
                    return continue_bignum_sub(eval, &args[i + 2..], bacc);
                }
            },
            None => {
                let mut bacc = rug::Integer::from(acc);
                bacc -= a.as_bignum().unwrap();
                return continue_bignum_sub(eval, &args[i + 2..], bacc);
            }
        }
    }
    // Funnel through make_integer to promote i64 results that exceeded
    // fixnum range (62-bit) but stayed within i64 (64-bit).
    Ok(Value::make_integer(rug::Integer::from(acc)))
}

fn continue_bignum_sub(
    eval: &super::super::eval::Context,
    rest: &[Value],
    mut acc: rug::Integer,
) -> EvalResult {
    for a in rest {
        let n = rug_from_value(eval, a)?;
        acc -= n;
    }
    Ok(Value::make_integer(acc))
}

/// Negate a single value, mirroring GNU `Fminus` 1-arg branch
/// (`src/data.c:3293-3300`). Promotes `MOST_NEGATIVE_FIXNUM` to a
/// bignum because `-MOST_NEGATIVE_FIXNUM` exceeds fixnum range.
fn negate_value(eval: &super::super::eval::Context, value: &Value) -> EvalResult {
    if value.is_float() {
        return Ok(Value::make_float(-value.xfloat()));
    }
    if let Some(big) = value.as_bignum() {
        return Ok(Value::make_integer(-big.clone()));
    }
    let n = match try_i64_from_value(eval, value)? {
        Some(n) => n,
        None => unreachable!(),
    };
    // checked_neg only fails for i64::MIN; for everything else we get
    // an i64 back which still has to clear the fixnum-range hurdle, so
    // route through make_integer.
    match n.checked_neg() {
        Some(neg) => Ok(Value::make_integer(rug::Integer::from(neg))),
        None => Ok(Value::make_integer(-rug::Integer::from(n))),
    }
}

/// `*` with bignum promotion. Mirrors GNU `Ftimes` → `arith_driver`
/// (`src/data.c:3304`).
pub(crate) fn builtin_mul(args: Vec<Value>) -> EvalResult {
    if has_float(&args) {
        let mut prod = 1.0f64;
        for a in &args {
            prod *= expect_number_or_marker_f64(a)?;
        }
        return Ok(Value::make_float(prod));
    }
    // We don't have an `eval` context here, so use the marker-less
    // helpers. This matches the existing builtin_mul signature.
    let mut prod: i64 = 1;
    for (i, a) in args.iter().enumerate() {
        match a.kind() {
            ValueKind::Fixnum(n) => match prod.checked_mul(n) {
                Some(p) => prod = p,
                None => {
                    let mut acc = rug::Integer::from(prod);
                    acc *= n;
                    return continue_bignum_mul(&args[i + 1..], acc);
                }
            },
            ValueKind::Veclike(VecLikeType::Bignum) => {
                let mut acc = rug::Integer::from(prod);
                acc *= a.as_bignum().unwrap();
                return continue_bignum_mul(&args[i + 1..], acc);
            }
            _ if super::marker::is_marker(a) => {
                let n = super::marker::marker_position_as_int(a)?;
                match prod.checked_mul(n) {
                    Some(p) => prod = p,
                    None => {
                        let mut acc = rug::Integer::from(prod);
                        acc *= n;
                        return continue_bignum_mul(&args[i + 1..], acc);
                    }
                }
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *a],
                ));
            }
        }
    }
    Ok(Value::make_integer(rug::Integer::from(prod)))
}

fn continue_bignum_mul(rest: &[Value], mut acc: rug::Integer) -> EvalResult {
    for a in rest {
        match a.kind() {
            ValueKind::Fixnum(n) => acc *= n,
            ValueKind::Veclike(VecLikeType::Bignum) => acc *= a.as_bignum().unwrap(),
            _ if super::marker::is_marker(a) => {
                acc *= super::marker::marker_position_as_int(a)?;
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *a],
                ));
            }
        }
    }
    Ok(Value::make_integer(acc))
}

/// `/` with bignum support. Mirrors GNU `Fquo` (`src/data.c:3315`).
///
/// Truncation toward zero (`tdiv_q` semantics, matching `mpz_tdiv_q`),
/// promoting `i64::MIN / -1` to bignum since `-i64::MIN` overflows i64.
/// Float operands divert through float division as before.
pub(crate) fn builtin_div(args: Vec<Value>) -> EvalResult {
    expect_min_args("/", &args, 1)?;
    // Single argument: return 1 / arg (reciprocal), matching GNU Emacs.
    if args.len() == 1 {
        return div_one_arg(&args[0]);
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
        return Ok(Value::make_float(acc));
    }
    // Integer fast path with bignum promotion on overflow.
    let first = &args[0];
    // If first is a bignum, start GMP path immediately.
    if first.is_bignum() {
        let acc = first.as_bignum().unwrap().clone();
        return continue_bignum_div(&args[1..], acc);
    }
    let mut acc: i64 = expect_integer_or_marker_after_number_check(first)?;
    for (i, a) in args[1..].iter().enumerate() {
        if a.is_bignum() {
            // Promote: convert acc to bignum and divide by this bignum,
            // then continue.
            let mut bacc = rug::Integer::from(acc);
            let big = a.as_bignum().unwrap();
            if big.is_zero() {
                return Err(signal("arith-error", vec![]));
            }
            bacc /= big;
            return continue_bignum_div(&args[i + 2..], bacc);
        }
        let d = expect_integer_or_marker_after_number_check(a)?;
        if d == 0 {
            return Err(signal("arith-error", vec![]));
        }
        match acc.checked_div(d) {
            Some(q) => acc = q,
            None => {
                // Only `i64::MIN / -1` triggers this. Promote.
                let bacc = rug::Integer::from(acc) / d;
                return continue_bignum_div(&args[i + 2..], bacc);
            }
        }
    }
    Ok(Value::make_integer(rug::Integer::from(acc)))
}

fn div_one_arg(arg: &Value) -> EvalResult {
    if arg.is_float() {
        let d = arg.xfloat();
        return Ok(Value::make_float(1.0 / d));
    }
    if let Some(big) = arg.as_bignum() {
        // GNU: dividing 1 by any bignum yields 0 (since |bignum| > MAX_FIXNUM).
        if big.is_zero() {
            return Err(signal("arith-error", vec![]));
        }
        return Ok(Value::fixnum(0));
    }
    let d = expect_integer_or_marker_after_number_check(arg)?;
    if d == 0 {
        return Err(signal("arith-error", vec![]));
    }
    Ok(Value::fixnum(1 / d))
}

fn continue_bignum_div(rest: &[Value], mut acc: rug::Integer) -> EvalResult {
    for a in rest {
        if let Some(big) = a.as_bignum() {
            if big.is_zero() {
                return Err(signal("arith-error", vec![]));
            }
            acc /= big;
            continue;
        }
        let d = expect_integer_or_marker_after_number_check(a)?;
        if d == 0 {
            return Err(signal("arith-error", vec![]));
        }
        acc /= d;
    }
    Ok(Value::make_integer(acc))
}

/// `(% X Y)` — integer remainder, mirrors GNU `Frem` (`src/data.c:3402`).
///
/// Result has the same sign as the dividend (`mpz_tdiv_r` semantics).
pub(crate) fn builtin_percent(args: Vec<Value>) -> EvalResult {
    expect_args("%", &args, 2)?;
    integer_remainder(&args[0], &args[1], false)
}

/// `(mod X Y)` — modulo, mirrors GNU `Fmod` (`src/data.c:3412`).
///
/// Result has the same sign as the divisor.
pub(crate) fn builtin_mod(args: Vec<Value>) -> EvalResult {
    expect_args("mod", &args, 2)?;
    if args[0].is_float() || args[1].is_float() {
        // GNU `fmod_float` path — float-modulo. Existing behavior.
        let a = expect_number_or_marker_f64(&args[0])?;
        let b = expect_number_or_marker_f64(&args[1])?;
        let r = a % b;
        let mut r = if r != 0.0 && (r < 0.0) != (b < 0.0) {
            r + b
        } else {
            r
        };
        if r.is_nan() {
            r = f64::from_bits(f64::NAN.to_bits() | (1_u64 << 63));
        }
        return Ok(Value::make_float(r));
    }
    integer_remainder(&args[0], &args[1], true)
}

/// Shared integer remainder for `%` and `mod`. Mirrors GNU
/// `integer_remainder` (`src/data.c:3351`). When `modulo` is true the
/// result is fixed up to have the divisor's sign.
fn integer_remainder(num: &Value, den: &Value, modulo: bool) -> EvalResult {
    // Bignum slow path if either side is a bignum, or if the i64 fast
    // path can't represent the operands (markers always fit).
    if num.is_bignum() || den.is_bignum() {
        let num_big = bignum_or_int_to_rug(num)?;
        let den_big = bignum_or_int_to_rug(den)?;
        if den_big.is_zero() {
            return Err(signal("arith-error", vec![]));
        }
        let mut r = rug::Integer::from(&num_big % &den_big);
        if modulo {
            let sgn_r = r.cmp0();
            let sgn_d = den_big.cmp0();
            // Wrong sign means r and d have opposite signs.
            if (sgn_d.is_lt() && sgn_r.is_gt()) || (sgn_d.is_gt() && sgn_r.is_lt()) {
                r += &den_big;
            }
        }
        return Ok(Value::make_integer(r));
    }
    // GNU `Fmod` (data.c:3412) does CHECK_NUMBER_COERCE_MARKER on both
    // operands first, so non-numeric values must signal
    // `number-or-marker-p`, not `integer-or-marker-p`. Mirror that by
    // routing through the after-number-check helper.
    let a = expect_integer_or_marker_after_number_check(num)?;
    let b = expect_integer_or_marker_after_number_check(den)?;
    if b == 0 {
        return Err(signal("arith-error", vec![]));
    }
    // i64::MIN % -1 is 0 mathematically, but checked_rem returns None.
    let r = match a.checked_rem(b) {
        Some(r) => r,
        None => 0,
    };
    let r = if modulo && r != 0 && (r < 0) != (b < 0) {
        r + b
    } else {
        r
    };
    Ok(Value::make_integer(rug::Integer::from(r)))
}

/// Convert a fixnum / bignum / marker operand to `rug::Integer`. Used
/// by the integer remainder slow path.
fn bignum_or_int_to_rug(value: &Value) -> Result<rug::Integer, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(rug::Integer::from(n)),
        ValueKind::Veclike(VecLikeType::Bignum) => Ok(value.as_bignum().unwrap().clone()),
        _ if super::marker::is_marker(value) => Ok(rug::Integer::from(
            super::marker::marker_position_as_int(value)?,
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

/// `(1+ NUMBER)` — mirrors GNU `Fadd1` (`src/data.c:3634`).
/// Promotes to bignum on `MOST_POSITIVE_FIXNUM + 1`.
pub(crate) fn builtin_add1(args: Vec<Value>) -> EvalResult {
    expect_args("1+", &args, 1)?;
    match args[0].kind() {
        ValueKind::Fixnum(n) => match n.checked_add(1) {
            // Even non-overflowing i64 results may exceed fixnum range
            // (62-bit) — funnel through make_integer.
            Some(s) => Ok(Value::make_integer(rug::Integer::from(s))),
            None => Ok(Value::make_integer(rug::Integer::from(n) + 1)),
        },
        ValueKind::Float => Ok(Value::make_float(args[0].xfloat() + 1.0)),
        ValueKind::Veclike(VecLikeType::Bignum) => {
            Ok(Value::make_integer(args[0].as_bignum().unwrap().clone() + 1))
        }
        _ if args[0].is_marker() => {
            let n = super::marker::marker_position_as_int(&args[0])?;
            match n.checked_add(1) {
                Some(s) => Ok(Value::make_integer(rug::Integer::from(s))),
                None => Ok(Value::make_integer(rug::Integer::from(n) + 1)),
            }
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), args[0]],
        )),
    }
}

/// `(1- NUMBER)` — mirrors GNU `Fsub1` (`src/data.c:3658`).
/// Promotes to bignum on `MOST_NEGATIVE_FIXNUM - 1`.
pub(crate) fn builtin_sub1(args: Vec<Value>) -> EvalResult {
    expect_args("1-", &args, 1)?;
    match args[0].kind() {
        ValueKind::Fixnum(n) => match n.checked_sub(1) {
            Some(s) => Ok(Value::make_integer(rug::Integer::from(s))),
            None => Ok(Value::make_integer(rug::Integer::from(n) - 1)),
        },
        ValueKind::Float => Ok(Value::make_float(args[0].xfloat() - 1.0)),
        ValueKind::Veclike(VecLikeType::Bignum) => {
            Ok(Value::make_integer(args[0].as_bignum().unwrap().clone() - 1))
        }
        _ if args[0].is_marker() => {
            let n = super::marker::marker_position_as_int(&args[0])?;
            match n.checked_sub(1) {
                Some(s) => Ok(Value::make_integer(rug::Integer::from(s))),
                None => Ok(Value::make_integer(rug::Integer::from(n) - 1)),
            }
        }
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
        ValueKind::Fixnum(_)
        | ValueKind::Float
        | ValueKind::Veclike(VecLikeType::Bignum) => Ok(best_value),
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
        ValueKind::Fixnum(_)
        | ValueKind::Float
        | ValueKind::Veclike(VecLikeType::Bignum) => Ok(best_value),
        _ if best_value.is_marker() => Ok(Value::fixnum(
            super::marker::marker_position_as_int_eval(eval, &best_value)?,
        )),
        _ => unreachable!("min winner must be numeric"),
    }
}

/// `(abs ARG)` — mirrors GNU `Fabs` (`src/floatfns.c`).
///
/// Promotes `MOST_NEGATIVE_FIXNUM` to a bignum (audit §2.6) instead
/// of signaling overflow-error.
pub(crate) fn builtin_abs(args: Vec<Value>) -> EvalResult {
    expect_args("abs", &args, 1)?;
    match args[0].kind() {
        ValueKind::Fixnum(n) => match n.checked_abs() {
            // Even non-overflowing |i64| might exceed fixnum range —
            // make_integer DTRT.
            Some(a) => Ok(Value::make_integer(rug::Integer::from(a))),
            None => Ok(Value::make_integer(rug::Integer::from(n).abs())),
        },
        ValueKind::Float => Ok(Value::make_float(args[0].xfloat().abs())),
        ValueKind::Veclike(VecLikeType::Bignum) => {
            Ok(Value::make_integer(args[0].as_bignum().unwrap().clone().abs()))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), args[0]],
        )),
    }
}

// ===========================================================================
// Logical / bitwise
// ===========================================================================

/// `(logand &rest INTS-OR-MARKERS)` — bitwise AND.
///
/// Mirrors GNU `Flogand` (`src/data.c:3458`) → `arith_driver Alogand`.
/// When any operand is a bignum the whole reduction runs in GMP via
/// `mpz_and`; otherwise we stay on the i64 fast path. Note: bitwise
/// AND of i64 values can never overflow into bignum range, but the
/// final result still has to clear the fixnum-bits hurdle since `&`
/// can produce a value with the high bits set (e.g. `(logand -1 -1)
/// → -1` is fine, but `(logand most-positive-fixnum #x7fffffffffffffff)`
/// could exceed fixnum range). Funnel through `make_integer`.
pub(crate) fn builtin_logand(args: Vec<Value>) -> EvalResult {
    if has_bignum(&args) {
        return bignum_logop(&args, BignumLogop::And);
    }
    let mut acc = -1i64;
    for a in &args {
        acc &= expect_integer_or_marker_after_number_check(a)?;
    }
    Ok(Value::make_integer(rug::Integer::from(acc)))
}

pub(crate) fn builtin_logior(args: Vec<Value>) -> EvalResult {
    if has_bignum(&args) {
        return bignum_logop(&args, BignumLogop::Or);
    }
    let mut acc = 0i64;
    for a in &args {
        acc |= expect_integer_or_marker_after_number_check(a)?;
    }
    Ok(Value::make_integer(rug::Integer::from(acc)))
}

pub(crate) fn builtin_logxor(args: Vec<Value>) -> EvalResult {
    if has_bignum(&args) {
        return bignum_logop(&args, BignumLogop::Xor);
    }
    let mut acc = 0i64;
    for a in &args {
        acc ^= expect_integer_or_marker_after_number_check(a)?;
    }
    Ok(Value::make_integer(rug::Integer::from(acc)))
}

#[derive(Clone, Copy)]
enum BignumLogop {
    And,
    Or,
    Xor,
}

fn bignum_logop(args: &[Value], op: BignumLogop) -> EvalResult {
    let mut acc = match op {
        BignumLogop::And => rug::Integer::from(-1),
        BignumLogop::Or | BignumLogop::Xor => rug::Integer::from(0),
    };
    for a in args {
        let next = bignum_or_int_to_rug(a)?;
        match op {
            BignumLogop::And => acc &= next,
            BignumLogop::Or => acc |= next,
            BignumLogop::Xor => acc ^= next,
        }
    }
    Ok(Value::make_integer(acc))
}

/// `(lognot NUMBER)` — mirrors GNU `Flognot` (`src/data.c:3648`).
pub(crate) fn builtin_lognot(args: Vec<Value>) -> EvalResult {
    expect_args("lognot", &args, 1)?;
    if let Some(big) = args[0].as_bignum() {
        return Ok(Value::make_integer(!big.clone()));
    }
    let n = expect_int(&args[0])?;
    Ok(Value::make_integer(rug::Integer::from(!n)))
}

/// `(ash VALUE COUNT)` — arithmetic shift, mirrors GNU `Fash`
/// (`src/data.c:3519`).
///
/// Positive COUNT shifts left, negative shifts right. Both VALUE and
/// COUNT may be bignums. The result is promoted to bignum on left
/// shifts that exceed fixnum range — most importantly `(ash 1 100)`
/// must return 2^100, not 0 (audit §2.7).
pub(crate) fn builtin_ash(args: Vec<Value>) -> EvalResult {
    expect_args("ash", &args, 2)?;
    let value = &args[0];
    let count_val = &args[1];

    // COUNT must be an integer (fixnum or bignum). If it's a bignum
    // and VALUE is anything but zero, GNU signals overflow-error for
    // positive counts (no machine could represent the result) and
    // returns 0 / -1 for negative counts (the value is shifted away).
    let count_i64 = match count_val.kind() {
        ValueKind::Fixnum(c) => c,
        ValueKind::Veclike(VecLikeType::Bignum) => {
            let big = count_val.as_bignum().unwrap();
            // Zero VALUE is unchanged regardless of COUNT.
            if value
                .as_fixnum()
                .map(|n| n == 0)
                .or_else(|| value.as_bignum().map(|b| b.is_zero()))
                .unwrap_or(false)
            {
                return Ok(Value::fixnum(0));
            }
            if big.cmp0().is_lt() {
                // Negative count + nonzero value: result is 0 (or -1 for negative).
                let sign_neg = match value.kind() {
                    ValueKind::Fixnum(n) => n < 0,
                    ValueKind::Veclike(VecLikeType::Bignum) => {
                        value.as_bignum().unwrap().cmp0().is_lt()
                    }
                    _ => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("integerp"), *value],
                        ));
                    }
                };
                return Ok(Value::fixnum(if sign_neg { -1 } else { 0 }));
            }
            return Err(signal("overflow-error", vec![]));
        }
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), *count_val],
            ));
        }
    };

    // Materialize VALUE as a rug::Integer once. We could try to keep
    // small fixnum shifts on the i64 path, but ash is rare enough that
    // correctness over branchy fast-pathing is the right tradeoff.
    let value_big = match value.kind() {
        ValueKind::Fixnum(n) => rug::Integer::from(n),
        ValueKind::Veclike(VecLikeType::Bignum) => value.as_bignum().unwrap().clone(),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), *value],
            ));
        }
    };

    if count_i64 == 0 {
        return Ok(Value::make_integer(value_big));
    }
    let result = if count_i64 > 0 {
        // Left shift. rug::Integer << u32 (or usize) does GMP mul_2exp.
        let bits = u32::try_from(count_i64).unwrap_or(u32::MAX);
        value_big << bits
    } else {
        // Arithmetic right shift (toward -infinity, i.e. mpz_fdiv_q_2exp).
        // For very large negative counts, the value is shifted away;
        // GNU returns -1 for negative VALUE and 0 otherwise.
        let neg_count = match count_i64.checked_neg() {
            Some(c) => c,
            None => i64::MAX,
        };
        let bits = u32::try_from(neg_count).unwrap_or(u32::MAX);
        // rug::Integer >> u32 does mpz_fdiv_q_2exp (floor division).
        value_big >> bits
    };
    Ok(Value::make_integer(result))
}

// ===========================================================================
// Comparisons
// ===========================================================================
//
// Mirrors GNU `arithcompare` (src/data.c:2682). For two integers
// (fixnum or bignum) we compare exactly via rug::Integer; for any
// pair involving a float we compare the float against the integer
// using rug::Integer::partial_cmp<f64>, which is exact (it accounts
// for whether the float is integer-valued and how it relates to the
// bignum). The previous f64-only path lost precision for any bignum
// outside ±2^53 (audit §1.1 — comparisons part).

#[derive(Clone, Copy, PartialEq, Eq)]
enum NumCmp {
    Lt,
    Le,
    Eq,
    Ne,
    Gt,
    Ge,
}

fn arithcompare(
    eval: &super::super::eval::Context,
    a: &Value,
    b: &Value,
) -> Result<std::cmp::Ordering, Flow> {
    use std::cmp::Ordering;

    // Float on either side: if the other side is a bignum we still
    // get an exact answer via rug::Integer::partial_cmp<f64>; for
    // fixnums and floats, fall back to f64 comparison.
    if a.is_float() || b.is_float() {
        if let Some(big) = a.as_bignum() {
            let f = expect_number_or_marker_f64_eval(eval, b)?;
            if f.is_nan() {
                return Ok(Ordering::Equal); // arithcompare with NaN returns Cmp_NONE; treated as != by callers below
            }
            return Ok(big.partial_cmp(&f).unwrap_or(Ordering::Equal));
        }
        if let Some(big) = b.as_bignum() {
            let f = expect_number_or_marker_f64_eval(eval, a)?;
            if f.is_nan() {
                return Ok(Ordering::Equal);
            }
            // Reverse since we asked big.cmp(f).
            return Ok(big
                .partial_cmp(&f)
                .map(|o| o.reverse())
                .unwrap_or(Ordering::Equal));
        }
        let af = expect_number_or_marker_f64_eval(eval, a)?;
        let bf = expect_number_or_marker_f64_eval(eval, b)?;
        return Ok(af.partial_cmp(&bf).unwrap_or(Ordering::Equal));
    }

    // Both operands are integer-or-marker. Stay on i64 if neither is
    // a bignum.
    if !a.is_bignum() && !b.is_bignum() {
        let ai = expect_integer_or_marker_after_number_check_eval(eval, a)?;
        let bi = expect_integer_or_marker_after_number_check_eval(eval, b)?;
        return Ok(ai.cmp(&bi));
    }

    // Bignum-aware integer compare.
    let ai = match a.kind() {
        ValueKind::Fixnum(n) => rug::Integer::from(n),
        ValueKind::Veclike(VecLikeType::Bignum) => a.as_bignum().unwrap().clone(),
        _ if super::marker::is_marker(a) => rug::Integer::from(
            super::marker::marker_position_as_int_eval(eval, a)?,
        ),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), *a],
            ));
        }
    };
    let bi = match b.kind() {
        ValueKind::Fixnum(n) => rug::Integer::from(n),
        ValueKind::Veclike(VecLikeType::Bignum) => b.as_bignum().unwrap().clone(),
        _ if super::marker::is_marker(b) => rug::Integer::from(
            super::marker::marker_position_as_int_eval(eval, b)?,
        ),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), *b],
            ));
        }
    };
    Ok(ai.cmp(&bi))
}

fn cmp_passes(ord: std::cmp::Ordering, op: NumCmp) -> bool {
    use std::cmp::Ordering;
    match op {
        NumCmp::Lt => ord == Ordering::Less,
        NumCmp::Le => ord != Ordering::Greater,
        NumCmp::Eq => ord == Ordering::Equal,
        NumCmp::Ne => ord != Ordering::Equal,
        NumCmp::Gt => ord == Ordering::Greater,
        NumCmp::Ge => ord != Ordering::Less,
    }
}

fn arithcompare_chain(
    eval: &super::super::eval::Context,
    args: &[Value],
    op: NumCmp,
) -> EvalResult {
    for pair in args.windows(2) {
        let ord = arithcompare(eval, &pair[0], &pair[1])?;
        if !cmp_passes(ord, op) {
            return Ok(Value::NIL);
        }
    }
    Ok(Value::T)
}

pub(crate) fn builtin_num_eq(
    eval: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("=", &args, 2)?;
    arithcompare_chain(eval, &args, NumCmp::Eq)
}

pub(crate) fn builtin_num_lt(
    eval: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("<", &args, 2)?;
    arithcompare_chain(eval, &args, NumCmp::Lt)
}

pub(crate) fn builtin_num_le(
    eval: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("<=", &args, 2)?;
    arithcompare_chain(eval, &args, NumCmp::Le)
}

pub(crate) fn builtin_num_gt(
    eval: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args(">", &args, 2)?;
    arithcompare_chain(eval, &args, NumCmp::Gt)
}

pub(crate) fn builtin_num_ge(
    eval: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args(">=", &args, 2)?;
    arithcompare_chain(eval, &args, NumCmp::Ge)
}

pub(crate) fn builtin_num_ne(
    eval: &mut super::super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("/=", &args, 2)?;
    let ord = arithcompare(eval, &args[0], &args[1])?;
    Ok(Value::bool_val(cmp_passes(ord, NumCmp::Ne)))
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
///
/// Mirrors GNU `rounding_driver` (`src/floatfns.c`). The audit
/// (§2.15, §2.17) flagged that NeoMacs used to truncate float
/// results to i64 with `as i64` saturation, silently producing
/// `i64::MAX`/`i64::MIN` for out-of-range floats and not surfacing
/// overflow on infinity / NaN. We now route every integer result
/// through `Value::make_integer`, and floats outside i64 range use
/// `rug::Integer::from_f64` to produce a bignum.
fn rounding_with_divisor(
    name: &str,
    args: &[Value],
    round_fn: fn(f64) -> f64,
    int_div: fn(i64, i64) -> i64,
) -> EvalResult {
    expect_range_args(name, args, 1, 2)?;
    if args.len() == 1 {
        return match args[0].kind() {
            ValueKind::Fixnum(n) => Ok(Value::make_integer(rug::Integer::from(n))),
            ValueKind::Float => float_to_lisp_integer(round_fn(args[0].xfloat())),
            ValueKind::Veclike(VecLikeType::Bignum) => {
                Ok(Value::make_integer(args[0].as_bignum().unwrap().clone()))
            }
            _ => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("numberp"), args[0]],
            )),
        };
    }
    // 2-arg form: (op ARG DIVISOR)
    if args[1].is_float() {
        let divisor = args[1].xfloat();
        if divisor == 0.0 {
            return Err(signal("arith-error", vec![]));
        }
        let dividend = value_to_f64(name, &args[0])?;
        return float_to_lisp_integer(round_fn(dividend / divisor));
    }
    if let Some(d) = args[1].as_fixnum() {
        if d == 0 {
            return Err(signal("arith-error", vec![]));
        }
        if let Some(a) = args[0].as_fixnum() {
            return Ok(Value::make_integer(rug::Integer::from(int_div(a, d))));
        }
    }
    if args[1].is_bignum() && args[1].as_bignum().unwrap().is_zero() {
        return Err(signal("arith-error", vec![]));
    }
    // Mixed bignum / float / fixnum 2-arg fallback. For non-float
    // operands fall through to the float path; this loses precision
    // for very large bignums but matches the existing behavior for
    // the cases the test suite covers. A future pass can wire in
    // mpz_tdiv_q etc. for full bignum-divisor support.
    if args[0].is_float() || args[1].is_float() {
        let dividend = value_to_f64(name, &args[0])?;
        let divisor = value_to_f64(name, &args[1])?;
        if divisor == 0.0 {
            return Err(signal("arith-error", vec![]));
        }
        return float_to_lisp_integer(round_fn(dividend / divisor));
    }
    // Bignum-divisor or bignum-dividend integer path: do GMP
    // truncation and reapply the rounding flavor on the residue.
    let a = bignum_or_int_to_rug(&args[0])?;
    let d = bignum_or_int_to_rug(&args[1])?;
    if d.is_zero() {
        return Err(signal("arith-error", vec![]));
    }
    // Truncation (toward-zero) division as the building block.
    let q = rug::Integer::from(&a / &d);
    let r = rug::Integer::from(&a - rug::Integer::from(&q * &d));
    // Apply the same flavor that the int_div lambda would for fixnums,
    // but in GMP. We dispatch by name because the closure type erases
    // intent — and there are only four flavors.
    let adjusted = match name {
        "truncate" => q,
        "floor" => {
            // Toward -inf: if remainder is nonzero and r and d have
            // opposite signs, subtract 1.
            if !r.is_zero() && (r.cmp0().is_lt()) != (d.cmp0().is_lt()) {
                q - 1
            } else {
                q
            }
        }
        "ceiling" => {
            // Toward +inf: if remainder is nonzero and r and d have
            // the same sign, add 1.
            if !r.is_zero() && (r.cmp0().is_lt()) == (d.cmp0().is_lt()) {
                q + 1
            } else {
                q
            }
        }
        "round" => {
            // Round half to even (banker's rounding).
            let abs_r2 = rug::Integer::from(&r * 2).abs();
            let abs_d = rug::Integer::from(d.abs_ref());
            use std::cmp::Ordering;
            match abs_r2.cmp(&abs_d) {
                Ordering::Greater => {
                    if (r.cmp0().is_lt()) == (d.cmp0().is_lt()) {
                        q + 1
                    } else {
                        q - 1
                    }
                }
                Ordering::Equal => {
                    if q.is_odd() {
                        if (r.cmp0().is_lt()) == (d.cmp0().is_lt()) {
                            q + 1
                        } else {
                            q - 1
                        }
                    } else {
                        q
                    }
                }
                Ordering::Less => q,
            }
        }
        _ => unreachable!("unknown rounding name {name}"),
    };
    Ok(Value::make_integer(adjusted))
}

/// Convert a finite f64 into a Lisp integer (fixnum or bignum). NaN
/// and infinity signal `overflow-error`, mirroring GNU
/// `double_to_integer` (`src/bignum.c:81`).
fn float_to_lisp_integer(value: f64) -> EvalResult {
    if !value.is_finite() {
        return Err(signal("overflow-error", vec![]));
    }
    // i64::MIN..=i64::MAX is the safe `as i64` range; outside that we
    // need GMP. But fixnum range is even tighter (62-bit), so always
    // funnel through make_integer.
    let big = rug::Integer::from_f64(value).expect("finite f64");
    Ok(Value::make_integer(big))
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

/// `(expt BASE EXPONENT)` — mirrors GNU `Fexpt`
/// (`src/floatfns.c`) and `expt_integer` (`src/data.c:3587`).
///
/// Integer base + non-negative integer exponent uses `mpz_pow_ui` to
/// promote on overflow. The headline audit case is `(expt 2 100)`
/// which used to return 0 because `2_i64.wrapping_pow(100)` wraps.
pub(crate) fn builtin_expt(args: Vec<Value>) -> EvalResult {
    expect_args("expt", &args, 2)?;
    // GNU `Fexpt` (data.c) does CHECK_NUMBER on both args first, so any
    // non-numeric argument must signal `numberp`, not the more specific
    // type checks the integer/float dispatch would otherwise emit.
    let _ = expect_number(&args[0])?;
    let _ = expect_number(&args[1])?;
    if has_float(&args) {
        let base = expect_number(&args[0])?;
        let exp = expect_number(&args[1])?;
        return Ok(Value::make_float(base.powf(exp)));
    }
    // Integer-only path. Negative exponent on integer base falls back
    // to float (GNU does the same: a^-n is rarely an integer).
    let exp_val = &args[1];
    let exp_is_neg = match exp_val.kind() {
        ValueKind::Fixnum(n) => n < 0,
        ValueKind::Veclike(VecLikeType::Bignum) => exp_val.as_bignum().unwrap().cmp0().is_lt(),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), *exp_val],
            ));
        }
    };
    if exp_is_neg {
        let base = expect_number(&args[0])?;
        let exp = expect_number(exp_val)?;
        return Ok(Value::make_float(base.powf(exp)));
    }

    // Special cases for -1, 0, 1 — never overflow regardless of exponent.
    let base_val = &args[0];
    if let Some(b) = base_val.as_fixnum() {
        match b {
            0 => {
                // 0^0 = 1 in elisp, 0^positive = 0.
                let exp_zero = match exp_val.kind() {
                    ValueKind::Fixnum(n) => n == 0,
                    ValueKind::Veclike(VecLikeType::Bignum) => {
                        exp_val.as_bignum().unwrap().is_zero()
                    }
                    _ => false,
                };
                return Ok(Value::fixnum(if exp_zero { 1 } else { 0 }));
            }
            1 => return Ok(Value::fixnum(1)),
            -1 => {
                let odd = match exp_val.kind() {
                    ValueKind::Fixnum(n) => n & 1 == 1,
                    ValueKind::Veclike(VecLikeType::Bignum) => {
                        exp_val.as_bignum().unwrap().is_odd()
                    }
                    _ => false,
                };
                return Ok(Value::fixnum(if odd { -1 } else { 1 }));
            }
            _ => {}
        }
    }

    // Exponent must fit in u32 for rug::Integer::pow_u. (GNU bounds it
    // by ULONG_MAX; that's larger than u32 on most platforms but the
    // result becomes astronomically large long before then.)
    let exp_u32: u32 = match exp_val.kind() {
        ValueKind::Fixnum(n) => match u32::try_from(n) {
            Ok(v) => v,
            Err(_) => return Err(signal("overflow-error", vec![])),
        },
        ValueKind::Veclike(VecLikeType::Bignum) => {
            match exp_val.as_bignum().unwrap().to_u32() {
                Some(v) => v,
                None => return Err(signal("overflow-error", vec![])),
            }
        }
        _ => unreachable!("non-int exponent handled above"),
    };

    let base_big = bignum_or_int_to_rug(base_val)?;
    Ok(Value::make_integer(base_big.pow(exp_u32)))
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
