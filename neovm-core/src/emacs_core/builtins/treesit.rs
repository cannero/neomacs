use super::*;
use libloading::Library;
use std::path::{Path, PathBuf};
use tree_sitter::{LANGUAGE_VERSION, Language, MIN_COMPATIBLE_LANGUAGE_VERSION, Parser, Query};
use tree_sitter_language::LanguageFn;

use crate::buffer::{Buffer, BufferId};
use crate::emacs_core::builtins::buffers::expect_buffer_id;
use crate::emacs_core::emacs_char::byte_to_char_pos;
use crate::emacs_core::treesit::{
    self as runtime, NODE_SLOT_PARSER, PARSER_SLOT_BUFFER, PARSER_SLOT_EMBED_LEVEL,
    PARSER_SLOT_LANGUAGE, PARSER_SLOT_NOTIFIERS, PARSER_SLOT_TAG, ParserTagFilter,
    QUERY_SLOT_LANGUAGE, QUERY_SLOT_SOURCE,
};

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

fn parse_symbol_arg(name: &str, value: &Value) -> Result<String, Flow> {
    value.as_symbol_name().map(str::to_owned).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *value, Value::symbol(name)],
        )
    })
}

fn expect_symbol_or_nil(name: &str, value: Value) -> Result<Value, Flow> {
    if value.is_nil() || value.as_symbol_name().is_some() {
        Ok(value)
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), value, Value::symbol(name)],
        ))
    }
}

fn parser_type_error(name: &str, value: Value) -> Flow {
    signal(
        "wrong-type-argument",
        vec![
            Value::symbol("treesit-parser-p"),
            value,
            Value::symbol(name),
        ],
    )
}

fn node_type_error(name: &str, value: Value) -> Flow {
    signal(
        "wrong-type-argument",
        vec![Value::symbol("treesit-node-p"), value, Value::symbol(name)],
    )
}

fn query_type_error(name: &str, value: Value) -> Flow {
    signal(
        "wrong-type-argument",
        vec![Value::symbol("treesit-query-p"), value, Value::symbol(name)],
    )
}

fn compiled_query_type_error(name: &str, value: Value) -> Flow {
    signal(
        "wrong-type-argument",
        vec![
            Value::symbol("treesit-compiled-query-p"),
            value,
            Value::symbol(name),
        ],
    )
}

fn parser_deleted_error(value: Value) -> Flow {
    signal("treesit-parser-deleted", vec![value])
}

fn node_outdated_error(value: Value) -> Flow {
    signal("treesit-node-outdated", vec![value])
}

fn node_buffer_killed_error(value: Value) -> Flow {
    signal("treesit-node-buffer-killed", vec![value])
}

fn treesit_parse_error(value: Value) -> Flow {
    signal("treesit-parse-error", vec![value])
}

fn treesit_query_error(message: impl Into<String>) -> Flow {
    signal("treesit-query-error", vec![Value::string(message.into())])
}

