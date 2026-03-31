use super::*;
use crate::emacs_core::regex::char_pos_to_byte;
use crate::emacs_core::value::{ValueKind, VecLikeType};

// ===========================================================================
// Search / Regex builtins (evaluator-dependent)
// ===========================================================================

pub(crate) fn builtin_search_forward(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_search_forward_with_state(case_fold, &mut eval.buffers, &mut eval.match_data, &args)
}

pub(crate) fn builtin_search_forward_with_state(
    case_fold: bool,
    buffers: &mut crate::buffer::BufferManager,
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    expect_range_args("search-forward", args, 1, 4)?;
    let pattern = expect_string(&args[0])?;
    let (current_id, opts, start_pt, start_char) =
        current_search_context_in_manager(buffers, args, SearchKind::ForwardLiteral)?;
    if opts.steps == 0 {
        return Ok(Value::fixnum(start_char));
    }

    let mut last_pos = None;
    for _ in 0..opts.steps {
        let result = {
            let buf = buffers
                .get_mut(current_id)
                .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
            match opts.direction {
                SearchDirection::Forward => super::regex::search_forward(
                    buf, &pattern, opts.bound, false, case_fold, match_data,
                ),
                SearchDirection::Backward => super::regex::search_backward(
                    buf, &pattern, opts.bound, false, case_fold, match_data,
                ),
            }
        };
        match result {
            Ok(Some(pos)) => last_pos = Some(pos),
            Ok(None) => {
                // regex::search_* with `noerror = false` never returns None.
                return Err(signal("search-failed", vec![Value::string(pattern)]));
            }
            Err(_) => {
                return handle_search_failure_in_manager(
                    buffers,
                    current_id,
                    &pattern,
                    opts,
                    start_pt,
                    SearchErrorKind::NotFound,
                );
            }
        }
    }

    let end = last_pos.expect("search loop should produce at least one match");
    buffer_byte_to_char_result_in_manager(buffers, current_id, end)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchDirection {
    Forward,
    Backward,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchNoErrorMode {
    Signal,
    KeepPoint,
    MoveToBound,
}

#[derive(Clone, Copy)]
enum SearchKind {
    ForwardLiteral,
    BackwardLiteral,
    ForwardRegexp,
    BackwardRegexp,
}

#[derive(Clone, Copy)]
enum SearchErrorKind {
    NotFound,
}

#[derive(Clone, Copy)]
struct SearchOptions {
    bound: Option<usize>,
    direction: SearchDirection,
    noerror_mode: SearchNoErrorMode,
    steps: usize,
}

fn search_count_arg(args: &[Value]) -> Result<i64, Flow> {
    match args.get(3) {
        None | Some(ValueKind::Nil) => Ok(1),
        Some(ValueKind::Fixnum(n)) => Ok(*n),
        Some(ValueKind::Char(c)) => Ok(*c as i64),
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), *other],
        )),
    }
}

fn search_bound_to_byte_in_manager(
    buffers: &crate::buffer::BufferManager,
    buf: &crate::buffer::Buffer,
    value: &Value,
) -> Result<usize, Flow> {
    let pos = super::buffers::expect_integer_or_marker_in_buffers(buffers, value)?;
    Ok(buf.lisp_pos_to_accessible_byte(pos))
}

fn parse_search_options_in_manager(
    buffers: &crate::buffer::BufferManager,
    buf: &crate::buffer::Buffer,
    args: &[Value],
    kind: SearchKind,
) -> Result<SearchOptions, Flow> {
    let count = search_count_arg(args)?;
    let noerror_mode = match args.get(2) {
        None | Some(ValueKind::Nil) => SearchNoErrorMode::Signal,
        Some(ValueKind::T) => SearchNoErrorMode::KeepPoint,
        Some(_) => SearchNoErrorMode::MoveToBound,
    };
    let (bound_lisp, bound) = match args.get(1) {
        Some(v) if !v.is_nil() => {
            let raw = super::buffers::expect_integer_or_marker_in_buffers(buffers, v)?;
            let byte = search_bound_to_byte_in_manager(buffers, buf, v)?;
            (Some(raw), Some(byte))
        }
        _ => (None, None),
    };

    let direction = match kind {
        SearchKind::ForwardLiteral | SearchKind::ForwardRegexp => {
            if count > 0 {
                SearchDirection::Forward
            } else {
                SearchDirection::Backward
            }
        }
        SearchKind::BackwardLiteral | SearchKind::BackwardRegexp => {
            if count < 0 {
                SearchDirection::Forward
            } else {
                SearchDirection::Backward
            }
        }
    };
    let steps = count.unsigned_abs() as usize;

    if let Some(limit) = bound_lisp {
        let point_lisp = buf.text.byte_to_char(buf.pt) as i64 + 1;
        match direction {
            SearchDirection::Forward if limit < point_lisp => {
                return Err(signal(
                    "error",
                    vec![Value::string("Invalid search bound (wrong side of point)")],
                ));
            }
            SearchDirection::Backward if limit > point_lisp => {
                return Err(signal(
                    "error",
                    vec![Value::string("Invalid search bound (wrong side of point)")],
                ));
            }
            _ => {}
        }
    }

    Ok(SearchOptions {
        bound,
        direction,
        noerror_mode,
        steps,
    })
}

