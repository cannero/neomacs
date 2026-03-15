use super::*;
use crate::emacs_core::regex::char_pos_to_byte;

// ===========================================================================
// Search / Regex builtins (evaluator-dependent)
// ===========================================================================

pub(crate) fn builtin_search_forward(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("search-forward", &args, 1, 4)?;
    let pattern = expect_string(&args[0])?;
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    let (current_id, opts, start_pt, start_char) =
        current_search_context(eval, &args, SearchKind::ForwardLiteral)?;

    if opts.steps == 0 {
        return Ok(Value::Int(start_char));
    }

    let mut last_pos = None;
    for _ in 0..opts.steps {
        let result = {
            let buf = eval
                .buffers
                .get_mut(current_id)
                .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
            match opts.direction {
                SearchDirection::Forward => super::regex::search_forward(
                    buf,
                    &pattern,
                    opts.bound,
                    false,
                    case_fold,
                    &mut eval.match_data,
                ),
                SearchDirection::Backward => super::regex::search_backward(
                    buf,
                    &pattern,
                    opts.bound,
                    false,
                    case_fold,
                    &mut eval.match_data,
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
                return handle_search_failure(
                    eval,
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
    buffer_byte_to_char_result(eval, current_id, end)
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
        None | Some(Value::Nil) => Ok(1),
        Some(Value::Int(n)) => Ok(*n),
        Some(Value::Char(c)) => Ok(*c as i64),
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), *other],
        )),
    }
}

fn search_bound_to_byte(
    eval: &super::eval::Evaluator,
    buf: &crate::buffer::Buffer,
    value: &Value,
) -> Result<usize, Flow> {
    let pos = expect_integer_or_marker_eval(eval, value)?;
    Ok(buf.lisp_pos_to_accessible_byte(pos))
}