fn treesit_query_error_from_query(err: tree_sitter::QueryError) -> Flow {
    signal(
        "treesit-query-error",
        vec![
            Value::string(err.message),
            Value::fixnum(err.offset as i64),
            Value::fixnum(err.row as i64),
            Value::fixnum(err.column as i64),
        ],
    )
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

fn load_language_from_path(path: &str, c_symbol: &str) -> Result<runtime::LoadedLanguage, String> {
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
    Ok(runtime::LoadedLanguage {
        language,
        filename,
        _library: library,
    })
}

fn load_language(
    eval: &mut super::eval::Context,
    language: &str,
) -> Result<(Language, Option<String>), Value> {
    let remapped_language = maybe_remap_language(eval, language);
    if let Some((loaded, filename)) = eval.treesit.loaded_language(&remapped_language) {
        return Ok((loaded, filename));
    }

    let default_lib_base = format!("libtree-sitter-{remapped_language}");
    let default_c_symbol = format!("tree_sitter_{}", remapped_language.replace('-', "_"));
    let (_lib_base_name, c_symbol) = treesit_override_names(eval, &remapped_language)
        .unwrap_or((default_lib_base, default_c_symbol));

    let candidates = treesit_candidate_paths(eval, language);
    let mut errors = Vec::new();
    for candidate in candidates {
        match load_language_from_path(&candidate, &c_symbol) {
            Ok(loaded) => {
                let result = (loaded.language.clone(), loaded.filename.clone());
                eval.treesit
                    .cache_loaded_language(remapped_language.clone(), loaded);
                return Ok(result);
            }
            Err(err) => errors.push(err),
        }
    }

    Err(Value::list(
        std::iter::once(Value::symbol("not-found"))
            .chain(errors.into_iter().map(Value::string))
            .collect(),
    ))
}

fn resolve_buffer_ids(
    eval: &super::eval::Context,
    arg: Option<&Value>,
) -> Result<(BufferId, BufferId, Value), Flow> {
    let orig_id = match arg {
        None => eval
            .buffers
            .current_buffer_id()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?,
        Some(value) if value.is_nil() => eval
            .buffers
            .current_buffer_id()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?,
        Some(value) => expect_buffer_id(value)?,
    };
    let orig_buffer = eval
        .buffers
        .get(orig_id)
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    let root_id = orig_buffer.base_buffer.unwrap_or(orig_id);
    Ok((orig_id, root_id, Value::make_buffer(orig_id)))
}

fn parser_record_slot(parser: Value, index: usize) -> Result<Value, Flow> {
    parser
        .as_record_data()
        .and_then(|items| items.get(index).copied())
        .ok_or_else(|| parser_type_error("treesit-parser-slot", parser))
}

fn query_record_slot(query: Value, index: usize) -> Result<Value, Flow> {
    query
        .as_record_data()
        .and_then(|items| items.get(index).copied())
        .ok_or_else(|| compiled_query_type_error("treesit-query-slot", query))
}

fn query_like_p(value: Value) -> bool {
    runtime::is_compiled_query(value) || value.is_cons() || value.is_string()
}

fn expect_parser_id(name: &str, value: Value) -> Result<u64, Flow> {
    runtime::parser_id(value).ok_or_else(|| parser_type_error(name, value))
}

fn expect_live_parser_id(
    eval: &super::eval::Context,
    name: &str,
    value: Value,
) -> Result<u64, Flow> {
    let id = expect_parser_id(name, value)?;
    let Some(entry) = eval.treesit.parser(id) else {
        return Err(parser_deleted_error(value));
    };
    if entry.deleted {
        return Err(parser_deleted_error(value));
    }
    Ok(id)
}

#[derive(Clone, Copy)]
struct NodeHandle {
    parser_id: u64,
    raw: tree_sitter::ffi::TSNode,
    generation: u64,
}

fn expect_node_handle(
    eval: &super::eval::Context,
    name: &str,
    value: Value,
) -> Result<NodeHandle, Flow> {
    let Some(id) = runtime::node_id(value) else {
        return Err(node_type_error(name, value));
    };
    let Some(entry) = eval.treesit.node(id) else {
        return Err(node_outdated_error(value));
    };
    Ok(NodeHandle {
        parser_id: entry.parser_id,
        raw: entry.raw,
        generation: entry.generation,
    })
}

fn ensure_current_node(
    eval: &super::eval::Context,
    name: &str,
    value: Value,
) -> Result<NodeHandle, Flow> {
    let handle = expect_node_handle(eval, name, value)?;
    let Some(parser) = eval.treesit.parser(handle.parser_id) else {
        return Err(node_outdated_error(value));
    };
    if parser.generation != handle.generation {
        return Err(node_outdated_error(value));
    }
    if eval.buffers.get(parser.orig_buffer_id).is_none() {
        return Err(node_buffer_killed_error(value));
    }
    Ok(handle)
}

fn parser_live_p(eval: &super::eval::Context, parser_id: u64) -> bool {
    let Some(parser) = eval.treesit.parser(parser_id) else {
        return false;
    };
    !parser.deleted && eval.buffers.get(parser.orig_buffer_id).is_some()
}

fn make_node_value_for_parser(
    eval: &mut super::eval::Context,
    parser_id: u64,
    node: tree_sitter::Node<'_>,
) -> Value {
    let (parser_value, generation) = {
        let parser = eval
            .treesit
            .parser(parser_id)
            .expect("parser should exist while creating node value");
        (parser.value, parser.generation)
    };
    let id = eval
        .treesit
        .insert_node(parser_id, node.into_raw(), generation);
    runtime::make_node_value(id, parser_value)
}

fn lisp_pos_to_relative_byte(buf: &Buffer, pos: i64) -> Result<usize, Flow> {
    let min = buf.point_min_char() as i64 + 1;
    let max = buf.point_max_char() as i64 + 1;
    if pos < min || pos > max {
        return Err(signal("args-out-of-range", vec![Value::fixnum(pos)]));
    }
    Ok(buf.lisp_pos_to_accessible_byte(pos) - buf.point_min_byte())
}

fn byte_offset_to_lisp_pos(buf: &Buffer, source: &str, byte_offset: usize) -> Value {
    let char_offset = byte_to_char_pos(source.as_bytes(), byte_offset) as i64;
    Value::fixnum(buf.point_min_char() as i64 + char_offset + 1)
}

fn ensure_parser_parsed(eval: &mut super::eval::Context, parser_id: u64) -> Result<(), Flow> {
    let (parser_value, orig_buffer_id) = {
        let parser = eval
            .treesit
            .parser(parser_id)
            .ok_or_else(|| signal("error", vec![Value::string("Missing tree-sitter parser")]))?;
        if parser.deleted {
            return Err(parser_deleted_error(parser.value));
        }
        (parser.value, parser.orig_buffer_id)
    };

    let source = {
        let buffer = eval.buffers.get(orig_buffer_id).ok_or_else(|| {
            signal(
                "error",
                vec![Value::string("Parser buffer has been killed")],
            )
        })?;
        buffer.buffer_string()
    };

    let mut reparsed = false;
    {
        let parser = eval
            .treesit
            .parser_mut(parser_id)
            .ok_or_else(|| signal("error", vec![Value::string("Missing tree-sitter parser")]))?;
        let needs_parse =
            parser.tree.is_none() || parser.last_source.as_deref() != Some(source.as_str());
        if needs_parse {
            let tree = parser
                .parser
                .parse(source.as_str(), parser.tree.as_ref())
                .ok_or_else(|| treesit_parse_error(parser_value))?;
            parser.tree = Some(tree);
            parser.last_source = Some(source);
            parser.generation = parser.generation.saturating_add(1);
            reparsed = true;
        }
    }
    if reparsed {
        eval.treesit.clear_nodes_for_parser(parser_id);
    }
    Ok(())
}

fn ensure_query_compiled(eval: &mut super::eval::Context, query: Value) -> Result<(), Flow> {
    let id = runtime::query_id(query)
        .ok_or_else(|| compiled_query_type_error("treesit-query-compile", query))?;
    if eval
        .treesit
        .query(id)
        .and_then(|entry| entry.compiled.as_ref())
        .is_some()
    {
        return Ok(());
    }

    let language = query_record_slot(query, QUERY_SLOT_LANGUAGE)?;
    let language_name = parse_symbol_arg("treesit-query-compile", &language)?;
    let source = query_record_slot(query, QUERY_SLOT_SOURCE)?;
    let source_string = if let Some(source) = source.as_str_owned() {
        source
    } else if source.is_cons() {
        return Err(treesit_query_error(
            "S-expression tree-sitter queries are not implemented yet",
        ));
    } else {
        return Err(query_type_error("treesit-query-compile", source));
    };

    let (lang, _) = load_language(eval, &language_name).map_err(|_| {
        treesit_query_error(format!(
            "Failed to load tree-sitter language `{language_name}`"
        ))
    })?;
    let compiled = Query::new(&lang, &source_string).map_err(treesit_query_error_from_query)?;
    let query_entry = eval
        .treesit
        .query_mut(id)
        .ok_or_else(|| compiled_query_type_error("treesit-query-compile", query))?;
    query_entry.compiled = Some(compiled);
    Ok(())
}

pub(crate) fn builtin_treesit_available_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-available-p", &args, 0)?;
    Ok(Value::T)
}