fn current_search_context_in_manager(
    buffers: &crate::buffer::BufferManager,
    args: &[Value],
    kind: SearchKind,
) -> Result<(crate::buffer::BufferId, SearchOptions, usize, i64), Flow> {
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let opts = parse_search_options_in_manager(buffers, buf, args, kind)?;
    let start_pt = buf.pt;
    let start_char = buf.text.byte_to_char(buf.pt) as i64 + 1;
    Ok((current_id, opts, start_pt, start_char))
}

fn buffer_byte_to_char_result_in_manager(
    buffers: &crate::buffer::BufferManager,
    buffer_id: crate::buffer::BufferId,
    byte: usize,
) -> EvalResult {
    let buf = buffers
        .get(buffer_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(Value::fixnum(buf.text.byte_to_char(byte) as i64 + 1))
}

fn search_failure_position(buf: &crate::buffer::Buffer, opts: SearchOptions) -> usize {
    match opts.bound {
        Some(limit) => limit.clamp(buf.begv, buf.zv),
        None => match opts.direction {
            SearchDirection::Forward => buf.zv,
            SearchDirection::Backward => buf.begv,
        },
    }
}

fn handle_search_failure_in_manager(
    buffers: &mut crate::buffer::BufferManager,
    buffer_id: crate::buffer::BufferId,
    pattern: &str,
    opts: SearchOptions,
    start_pt: usize,
    kind: SearchErrorKind,
) -> EvalResult {
    match kind.kind() {
        SearchErrorKind::NotFound => match opts.noerror_mode {
            SearchNoErrorMode::Signal => {
                let _ = buffers.goto_buffer_byte(buffer_id, start_pt);
                Err(signal("search-failed", vec![Value::string(pattern)]))
            }
            SearchNoErrorMode::KeepPoint => {
                let _ = buffers.goto_buffer_byte(buffer_id, start_pt);
                Ok(ValueKind::Nil)
            }
            SearchNoErrorMode::MoveToBound => {
                let target = buffers
                    .get(buffer_id)
                    .map(|buf| search_failure_position(buf, opts))
                    .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
                let _ = buffers.goto_buffer_byte(buffer_id, target);
                Ok(ValueKind::Nil)
            }
        },
    }
}

pub(crate) fn builtin_search_backward(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_search_backward_with_state(case_fold, &mut eval.buffers, &mut eval.match_data, &args)
}

pub(crate) fn builtin_search_backward_with_state(
    case_fold: bool,
    buffers: &mut crate::buffer::BufferManager,
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    expect_range_args("search-backward", args, 1, 4)?;
    let pattern = expect_string(&args[0])?;
    let (current_id, opts, start_pt, start_char) =
        current_search_context_in_manager(buffers, args, SearchKind::BackwardLiteral)?;
    if opts.steps == 0 {
        return Ok(Value::fixnum(start_char));
    }

    let mut last_pos = None;
    for _ in 0..opts.steps {
        let result = {
            let buf = buffers
                .get_mut(current_id)
                .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
            match opts.direction {
                SearchDirection::Forward => super::regex::search_forward(
                    buf, &pattern, opts.bound, false, case_fold, match_data,
                ),
                SearchDirection::Backward => super::regex::search_backward(
                    buf, &pattern, opts.bound, false, case_fold, match_data,
                ),
            }
        };
        match result {
            Ok(Some(pos)) => last_pos = Some(pos),
            Ok(None) => {
                return Err(signal("search-failed", vec![Value::string(pattern)]));
            }
            Err(_) => {
                return handle_search_failure_in_manager(
                    buffers,
                    current_id,
                    &pattern,
                    opts,
                    start_pt,
                    SearchErrorKind::NotFound,
                );
            }
        }
    }

    let end = last_pos.expect("search loop should produce at least one match");
    buffer_byte_to_char_result_in_manager(buffers, current_id, end)
}

pub(crate) fn builtin_re_search_forward(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_re_search_forward_with_state(case_fold, &mut eval.buffers, &mut eval.match_data, &args)
}

pub(crate) fn builtin_re_search_forward_with_state(
    case_fold: bool,
    buffers: &mut crate::buffer::BufferManager,
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    expect_range_args("re-search-forward", args, 1, 4)?;
    let pattern = expect_string(&args[0])?;
    let (current_id, opts, start_pt, start_char) =
        current_search_context_in_manager(buffers, args, SearchKind::ForwardRegexp)?;
    if opts.steps == 0 {
        return Ok(Value::fixnum(start_char));
    }

    let mut last_pos = None;
    for _ in 0..opts.steps {
        let result = {
            let buf = buffers
                .get_mut(current_id)
                .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
            match opts.direction {
                SearchDirection::Forward => super::regex::re_search_forward(
                    buf, &pattern, opts.bound, false, case_fold, match_data,
                ),
                SearchDirection::Backward => super::regex::re_search_backward(
                    buf, &pattern, opts.bound, false, case_fold, match_data,
                ),
            }
        };

        match result {
            Ok(Some(pos)) => last_pos = Some(pos),
            Ok(None) => {
                return Err(signal("search-failed", vec![Value::string(pattern)]));
            }
            Err(msg) if msg.starts_with("Invalid regexp:") => {
                let _ = buffers.goto_buffer_byte(current_id, start_pt);
                return Err(signal("invalid-regexp", vec![Value::string(msg)]));
            }
            Err(_) => {
                return handle_search_failure_in_manager(
                    buffers,
                    current_id,
                    &pattern,
                    opts,
                    start_pt,
                    SearchErrorKind::NotFound,
                );
            }
        }
    }

    let end = last_pos.expect("search loop should produce at least one match");
    buffer_byte_to_char_result_in_manager(buffers, current_id, end)
}

pub(crate) fn builtin_re_search_backward(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_re_search_backward_with_state(case_fold, &mut eval.buffers, &mut eval.match_data, &args)
}

pub(crate) fn builtin_re_search_backward_with_state(
    case_fold: bool,
    buffers: &mut crate::buffer::BufferManager,
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    expect_range_args("re-search-backward", args, 1, 4)?;
    let pattern = expect_string(&args[0])?;
    let (current_id, opts, start_pt, start_char) =
        current_search_context_in_manager(buffers, args, SearchKind::BackwardRegexp)?;
    if opts.steps == 0 {
        return Ok(Value::fixnum(start_char));
    }

    let mut last_pos = None;
    for _ in 0..opts.steps {
        let result = {
            let buf = buffers
                .get_mut(current_id)
                .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
            match opts.direction {
                SearchDirection::Forward => super::regex::re_search_forward(
                    buf, &pattern, opts.bound, false, case_fold, match_data,
                ),
                SearchDirection::Backward => super::regex::re_search_backward(
                    buf, &pattern, opts.bound, false, case_fold, match_data,
                ),
            }
        };

        match result {
            Ok(Some(pos)) => last_pos = Some(pos),
            Ok(None) => {
                return Err(signal("search-failed", vec![Value::string(pattern)]));
            }
            Err(msg) if msg.starts_with("Invalid regexp:") => {
                let _ = buffers.goto_buffer_byte(current_id, start_pt);
                return Err(signal("invalid-regexp", vec![Value::string(msg)]));
            }
            Err(_) => {
                return handle_search_failure_in_manager(
                    buffers,
                    current_id,
                    &pattern,
                    opts,
                    start_pt,
                    SearchErrorKind::NotFound,
                );
            }
        }
    }

    let end = last_pos.expect("search loop should produce at least one match");
    buffer_byte_to_char_result_in_manager(buffers, current_id, end)
}

pub(crate) fn builtin_search_forward_regexp(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_search_forward_regexp_with_state(
        case_fold,
        &mut eval.buffers,
        &mut eval.match_data,
        &args,
    )
}

pub(crate) fn builtin_search_forward_regexp_with_state(
    case_fold: bool,
    buffers: &mut crate::buffer::BufferManager,
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    expect_range_args("search-forward-regexp", args, 1, 4)?;
    builtin_re_search_forward_with_state(case_fold, buffers, match_data, args)
}

pub(crate) fn builtin_search_backward_regexp(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_search_backward_regexp_with_state(
        case_fold,
        &mut eval.buffers,
        &mut eval.match_data,
        &args,
    )
}

pub(crate) fn builtin_search_backward_regexp_with_state(
    case_fold: bool,
    buffers: &mut crate::buffer::BufferManager,
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    expect_range_args("search-backward-regexp", args, 1, 4)?;
    builtin_re_search_backward_with_state(case_fold, buffers, match_data, args)
}

pub(crate) fn builtin_looking_at(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_looking_at_with_state(case_fold, &eval.buffers, &mut eval.match_data, &args)
}

pub(crate) fn builtin_looking_at_with_state(
    case_fold: bool,
    buffers: &crate::buffer::BufferManager,
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    expect_range_args("looking-at", args, 1, 2)?;
    let pattern = expect_string(&args[0])?;
    let inhibit_modify = args.get(1).is_some_and(|arg| !arg.is_nil());

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let result = if inhibit_modify {
        let mut preserved_match_data = match_data.clone();
        super::regex::looking_at(buf, &pattern, case_fold, &mut preserved_match_data)
    } else {
        super::regex::looking_at(buf, &pattern, case_fold, match_data)
    };

    match result {
        Ok(matched) => Ok(Value::bool_val(matched)),
        Err(msg) => Err(signal("invalid-regexp", vec![Value::string(msg)])),
    }
}

pub(crate) fn builtin_looking_at_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_looking_at_p_with_state(case_fold, &eval.buffers, &args)
}

