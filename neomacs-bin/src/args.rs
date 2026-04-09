//! Argv-handling primitives, ported from GNU Emacs `src/emacs.c`.
//!
//! This module mirrors the C-side of GNU's command-line parsing so the
//! Rust binary can stay bug-compatible with `emacs` for the small set of
//! flags that must be observed before the Lisp evaluator runs. Flags
//! that GNU forwards to `lisp/startup.el` are also forwarded here — see
//! `parse_startup_options` in `main.rs` for the consume vs. forward
//! boundary, and `drafts/argv-parity-audit.md` for the full ground-truth
//! table.
//!
//! Currently provides:
//!
//! - [`argmatch`] — ports `emacs.c:686-738`. The single primitive used to
//!   recognize a flag at a given index in argv, in either short
//!   (`-nw`) or long-prefix (`--no-window-system` or `--no-windows`)
//!   form, with both `--opt VAL` and `--opt=VAL` value forms.

/// Result of one [`argmatch`] call against the next position in argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArgMatch {
    /// No match at this position. The caller's index has not been
    /// advanced and the args are unchanged.
    NoMatch,
    /// Matched a no-value flag. The caller's index has been advanced by
    /// one position past the flag.
    Bare,
    /// Matched a value-taking flag. The caller's index has been advanced
    /// by one or two positions depending on whether the value was inline
    /// (`--opt=VAL`) or in the next argv slot (`--opt VAL` / `-o VAL`).
    Value(String),
    /// The flag matched in long form but the value-taking variant
    /// could not find a value. Mirrors GNU's "Option requires an
    /// argument" failure for the value form. The caller's index is
    /// unchanged.
    MissingValue,
}

/// Mirror of `argmatch` from GNU `src/emacs.c:685-738`.
///
/// Tests `args[*idx + 1]` (the slot **after** the current cursor) against
/// either `sstr` (short form like `-nw`) or `lstr` (long form like
/// `--no-window-system`). The long form matches a prefix of the user's
/// argument so long as the prefix is at least `minlen` characters; this
/// is what lets `emacs --no-windows` distinguish between
/// `--no-window-system` and `--no-windows` even though both share a
/// prefix.
///
/// `*idx` is the GNU `*skipptr` — the index of the last consumed entry
/// in argv. The very first call uses `idx = 0` (so we look at argv[1]),
/// and each successful match advances it past the consumed entry.
///
/// `takes_value = true` corresponds to GNU's `valptr != NULL` branch.
/// On success, the value is returned via [`ArgMatch::Value`]. On a long
/// match where the value is missing, [`ArgMatch::MissingValue`] is
/// returned (GNU returns 0 in that branch — we surface a richer signal
/// so the caller can produce a useful error message).
///
/// **Index semantics**: `*idx` mirrors GNU's `*skipptr`, which points to
/// the last *consumed* slot. So `args[*idx + 1]` is the slot under
/// consideration, and a successful match advances `*idx` by 1 (no
/// value), 1 (`--opt=VAL`), or 2 (`--opt VAL` / `-o VAL`).
pub(crate) fn argmatch(
    args: &[String],
    idx: &mut usize,
    sstr: &str,
    lstr: Option<&str>,
    minlen: usize,
    takes_value: bool,
) -> ArgMatch {
    // GNU's `if (argc <= *skipptr + 1) return 0;` — give up if there is
    // no slot at the cursor position.
    if args.len() <= *idx + 1 {
        return ArgMatch::NoMatch;
    }
    let arg = &args[*idx + 1];

    // Exact short-form match.
    if arg == sstr {
        if takes_value {
            // GNU: *valptr = argv[*skipptr+2]; *skipptr += 2;
            if let Some(value) = args.get(*idx + 2) {
                let value = value.clone();
                *idx += 2;
                return ArgMatch::Value(value);
            }
            return ArgMatch::MissingValue;
        }
        *idx += 1;
        return ArgMatch::Bare;
    }

    // Long-form prefix match. GNU computes `arglen` as the index of an
    // optional `=` in the user's argument (only when `valptr != NULL`),
    // otherwise the full string length. Then it checks
    //   arglen >= minlen && strncmp(arg, lstr, arglen) == 0
    let Some(lstr) = lstr else {
        return ArgMatch::NoMatch;
    };
    let eq_pos = if takes_value { arg.find('=') } else { None };
    let arglen = eq_pos.unwrap_or(arg.len());
    if arglen < minlen {
        return ArgMatch::NoMatch;
    }
    if !lstr.starts_with(&arg[..arglen]) {
        return ArgMatch::NoMatch;
    }

    if !takes_value {
        *idx += 1;
        return ArgMatch::Bare;
    }

    if let Some(eq_pos) = eq_pos {
        // GNU: *valptr = p+1; *skipptr += 1;
        let value = arg[eq_pos + 1..].to_string();
        *idx += 1;
        return ArgMatch::Value(value);
    }

    // GNU: *valptr = argv[*skipptr+2]; *skipptr += 2;
    if let Some(value) = args.get(*idx + 2) {
        let value = value.clone();
        *idx += 2;
        return ArgMatch::Value(value);
    }
    ArgMatch::MissingValue
}

#[cfg(test)]
#[path = "args_test.rs"]
mod args_test;