pub(crate) fn builtin_treesit_compiled_query_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-compiled-query-p", &args, 1)?;
    Ok(if runtime::is_compiled_query(args[0]) {
        Value::T
    } else {
        Value::NIL
    })
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
        Ok((loaded, _)) => Ok(Value::fixnum(loaded.abi_version() as i64)),
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

pub(crate) fn builtin_treesit_node_check(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-node-check", &args, 2)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let property = args[1].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![
                Value::symbol("symbolp"),
                args[1],
                Value::symbol("treesit-node-check"),
            ],
        )
    })?;

    let handle = expect_node_handle(eval, "treesit-node-check", args[0])?;
    let parser = eval
        .treesit
        .parser(handle.parser_id)
        .ok_or_else(|| node_outdated_error(args[0]))?;

    if property == "outdated" {
        return Ok(if parser.generation == handle.generation {
            Value::NIL
        } else {
            Value::T
        });
    }

    let node = ensure_current_node(eval, "treesit-node-check", args[0])?;
    let ts_node = unsafe { tree_sitter::Node::from_raw(node.raw) };
    let result = match property {
        "named" => ts_node.is_named(),
        "missing" => ts_node.is_missing(),
        "extra" => ts_node.is_extra(),
        "has-error" => ts_node.has_error(),
        "live" => parser_live_p(eval, handle.parser_id),
        _ => {
            return Err(signal(
                "error",
                vec![
                    Value::string(
                        "Expecting `named', `missing', `extra', `outdated', `has-error', or `live'",
                    ),
                    args[1],
                ],
            ));
        }
    };
    Ok(if result { Value::T } else { Value::NIL })
}