pub(crate) fn builtin_looking_at_p_with_state(
    case_fold: bool,
    buffers: &crate::buffer::BufferManager,
    args: &[Value],
) -> EvalResult {
    expect_args("looking-at-p", args, 1)?;
    let pattern = expect_string(&args[0])?;

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let mut throwaway_match_data = None;
    match super::regex::looking_at(buf, &pattern, case_fold, &mut throwaway_match_data) {
        Ok(matched) => Ok(Value::bool_val(matched)),
        Err(msg) => Err(signal("invalid-regexp", vec![Value::string(msg)])),
    }
}

pub(crate) fn builtin_posix_looking_at(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_posix_looking_at_with_state(case_fold, &eval.buffers, &mut eval.match_data, &args)
}

pub(crate) fn builtin_posix_looking_at_with_state(
    case_fold: bool,
    buffers: &crate::buffer::BufferManager,
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    expect_range_args("posix-looking-at", args, 1, 2)?;
    builtin_looking_at_with_state(case_fold, buffers, match_data, args)
}

pub(crate) fn builtin_string_match_with_state(
    case_fold: bool,
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::StringMatch,
        || {
            expect_range_args("string-match", args, 2, 4)?;
            let inhibit_modify = args.get(3).is_some_and(|v| v.is_truthy());

            match (args[0].kind(), args[1].kind()) {
                (ValueKind::String, ValueKind::String) => with_heap(|h| {
                    let pattern = h.get_string(*pattern_id);
                    let string = h.get_lisp_string(*string_id);
                    let start = crate::emacs_core::search::normalize_lisp_string_start_arg(
                        string,
                        args.get(2),
                    )?;
                    let mut throwaway = None;
                    let target = if inhibit_modify {
                        &mut throwaway
                    } else {
                        match_data
                    };
                    match super::regex::string_match_full_with_case_fold_source_lisp(
                        pattern,
                        string,
                        super::regex::SearchedString::Heap(*string_id),
                        start,
                        case_fold,
                        target,
                    ) {
                        Ok(Some(char_pos)) => Ok(Value::fixnum(char_pos as i64)),
                        Ok(None) => Ok(Value::NIL),
                        Err(msg) => Err(signal("invalid-regexp", vec![Value::string(msg)])),
                    }
                }),
                _ => {
                    let pattern = expect_string(&args[0])?;
                    let s = expect_string(&args[1])?;
                    let start = normalize_string_start_arg(&s, args.get(2))?;
                    let mut throwaway = None;
                    let target = if inhibit_modify {
                        &mut throwaway
                    } else {
                        match_data
                    };
                    match super::regex::string_match_full_with_case_fold(
                        &pattern, &s, start, case_fold, target,
                    ) {
                        Ok(Some(char_pos)) => Ok(Value::fixnum(char_pos as i64)),
                        Ok(None) => Ok(Value::NIL),
                        Err(msg) => Err(signal("invalid-regexp", vec![Value::string(msg)])),
                    }
                }
            }
        },
    )
}