fn parse_search_options(
    eval: &super::eval::Evaluator,
    buf: &crate::buffer::Buffer,
    args: &[Value],
    kind: SearchKind,
) -> Result<SearchOptions, Flow> {
    let count = search_count_arg(args)?;
    let noerror_mode = match args.get(2) {
        None | Some(Value::Nil) => SearchNoErrorMode::Signal,
        Some(Value::True) => SearchNoErrorMode::KeepPoint,
        Some(_) => SearchNoErrorMode::MoveToBound,
    };
    let (bound_lisp, bound) = match args.get(1) {
        Some(v) if !v.is_nil() => {
            let raw = expect_integer_or_marker_eval(eval, v)?;
            let byte = search_bound_to_byte(eval, buf, v)?;
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

fn current_search_context(
    eval: &super::eval::Evaluator,
    args: &[Value],
    kind: SearchKind,
) -> Result<(crate::buffer::BufferId, SearchOptions, usize, i64), Flow> {
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = eval
        .buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let opts = parse_search_options(eval, buf, args, kind)?;
    let start_pt = buf.pt;
    let start_char = buf.text.byte_to_char(buf.pt) as i64 + 1;
    Ok((current_id, opts, start_pt, start_char))
}

fn buffer_byte_to_char_result(
    eval: &super::eval::Evaluator,
    buffer_id: crate::buffer::BufferId,
    byte: usize,
) -> EvalResult {
    let buf = eval
        .buffers
        .get(buffer_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(Value::Int(buf.text.byte_to_char(byte) as i64 + 1))
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

fn handle_search_failure(
    eval: &mut super::eval::Evaluator,
    buffer_id: crate::buffer::BufferId,
    pattern: &str,
    opts: SearchOptions,
    start_pt: usize,
    kind: SearchErrorKind,
) -> EvalResult {
    match kind {
        SearchErrorKind::NotFound => match opts.noerror_mode {
            SearchNoErrorMode::Signal => {
                let _ = eval.buffers.goto_buffer_byte(buffer_id, start_pt);
                Err(signal("search-failed", vec![Value::string(pattern)]))
            }
            SearchNoErrorMode::KeepPoint => {
                let _ = eval.buffers.goto_buffer_byte(buffer_id, start_pt);
                Ok(Value::Nil)
            }
            SearchNoErrorMode::MoveToBound => {
                let target = eval
                    .buffers
                    .get(buffer_id)
                    .map(|buf| search_failure_position(buf, opts))
                    .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
                let _ = eval.buffers.goto_buffer_byte(buffer_id, target);
                Ok(Value::Nil)
            }
        },
    }
}

pub(crate) fn builtin_search_backward(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("search-backward", &args, 1, 4)?;
    let pattern = expect_string(&args[0])?;
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    let (current_id, opts, start_pt, start_char) =
        current_search_context(eval, &args, SearchKind::BackwardLiteral)?;

    if opts.steps == 0 {
        return Ok(Value::Int(start_char));
    }

    let mut last_pos = None;
    for _ in 0..opts.steps {
        let result = {
            let buf = eval
                .buffers
                .get_mut(current_id)
                .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
            match opts.direction {
                SearchDirection::Forward => super::regex::search_forward(
                    buf,
                    &pattern,
                    opts.bound,
                    false,
                    case_fold,
                    &mut eval.match_data,
                ),
                SearchDirection::Backward => super::regex::search_backward(
                    buf,
                    &pattern,
                    opts.bound,
                    false,
                    case_fold,
                    &mut eval.match_data,
                ),
            }
        };
        match result {
            Ok(Some(pos)) => last_pos = Some(pos),
            Ok(None) => {
                return Err(signal("search-failed", vec![Value::string(pattern)]));
            }
            Err(_) => {
                return handle_search_failure(
                    eval,
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
    buffer_byte_to_char_result(eval, current_id, end)
}

pub(crate) fn builtin_re_search_forward(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("re-search-forward", &args, 1, 4)?;
    let pattern = expect_string(&args[0])?;
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    let (current_id, opts, start_pt, start_char) =
        current_search_context(eval, &args, SearchKind::ForwardRegexp)?;

    if opts.steps == 0 {
        return Ok(Value::Int(start_char));
    }

    let mut last_pos = None;
    for _ in 0..opts.steps {
        let result = {
            let buf = eval
                .buffers
                .get_mut(current_id)
                .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
            match opts.direction {
                SearchDirection::Forward => super::regex::re_search_forward(
                    buf,
                    &pattern,
                    opts.bound,
                    false,
                    case_fold,
                    &mut eval.match_data,
                ),
                SearchDirection::Backward => super::regex::re_search_backward(
                    buf,
                    &pattern,
                    opts.bound,
                    false,
                    case_fold,
                    &mut eval.match_data,
                ),
            }
        };

        match result {
            Ok(Some(pos)) => last_pos = Some(pos),
            Ok(None) => {
                return Err(signal("search-failed", vec![Value::string(pattern)]));
            }
            Err(msg) if msg.starts_with("Invalid regexp:") => {
                let _ = eval.buffers.goto_buffer_byte(current_id, start_pt);
                return Err(signal("invalid-regexp", vec![Value::string(msg)]));
            }
            Err(_) => {
                return handle_search_failure(
                    eval,
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
    buffer_byte_to_char_result(eval, current_id, end)
}

pub(crate) fn builtin_re_search_backward(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("re-search-backward", &args, 1, 4)?;
    let pattern = expect_string(&args[0])?;
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    let (current_id, opts, start_pt, start_char) =
        current_search_context(eval, &args, SearchKind::BackwardRegexp)?;

    if opts.steps == 0 {
        return Ok(Value::Int(start_char));
    }

    let mut last_pos = None;
    for _ in 0..opts.steps {
        let result = {
            let buf = eval
                .buffers
                .get_mut(current_id)
                .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
            match opts.direction {
                SearchDirection::Forward => super::regex::re_search_forward(
                    buf,
                    &pattern,
                    opts.bound,
                    false,
                    case_fold,
                    &mut eval.match_data,
                ),
                SearchDirection::Backward => super::regex::re_search_backward(
                    buf,
                    &pattern,
                    opts.bound,
                    false,
                    case_fold,
                    &mut eval.match_data,
                ),
            }
        };

        match result {
            Ok(Some(pos)) => last_pos = Some(pos),
            Ok(None) => {
                return Err(signal("search-failed", vec![Value::string(pattern)]));
            }
            Err(msg) if msg.starts_with("Invalid regexp:") => {
                let _ = eval.buffers.goto_buffer_byte(current_id, start_pt);
                return Err(signal("invalid-regexp", vec![Value::string(msg)]));
            }
            Err(_) => {
                return handle_search_failure(
                    eval,
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
    buffer_byte_to_char_result(eval, current_id, end)
}

pub(crate) fn builtin_search_forward_regexp(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("search-forward-regexp", &args, 1, 4)?;
    builtin_re_search_forward(eval, args)
}

pub(crate) fn builtin_search_backward_regexp(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("search-backward-regexp", &args, 1, 4)?;
    builtin_re_search_backward(eval, args)
}

pub(crate) fn builtin_looking_at(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("looking-at", &args, 1, 2)?;
    let pattern = expect_string(&args[0])?;
    let inhibit_modify = args.get(1).is_some_and(|arg| !arg.is_nil());

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);

    let result = if inhibit_modify {
        let mut preserved_match_data = eval.match_data.clone();
        super::regex::looking_at(buf, &pattern, case_fold, &mut preserved_match_data)
    } else {
        super::regex::looking_at(buf, &pattern, case_fold, &mut eval.match_data)
    };

    match result {
        Ok(matched) => Ok(Value::bool(matched)),
        Err(msg) => Err(signal("invalid-regexp", vec![Value::string(msg)])),
    }
}

pub(crate) fn builtin_looking_at_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("looking-at-p", &args, 1)?;
    let pattern = expect_string(&args[0])?;

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);

    let mut throwaway_match_data = None;
    match super::regex::looking_at(buf, &pattern, case_fold, &mut throwaway_match_data) {
        Ok(matched) => Ok(Value::bool(matched)),
        Err(msg) => Err(signal("invalid-regexp", vec![Value::string(msg)])),
    }
}

pub(crate) fn builtin_posix_looking_at(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("posix-looking-at", &args, 1, 2)?;
    builtin_looking_at(eval, args)
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

            match (&args[0], &args[1]) {
                (Value::Str(pattern_id), Value::Str(string_id)) => with_heap(|h| {
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
                        Ok(Some(char_pos)) => Ok(Value::Int(char_pos as i64)),
                        Ok(None) => Ok(Value::Nil),
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
                        Ok(Some(char_pos)) => Ok(Value::Int(char_pos as i64)),
                        Ok(None) => Ok(Value::Nil),
                        Err(msg) => Err(signal("invalid-regexp", vec![Value::string(msg)])),
                    }
                }
            }
        },
    )
}

/// Evaluator-dependent `string-match`: updates match data on the evaluator.
pub(crate) fn builtin_string_match_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_string_match_with_state(case_fold, &mut eval.match_data, &args)
}

pub(crate) fn builtin_string_match_p_with_case_fold(case_fold: bool, args: &[Value]) -> EvalResult {
    expect_range_args("string-match-p", args, 2, 3)?;
    match (&args[0], &args[1]) {
        (Value::Str(pattern_id), Value::Str(string_id)) => with_heap(|h| {
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
                Ok(Some(char_pos)) => Ok(Value::Int(char_pos as i64)),
                Ok(None) => Ok(Value::Nil),
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
                Ok(Some(char_pos)) => Ok(Value::Int(char_pos as i64)),
                Ok(None) => Ok(Value::Nil),
                Err(msg) => Err(signal("invalid-regexp", vec![Value::string(msg)])),
            }
        }
    }
}

pub(crate) fn builtin_string_match_p_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    builtin_string_match_p_with_case_fold(case_fold, &args)
}

pub(crate) fn builtin_posix_string_match(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("posix-string-match", &args, 2, 4)?;
    builtin_string_match_eval(eval, args)
}

pub(crate) fn builtin_match_string(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("match-string", &args, 1, 2)?;
    let group = expect_int(&args[0])?;
    if group < 0 {
        return Err(signal(
            "args-out-of-range",
            vec![Value::Int(group), Value::Int(0)],
        ));
    }
    let group = group as usize;

    let md = match &eval.match_data {
        Some(md) => md,
        None => return Ok(Value::Nil),
    };

    let (start, end) = match md.groups.get(group) {
        Some(Some(pair)) => *pair,
        _ => return Ok(Value::Nil),
    };

    // If an optional second arg is a string, use that first.
    if args.len() > 1 {
        if let Value::Str(id) = args[1] {
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
                Ok(Value::Nil)
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
            return Ok(Value::Nil);
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
                Ok(Value::Nil)
            });
        }

        return searched.with_str(|searched| {
            let byte_start = char_pos_to_byte(searched, start);
            let byte_end = char_pos_to_byte(searched, end);
            if byte_end <= searched.len() {
                Ok(Value::string(&searched[byte_start..byte_end]))
            } else {
                Ok(Value::Nil)
            }
        });
    }

    let buf = match eval.buffers.current_buffer() {
        Some(b) => b,
        None => return Ok(Value::Nil),
    };
    if end <= buf.text.len() {
        Ok(Value::string(buf.text.text_range(start, end)))
    } else {
        Ok(Value::Nil)
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
                    vec![Value::Int(group), Value::Int(0)],
                ));
            }
            let group = group as usize;

            let md = match match_data {
                Some(md) => md,
                None => return Ok(Value::Nil),
            };

            match md.groups.get(group) {
                Some(Some((start, _end))) => {
                    if md.searched_string.is_some() {
                        Ok(Value::Int(*start as i64))
                    } else if let Some(buf) = md
                        .searched_buffer
                        .and_then(|buffer_id| buffers.and_then(|bufs| bufs.get(buffer_id)))
                        .or_else(|| buffers.and_then(|bufs| bufs.current_buffer()))
                    {
                        if *start <= buf.text.len() {
                            let pos = buf.text.byte_to_char(*start) as i64 + 1;
                            Ok(Value::Int(pos))
                        } else {
                            Ok(Value::Int(*start as i64))
                        }
                    } else {
                        Ok(Value::Int(*start as i64))
                    }
                }
                Some(None) => Ok(Value::Nil),
                None => Ok(Value::Nil),
            }
        },
    )
}

pub(crate) fn builtin_match_beginning(
    eval: &mut super::eval::Evaluator,
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
                    vec![Value::Int(group), Value::Int(0)],
                ));
            }
            let group = group as usize;

            let md = match match_data {
                Some(md) => md,
                None => return Ok(Value::Nil),
            };

            match md.groups.get(group) {
                Some(Some((_start, end))) => {
                    if md.searched_string.is_some() {
                        Ok(Value::Int(*end as i64))
                    } else if let Some(buf) = md
                        .searched_buffer
                        .and_then(|buffer_id| buffers.and_then(|bufs| bufs.get(buffer_id)))
                        .or_else(|| buffers.and_then(|bufs| bufs.current_buffer()))
                    {
                        if *end <= buf.text.len() {
                            let pos = buf.text.byte_to_char(*end) as i64 + 1;
                            Ok(Value::Int(pos))
                        } else {
                            Ok(Value::Int(*end as i64))
                        }
                    } else {
                        Ok(Value::Int(*end as i64))
                    }
                }
                Some(None) => Ok(Value::Nil),
                None => Ok(Value::Nil),
            }
        },
    )
}