pub(crate) fn builtin_treesit_node_child(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-node-child", &args, 2, 3)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let handle = ensure_current_node(eval, "treesit-node-child", args[0])?;
    let mut idx = expect_int(&args[1])?;
    let named = args.get(2).is_some_and(|value| !value.is_nil());
    let node = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    let count = if named {
        node.named_child_count() as i64
    } else {
        node.child_count() as i64
    };
    if idx < 0 {
        idx += count;
    }
    if idx < 0 {
        return Ok(Value::NIL);
    }
    let idx = u32::try_from(idx).map_err(|_| signal("args-out-of-range", vec![args[1]]))?;
    let child = if named {
        node.named_child(idx)
    } else {
        node.child(idx)
    };
    Ok(child.map_or(Value::NIL, |child| {
        make_node_value_for_parser(eval, handle.parser_id, child)
    }))
}

pub(crate) fn builtin_treesit_node_child_by_field_name(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-node-child-by-field-name", &args, 2)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let handle = ensure_current_node(eval, "treesit-node-child-by-field-name", args[0])?;
    let field_name = expect_string(&args[1])?;
    let node = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    Ok(node
        .child_by_field_name(&field_name)
        .map_or(Value::NIL, |child| {
            make_node_value_for_parser(eval, handle.parser_id, child)
        }))
}

pub(crate) fn builtin_treesit_node_child_count(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-node-child-count", &args, 1, 2)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let handle = ensure_current_node(eval, "treesit-node-child-count", args[0])?;
    let named = args.get(1).is_some_and(|value| !value.is_nil());
    let node = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    Ok(Value::fixnum(if named {
        node.named_child_count() as i64
    } else {
        node.child_count() as i64
    }))
}

