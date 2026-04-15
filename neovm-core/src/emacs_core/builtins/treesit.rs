use super::*;
use ::regex::Regex;
use libloading::Library;
use std::path::{Path, PathBuf};
use tree_sitter::{
    LANGUAGE_VERSION, Language, MIN_COMPATIBLE_LANGUAGE_VERSION, Parser, Point, Query, QueryCursor,
    Range as TSRange, StreamingIterator,
};
use tree_sitter_language::LanguageFn;

use crate::buffer::{Buffer, BufferId};
use crate::emacs_core::builtins::buffers::expect_buffer_id;
use crate::emacs_core::emacs_char::byte_to_char_pos;
use crate::emacs_core::intern::{SymId, resolve_sym};
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

fn parse_symbol_arg(name: &str, value: &Value) -> Result<SymId, Flow> {
    value.as_symbol_id().ok_or_else(|| {
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

fn treesit_buffer_source(buffer: &crate::buffer::Buffer) -> String {
    buffer.text.text_range(buffer.begv_byte, buffer.zv_byte)
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

fn list_assoc_symbol_key(value: Option<Value>, key: SymId) -> Option<Vec<Value>> {
    let list = crate::emacs_core::value::list_to_vec(&value?)?;
    for entry in list {
        let items = crate::emacs_core::value::list_to_vec(&entry)?;
        let Some(lang) = items.first().and_then(|v| v.as_symbol_id()) else {
            continue;
        };
        if lang == key {
            return Some(items);
        }
    }
    None
}

fn maybe_remap_language(eval: &super::eval::Context, language: SymId) -> SymId {
    let remapped =
        super::misc_eval::dynamic_or_global_symbol_value(eval, "treesit-language-remap-alist");
    let Some(items) = list_assoc_symbol_key(remapped, language) else {
        return language;
    };
    items
        .get(1)
        .and_then(|v| v.as_symbol_id())
        .unwrap_or(language)
}

fn language_requires_linecol_tracking(eval: &super::eval::Context, language: SymId) -> bool {
    let remapped = maybe_remap_language(eval, language);
    let Some(languages) = super::misc_eval::dynamic_or_global_symbol_value(
        eval,
        "treesit-languages-require-line-column-tracking",
    ) else {
        return false;
    };
    crate::emacs_core::value::list_to_vec(&languages)
        .unwrap_or_default()
        .into_iter()
        .any(|value| value.as_symbol_id().is_some_and(|name| name == remapped))
}

fn treesit_override_names(
    eval: &super::eval::Context,
    language: SymId,
) -> Option<(String, String)> {
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

fn treesit_candidate_paths(eval: &super::eval::Context, language: SymId) -> Vec<String> {
    let remapped_language = maybe_remap_language(eval, language);
    let remapped_name = resolve_sym(remapped_language);
    let default_lib_base = format!("libtree-sitter-{remapped_name}");
    let default_c_symbol = format!("tree_sitter_{}", remapped_name.replace('-', "_"));
    let (lib_base_name, _c_symbol) = treesit_override_names(eval, remapped_language)
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
        _library: Some(library),
    })
}

fn load_language(
    eval: &mut super::eval::Context,
    language: SymId,
) -> Result<(Language, Option<String>), Value> {
    let remapped_language = maybe_remap_language(eval, language);
    if let Some((loaded, filename)) = eval.treesit.loaded_language(remapped_language) {
        return Ok((loaded, filename));
    }

    let remapped_name = resolve_sym(remapped_language);
    let default_lib_base = format!("libtree-sitter-{remapped_name}");
    let default_c_symbol = format!("tree_sitter_{}", remapped_name.replace('-', "_"));
    let (_lib_base_name, c_symbol) = treesit_override_names(eval, remapped_language)
        .unwrap_or((default_lib_base, default_c_symbol));

    let candidates = treesit_candidate_paths(eval, language);
    let mut errors = Vec::new();
    for candidate in candidates {
        match load_language_from_path(&candidate, &c_symbol) {
            Ok(loaded) => {
                let result = (loaded.language.clone(), loaded.filename.clone());
                eval.treesit
                    .cache_loaded_language(remapped_language, loaded);
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
        treesit_buffer_source(buffer)
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
    let language_sym = parse_symbol_arg("treesit-query-compile", &language)?;
    let source = query_record_slot(query, QUERY_SLOT_SOURCE)?;
    let source_string = expand_query_value("treesit-query-compile", source)?;

    let (lang, _) = load_language(eval, language_sym).map_err(|_| {
        treesit_query_error(format!(
            "Failed to load tree-sitter language `{}`",
            resolve_sym(language_sym)
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

fn ensure_parser_parsed_with_changes(
    eval: &mut super::eval::Context,
    parser_id: u64,
) -> Result<Option<Vec<(usize, usize)>>, Flow> {
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
        treesit_buffer_source(buffer)
    };

    let (changed_ranges, reparsed) = {
        let parser = eval
            .treesit
            .parser_mut(parser_id)
            .ok_or_else(|| signal("error", vec![Value::string("Missing tree-sitter parser")]))?;
        let needs_parse =
            parser.tree.is_none() || parser.last_source.as_deref() != Some(source.as_str());
        if !needs_parse {
            return Ok(None);
        }

        let old_tree = parser.tree.clone();
        let tree = parser
            .parser
            .parse(source.as_str(), parser.tree.as_ref())
            .ok_or_else(|| treesit_parse_error(parser_value))?;
        let ranges = if let Some(old_tree) = old_tree.as_ref() {
            old_tree
                .changed_ranges(&tree)
                .map(|range| (range.start_byte, range.end_byte))
                .collect::<Vec<_>>()
        } else if source.is_empty() {
            Vec::new()
        } else {
            vec![(0, source.len())]
        };
        parser.tree = Some(tree);
        parser.last_source = Some(source);
        parser.generation = parser.generation.saturating_add(1);
        parser.last_changed_ranges = ranges.clone();
        (Some(ranges), true)
    };
    if reparsed {
        eval.treesit.clear_nodes_for_parser(parser_id);
    }
    Ok(changed_ranges)
}

fn expand_query_string(source: &str) -> String {
    let mut expanded = String::with_capacity(source.len() + 2);
    expanded.push('"');
    for ch in source.chars() {
        match ch {
            '\0' => expanded.push_str("\\0"),
            '\n' => expanded.push_str("\\n"),
            '\r' => expanded.push_str("\\r"),
            '\t' => expanded.push_str("\\t"),
            '"' | '\\' => {
                expanded.push('\\');
                expanded.push(ch);
            }
            _ => expanded.push(ch),
        }
    }
    expanded.push('"');
    expanded
}

fn pattern_keyword_expansion(name: &str) -> Option<&'static str> {
    match name {
        ":anchor" => Some("."),
        ":?" => Some("?"),
        ":*" => Some("*"),
        ":+" => Some("+"),
        ":equal" | ":eq?" => Some("#eq?"),
        ":match" | ":match?" => Some("#match?"),
        ":pred" | ":pred?" => Some("#pred?"),
        _ => None,
    }
}

fn expand_pattern_value(pattern: Value) -> Result<String, Flow> {
    if let Some(name) = pattern.as_symbol_name() {
        if let Some(expanded) = pattern_keyword_expansion(name) {
            return Ok(expanded.to_string());
        }
    }

    if let Some(text) = pattern.as_str_owned() {
        return Ok(expand_query_string(&text));
    }

    if let Some(items) = pattern.as_vector_data() {
        let mut pieces = Vec::with_capacity(items.len());
        for item in items {
            pieces.push(expand_pattern_value(*item)?);
        }
        return Ok(format!("[{}]", pieces.join(" ")));
    }

    if let Some(items) = crate::emacs_core::value::list_to_vec(&pattern) {
        let mut pieces = Vec::with_capacity(items.len());
        for item in items {
            pieces.push(expand_pattern_value(item)?);
        }
        return Ok(format!("({})", pieces.join(" ")));
    }

    Ok(crate::emacs_core::print::print_value(&pattern))
}

fn expand_query_value(caller: &str, query: Value) -> Result<String, Flow> {
    if let Some(source) = query.as_str_owned() {
        return Ok(source);
    }
    if let Some(items) = crate::emacs_core::value::list_to_vec(&query) {
        let mut pieces = Vec::with_capacity(items.len());
        for item in items {
            pieces.push(expand_pattern_value(item)?);
        }
        return Ok(pieces.join(" "));
    }
    Err(query_type_error(caller, query))
}

fn byte_offset_to_linecol(
    source: &str,
    byte_offset: usize,
    hint: runtime::LineColCache,
) -> runtime::LineColCache {
    let bytes = source.as_bytes();
    let target = byte_offset.min(bytes.len());
    let (mut line, mut col, mut idx) =
        if hint.bytepos <= target && hint.bytepos <= bytes.len() && hint.line > 0 && hint.col > 0 {
            (hint.line, hint.col, hint.bytepos)
        } else {
            (1, 1, 0)
        };

    while idx < target {
        if bytes[idx] == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
        idx += 1;
    }

    runtime::LineColCache {
        line,
        col,
        bytepos: target,
    }
}

fn byte_offset_to_point(source: &str, byte_offset: usize, hint: runtime::LineColCache) -> Point {
    let linecol = byte_offset_to_linecol(source, byte_offset, hint);
    Point {
        row: linecol.line.saturating_sub(1) as usize,
        column: linecol.col.saturating_sub(1) as usize,
    }
}

fn query_range_bytes(
    buf: &Buffer,
    args: &[Value],
    beg_index: usize,
    end_index: usize,
) -> Result<Option<std::ops::Range<usize>>, Flow> {
    let Some(beg) = args.get(beg_index).copied() else {
        return Ok(None);
    };
    let Some(end) = args.get(end_index).copied() else {
        return Ok(None);
    };
    if beg.is_nil() || end.is_nil() {
        return Ok(None);
    }
    let start = lisp_pos_to_relative_byte(buf, expect_integer_or_marker(&beg)?)?;
    let finish = lisp_pos_to_relative_byte(buf, expect_integer_or_marker(&end)?)?;
    Ok(Some(start..finish))
}

fn changed_ranges_to_lisp(
    eval: &super::eval::Context,
    parser_id: u64,
    changed_ranges: &[(usize, usize)],
) -> Result<Value, Flow> {
    let parser = eval
        .treesit
        .parser(parser_id)
        .ok_or_else(|| signal("error", vec![Value::string("Missing tree-sitter parser")]))?;
    let buf = eval.buffers.get(parser.orig_buffer_id).ok_or_else(|| {
        signal(
            "error",
            vec![Value::string("Parser buffer has been killed")],
        )
    })?;
    let source = parser
        .last_source
        .as_deref()
        .ok_or_else(|| treesit_parse_error(parser.value))?;
    Ok(Value::list(
        changed_ranges
            .iter()
            .map(|(start, end)| {
                Value::cons(
                    byte_offset_to_lisp_pos(buf, source, *start),
                    byte_offset_to_lisp_pos(buf, source, *end),
                )
            })
            .collect(),
    ))
}

fn resolve_compiled_query_value(
    eval: &mut super::eval::Context,
    language_symbol: Value,
    query: Value,
    caller: &str,
) -> Result<Value, Flow> {
    let language_sym = parse_symbol_arg(caller, &language_symbol)?;
    let compiled_query = if runtime::is_compiled_query(query) {
        query
    } else {
        builtin_treesit_query_compile(eval, vec![language_symbol, query, Value::T])?
    };
    let query_language = query_record_slot(compiled_query, QUERY_SLOT_LANGUAGE)?;
    if query_language != language_symbol {
        return Err(treesit_query_error(format!(
            "Query language mismatch: expected `{}`",
            resolve_sym(language_sym)
        )));
    }
    ensure_query_compiled(eval, compiled_query)?;
    Ok(compiled_query)
}

#[derive(Clone, Copy)]
struct ResolvedNodeInput {
    parser_id: u64,
    parser_value: Value,
    language_symbol: Value,
    node_value: Value,
    node_raw: tree_sitter::ffi::TSNode,
}

fn resolve_node_input(
    eval: &mut super::eval::Context,
    value: Value,
    caller: &str,
) -> Result<ResolvedNodeInput, Flow> {
    if runtime::is_node(value) {
        let handle = ensure_current_node(eval, caller, value)?;
        let parser = eval
            .treesit
            .parser(handle.parser_id)
            .ok_or_else(|| node_outdated_error(value))?;
        return Ok(ResolvedNodeInput {
            parser_id: handle.parser_id,
            parser_value: parser.value,
            language_symbol: parser_record_slot(parser.value, PARSER_SLOT_LANGUAGE)?,
            node_value: value,
            node_raw: handle.raw,
        });
    }

    if runtime::is_parser(value) {
        let parser_id = expect_live_parser_id(eval, caller, value)?;
        ensure_parser_parsed(eval, parser_id)?;
        let root_raw = {
            let parser = eval
                .treesit
                .parser(parser_id)
                .ok_or_else(|| parser_deleted_error(value))?;
            parser
                .tree
                .as_ref()
                .map(|tree| tree.root_node().into_raw())
                .ok_or_else(|| treesit_parse_error(value))?
        };
        let node_value = make_node_value_for_parser(eval, parser_id, unsafe {
            tree_sitter::Node::from_raw(root_raw)
        });
        return resolve_node_input(eval, node_value, caller);
    }

    if value.as_symbol_name().is_some() {
        let parser =
            builtin_treesit_parser_create(eval, vec![value, Value::NIL, Value::NIL, Value::NIL])?;
        return resolve_node_input(eval, parser, caller);
    }

    Err(node_type_error(caller, value))
}

fn lookup_thing_definition(
    eval: &super::eval::Context,
    language_symbol: Value,
    thing_symbol: Value,
) -> Option<Value> {
    let language_name = language_symbol.as_symbol_name()?;
    let thing_name = thing_symbol.as_symbol_name()?;
    let settings =
        super::misc_eval::dynamic_or_global_symbol_value(eval, "treesit-thing-settings")?;
    let languages = crate::emacs_core::value::list_to_vec(&settings)?;
    for language_entry in languages {
        if !language_entry.is_cons() {
            continue;
        }
        let lang = language_entry.cons_car();
        if lang.as_symbol_name() != Some(language_name) {
            continue;
        }
        let defs = language_entry.cons_cdr();
        let defs = if defs.is_cons() && defs.cons_cdr().is_nil() {
            defs.cons_car()
        } else {
            defs
        };
        let defs = crate::emacs_core::value::list_to_vec(&defs)?;
        for def in defs {
            if !def.is_cons() {
                continue;
            }
            let key = def.cons_car();
            if key.as_symbol_name() != Some(thing_name) {
                continue;
            }
            let rest = def.cons_cdr();
            return Some(if rest.is_cons() {
                rest.cons_car()
            } else {
                rest
            });
        }
    }
    None
}

fn treesit_predicate_not_found(predicate: Value) -> Flow {
    signal("treesit-predicate-not-found", vec![predicate])
}

fn treesit_invalid_predicate(message: impl Into<String>, predicate: Value) -> Flow {
    signal(
        "treesit-invalid-predicate",
        vec![Value::string(message.into()), predicate],
    )
}

fn predicate_function_p(eval: &super::eval::Context, predicate: Value) -> bool {
    if predicate.is_nil() {
        return false;
    }
    if predicate.as_symbol_name().is_some() {
        return eval
            .obarray()
            .symbol_function_of_value(&predicate)
            .is_some();
    }
    true
}

fn call_node_predicate(
    eval: &mut super::eval::Context,
    predicate: Value,
    parser_id: u64,
    node: tree_sitter::Node<'_>,
) -> Result<bool, Flow> {
    let node_value = make_node_value_for_parser(eval, parser_id, node);
    Ok(!eval.funcall_general(predicate, vec![node_value])?.is_nil())
}

fn predicate_matches_node(
    eval: &mut super::eval::Context,
    parser_id: u64,
    parser_value: Value,
    node: tree_sitter::Node<'_>,
    predicate: Value,
    named_only: bool,
    ignore_missing: bool,
) -> Result<bool, Flow> {
    if named_only && !node.is_named() {
        return Ok(false);
    }

    if let Some(pattern) = predicate.as_str_owned() {
        let regex = Regex::new(&pattern)
            .map_err(|err| treesit_invalid_predicate(err.to_string(), predicate))?;
        return Ok(regex.is_match(node.kind()));
    }

    if predicate.as_symbol_name() == Some("named") {
        return Ok(node.is_named());
    }
    if predicate.as_symbol_name() == Some("anonymous") {
        return Ok(!node.is_named());
    }

    if let Some(definition) = lookup_thing_definition(
        eval,
        parser_record_slot(parser_value, PARSER_SLOT_LANGUAGE)?,
        predicate,
    ) {
        return predicate_matches_node(
            eval,
            parser_id,
            parser_value,
            node,
            definition,
            named_only,
            ignore_missing,
        );
    }

    if predicate.as_symbol_name().is_some() && !predicate_function_p(eval, predicate) {
        return if ignore_missing {
            Ok(false)
        } else {
            Err(treesit_predicate_not_found(predicate))
        };
    }

    if predicate_function_p(eval, predicate) && !predicate.is_cons() {
        return call_node_predicate(eval, predicate, parser_id, node);
    }

    if !predicate.is_cons() {
        return Err(treesit_invalid_predicate(
            "Unsupported tree-sitter predicate",
            predicate,
        ));
    }

    let head = predicate.cons_car();
    let tail = predicate.cons_cdr();
    if head.as_symbol_name() == Some("not") {
        let args = crate::emacs_core::value::list_to_vec(&tail)
            .ok_or_else(|| treesit_invalid_predicate("`not' expects one predicate", predicate))?;
        if args.len() != 1 {
            return Err(treesit_invalid_predicate(
                "`not' expects one predicate",
                predicate,
            ));
        }
        return Ok(!predicate_matches_node(
            eval,
            parser_id,
            parser_value,
            node,
            args[0],
            named_only,
            ignore_missing,
        )?);
    }
    if head.as_symbol_name() == Some("or") || head.as_symbol_name() == Some("and") {
        let args = crate::emacs_core::value::list_to_vec(&tail).ok_or_else(|| {
            treesit_invalid_predicate("Malformed boolean tree-sitter predicate", predicate)
        })?;
        if head.as_symbol_name() == Some("or") {
            for item in args {
                if predicate_matches_node(
                    eval,
                    parser_id,
                    parser_value,
                    node,
                    item,
                    named_only,
                    ignore_missing,
                )? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        for item in args {
            if !predicate_matches_node(
                eval,
                parser_id,
                parser_value,
                node,
                item,
                named_only,
                ignore_missing,
            )? {
                return Ok(false);
            }
        }
        return Ok(true);
    }

    if let Some(pattern) = head.as_str_owned() {
        if !predicate_function_p(eval, tail) {
            return Err(treesit_invalid_predicate(
                "Dotted tree-sitter predicates expect a callable cdr",
                predicate,
            ));
        }
        let regex = Regex::new(&pattern)
            .map_err(|err| treesit_invalid_predicate(err.to_string(), predicate))?;
        if !regex.is_match(node.kind()) {
            return Ok(false);
        }
        return call_node_predicate(eval, tail, parser_id, node);
    }

    Err(treesit_invalid_predicate(
        "Malformed tree-sitter predicate",
        predicate,
    ))
}

fn first_child_for_search(
    node: tree_sitter::Node<'_>,
    forward: bool,
    named_only: bool,
) -> Option<tree_sitter::Node<'_>> {
    if named_only {
        let count = node.named_child_count();
        if count == 0 {
            None
        } else if forward {
            node.named_child(0)
        } else {
            node.named_child((count - 1) as u32)
        }
    } else {
        let count = node.child_count();
        if count == 0 {
            None
        } else if forward {
            node.child(0)
        } else {
            node.child((count - 1) as u32)
        }
    }
}

fn sibling_for_search(
    node: tree_sitter::Node<'_>,
    forward: bool,
    named_only: bool,
) -> Option<tree_sitter::Node<'_>> {
    match (forward, named_only) {
        (true, true) => node.next_named_sibling(),
        (true, false) => node.next_sibling(),
        (false, true) => node.prev_named_sibling(),
        (false, false) => node.prev_sibling(),
    }
}

fn descend_to_leaf(mut node: tree_sitter::Node<'_>, forward: bool) -> tree_sitter::Node<'_> {
    while let Some(child) = first_child_for_search(node, forward, false) {
        node = child;
    }
    node
}

fn search_subtree_impl<'tree>(
    eval: &mut super::eval::Context,
    parser_id: u64,
    parser_value: Value,
    node: tree_sitter::Node<'tree>,
    predicate: Value,
    forward: bool,
    named_only: bool,
    depth: i64,
    skip_root: bool,
) -> Result<Option<tree_sitter::Node<'tree>>, Flow> {
    if !skip_root
        && predicate_matches_node(
            eval,
            parser_id,
            parser_value,
            node,
            predicate,
            named_only,
            false,
        )?
    {
        return Ok(Some(node));
    }
    if depth == 0 {
        return Ok(None);
    }
    let Some(mut child) = first_child_for_search(node, forward, named_only) else {
        return Ok(None);
    };
    loop {
        if let Some(found) = search_subtree_impl(
            eval,
            parser_id,
            parser_value,
            child,
            predicate,
            forward,
            named_only,
            depth - 1,
            false,
        )? {
            return Ok(Some(found));
        }
        let Some(next) = sibling_for_search(child, forward, false) else {
            break;
        };
        child = next;
    }
    Ok(None)
}

fn search_forward_impl<'tree>(
    eval: &mut super::eval::Context,
    parser_id: u64,
    parser_value: Value,
    start: tree_sitter::Node<'tree>,
    predicate: Value,
    forward: bool,
    named_only: bool,
) -> Result<Option<tree_sitter::Node<'tree>>, Flow> {
    let mut current = start;
    loop {
        while let Some(sibling) = sibling_for_search(current, forward, named_only) {
            let candidate = descend_to_leaf(sibling, forward);
            if predicate_matches_node(
                eval,
                parser_id,
                parser_value,
                candidate,
                predicate,
                named_only,
                false,
            )? {
                return Ok(Some(candidate));
            }
            current = candidate;
            break;
        }
        if sibling_for_search(current, forward, named_only).is_some() {
            continue;
        }
        let Some(parent) = current.parent() else {
            return Ok(None);
        };
        current = parent;
        if predicate_matches_node(
            eval,
            parser_id,
            parser_value,
            current,
            predicate,
            named_only,
            false,
        )? {
            return Ok(Some(current));
        }
    }
}

fn subtree_stats(node: tree_sitter::Node<'_>) -> (i64, i64, i64) {
    let child_count = node.child_count() as i64;
    let mut max_depth = 1;
    let mut max_width = child_count;
    let mut count = 1;
    for idx in 0..node.child_count() {
        if let Some(child) = node.child(idx as u32) {
            let (child_depth, child_width, child_count) = subtree_stats(child);
            max_depth = max_depth.max(child_depth + 1);
            max_width = max_width.max(child_width);
            count += child_count;
        }
    }
    (max_depth, max_width, count)
}

fn build_sparse_tree(
    eval: &mut super::eval::Context,
    parser_id: u64,
    parser_value: Value,
    node: tree_sitter::Node<'_>,
    predicate: Value,
    process_fn: Value,
    depth: i64,
) -> Result<Option<Value>, Flow> {
    let matched =
        predicate_matches_node(eval, parser_id, parser_value, node, predicate, false, true)?;
    let mut children = Vec::new();
    if depth != 0 {
        for idx in 0..node.child_count() {
            if let Some(child) = node.child(idx as u32)
                && let Some(item) = build_sparse_tree(
                    eval,
                    parser_id,
                    parser_value,
                    child,
                    predicate,
                    process_fn,
                    depth.saturating_sub(1),
                )?
            {
                children.push(item);
            }
        }
    }
    if !matched && children.is_empty() {
        return Ok(None);
    }
    let payload = if matched {
        let node_value = make_node_value_for_parser(eval, parser_id, node);
        if process_fn.is_nil() {
            node_value
        } else {
            eval.funcall_general(process_fn, vec![node_value])?
        }
    } else {
        Value::NIL
    };
    Ok(Some(Value::cons(payload, Value::list(children))))
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

pub(crate) fn builtin_treesit_induce_sparse_tree(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-induce-sparse-tree", &args, 2, 4)?;
    let depth = match args.get(3).copied().unwrap_or(Value::NIL) {
        value if value.is_nil() => 1000,
        value => expect_fixnum(&value)?,
    };
    let handle = ensure_current_node(eval, "treesit-induce-sparse-tree", args[0])?;
    let parser_value = eval
        .treesit
        .parser(handle.parser_id)
        .ok_or_else(|| node_outdated_error(args[0]))?
        .value;
    let root = unsafe { tree_sitter::Node::from_raw(handle.raw) };
    match build_sparse_tree(
        eval,
        handle.parser_id,
        parser_value,
        root,
        args[1],
        args.get(2).copied().unwrap_or(Value::NIL),
        depth,
    ) {
        Ok(Some(tree)) => Ok(tree),
        Ok(None) => Ok(Value::NIL),
        Err(Flow::Signal(sig)) if sig.symbol_name() == "treesit-predicate-not-found" => {
            Ok(Value::NIL)
        }
        Err(err) => Err(err),
    }
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
    match load_language(eval, language) {
        Ok((loaded, _)) => Ok(Value::fixnum(loaded.abi_version() as i64)),
        Err(_) => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_treesit_language_version(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_treesit_language_abi_version(eval, args)
}

pub(crate) fn builtin_treesit_language_available_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-language-available-p", &args, 1, 2)?;
    let language = parse_symbol_arg("treesit-language-available-p", &args[0])?;
    let detail = args.get(1).is_some_and(|value| !value.is_nil());
    match load_language(eval, language) {
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

pub(crate) fn builtin_treesit_node_match_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-node-match-p", &args, 2, 3)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    let ignore_missing = args.get(2).is_some_and(|value| !value.is_nil());
    let handle = ensure_current_node(eval, "treesit-node-match-p", args[0])?;
    let parser_value = eval
        .treesit
        .parser(handle.parser_id)
        .ok_or_else(|| node_outdated_error(args[0]))?
        .value;
    let matched = predicate_matches_node(
        eval,
        handle.parser_id,
        parser_value,
        unsafe { tree_sitter::Node::from_raw(handle.raw) },
        args[1],
        false,
        ignore_missing,
    )?;
    Ok(if matched { Value::T } else { Value::NIL })
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

pub(crate) fn builtin_treesit_parser_add_notifier(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-add-notifier", &args, 2)?;
    let _ = expect_live_parser_id(eval, "treesit-parser-add-notifier", args[0])?;
    if args[1].as_symbol_name().is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[1]],
        ));
    }
    let mut items =
        crate::emacs_core::value::list_to_vec(&parser_record_slot(args[0], PARSER_SLOT_NOTIFIERS)?)
            .unwrap_or_default();
    if !items.contains(&args[1]) {
        items.insert(0, args[1]);
        if !args[0].set_record_slot(PARSER_SLOT_NOTIFIERS, Value::list(items)) {
            return Err(signal(
                "error",
                vec![Value::string("Failed to update parser notifiers")],
            ));
        }
    }
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
            .find_reusable_parser(orig_buffer_id, language, tag)
        {
            return Ok(existing);
        }
    }

    let (loaded_language, _) = load_language(eval, language).map_err(|detail| {
        signal(
            "error",
            vec![
                Value::string(format!(
                    "Failed to load tree-sitter language `{}`",
                    resolve_sym(language)
                )),
                detail,
            ],
        )
    })?;
    let mut parser = Parser::new();
    parser
        .set_language(&loaded_language)
        .map_err(|err| signal("error", vec![Value::string(format!("ABI mismatch: {err}"))]))?;

    let tracking_linecol = eval.treesit.linecol_cache(orig_buffer_id).is_some()
        || language_requires_linecol_tracking(eval, language);
    if tracking_linecol {
        eval.treesit.enable_linecol_tracking(orig_buffer_id);
    }
    let placeholder = Value::NIL;
    let id = eval.treesit.insert_parser(
        placeholder,
        orig_buffer_id,
        root_buffer_id,
        language,
        tag,
        parser,
        tracking_linecol,
    );
    let value = runtime::make_parser_value(id, Value::symbol(language), buffer_value, tag);
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
    let (need_to_gc_buffer, buffer_id) = {
        let parser = eval
            .treesit
            .parser(parser_id)
            .ok_or_else(|| parser_deleted_error(args[0]))?;
        (parser.need_to_gc_buffer, parser.orig_buffer_id)
    };
    let _ = eval.treesit.mark_parser_deleted(parser_id);
    if need_to_gc_buffer {
        let _ = eval.buffers.kill_buffer(buffer_id);
    }
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
    let items =
        eval.treesit
            .parser_values_for(root_buffer_id, orig_buffer_id, language, tag_filter);
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

pub(crate) fn builtin_treesit_parser_remove_notifier(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-remove-notifier", &args, 2)?;
    let _ = expect_live_parser_id(eval, "treesit-parser-remove-notifier", args[0])?;
    if args[1].as_symbol_name().is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[1]],
        ));
    }
    let items =
        crate::emacs_core::value::list_to_vec(&parser_record_slot(args[0], PARSER_SLOT_NOTIFIERS)?)
            .unwrap_or_default()
            .into_iter()
            .filter(|item| *item != args[1])
            .collect::<Vec<_>>();
    if !args[0].set_record_slot(PARSER_SLOT_NOTIFIERS, Value::list(items)) {
        return Err(signal(
            "error",
            vec![Value::string("Failed to update parser notifiers")],
        ));
    }
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

pub(crate) fn builtin_treesit_parser_set_included_ranges(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-set-included-ranges", &args, 2)?;
    let parser_id = expect_live_parser_id(eval, "treesit-parser-set-included-ranges", args[0])?;
    let current_ranges = parser_record_slot(args[0], runtime::PARSER_SLOT_INCLUDED_RANGES)?;
    if current_ranges == args[1] {
        return Ok(Value::NIL);
    }

    let source = {
        let parser = eval
            .treesit
            .parser(parser_id)
            .ok_or_else(|| parser_deleted_error(args[0]))?;
        let buffer = eval.buffers.get(parser.orig_buffer_id).ok_or_else(|| {
            signal(
                "error",
                vec![Value::string("Parser buffer has been killed")],
            )
        })?;
        treesit_buffer_source(buffer)
    };
    let buffer = {
        let parser = eval
            .treesit
            .parser(parser_id)
            .ok_or_else(|| parser_deleted_error(args[0]))?;
        eval.buffers.get(parser.orig_buffer_id).ok_or_else(|| {
            signal(
                "error",
                vec![Value::string("Parser buffer has been killed")],
            )
        })?
    };

    let ts_ranges = if args[1].is_nil() {
        Vec::new()
    } else {
        let mut hint = runtime::LineColCache {
            line: 1,
            col: 1,
            bytepos: 0,
        };
        let mut ranges = Vec::new();
        for value in crate::emacs_core::value::list_to_vec(&args[1]).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![
                    Value::symbol("consp"),
                    args[1],
                    Value::symbol("treesit-parser-set-included-ranges"),
                ],
            )
        })? {
            if !value.is_cons() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("consp"), value],
                ));
            }
            let start =
                lisp_pos_to_relative_byte(buffer, expect_integer_or_marker(&value.cons_car())?)?;
            let end =
                lisp_pos_to_relative_byte(buffer, expect_integer_or_marker(&value.cons_cdr())?)?;
            let start_point = byte_offset_to_point(&source, start, hint);
            let next_hint = byte_offset_to_linecol(&source, end, hint);
            let end_point = Point {
                row: next_hint.line.saturating_sub(1) as usize,
                column: next_hint.col.saturating_sub(1) as usize,
            };
            hint = next_hint;
            ranges.push(TSRange {
                start_byte: start,
                end_byte: end,
                start_point,
                end_point,
            });
        }
        ranges
    };

    let parser = eval
        .treesit
        .parser_mut(parser_id)
        .ok_or_else(|| parser_deleted_error(args[0]))?;
    parser
        .parser
        .set_included_ranges(&ts_ranges)
        .map_err(|err| {
            signal(
                "error",
                vec![Value::string(format!(
                    "Invalid tree-sitter ranges at index {}",
                    err.0
                ))],
            )
        })?;
    parser.tree = None;
    parser.last_source = None;
    parser.last_changed_ranges.clear();
    if !args[0].set_record_slot(runtime::PARSER_SLOT_INCLUDED_RANGES, args[1]) {
        return Err(signal(
            "error",
            vec![Value::string("Failed to update parser included ranges")],
        ));
    }
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
    Ok(Value::string(expand_pattern_value(args[0])?))
}

