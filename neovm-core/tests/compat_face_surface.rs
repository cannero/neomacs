mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use common::{oracle_enabled, oracle_emacs_path, run_neovm_eval, run_oracle_eval};
use regex::Regex;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ImplBucket {
    Xfaces,
    Font,
    Display,
    Symbols,
    Strings,
    Stubs,
    Other,
}

impl ImplBucket {
    fn label(self) -> &'static str {
        match self {
            Self::Xfaces => "xfaces",
            Self::Font => "font",
            Self::Display => "display",
            Self::Symbols => "symbols",
            Self::Strings => "strings",
            Self::Stubs => "stubs",
            Self::Other => "other",
        }
    }
}

fn crate_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn gnu_xfaces_c_path() -> Option<std::path::PathBuf> {
    let mut dir = crate_root().parent()?.to_path_buf();
    for _ in 0..5 {
        let candidate = dir.join("emacs-mirror/emacs/src/xfaces.c");
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn parse_gnu_xfaces_defuns(path: &Path) -> BTreeSet<String> {
    let source = fs::read_to_string(path).expect("read GNU xfaces.c");
    let re = Regex::new(r#"DEFUN \("([^"]+)","#).expect("xfaces.c DEFUN regex");
    re.captures_iter(&source)
        .map(|caps| caps[1].to_string())
        .collect()
}

fn parse_defsubr_targets(path: &Path) -> BTreeMap<String, String> {
    let source = fs::read_to_string(path).expect("read builtins/mod.rs");
    let mut out = BTreeMap::new();
    let mut block = String::new();
    let mut capturing = false;

    for line in source.lines() {
        if !capturing {
            if let Some(idx) = line.find("ctx.defsubr(") {
                capturing = true;
                block.clear();
                block.push_str(&line[idx..]);
                block.push('\n');
                if line.contains(");")
                    && let Some((name, target)) = extract_defsubr_name_and_target(&block)
                {
                    out.insert(name, target);
                    block.clear();
                    capturing = false;
                }
            }
            continue;
        }

        block.push_str(line);
        block.push('\n');

        if line.contains(");")
            && let Some((name, target)) = extract_defsubr_name_and_target(&block)
        {
            out.insert(name, target);
            block.clear();
            capturing = false;
        }
    }

    out
}

fn extract_defsubr_name_and_target(block: &str) -> Option<(String, String)> {
    let re = Regex::new(r#"ctx\.defsubr\(\s*"([^"]+)""#).expect("defsubr regex");
    let caps = re.captures(block)?;
    let name = caps.get(1)?.as_str().to_string();
    let full = caps.get(0)?;
    let rest = &block[full.end()..];
    let comma = rest.find(',')?;
    let mut target = String::new();
    let mut paren_depth = 0usize;
    let mut in_pipe = false;
    let mut started = false;

    for ch in rest[comma + 1..].chars() {
        if !started && ch.is_whitespace() {
            continue;
        }
        started = true;
        match ch {
            '|' if paren_depth == 0 => {
                in_pipe = !in_pipe;
                target.push(ch);
            }
            '(' if !in_pipe => {
                paren_depth += 1;
                target.push(ch);
            }
            ')' if !in_pipe => {
                paren_depth = paren_depth.saturating_sub(1);
                target.push(ch);
            }
            ',' if !in_pipe && paren_depth == 0 => break,
            _ => target.push(ch),
        }
    }

    Some((name, target.split_whitespace().collect::<Vec<_>>().join(" ")))
}

fn symbol_list_literal(names: &BTreeSet<String>) -> String {
    let body = names.iter().cloned().collect::<Vec<_>>().join(" ");
    format!("'({body})")
}

fn parse_symbol_list(output: &str) -> BTreeSet<String> {
    let payload = output.strip_prefix("OK ").unwrap_or(output);
    let re = Regex::new(r#"([A-Za-z0-9!$%&*+\-./:<=>?@^_~]+)"#).expect("symbol regex");
    re.captures_iter(payload)
        .map(|caps| caps[1].to_string())
        .collect()
}

fn classify_target(target: &str, stubs_source: &str) -> ImplBucket {
    if target.contains("super::xfaces::") {
        return ImplBucket::Xfaces;
    }
    if target.contains("super::font::") {
        return ImplBucket::Font;
    }
    if target.contains("super::display::") {
        return ImplBucket::Display;
    }
    if target.contains("symbols::") {
        return ImplBucket::Symbols;
    }
    if target.contains("super::builtins::strings::")
        || target.contains("builtin_clear_face_cache")
    {
        return ImplBucket::Strings;
    }
    let re = Regex::new(r#"(builtin_[A-Za-z0-9_]+)"#).expect("builtin ident regex");
    if let Some(caps) = re.captures(target) {
        let ident = &caps[1];
        if stubs_source.contains(&format!("fn {ident}("))
            || stubs_source.contains(&format!("fn {ident} ("))
        {
            return ImplBucket::Stubs;
        }
    }
    ImplBucket::Other
}

#[test]
fn compat_face_surface_matches_gnu_emacs() {
    if !oracle_enabled() {
        eprintln!(
            "skipping face surface audit: set NEOVM_FORCE_ORACLE_PATH or place GNU Emacs mirror alongside the repo"
        );
        return;
    }

    let Some(gnu_xfaces_c) = gnu_xfaces_c_path() else {
        eprintln!("skipping face surface audit: GNU xfaces.c not found");
        return;
    };
    let _oracle = oracle_emacs_path().expect("GNU Emacs oracle binary");

    let builtins_mod = crate_root().join("src/emacs_core/builtins/mod.rs");
    let stubs_rs = crate_root().join("src/emacs_core/builtins/stubs.rs");

    let xfaces_defuns = parse_gnu_xfaces_defuns(&gnu_xfaces_c);
    let source_name_list = symbol_list_literal(&xfaces_defuns);
    let exported_form = format!(
        r#"(delq nil (mapcar (lambda (name) (and (fboundp name) name)) {source_name_list}))"#
    );
    let exported = parse_symbol_list(
        &run_oracle_eval(&exported_form).expect("GNU Emacs xfaces surface evaluation"),
    );

    let defsubr_targets = parse_defsubr_targets(&builtins_mod);
    let stubs_source = fs::read_to_string(&stubs_rs).expect("read builtins/stubs.rs");

    let missing_registrations = exported
        .iter()
        .filter(|name| !defsubr_targets.contains_key(*name))
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        missing_registrations.is_empty(),
        "GNU xfaces runtime exports missing NeoVM defsubr registrations: {}",
        missing_registrations.join(", ")
    );

    let mut by_bucket: BTreeMap<ImplBucket, Vec<String>> = BTreeMap::new();
    for name in &exported {
        let target = defsubr_targets
            .get(name)
            .expect("xfaces primitive target should exist");
        by_bucket
            .entry(classify_target(target, &stubs_source))
            .or_default()
            .push(name.clone());
    }

    for bucket in [
        ImplBucket::Xfaces,
        ImplBucket::Font,
        ImplBucket::Display,
        ImplBucket::Symbols,
        ImplBucket::Strings,
        ImplBucket::Stubs,
        ImplBucket::Other,
    ] {
        let names = by_bucket.get(&bucket).cloned().unwrap_or_default();
        println!(
            "xfaces audit bucket {} count={} names={}",
            bucket.label(),
            names.len(),
            names.join(", ")
        );
    }

    let exported_list = symbol_list_literal(&exported);
    let form = format!(
        r#"(mapcar (lambda (name) (list name (fboundp name) (and (fboundp name) (subrp (symbol-function name))))) {exported_list})"#
    );

    let gnu = run_oracle_eval(&form).expect("GNU Emacs face surface evaluation");
    let neovm = run_neovm_eval(&form).expect("NeoVM face surface evaluation");
    assert_eq!(
        neovm, gnu,
        "face subr surface mismatch:\nGNU: {}\nNeoVM: {}",
        gnu, neovm
    );
}