pub(crate) fn builtin_treesit_node_descendant_for_range(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-node-descendant-for-range", &args, 3, 4)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let handle = ensure_current_node(eval, "treesit-node-descendant-for-range", args[0])?;
    let parser = eval
        .treesit
        .parser(handle.parser_id)
        .ok_or_else(|| node_outdated_error(args[0]))?;
    let buf = eval
        .buffers
        .get(parser.orig_buffer_id)
        .ok_or_else(|| node_buffer_killed_error(args[0]))?;
    let start_byte = lisp_pos_to_relative_byte(buf, expect_integer_or_marker(&args[1])?)?;
    let end_byte = lisp_pos_to_relative_byte(buf, expect_integer_or_marker(&args[2])?)?;
    let named = args.get(3).is_some_and(|value| !value.is_nil());
    let node = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    let descendant = if named {
        node.named_descendant_for_byte_range(start_byte, end_byte)
    } else {
        node.descendant_for_byte_range(start_byte, end_byte)
    };
    Ok(descendant.map_or(Value::NIL, |child| {
        make_node_value_for_parser(eval, handle.parser_id, child)
    }))
}

pub(crate) fn builtin_treesit_node_end(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-node-end", &args, 1)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let handle = ensure_current_node(eval, "treesit-node-end", args[0])?;
    let parser = eval
        .treesit
        .parser(handle.parser_id)
        .ok_or_else(|| node_outdated_error(args[0]))?;
    let buf = eval
        .buffers
        .get(parser.orig_buffer_id)
        .ok_or_else(|| node_buffer_killed_error(args[0]))?;
    let source = parser
        .last_source
        .as_deref()
        .ok_or_else(|| node_outdated_error(args[0]))?;
    let node = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    Ok(byte_offset_to_lisp_pos(buf, source, node.end_byte()))
}

pub(crate) fn builtin_treesit_node_eq(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-node-eq", &args, 2)?;
    if args[0].is_nil() || args[1].is_nil() {
        return Ok(Value::NIL);
    }
    let left = ensure_current_node(eval, "treesit-node-eq", args[0])?;
    let right = ensure_current_node(eval, "treesit-node-eq", args[1])?;
    let equal = left.parser_id == right.parser_id
        && left.generation == right.generation
        && unsafe {
            tree_sitter::Node::from_raw(left.raw) == tree_sitter::Node::from_raw(right.raw)
        };
    Ok(if equal { Value::T } else { Value::NIL })
}

pub(crate) fn builtin_treesit_node_field_name_for_child(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-node-field-name-for-child", &args, 2)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let handle = ensure_current_node(eval, "treesit-node-field-name-for-child", args[0])?;
    let mut idx = expect_int(&args[1])?;
    let node = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    let count = node.child_count() as i64;
    if idx < 0 {
        idx += count;
    }
    if idx < 0 {
        return Ok(Value::NIL);
    }
    let idx = u32::try_from(idx).map_err(|_| signal("args-out-of-range", vec![args[1]]))?;
    Ok(node
        .field_name_for_child(idx)
        .map_or(Value::NIL, Value::string))
}

pub(crate) fn builtin_treesit_node_first_child_for_pos(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-node-first-child-for-pos", &args, 2, 3)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let handle = ensure_current_node(eval, "treesit-node-first-child-for-pos", args[0])?;
    let parser = eval
        .treesit
        .parser(handle.parser_id)
        .ok_or_else(|| node_outdated_error(args[0]))?;
    let buf = eval
        .buffers
        .get(parser.orig_buffer_id)
        .ok_or_else(|| node_buffer_killed_error(args[0]))?;
    let byte = lisp_pos_to_relative_byte(buf, expect_integer_or_marker(&args[1])?)?;
    let named = args.get(2).is_some_and(|value| !value.is_nil());
    let node = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    let child = if named {
        node.first_named_child_for_byte(byte)
    } else {
        node.first_child_for_byte(byte)
    };
    Ok(child.map_or(Value::NIL, |child| {
        make_node_value_for_parser(eval, handle.parser_id, child)
    }))
}