pub(crate) fn builtin_match_end(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
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
            vec![Value::symbol("match-data"), Value::Int(args.len() as i64)],
        ));
    }

    let Some(md) = match_data else {
        return Ok(Value::Nil);
    };
    let integers = args.first().is_some_and(|arg| arg.is_truthy());
    let current_buffer_id = if md.searched_string.is_none() {
        md.searched_buffer.or_else(|| {
            buffers
                .as_ref()
                .and_then(|bufs| bufs.current_buffer().map(|buffer| buffer.id))
        })
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
                    flat.push(Value::Int(*start as i64));
                    flat.push(Value::Int(*end as i64));
                    continue;
                }

                let buffer_positions = current_buffer_id.and_then(|buffer_id| {
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
                        flat.push(Value::Int(start_pos));
                        flat.push(Value::Int(end_pos));
                    } else {
                        flat.push(Value::Int(*start as i64));
                        flat.push(Value::Int(*end as i64));
                    }
                    continue;
                }

                if let (Some((start_pos, end_pos)), Some(bufs), Some(buffer_id)) =
                    (buffer_positions, buffers.as_deref_mut(), current_buffer_id)
                {
                    flat.push(super::marker::make_registered_buffer_marker(
                        bufs, buffer_id, start_pos, false,
                    ));
                    flat.push(super::marker::make_registered_buffer_marker(
                        bufs, buffer_id, end_pos, false,
                    ));
                    continue;
                }

                flat.push(Value::Int(*start as i64));
                flat.push(Value::Int(*end as i64));
            }
            None => {
                flat.push(Value::Nil);
                flat.push(Value::Nil);
            }
        }
    }

    if integers && md.searched_string.is_none() {
        if let Some(buffer_id) = current_buffer_id {
            flat.push(Value::Buffer(buffer_id));
        }
    }
    Ok(Value::list(flat))
}