pub(crate) fn builtin_treesit_query_capture(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-query-capture", &args, 2, 6)?;
    let input = resolve_node_input(eval, args[0], "treesit-query-capture")?;
    let compiled_query = resolve_compiled_query_value(
        eval,
        input.language_symbol,
        args[1],
        "treesit-query-capture",
    )?;
    let query_id = runtime::query_id(compiled_query)
        .ok_or_else(|| compiled_query_type_error("treesit-query-capture", compiled_query))?;
    let byte_range = {
        let parser = eval
            .treesit
            .parser(input.parser_id)
            .ok_or_else(|| parser_deleted_error(input.parser_value))?;
        let buf = eval.buffers.get(parser.orig_buffer_id).ok_or_else(|| {
            signal(
                "error",
                vec![Value::string("Parser buffer has been killed")],
            )
        })?;
        query_range_bytes(buf, &args, 2, 3)?
    };
    let node_only = args.get(4).is_some_and(|value| !value.is_nil());
    let grouped = args.get(5).is_some_and(|value| !value.is_nil());

    enum CaptureResult {
        Flat(Vec<(u32, tree_sitter::ffi::TSNode)>),
        Grouped(Vec<Vec<(u32, tree_sitter::ffi::TSNode)>>),
    }

    let (capture_names, captures) = {
        let parser = eval
            .treesit
            .parser(input.parser_id)
            .ok_or_else(|| parser_deleted_error(input.parser_value))?;
        let query = eval
            .treesit
            .query(query_id)
            .and_then(|entry| entry.compiled.as_ref())
            .ok_or_else(|| compiled_query_type_error("treesit-query-capture", compiled_query))?;
        let source = parser
            .last_source
            .as_deref()
            .ok_or_else(|| treesit_parse_error(input.parser_value))?;
        let capture_names = query
            .capture_names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        let node = unsafe { tree_sitter::Node::from_raw(input.node_raw) };
        let mut cursor = QueryCursor::new();
        if let Some(range) = byte_range.clone() {
            cursor.set_byte_range(range);
        }
        if grouped {
            let mut matches = cursor.matches(query, node, source.as_bytes());
            matches.advance();
            let mut out = Vec::new();
            while let Some(query_match) = matches.get() {
                out.push(
                    query_match
                        .captures
                        .iter()
                        .map(|capture| (capture.index, capture.node.into_raw()))
                        .collect::<Vec<_>>(),
                );
                matches.advance();
            }
            (capture_names, CaptureResult::Grouped(out))
        } else {
            let mut matches = cursor.captures(query, node, source.as_bytes());
            matches.advance();
            let mut out = Vec::new();
            while let Some((query_match, capture_index)) = matches.get() {
                let capture = query_match.captures[*capture_index];
                out.push((capture.index, capture.node.into_raw()));
                matches.advance();
            }
            (capture_names, CaptureResult::Flat(out))
        }
    };

    let result = match captures {
        CaptureResult::Flat(items) => Value::list(
            items
                .into_iter()
                .map(|(capture_index, raw)| {
                    let node = make_node_value_for_parser(eval, input.parser_id, unsafe {
                        tree_sitter::Node::from_raw(raw)
                    });
                    if node_only {
                        node
                    } else {
                        Value::cons(Value::symbol(&capture_names[capture_index as usize]), node)
                    }
                })
                .collect(),
        ),
        CaptureResult::Grouped(groups) => Value::list(
            groups
                .into_iter()
                .map(|group| {
                    Value::list(
                        group
                            .into_iter()
                            .map(|(capture_index, raw)| {
                                let node =
                                    make_node_value_for_parser(eval, input.parser_id, unsafe {
                                        tree_sitter::Node::from_raw(raw)
                                    });
                                if node_only {
                                    node
                                } else {
                                    Value::cons(
                                        Value::symbol(&capture_names[capture_index as usize]),
                                        node,
                                    )
                                }
                            })
                            .collect(),
                    )
                })
                .collect(),
        ),
    };
    Ok(result)
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
    let language_sym = parse_symbol_arg("treesit-query-compile", &language)?;
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

    let id = eval.treesit.insert_query(language_sym);
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
    let source = match args[0] {
        value if runtime::is_compiled_query(value) => query_record_slot(value, QUERY_SLOT_SOURCE)?,
        value => value,
    };
    Ok(Value::string(expand_query_value(
        "treesit-query-expand",
        source,
    )?))
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

pub(crate) fn builtin_treesit_search_forward(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-search-forward", &args, 2, 4)?;
    let handle = ensure_current_node(eval, "treesit-search-forward", args[0])?;
    let parser_value = eval
        .treesit
        .parser(handle.parser_id)
        .ok_or_else(|| node_outdated_error(args[0]))?
        .value;
    let forward = args.get(2).is_none_or(|value| value.is_nil());
    let named_only = args.get(3).is_none_or(|value| value.is_nil());
    match search_forward_impl(
        eval,
        handle.parser_id,
        parser_value,
        unsafe { tree_sitter::Node::from_raw(handle.raw) },
        args[1],
        forward,
        named_only,
    ) {
        Ok(Some(node)) => Ok(make_node_value_for_parser(eval, handle.parser_id, node)),
        Ok(None) => Ok(Value::NIL),
        Err(Flow::Signal(sig)) if sig.symbol_name() == "treesit-predicate-not-found" => {
            Ok(Value::NIL)
        }
        Err(err) => Err(err),
    }
}

pub(crate) fn builtin_treesit_search_subtree(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-search-subtree", &args, 2, 5)?;
    let handle = ensure_current_node(eval, "treesit-search-subtree", args[0])?;
    let parser_value = eval
        .treesit
        .parser(handle.parser_id)
        .ok_or_else(|| node_outdated_error(args[0]))?
        .value;
    let forward = args.get(2).is_none_or(|value| value.is_nil());
    let named_only = args.get(3).is_none_or(|value| value.is_nil());
    let depth = match args.get(4).copied().unwrap_or(Value::NIL) {
        value if value.is_nil() => 1000,
        value => expect_fixnum(&value)?,
    };
    match search_subtree_impl(
        eval,
        handle.parser_id,
        parser_value,
        unsafe { tree_sitter::Node::from_raw(handle.raw) },
        args[1],
        forward,
        named_only,
        depth,
        false,
    ) {
        Ok(Some(node)) => Ok(make_node_value_for_parser(eval, handle.parser_id, node)),
        Ok(None) => Ok(Value::NIL),
        Err(Flow::Signal(sig)) if sig.symbol_name() == "treesit-predicate-not-found" => {
            Ok(Value::NIL)
        }
        Err(err) => Err(err),
    }
}

pub(crate) fn builtin_treesit_subtree_stat(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-subtree-stat", &args, 1)?;
    let handle = ensure_current_node(eval, "treesit-subtree-stat", args[0])?;
    let (depth, width, count) = subtree_stats(unsafe { tree_sitter::Node::from_raw(handle.raw) });
    Ok(Value::list(vec![
        Value::fixnum(depth),
        Value::fixnum(width),
        Value::fixnum(count),
    ]))
}

pub(crate) fn builtin_treesit_grammar_location(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-grammar-location", &args, 1)?;
    let language = parse_symbol_arg("treesit-grammar-location", &args[0])?;
    match load_language(eval, language) {
        Ok((_, filename)) => Ok(filename.map_or(Value::NIL, Value::string)),
        Err(_) => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_treesit_tracking_line_column_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("treesit-tracking-line-column-p", &args, 0, 1)?;
    let buffer_id = match args.first().copied().unwrap_or(Value::NIL) {
        value if value.is_nil() => eval
            .buffers
            .current_buffer_id()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?,
        value => expect_buffer_id(&value)?,
    };
    Ok(if eval.treesit.linecol_cache(buffer_id).is_some() {
        Value::T
    } else {
        Value::NIL
    })
}

pub(crate) fn builtin_treesit_parser_tracking_line_column_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-tracking-line-column-p", &args, 1)?;
    let parser_id = expect_live_parser_id(eval, "treesit-parser-tracking-line-column-p", args[0])?;
    let tracking = eval
        .treesit
        .parser(parser_id)
        .ok_or_else(|| parser_deleted_error(args[0]))?
        .tracking_linecol;
    Ok(if tracking { Value::T } else { Value::NIL })
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

pub(crate) fn builtin_treesit_parse_string(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parse-string", &args, 2)?;
    let language = expect_symbol_or_nil("treesit-parse-string", args[1])?;
    if language.is_nil() {
        return Err(query_type_error("treesit-parse-string", language));
    }
    let text = expect_string(&args[0])?;
    let name = format!(" *treesit-parse-string-{}*", eval.treesit.roots().len() + 1);
    let buffer_id = eval.buffers.create_buffer_with_hook_inhibition(&name, true);
    let saved_current = eval.buffers.current_buffer_id();
    let _ = eval.buffers.switch_current(buffer_id);
    if let Some(buffer) = eval.buffers.current_buffer_mut() {
        buffer.insert(&text);
    }
    let parser = builtin_treesit_parser_create(
        eval,
        vec![
            language,
            Value::make_buffer(buffer_id),
            Value::T,
            Value::NIL,
        ],
    )?;
    if let Some(parser_id) = runtime::parser_id(parser)
        && let Some(entry) = eval.treesit.parser_mut(parser_id)
    {
        entry.need_to_gc_buffer = true;
    }
    if let Some(saved) = saved_current {
        let _ = eval.buffers.switch_current(saved);
    }
    builtin_treesit_parser_root_node(eval, vec![parser])
}

pub(crate) fn builtin_treesit_parser_changed_regions(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit-parser-changed-regions", &args, 1)?;
    let parser_id = expect_live_parser_id(eval, "treesit-parser-changed-regions", args[0])?;
    let changed_ranges = ensure_parser_parsed_with_changes(eval, parser_id)?;
    let Some(changed_ranges) = changed_ranges else {
        return Ok(Value::NIL);
    };
    if changed_ranges.is_empty() {
        return Ok(Value::NIL);
    }
    let regions = changed_ranges_to_lisp(eval, parser_id, &changed_ranges)?;
    for notifier in
        crate::emacs_core::value::list_to_vec(&parser_record_slot(args[0], PARSER_SLOT_NOTIFIERS)?)
            .unwrap_or_default()
    {
        let _ = eval.funcall_general(notifier, vec![regions, args[0]])?;
    }
    Ok(regions)
}

pub(crate) fn builtin_treesit_parser_changed_ranges(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_treesit_parser_changed_regions(eval, args)
}

pub(crate) fn builtin_treesit_linecol_at(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit--linecol-at", &args, 1)?;
    let pos = expect_integer_or_marker(&args[0])?;
    let buffer_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buffer = eval
        .buffers
        .get(buffer_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let source = treesit_buffer_source(buffer);
    let byte_offset = lisp_pos_to_relative_byte(buffer, pos)?;
    let hint = eval
        .treesit
        .linecol_cache(buffer_id)
        .unwrap_or(runtime::LineColCache {
            line: 1,
            col: 1,
            bytepos: 0,
        });
    let linecol = byte_offset_to_linecol(&source, byte_offset, hint);
    Ok(Value::cons(
        Value::fixnum(linecol.line),
        Value::fixnum(linecol.col),
    ))
}

pub(crate) fn builtin_treesit_linecol_cache_set(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit--linecol-cache-set", &args, 3)?;
    let buffer_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let line = expect_fixnum(&args[0])?;
    let col = expect_fixnum(&args[1])?;
    let bytepos = expect_fixnum(&args[2])?;
    eval.treesit.set_linecol_cache(
        buffer_id,
        runtime::LineColCache {
            line,
            col,
            bytepos: bytepos.max(0) as usize,
        },
    );
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_linecol_cache(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("treesit--linecol-cache", &args, 0)?;
    let buffer_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let Some(cache) = eval.treesit.linecol_cache(buffer_id) else {
        return Ok(Value::NIL);
    };
    let buffer = eval
        .buffers
        .get(buffer_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let source = treesit_buffer_source(buffer);
    let pos = byte_offset_to_lisp_pos(buffer, &source, cache.bytepos);
    Ok(Value::list(vec![
        Value::keyword(":line"),
        Value::fixnum(cache.line),
        Value::keyword(":col"),
        Value::fixnum(cache.col),
        Value::keyword(":pos"),
        pos,
        Value::keyword(":bytepos"),
        Value::fixnum(cache.bytepos as i64),
    ]))
}