pub(crate) fn builtin_treesit_node_match_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-match-p", &args, 2, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_node_next_sibling(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-node-next-sibling", &args, 1, 2)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let handle = ensure_current_node(eval, "treesit-node-next-sibling", args[0])?;
    let named = args.get(1).is_some_and(|value| !value.is_nil());
    let node = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    let sibling = if named {
        node.next_named_sibling()
    } else {
        node.next_sibling()
    };
    Ok(sibling.map_or(Value::NIL, |sibling| {
        make_node_value_for_parser(eval, handle.parser_id, sibling)
    }))
}

pub(crate) fn builtin_treesit_node_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-p", &args, 1)?;
    Ok(if runtime::is_node(args[0]) {
        Value::T
    } else {
        Value::NIL
    })
}

pub(crate) fn builtin_treesit_node_parent(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-node-parent", &args, 1)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let handle = ensure_current_node(eval, "treesit-node-parent", args[0])?;
    let node = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    Ok(node.parent().map_or(Value::NIL, |parent| {
        make_node_value_for_parser(eval, handle.parser_id, parent)
    }))
}

pub(crate) fn builtin_treesit_node_parser(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-parser", &args, 1)?;
    let Some(node) = args[0].as_record_data() else {
        return Err(node_type_error("treesit-node-parser", args[0]));
    };
    Ok(node.get(NODE_SLOT_PARSER).copied().unwrap_or(Value::NIL))
}

pub(crate) fn builtin_treesit_node_prev_sibling(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-node-prev-sibling", &args, 1, 2)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let handle = ensure_current_node(eval, "treesit-node-prev-sibling", args[0])?;
    let named = args.get(1).is_some_and(|value| !value.is_nil());
    let node = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    let sibling = if named {
        node.prev_named_sibling()
    } else {
        node.prev_sibling()
    };
    Ok(sibling.map_or(Value::NIL, |sibling| {
        make_node_value_for_parser(eval, handle.parser_id, sibling)
    }))
}

pub(crate) fn builtin_treesit_node_start(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-node-start", &args, 1)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let handle = ensure_current_node(eval, "treesit-node-start", args[0])?;
    let parser = eval
        .treesit
        .parser(handle.parser_id)
        .ok_or_else(|| node_outdated_error(args[0]))?;
    let buf = eval
        .buffers
        .get(parser.orig_buffer_id)
        .ok_or_else(|| node_buffer_killed_error(args[0]))?;
    let source = parser
        .last_source
        .as_deref()
        .ok_or_else(|| node_outdated_error(args[0]))?;
    let node = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    Ok(byte_offset_to_lisp_pos(buf, source, node.start_byte()))
}

pub(crate) fn builtin_treesit_node_string(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-node-string", &args, 1)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let handle = ensure_current_node(eval, "treesit-node-string", args[0])?;
    let node = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    Ok(Value::string(node.to_sexp()))
}

pub(crate) fn builtin_treesit_node_type(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-node-type", &args, 1)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let handle = ensure_current_node(eval, "treesit-node-type", args[0])?;
    let node = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    Ok(Value::string(node.kind()))
}

pub(crate) fn builtin_treesit_parser_add_notifier(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-add-notifier", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_buffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-buffer", &args, 1)?;
    let parser_id = expect_live_parser_id(eval, "treesit-parser-buffer", args[0])?;
    let parser = eval
        .treesit
        .parser(parser_id)
        .ok_or_else(|| parser_deleted_error(args[0]))?;
    Ok(Value::make_buffer(parser.orig_buffer_id))
}