/// Context-dependent `string-match`: updates match data on the evaluator.
pub(crate) fn builtin_string_match(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_string_match_with_state(case_fold, &mut eval.match_data, &args)
}

pub(crate) fn builtin_posix_string_match_with_state(
    case_fold: bool,
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    expect_range_args("posix-string-match", args, 2, 4)?;
    builtin_string_match_with_state(case_fold, match_data, args)
}

pub(crate) fn builtin_string_match_p_with_case_fold(case_fold: bool, args: &[Value]) -> EvalResult {
    expect_range_args("string-match-p", args, 2, 3)?;
    match (args[0].kind(), args[1].kind()) {
        (ValueKind::String, ValueKind::String) => with_heap(|h| {
            let pattern = h.get_string(*pattern_id);
            let string = h.get_lisp_string(*string_id);
            let start =
                crate::emacs_core::search::normalize_lisp_string_start_arg(string, args.get(2))?;
            let mut throwaway = None;
            match super::regex::string_match_full_with_case_fold_source_lisp(
                pattern,
                string,
                super::regex::SearchedString::Heap(*string_id),
                start,
                case_fold,
                &mut throwaway,
            ) {
                Ok(Some(char_pos)) => Ok(Value::fixnum(char_pos as i64)),
                Ok(None) => Ok(Value::NIL),
                Err(msg) => Err(signal("invalid-regexp", vec![Value::string(msg)])),
            }
        }),
        _ => {
            let pattern = expect_string(&args[0])?;
            let s = expect_string(&args[1])?;
            let start = normalize_string_start_arg(&s, args.get(2))?;
            let mut throwaway = None;

            match super::regex::string_match_full_with_case_fold(
                &pattern,
                &s,
                start,
                case_fold,
                &mut throwaway,
            ) {
                Ok(Some(char_pos)) => Ok(Value::fixnum(char_pos as i64)),
                Ok(None) => Ok(Value::NIL),
                Err(msg) => Err(signal("invalid-regexp", vec![Value::string(msg)])),
            }
        }
    }
}