pub(crate) fn builtin_match_data_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_match_data_with_state(Some(&mut eval.buffers), &eval.match_data, &args)
}

pub(crate) fn builtin_set_match_data_with_state(
    match_data: &mut Option<super::regex::MatchData>,
    args: &[Value],
) -> EvalResult {
    expect_min_args("set-match-data", args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("set-match-data"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    if args[0].is_nil() {
        *match_data = None;
        return Ok(Value::Nil);
    }

    let items = list_to_vec(&args[0])
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), args[0]]))?;

    let mut groups: Vec<Option<(usize, usize)>> = Vec::with_capacity(items.len() / 2);
    let mut i = 0usize;
    while i + 1 < items.len() {
        let start_v = &items[i];
        let end_v = &items[i + 1];

        if start_v.is_nil() && end_v.is_nil() {
            groups.push(None);
            i += 2;
            continue;
        }

        let start = expect_integer_or_marker(start_v)?;
        let end = expect_integer_or_marker(end_v)?;

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
        *match_data = Some(super::regex::MatchData {
            groups,
            searched_string: None,
            searched_buffer: None,
        });
    }

    Ok(Value::Nil)
}

pub(crate) fn builtin_set_match_data_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_match_data_with_state(&mut eval.match_data, &args)
}

pub(crate) fn builtin_replace_match(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("replace-match", &args, 1)?;
    if args.len() > 5 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("replace-match"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let newtext = expect_strict_string(&args[0])?;
    let fixedcase = args.len() > 1 && args[1].is_truthy();
    let literal = args.len() > 2 && args[2].is_truthy();
    let string_arg = if args.len() > 3 && !args[3].is_nil() {
        Some(expect_strict_string(&args[3])?)
    } else {
        None
    };
    let subexp = if args.len() > 4 && !args[4].is_nil() {
        let n = expect_int(&args[4])?;
        if n < 0 {
            return if let Some(source) = string_arg.as_ref() {
                Err(signal(
                    "args-out-of-range",
                    vec![
                        Value::Int(n),
                        Value::Int(0),
                        Value::Int(source.chars().count() as i64),
                    ],
                ))
            } else {
                Err(signal("args-out-of-range", vec![Value::Int(n)]))
            };
        }
        n as usize
    } else {
        0usize
    };

    // Clone match_data to avoid borrow conflict
    let md = eval.match_data.clone();
    let missing_subexp_error = super::regex::REPLACE_MATCH_SUBEXP_MISSING;

    if let Some(source) = string_arg {
        if md
            .as_ref()
            .and_then(|m| m.groups.first())
            .and_then(|g| *g)
            .is_none()
            && subexp == 0
        {
            return Err(signal("args-out-of-range", vec![Value::Int(0)]));
        }
        return match super::regex::replace_match_string(
            &source, &newtext, fixedcase, literal, subexp, &md,
        ) {
            Ok(result) => Ok(Value::string(result)),
            Err(msg) if msg == missing_subexp_error && subexp == 0 => {
                Err(signal("args-out-of-range", vec![Value::Int(0)]))
            }
            Err(msg) if msg == missing_subexp_error => Err(signal(
                "error",
                vec![Value::string(msg), Value::Int(subexp as i64)],
            )),
            Err(msg) => Err(signal("error", vec![Value::string(msg)])),
        };
    }

    if md.as_ref().is_some_and(|m| m.searched_string.is_some()) {
        return Err(signal("args-out-of-range", vec![Value::Int(0)]));
    }

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let result = {
        let buf = eval
            .buffers
            .get_mut(current_id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        super::regex::replace_match_buffer(buf, &newtext, fixedcase, literal, subexp, &md)
    };
    match result {
        Ok(()) => Ok(Value::Nil), // Emacs returns nil on buffer replacement
        Err(msg) if msg == missing_subexp_error && subexp == 0 => {
            Err(signal("args-out-of-range", vec![Value::Int(0)]))
        }
        Err(msg) if msg == missing_subexp_error => Err(signal(
            "error",
            vec![Value::string(msg), Value::Int(subexp as i64)],
        )),
        Err(msg) => Err(signal("error", vec![Value::string(msg)])),
    }
}