pub(crate) fn builtin_treesit_parser_create(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-parser-create", &args, 1, 4)?;
    let language = parse_symbol_arg("treesit-parser-create", &args[0])?;
    let tag = expect_symbol_or_nil(
        "treesit-parser-create",
        args.get(3).copied().unwrap_or(Value::NIL),
    )?;
    if tag.is_t() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::list(vec![Value::symbol("not"), Value::T]), Value::T],
        ));
    }

    let (orig_buffer_id, root_buffer_id, buffer_value) = resolve_buffer_ids(eval, args.get(1))?;
    if args.get(2).is_none_or(|value| value.is_nil()) {
        if let Some(existing) = eval
            .treesit
            .find_reusable_parser(orig_buffer_id, &language, tag)
        {
            return Ok(existing);
        }
    }

    let (loaded_language, _) = load_language(eval, &language).map_err(|detail| {
        signal(
            "error",
            vec![
                Value::string(format!("Failed to load tree-sitter language `{language}`")),
                detail,
            ],
        )
    })?;
    let mut parser = Parser::new();
    parser
        .set_language(&loaded_language)
        .map_err(|err| signal("error", vec![Value::string(format!("ABI mismatch: {err}"))]))?;

    let placeholder = Value::NIL;
    let id = eval.treesit.insert_parser(
        placeholder,
        orig_buffer_id,
        root_buffer_id,
        language.clone(),
        tag,
        parser,
    );
    let value = runtime::make_parser_value(id, Value::symbol(&language), buffer_value, tag);
    let entry = eval.treesit.parser_mut(id).ok_or_else(|| {
        signal(
            "error",
            vec![Value::string("Failed to register tree-sitter parser")],
        )
    })?;
    entry.value = value;
    Ok(value)
}

pub(crate) fn builtin_treesit_parser_delete(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-delete", &args, 1)?;
    let parser_id = expect_live_parser_id(eval, "treesit-parser-delete", args[0])?;
    let _ = eval.treesit.mark_parser_deleted(parser_id);
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_included_ranges(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-included-ranges", &args, 1)?;
    let _ = expect_live_parser_id(eval, "treesit-parser-included-ranges", args[0])?;
    parser_record_slot(args[0], runtime::PARSER_SLOT_INCLUDED_RANGES)
}

pub(crate) fn builtin_treesit_parser_language(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-language", &args, 1)?;
    let _ = expect_live_parser_id(eval, "treesit-parser-language", args[0])?;
    parser_record_slot(args[0], PARSER_SLOT_LANGUAGE)
}

pub(crate) fn builtin_treesit_parser_list(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-parser-list", &args, 0, 3)?;
    let (orig_buffer_id, root_buffer_id, _) = resolve_buffer_ids(eval, args.first())?;
    let language = match args.get(1).copied().unwrap_or(Value::NIL) {
        value if value.is_nil() => None,
        value => Some(parse_symbol_arg("treesit-parser-list", &value)?),
    };
    let tag = args.get(2).copied().unwrap_or(Value::NIL);
    let tag_filter = if tag.is_t() {
        ParserTagFilter::Any
    } else {
        expect_symbol_or_nil("treesit-parser-list", tag)?;
        ParserTagFilter::Exact(tag)
    };
    let items = eval.treesit.parser_values_for(
        root_buffer_id,
        orig_buffer_id,
        language.as_deref(),
        tag_filter,
    );
    Ok(Value::list(items))
}

pub(crate) fn builtin_treesit_parser_notifiers(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-notifiers", &args, 1)?;
    let _ = expect_live_parser_id(eval, "treesit-parser-notifiers", args[0])?;
    parser_record_slot(args[0], PARSER_SLOT_NOTIFIERS)
}

pub(crate) fn builtin_treesit_parser_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-p", &args, 1)?;
    Ok(if runtime::is_parser(args[0]) {
        Value::T
    } else {
        Value::NIL
    })
}

pub(crate) fn builtin_treesit_parser_remove_notifier(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-remove-notifier", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_root_node(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-root-node", &args, 1)?;
    let parser_id = expect_live_parser_id(eval, "treesit-parser-root-node", args[0])?;
    ensure_parser_parsed(eval, parser_id)?;
    let root = {
        let parser = eval
            .treesit
            .parser(parser_id)
            .ok_or_else(|| parser_deleted_error(args[0]))?;
        parser
            .tree
            .as_ref()
            .map(|tree| tree.root_node())
            .map(tree_sitter::Node::into_raw)
            .ok_or_else(|| treesit_parse_error(args[0]))?
    };
    Ok(make_node_value_for_parser(eval, parser_id, unsafe {
        tree_sitter::Node::from_raw(root)
    }))
}