pub(crate) fn builtin_string_match_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_string_match_p_with_case_fold(case_fold, &args)
}

pub(crate) fn builtin_posix_string_match(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_posix_string_match_with_state(case_fold, &mut eval.match_data, &args)
}

pub(crate) fn builtin_match_string(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("match-string", &args, 1, 2)?;
    let group = expect_int(&args[0])?;
    if group < 0 {
        return Err(signal(
            "args-out-of-range",
            vec![Value::fixnum(group), Value::fixnum(0)],
        ));
    }
    let group = group as usize;

    let md = match &eval.match_data {
        Some(md) => md,
        None => return Ok(Value::NIL),
    };

    let (start, end) = match md.groups.get(group) {
        Some(Some(pair)) => *pair,
        _ => return Ok(Value::NIL),
    };

    // If an optional second arg is a string, use that first.
    if args.len() > 1 {
        if args[1].is_string() {
            return with_heap(|h| {
                let string = h.get_lisp_string(id);
                let text = string.as_str();
                let (byte_start, byte_end) = if md.searched_string.is_some() {
                    (char_pos_to_byte(text, start), char_pos_to_byte(text, end))
                } else {
                    (start, end)
                };
                if byte_end <= text.len() && byte_start <= byte_end {
                    if let Some(slice) = string.slice(byte_start, byte_end) {
                        return Ok(Value::heap_string(slice));
                    }
                }
                Ok(Value::NIL)
            });
        }

        if let Some(s) = args[1].as_str() {
            let (byte_start, byte_end) = if md.searched_string.is_some() {
                (char_pos_to_byte(s, start), char_pos_to_byte(s, end))
            } else {
                (start, end)
            };
            if byte_end <= s.len() && byte_start <= byte_end {
                return Ok(Value::string(&s[byte_start..byte_end]));
            }
            return Ok(Value::NIL);
        }
    }

    // Otherwise, if the match was against a string, use that string.
    if let Some(ref searched) = md.searched_string {
        if let super::regex::SearchedString::Heap(id) = searched {
            return with_heap(|h| {
                let string = h.get_lisp_string(*id);
                let text = string.as_str();
                let byte_start = char_pos_to_byte(text, start);
                let byte_end = char_pos_to_byte(text, end);
                if byte_end <= text.len() && byte_start <= byte_end {
                    if let Some(slice) = string.slice(byte_start, byte_end) {
                        return Ok(Value::heap_string(slice));
                    }
                }
                Ok(Value::NIL)
            });
        }

        return searched.with_str(|searched| {
            let byte_start = char_pos_to_byte(searched, start);
            let byte_end = char_pos_to_byte(searched, end);
            if byte_end <= searched.len() {
                Ok(Value::string(&searched[byte_start..byte_end]))
            } else {
                Ok(Value::NIL)
            }
        });
    }

    let buf = match eval.buffers.current_buffer() {
        Some(b) => b,
        None => return Ok(Value::NIL),
    };
    if end <= buf.text.len() {
        Ok(Value::string(buf.text.text_range(start, end)))
    } else {
        Ok(Value::NIL)
    }
}

pub(crate) fn builtin_match_beginning_with_state(
    buffers: Option<&crate::buffer::BufferManager>,
    match_data: &Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::MatchBeginning,
        || {
            expect_args("match-beginning", args, 1)?;
            let group = expect_int(&args[0])?;
            if group < 0 {
                return Err(signal(
                    "args-out-of-range",
                    vec![Value::fixnum(group), Value::fixnum(0)],
                ));
            }
            let group = group as usize;

            let md = match match_data {
                Some(md) => md,
                None => return Ok(Value::NIL),
            };

            match md.groups.get(group) {
                Some(Some((start, _end))) => {
                    if md.searched_string.is_some() {
                        Ok(Value::fixnum(*start as i64))
                    } else if let Some(buf) = md
                        .searched_buffer
                        .and_then(|buffer_id| buffers.and_then(|bufs| bufs.get(buffer_id)))
                    {
                        if *start <= buf.text.len() {
                            let pos = buf.text.byte_to_char(*start) as i64 + 1;
                            Ok(Value::fixnum(pos))
                        } else {
                            Ok(Value::fixnum(*start as i64))
                        }
                    } else {
                        Ok(Value::fixnum(*start as i64))
                    }
                }
                Some(None) => Ok(Value::NIL),
                None => Ok(Value::NIL),
            }
        },
    )
}

pub(crate) fn builtin_match_beginning(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_match_beginning_with_state(Some(&eval.buffers), &eval.match_data, &args)
}

