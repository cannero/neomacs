use super::*;

// =========================================================================
// treesit.c stubs
// =========================================================================

pub(crate) fn builtin_treesit_available_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-available-p", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_compiled_query_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-compiled-query-p", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_induce_sparse_tree(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-induce-sparse-tree", &args, 2, 4)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_language_abi_version(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-language-abi-version", &args, 0, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_language_available_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-language-available-p", &args, 1, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_library_abi_version(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-library-abi-version", &args, 0, 1)?;
    Ok(Value::NIL)
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

pub(crate) fn builtin_treesit_grammar_location(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-grammar-location", &args, 1)?;
    Ok(Value::NIL)
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
