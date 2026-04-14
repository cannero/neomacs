use super::*;
use libloading::Library;
use std::path::{Path, PathBuf};
use tree_sitter::{LANGUAGE_VERSION, Language, MIN_COMPATIBLE_LANGUAGE_VERSION, Parser};
use tree_sitter_language::LanguageFn;

#[derive(Debug)]
struct LoadedLanguage {
    language: Language,
    filename: Option<String>,
    _library: Library,
}

fn default_dynamic_library_suffixes() -> &'static [&'static str] {
    #[cfg(target_os = "windows")]
    {
        &[".dll"]
    }
    #[cfg(target_os = "macos")]
    {
        &[".dylib"]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        &[".so"]
    }
}

fn posix_versioned_candidates(base: &str, suffix: &str) -> Vec<String> {
    #[cfg(unix)]
    {
        #[cfg(target_os = "macos")]
        {
            vec![format!("{base}{suffix}")]
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mut out = vec![format!("{base}{suffix}")];
            out.push(format!("{base}{suffix}.0"));
            out.push(format!("{base}{suffix}.0.0"));
            for version in MIN_COMPATIBLE_LANGUAGE_VERSION..=LANGUAGE_VERSION {
                out.push(format!("{base}{suffix}.{version}.0"));
            }
            out
        }
    }
    #[cfg(not(unix))]
    {
        vec![format!("{base}{suffix}")]
    }
}

fn treesit_error_data(kind: &str, detail: impl Into<String>) -> Value {
    Value::list(vec![Value::symbol(kind), Value::string(detail.into())])
}

fn parse_symbol_arg(name: &str, value: &Value) -> Result<String, Flow> {
    value.as_symbol_name().map(str::to_owned).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *value, Value::symbol(name)],
        )
    })
}

fn list_value_to_strings(value: Option<Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    crate::emacs_core::value::list_to_vec(&value)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| item.as_str_owned())
        .collect()
}

fn list_assoc_symbol_key(value: Option<Value>, key: &str) -> Option<Vec<Value>> {
    let list = crate::emacs_core::value::list_to_vec(&value?)?;
    for entry in list {
        let items = crate::emacs_core::value::list_to_vec(&entry)?;
        let Some(lang) = items.first().and_then(|v| v.as_symbol_name()) else {
            continue;
        };
        if lang == key {
            return Some(items);
        }
    }
    None
}

fn maybe_remap_language(eval: &super::eval::Context, language: &str) -> String {
    let remapped =
        super::misc_eval::dynamic_or_global_symbol_value(eval, "treesit-language-remap-alist");
    let Some(items) = list_assoc_symbol_key(remapped, language) else {
        return language.to_owned();
    };
    items
        .get(1)
        .and_then(|v| v.as_symbol_name())
        .unwrap_or(language)
        .to_owned()
}

fn treesit_override_names(eval: &super::eval::Context, language: &str) -> Option<(String, String)> {
    let overrides =
        super::misc_eval::dynamic_or_global_symbol_value(eval, "treesit-load-name-override-list");
    let items = list_assoc_symbol_key(overrides, language)?;
    let lib_name = items.get(1)?.as_str_owned()?;
    let c_symbol = items.get(2)?.as_str_owned()?;
    Some((lib_name, c_symbol))
}

fn treesit_user_emacs_dir(eval: &super::eval::Context) -> Option<String> {
    super::misc_eval::dynamic_or_global_symbol_value(eval, "user-emacs-directory")
        .and_then(|value| value.as_str_owned())
}

fn treesit_candidate_paths(eval: &super::eval::Context, language: &str) -> Vec<String> {
    let remapped_language = maybe_remap_language(eval, language);
    let default_lib_base = format!("libtree-sitter-{remapped_language}");
    let default_c_symbol = format!("tree_sitter_{}", remapped_language.replace('-', "_"));
    let (lib_base_name, _c_symbol) = treesit_override_names(eval, &remapped_language)
        .unwrap_or((default_lib_base, default_c_symbol));

    let mut candidates = Vec::new();

    for suffix in default_dynamic_library_suffixes() {
        candidates.extend(posix_versioned_candidates(&lib_base_name, suffix));
    }

    if let Some(user_emacs_dir) = treesit_user_emacs_dir(eval) {
        let base = Path::new(&user_emacs_dir)
            .join("tree-sitter")
            .join(&lib_base_name);
        let base = base.to_string_lossy().into_owned();
        for suffix in default_dynamic_library_suffixes() {
            candidates.extend(posix_versioned_candidates(&base, suffix));
        }
    }

    for dir in list_value_to_strings(super::misc_eval::dynamic_or_global_symbol_value(
        eval,
        "treesit-extra-load-path",
    )) {
        let base = Path::new(&dir).join(&lib_base_name);
        let base = base.to_string_lossy().into_owned();
        for suffix in default_dynamic_library_suffixes() {
            candidates.extend(posix_versioned_candidates(&base, suffix));
        }
    }

    candidates
}

fn load_language_from_path(path: &str, c_symbol: &str) -> Result<LoadedLanguage, String> {
    let library = unsafe { Library::new(path) }.map_err(|err| err.to_string())?;
    let symbol_name = format!("{c_symbol}\0");
    let lang_fn = unsafe {
        library
            .get::<unsafe extern "C" fn() -> *const ()>(symbol_name.as_bytes())
            .map_err(|err| err.to_string())?
    };
    let language = Language::new(unsafe { LanguageFn::from_raw(*lang_fn) });
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|err| format!("ABI mismatch: {err}"))?;
    let filename = PathBuf::from(path)
        .canonicalize()
        .ok()
        .map(|resolved| resolved.to_string_lossy().into_owned())
        .or_else(|| Some(path.to_owned()));
    Ok(LoadedLanguage {
        language,
        filename,
        _library: library,
    })
}