pub(crate) fn builtin_match_end_with_state(
    buffers: Option<&crate::buffer::BufferManager>,
    match_data: &Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::MatchEnd,
        || {
            expect_args("match-end", args, 1)?;
            let group = expect_int(&args[0])?;
            if group < 0 {
                return Err(signal(
                    "args-out-of-range",
                    vec![Value::fixnum(group), Value::fixnum(0)],
                ));
            }
            let group = group as usize;

            let md = match match_data {
                Some(md) => md,
                None => return Ok(Value::NIL),
            };

            match md.groups.get(group) {
                Some(Some((_start, end))) => {
                    if md.searched_string.is_some() {
                        Ok(Value::fixnum(*end as i64))
                    } else if let Some(buf) = md
                        .searched_buffer
                        .and_then(|buffer_id| buffers.and_then(|bufs| bufs.get(buffer_id)))
                    {
                        if *end <= buf.text.len() {
                            let pos = buf.text.byte_to_char(*end) as i64 + 1;
                            Ok(Value::fixnum(pos))
                        } else {
                            Ok(Value::fixnum(*end as i64))
                        }
                    } else {
                        Ok(Value::fixnum(*end as i64))
                    }
                }
                Some(None) => Ok(Value::NIL),
                None => Ok(Value::NIL),
            }
        },
    )
}

pub(crate) fn builtin_match_end(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_match_end_with_state(Some(&eval.buffers), &eval.match_data, &args)
}

pub(crate) fn builtin_match_data_with_state(
    mut buffers: Option<&mut crate::buffer::BufferManager>,
    match_data: &Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("match-data"), Value::fixnum(args.len() as i64)],
        ));
    }

    let Some(md) = match_data else {
        return Ok(Value::NIL);
    };
    let integers = args.first().is_some_and(|arg| arg.is_truthy());
    let searched_buffer_id = if md.searched_string.is_none() {
        md.searched_buffer
    } else {
        None
    };

    // Emacs trims trailing unmatched groups from match-data output.
    let mut trailing = md.groups.len();
    while trailing > 0 && md.groups[trailing - 1].is_none() {
        trailing -= 1;
    }

    let mut flat: Vec<Value> = Vec::with_capacity(trailing * 2);
    for grp in md.groups.iter().take(trailing) {
        match grp {
            Some((start, end)) => {
                if md.searched_string.is_some() {
                    flat.push(Value::fixnum(*start as i64));
                    flat.push(Value::fixnum(*end as i64));
                    continue;
                }

                let buffer_positions = searched_buffer_id.and_then(|buffer_id| {
                    buffers.as_deref().and_then(|bufs| {
                        bufs.get(buffer_id).and_then(|buffer| {
                            if *start <= *end && *end <= buffer.text.len() {
                                Some((
                                    buffer.text.byte_to_char(*start) as i64 + 1,
                                    buffer.text.byte_to_char(*end) as i64 + 1,
                                ))
                            } else {
                                None
                            }
                        })
                    })
                });

                if integers {
                    if let Some((start_pos, end_pos)) = buffer_positions {
                        flat.push(Value::fixnum(start_pos));
                        flat.push(Value::fixnum(end_pos));
                    } else {
                        flat.push(Value::fixnum(*start as i64));
                        flat.push(Value::fixnum(*end as i64));
                    }
                    continue;
                }

                if let (Some((start_pos, end_pos)), Some(bufs), Some(buffer_id)) =
                    (buffer_positions, buffers.as_deref_mut(), searched_buffer_id)
                {
                    flat.push(super::marker::make_registered_buffer_marker(
                        bufs, buffer_id, start_pos, false,
                    ));
                    flat.push(super::marker::make_registered_buffer_marker(
                        bufs, buffer_id, end_pos, false,
                    ));
                    continue;
                }

                flat.push(Value::fixnum(*start as i64));
                flat.push(Value::fixnum(*end as i64));
            }
            None => {
                flat.push(ValueKind::Nil);
                flat.push(ValueKind::Nil);
            }
        }
    }

    if integers && md.searched_string.is_none() {
        if let Some(buffer_id) = searched_buffer_id {
            flat.push(Value::make_buffer(buffer_id));
        }
    }
    Ok(Value::list(flat))
}

fn match_data_item_buffer_id_in_manager(
    buffers: &crate::buffer::BufferManager,
    value: &Value,
) -> Option<crate::buffer::BufferId> {
    match value.kind() {
        ValueKind::Veclike(VecLikeType::Buffer) => Some(*buffer_id),
        marker if super::marker::is_marker(marker) => super::marker::marker_logical_fields(marker)
            .and_then(|(buffer_id, _, _)| buffer_id)
            .filter(|buffer_id| buffers.get(*buffer_id).is_some()),
        _ => None,
    }
}