pub(crate) fn builtin_treesit_parser_set_included_ranges(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-set-included-ranges", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_tag(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-tag", &args, 1)?;
    let _ = expect_live_parser_id(eval, "treesit-parser-tag", args[0])?;
    parser_record_slot(args[0], PARSER_SLOT_TAG)
}

pub(crate) fn builtin_treesit_pattern_expand(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-pattern-expand", &args, 1)?;
    Ok(args[0])
}

pub(crate) fn builtin_treesit_query_capture(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-query-capture", &args, 2, 5)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_query_compile(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-query-compile", &args, 2, 3)?;
    let language = expect_symbol_or_nil("treesit-query-compile", args[0])?;
    if language.is_nil() {
        return Err(query_type_error("treesit-query-compile", language));
    }
    let language_name = parse_symbol_arg("treesit-query-compile", &language)?;
    let query = args[1];
    let eager = args.get(2).is_some_and(|value| !value.is_nil());

    if runtime::is_compiled_query(query) {
        if eager {
            ensure_query_compiled(eval, query)?;
        }
        return Ok(query);
    }

    if !query_like_p(query) {
        return Err(query_type_error("treesit-query-compile", query));
    }

    let id = eval.treesit.insert_query(language_name);
    let value = runtime::make_query_value(id, language, query);
    if eager {
        ensure_query_compiled(eval, value)?;
    }
    Ok(value)
}

pub(crate) fn builtin_treesit_query_eagerly_compiled_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-query-eagerly-compiled-p", &args, 1)?;
    let id = runtime::query_id(args[0])
        .ok_or_else(|| compiled_query_type_error("treesit-query-eagerly-compiled-p", args[0]))?;
    let compiled = eval
        .treesit
        .query(id)
        .and_then(|entry| entry.compiled.as_ref())
        .is_some();
    Ok(if compiled { Value::T } else { Value::NIL })
}

pub(crate) fn builtin_treesit_query_expand(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-expand", &args, 1)?;
    Ok(match args[0] {
        value if runtime::is_compiled_query(value) => query_record_slot(value, QUERY_SLOT_SOURCE)?,
        value if query_like_p(value) => value,
        value => return Err(query_type_error("treesit-query-expand", value)),
    })
}

pub(crate) fn builtin_treesit_query_language(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-language", &args, 1)?;
    let _ = runtime::query_id(args[0])
        .ok_or_else(|| compiled_query_type_error("treesit-query-language", args[0]))?;
    query_record_slot(args[0], QUERY_SLOT_LANGUAGE)
}

pub(crate) fn builtin_treesit_query_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-p", &args, 1)?;
    Ok(if query_like_p(args[0]) {
        Value::T
    } else {
        Value::NIL
    })
}

pub(crate) fn builtin_treesit_query_source(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-source", &args, 1)?;
    let _ = runtime::query_id(args[0])
        .ok_or_else(|| compiled_query_type_error("treesit-query-source", args[0]))?;
    query_record_slot(args[0], QUERY_SLOT_SOURCE)
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
        Ok((_, filename)) => Ok(filename.map_or(Value::NIL, Value::string)),
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

pub(crate) fn builtin_treesit_parser_embed_level(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-embed-level", &args, 1)?;
    let _ = expect_live_parser_id(eval, "treesit-parser-embed-level", args[0])?;
    parser_record_slot(args[0], PARSER_SLOT_EMBED_LEVEL)
}

pub(crate) fn builtin_treesit_parser_set_embed_level(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-set-embed-level", &args, 2)?;
    let _ = expect_live_parser_id(eval, "treesit-parser-set-embed-level", args[0])?;
    let level = if args[1].is_nil() {
        Value::NIL
    } else {
        let level = expect_wholenump(&args[1])?;
        Value::fixnum(level as i64)
    };
    if !args[0].set_record_slot(PARSER_SLOT_EMBED_LEVEL, level) {
        return Err(signal(
            "error",
            vec![Value::string("Failed to update parser embed level")],
        ));
    }
    Ok(level)
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