fn load_language(eval: &super::eval::Context, language: &str) -> Result<LoadedLanguage, Value> {
    let remapped_language = maybe_remap_language(eval, language);
    let default_lib_base = format!("libtree-sitter-{remapped_language}");
    let default_c_symbol = format!("tree_sitter_{}", remapped_language.replace('-', "_"));
    let (_lib_base_name, c_symbol) = treesit_override_names(eval, &remapped_language)
        .unwrap_or((default_lib_base, default_c_symbol));

    let candidates = treesit_candidate_paths(eval, language);
    let mut errors = Vec::new();
    for candidate in candidates {
        match load_language_from_path(&candidate, &c_symbol) {
            Ok(language) => return Ok(language),
            Err(err) => errors.push(err),
        }
    }

    Err(Value::list(
        std::iter::once(Value::symbol("not-found"))
            .chain(errors.into_iter().map(Value::string))
            .collect(),
    ))
}

// =========================================================================
// treesit.c stubs
// =========================================================================

pub(crate) fn builtin_treesit_available_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-available-p", &args, 0)?;
    Ok(Value::T)
}

pub(crate) fn builtin_treesit_compiled_query_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-compiled-query-p", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_induce_sparse_tree(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-induce-sparse-tree", &args, 2, 4)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_language_abi_version(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-language-abi-version", &args, 0, 1)?;
    let Some(language_arg) = args.first() else {
        return Ok(Value::NIL);
    };
    if language_arg.is_nil() {
        return Ok(Value::NIL);
    }
    let language = parse_symbol_arg("treesit-language-abi-version", language_arg)?;
    match load_language(eval, &language) {
        Ok(loaded) => Ok(Value::fixnum(loaded.language.abi_version() as i64)),
        Err(_) => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_treesit_language_available_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-language-available-p", &args, 1, 2)?;
    let language = parse_symbol_arg("treesit-language-available-p", &args[0])?;
    let detail = args.get(1).is_some_and(|value| !value.is_nil());
    match load_language(eval, &language) {
        Ok(_) if detail => Ok(Value::cons(Value::T, Value::NIL)),
        Ok(_) => Ok(Value::T),
        Err(data) if detail => Ok(Value::cons(Value::NIL, data)),
        Err(_) => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_treesit_library_abi_version(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-library-abi-version", &args, 0, 1)?;
    if args.first().is_some_and(|value| !value.is_nil()) {
        Ok(Value::fixnum(MIN_COMPATIBLE_LANGUAGE_VERSION as i64))
    } else {
        Ok(Value::fixnum(LANGUAGE_VERSION as i64))
    }
}

pub(crate) fn builtin_treesit_node_check(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-check", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_child(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-child", &args, 2, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_child_by_field_name(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-child-by-field-name", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_child_count(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-child-count", &args, 1, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_descendant_for_range(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-descendant-for-range", &args, 3, 4)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_end(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-end", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_eq(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-eq", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_field_name_for_child(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-field-name-for-child", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_first_child_for_pos(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-first-child-for-pos", &args, 2, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_match_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-match-p", &args, 2, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_next_sibling(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-next-sibling", &args, 1, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-p", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_parent(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-parent", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_parser(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-parser", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_prev_sibling(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-prev-sibling", &args, 1, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_start(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-start", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_string(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-string", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_type(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-type", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_add_notifier(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-add-notifier", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_buffer(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-buffer", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_create(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-parser-create", &args, 1, 4)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_delete(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-delete", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_included_ranges(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-included-ranges", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_language(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-language", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_list(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-parser-list", &args, 0, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_notifiers(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-notifiers", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-p", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_remove_notifier(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-remove-notifier", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_root_node(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-root-node", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_set_included_ranges(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-set-included-ranges", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_tag(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-tag", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_pattern_expand(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-pattern-expand", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_query_capture(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-query-capture", &args, 2, 5)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_query_compile(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-query-compile", &args, 2, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_query_eagerly_compiled_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-eagerly-compiled-p", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_query_expand(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-expand", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_query_language(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-language", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_query_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-p", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_query_source(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-source", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_search_forward(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-search-forward", &args, 2, 4)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_search_subtree(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-search-subtree", &args, 2, 5)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_subtree_stat(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-subtree-stat", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_grammar_location(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-grammar-location", &args, 1)?;
    let language = parse_symbol_arg("treesit-grammar-location", &args[0])?;
    match load_language(eval, &language) {
        Ok(loaded) => Ok(loaded.filename.map_or(Value::NIL, Value::string)),
        Err(_) => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_treesit_tracking_line_column_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-tracking-line-column-p", &args, 0, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_tracking_line_column_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-tracking-line-column-p", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_embed_level(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-embed-level", &args, 1)?;
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_treesit_parser_set_embed_level(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-set-embed-level", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parse_string(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parse-string", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_changed_regions(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-changed-regions", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_linecol_at(args: Vec<Value>) -> EvalResult {
    expect_args("treesit--linecol-at", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_linecol_cache_set(args: Vec<Value>) -> EvalResult {
    expect_args("treesit--linecol-cache-set", &args, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_linecol_cache(args: Vec<Value>) -> EvalResult {
    expect_args("treesit--linecol-cache", &args, 0)?;
    Ok(Value::NIL)
}