fn expect_match_data_item_in_manager(
    buffers: &crate::buffer::BufferManager,
    value: &Value,
) -> Result<(i64, Option<crate::buffer::BufferId>), Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok((n, None)),
        ValueKind::Char(c) => Ok((c as i64, None)),
        marker if super::marker::is_marker(marker) => Ok((
            super::marker::marker_position_as_int_with_buffers(buffers, marker)?,
            match_data_item_buffer_id_in_manager(buffers, marker),
        )),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

pub(crate) fn builtin_match_data(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_match_data_with_state(Some(&mut eval.buffers), &eval.match_data, &args)
}

pub(crate) fn builtin_set_match_data_with_state(
    buffers: &crate::buffer::BufferManager,
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    expect_min_args("set-match-data", args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("set-match-data"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    if args[0].is_nil() {
        *match_data = None;
        return Ok(Value::NIL);
    }

    let items = list_to_vec(&args[0])
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), args[0]]))?;

    let explicit_buffer_id = if items.len() % 2 == 1 {
        items.last().and_then(|value| match value {
            Value::make_buffer(buffer_id) => Some(*buffer_id),
            _ => None,
        })
    } else {
        None
    };
    let pair_len = items.len() - usize::from(explicit_buffer_id.is_some());

    let mut groups: Vec<Option<(usize, usize)>> = Vec::with_capacity(pair_len / 2);
    let mut searched_buffer = explicit_buffer_id;
    let mut i = 0usize;
    while i + 1 < pair_len {
        let start_v = &items[i];
        let end_v = &items[i + 1];

        if start_v.is_nil() && end_v.is_nil() {
            groups.push(None);
            i += 2;
            continue;
        }

        let (start, start_buffer) = expect_match_data_item_in_manager(buffers, start_v)?;
        let (end, end_buffer) = expect_match_data_item_in_manager(buffers, end_v)?;
        if searched_buffer.is_none() {
            searched_buffer = start_buffer.or(end_buffer);
        }

        // Emacs treats negative marker positions as an end sentinel and
        // truncates remaining groups.
        if start < 0 || end < 0 {
            break;
        }

        groups.push(Some((start as usize, end as usize)));
        i += 2;
    }

    if groups.is_empty() {
        *match_data = None;
    } else {
        if let Some(buffer_id) = searched_buffer
            && let Some(buffer) = buffers.get(buffer_id)
        {
            for group in groups.iter_mut() {
                if let Some((start, end)) = group {
                    *start = buffer.lisp_pos_to_byte(*start as i64);
                    *end = buffer.lisp_pos_to_byte(*end as i64);
                }
            }
        }
        *match_data = Some(super::regex::MatchData {
            groups,
            searched_string: None,
            searched_buffer,
        });
    }

    Ok(Value::NIL)
}

pub(crate) fn builtin_set_match_data(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_match_data_with_state(&eval.buffers, &mut eval.match_data, &args)
}

fn translate_match_data(match_data: &mut Option<super::regex::MatchData>, delta: i64) {
    if let Some(md) = match_data {
        for group in md.groups.iter_mut() {
            if let Some((start, end)) = group {
                *start = (*start as i64 + delta).max(0) as usize;
                *end = (*end as i64 + delta).max(0) as usize;
            }
        }
    }
}

pub(crate) fn builtin_match_data_translate_with_state(
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    expect_args("match-data--translate", args, 1)?;
    let delta = expect_fixnum(&args[0])?;
    translate_match_data(match_data, delta);
    Ok(Value::NIL)
}

pub(crate) fn builtin_match_data_translate(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_match_data_translate_with_state(&mut eval.match_data, &args)
}

fn update_match_data_after_buffer_replace(
    match_data: &mut Option<super::regex::MatchData>,
    oldstart: usize,
    oldend: usize,
    newend: usize,
) {
    let Some(md) = match_data else {
        return;
    };

    let change = newend as i64 - oldend as i64;
    for group in md.groups.iter_mut() {
        let Some((start, end)) = group.as_mut() else {
            continue;
        };

        if *start <= oldstart {
            // Keep starts for enclosing groups, matching GNU's optimistic
            // `update_search_regs` heuristic.
        } else if *start >= oldend {
            *start = (*start as i64 + change) as usize;
        } else {
            *start = oldstart;
        }

        if *end >= oldend {
            *end = (*end as i64 + change) as usize;
        } else if *end > oldstart {
            *end = oldstart;
        }
    }
}

pub(crate) fn builtin_replace_match_with_state(
    buffers: &mut crate::buffer::BufferManager,
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    expect_min_args("replace-match", args, 1)?;
    if args.len() > 5 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("replace-match"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let newtext = expect_strict_string(&args[0])?;
    let fixedcase = args.get(1).is_some_and(|arg| arg.is_truthy());
    let literal = args.get(2).is_some_and(|arg| arg.is_truthy());
    let raw_subexp = args.get(4).copied().unwrap_or(Value::NIL);
    let string_arg = if args.get(3).is_some_and(|arg| !arg.is_nil()) {
        Some(expect_strict_string(&args[3])?)
    } else {
        None
    };
    let subexp = if args.get(4).is_some_and(|arg| !arg.is_nil()) {
        let n = expect_int(&args[4])?;
        if n < 0 {
            return if let Some(source) = string_arg.as_ref() {
                Err(signal(
                    "args-out-of-range",
                    vec![
                        Value::fixnum(n),
                        Value::fixnum(0),
                        Value::fixnum(source.chars().count() as i64),
                    ],
                ))
            } else {
                Err(signal("args-out-of-range", vec![Value::fixnum(n)]))
            };
        }
        n as usize
    } else {
        0usize
    };

    let md_snapshot = match_data.clone();
    let missing_subexp_error = super::regex::REPLACE_MATCH_SUBEXP_MISSING;
    let missing_subexp_signal = |subexp_value: Value| {
        signal(
            "error",
            vec![Value::string(missing_subexp_error), subexp_value],
        )
    };

    if let Some(source) = string_arg {
        if md_snapshot.is_none() {
            return Err(missing_subexp_signal(raw_subexp));
        }
        return match super::regex::replace_match_string(
            &source,
            &newtext,
            fixedcase,
            literal,
            subexp,
            &md_snapshot,
        ) {
            Ok(result) => Ok(Value::string(result)),
            Err(msg) if msg == missing_subexp_error => Err(missing_subexp_signal(raw_subexp)),
            Err(msg) => Err(signal("error", vec![Value::string(msg)])),
        };
    }

    if md_snapshot
        .as_ref()
        .is_some_and(|m| m.searched_string.is_some())
    {
        return Err(signal("args-out-of-range", vec![Value::fixnum(0)]));
    }

    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let (oldstart, oldend, replacement_len) = {
        let md = md_snapshot
            .as_ref()
            .ok_or_else(|| missing_subexp_signal(raw_subexp))?;
        let (oldstart, oldend) = match md.groups.get(subexp) {
            Some(Some(pair)) => *pair,
            Some(None) | None => return Err(missing_subexp_signal(raw_subexp)),
        };

        let buf = buffers
            .get(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let source = buf.text.text_range(0, buf.text.len());
        let replacement = super::regex::replace_match_string(
            &source,
            &newtext,
            fixedcase,
            literal,
            subexp,
            &md_snapshot,
        )
        .map_err(|msg| {
            if msg == missing_subexp_error {
                missing_subexp_signal(raw_subexp)
            } else {
                signal("error", vec![Value::string(msg)])
            }
        })?;
        let replacement_len = replacement.len() - (source.len() - (oldend - oldstart));
        (oldstart, oldend, replacement_len)
    };

    let result = {
        let buf = buffers
            .get_mut(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        super::regex::replace_match_buffer(buf, &newtext, fixedcase, literal, subexp, &md_snapshot)
    };
    match result {
        Ok(()) => {
            let newend = oldstart + replacement_len;
            update_match_data_after_buffer_replace(match_data, oldstart, oldend, newend);
            Ok(ValueKind::Nil)
        }
        Err(msg) if msg == missing_subexp_error => Err(missing_subexp_signal(raw_subexp)),
        Err(msg) => Err(signal("error", vec![Value::string(msg)])),
    }
}

pub(crate) fn builtin_replace_match(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    // Determine whether this is a buffer replacement (4th arg nil/absent) so we
    // can fire modification hooks.  String replacements don't touch the buffer.
    let is_buffer_replace = args.len() < 4 || args[3].is_nil();

    if is_buffer_replace {
        // Try to compute the match region for before-change signalling.
        let subexp = if args.len() >= 5 && !args[4].is_nil() {
            expect_int(&args[4]).unwrap_or(0) as usize
        } else {
            0usize
        };
        if let Some(ref md) = eval.match_data {
            if md.searched_string.is_none() {
                if let Some(Some((oldstart, oldend))) = md.groups.get(subexp) {
                    let (oldstart, oldend) = (*oldstart, *oldend);
                    super::editfns::signal_before_change(eval, oldstart, oldend)?;
                    let result = builtin_replace_match_with_state(
                        &mut eval.buffers,
                        &mut eval.match_data,
                        &args,
                    )?;
                    // After the replacement the new end = oldstart + replacement_len.
                    // Compute new buffer size at that region from match_data update.
                    let new_end = eval
                        .match_data
                        .as_ref()
                        .and_then(|md| md.groups.first())
                        .and_then(|g| g.as_ref())
                        .map(|(_, e)| *e)
                        .unwrap_or(oldstart);
                    let old_len =
                        super::editfns::current_buffer_byte_span_char_len(eval, oldstart, oldend);
                    super::editfns::signal_after_change(eval, oldstart, new_end, old_len)?;
                    return Ok(result);
                }
            }
        }
    }

    // Fallback: string replacement or no match data — no buffer hooks needed.
    builtin_replace_match_with_state(&mut eval.buffers, &mut eval.match_data, &args)
}
