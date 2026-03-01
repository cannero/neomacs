//! Oracle-backed Elisp parity tests.

mod abs;
mod advice;
#[path = "advice-advanced.rs"]
mod advice_advanced;
#[path = "advice-patterns-advanced.rs"]
mod advice_patterns_advanced;
#[path = "alist-get.rs"]
mod alist_get;
#[path = "alist-operations.rs"]
mod alist_operations;
mod r#and;
mod apply;
#[path = "apply-advanced.rs"]
mod apply_advanced;
#[path = "apply-funcall-advanced.rs"]
mod apply_funcall_advanced;
#[path = "apply-funcall-patterns.rs"]
mod apply_funcall_patterns;
mod arithmetic;
#[path = "arithmetic-advanced.rs"]
mod arithmetic_advanced;
#[path = "ash-logand-logior-patterns.rs"]
mod ash_logand_logior_patterns;
mod assoc;
mod assq;
#[path = "assoc-assq-advanced.rs"]
mod assoc_assq_advanced;
mod backquote;
#[path = "backquote-advanced.rs"]
mod backquote_advanced;
#[path = "beginning-of-line.rs"]
mod beginning_of_line;
mod bitwise;
#[path = "bool-vector-operations.rs"]
mod bool_vector_operations;
#[path = "buffer-multi-operations.rs"]
mod buffer_multi_operations;
#[path = "buffer-operations.rs"]
mod buffer_operations;
#[path = "buffer-operations-advanced.rs"]
mod buffer_operations_advanced;
#[path = "buffer-name.rs"]
mod buffer_name;
#[path = "buffer-position.rs"]
mod buffer_position;
#[path = "buffer-string.rs"]
mod buffer_string;
#[path = "buffer-substring.rs"]
mod buffer_substring;
#[path = "buffer-substring-advanced.rs"]
mod buffer_substring_advanced;
#[path = "aref-aset.rs"]
mod aref_aset;
#[path = "car-safe.rs"]
mod car_safe;
#[path = "car-cdr-combinations.rs"]
mod car_cdr_combinations;
mod r#catch;
#[path = "catch-throw-advanced.rs"]
mod catch_throw_advanced;
#[path = "catch-throw-patterns.rs"]
mod catch_throw_patterns;
#[path = "char-after.rs"]
mod char_after;
#[path = "char-before-operations.rs"]
mod char_before_operations;
#[path = "char-literal.rs"]
mod char_literal;
#[path = "char-literal-advanced.rs"]
mod char_literal_advanced;
#[path = "char-table.rs"]
mod char_table;
#[path = "char-table-advanced.rs"]
mod char_table_advanced;
#[path = "char-table-extra-slot.rs"]
mod char_table_extra_slot;
#[path = "char-table-range-advanced.rs"]
mod char_table_range_advanced;
#[path = "char-operations.rs"]
mod char_operations;
#[path = "char-syntax-advanced.rs"]
mod char_syntax_advanced;
#[path = "char-to-string.rs"]
mod char_to_string;
#[path = "char-width-advanced.rs"]
mod char_width_advanced;
mod charset;
#[path = "charset-advanced.rs"]
mod charset_advanced;
#[path = "cl-lib-patterns.rs"]
mod cl_lib_patterns;
#[path = "cl-lib-patterns-advanced.rs"]
mod cl_lib_patterns_advanced;
#[path = "cl-loop-patterns.rs"]
mod cl_loop_patterns;
mod closure;
#[path = "closure-advanced.rs"]
mod closure_advanced;
mod coding;
#[path = "coding-advanced.rs"]
mod coding_advanced;
#[path = "concat-extended.rs"]
mod concat_extended;
#[path = "concat-extended-advanced.rs"]
mod concat_extended_advanced;
#[path = "copy-sequence.rs"]
mod copy_sequence;
#[path = "copy-sequence-advanced.rs"]
mod copy_sequence_advanced;
#[path = "copy-syntax-table-advanced.rs"]
mod copy_syntax_table_advanced;
#[path = "coding-metadata.rs"]
mod coding_metadata;
#[path = "coding-string.rs"]
mod coding_string;
#[path = "coding-string-advanced.rs"]
mod coding_string_advanced;
#[path = "coding-system-put-advanced.rs"]
mod coding_system_put_advanced;
#[path = "commandp-functionp-advanced.rs"]
mod commandp_functionp_advanced;
mod combination;
mod combination_abstract_algebra;
mod combination_abstract_data_types;
mod combination_abstract_machines;
mod combination_actor_model;
mod combination_advanced;
mod combination_advanced_error_handling;
mod combination_algorithms;
mod combination_algorithm_challenges;
mod combination_automaton_patterns;
mod combination_avl_tree;
mod combination_alist_patterns;
mod combination_buffer_advanced;
mod combination_buffer_algorithms;
mod combination_buffer_editing;
mod combination_buffer_processing;
mod combination_buffer_text_processing;
mod combination_bloom_filter;
mod combination_b_tree;
mod combination_bytevector_ops;
mod combination_cache_strategies;
mod combination_calculator_repl;
mod combination_channel_patterns;
mod combination_closures;
mod combination_closures_advanced;
mod combination_collections;
mod combination_compiler_patterns;
mod combination_compression;
mod combination_complex;
mod combination_consensus;
mod combination_concurrent_patterns;
mod combination_config_system;
mod combination_constraint_solving;
mod combination_contract_system;
mod combination_control_flow;
mod combination_coroutine_patterns;
mod combination_cps_transform;
mod combination_cryptography;
mod combination_csp_solver;
mod combination_data_structures;
mod combination_data_structures_advanced;
mod combination_data_transformations;
mod combination_database_ops;
mod combination_database_patterns;
mod combination_dataflow_analysis;
mod combination_dependency_resolver;
mod combination_design_patterns;
mod combination_diff_algorithm;
mod combination_dynamic_programming;
mod combination_effect_system;
mod combination_elisp_idioms;
mod combination_encoding_algorithms;
mod combination_error_handling;
mod combination_event_driven;
mod combination_event_system;
mod combination_expression_evaluator;
mod combination_finite_automata;
mod combination_string_advanced;
mod combination_string_algorithms;
mod combination_string_algorithms_advanced;
mod combination_string_formatting;
mod combination_string_interning;
mod combination_string_parsing;
mod combination_functional;
mod combination_functional_advanced;
mod combination_functional_composition;
mod combination_functional_programming;
mod combination_genetic_algorithm;
mod combination_graph_algorithms;
mod combination_graph_patterns;
mod combination_graph_traversal;
mod combination_hash_algorithms;
mod combination_heap_datastructure;
mod combination_higher_order;
mod combination_huffman_coding;
mod combination_immutable_data;
mod combination_interpreter_advanced;
mod combination_interpreter_advanced2;
mod combination_interpreter_patterns;
mod combination_interpreters;
mod combination_iterative_algorithms;
mod combination_iterator_patterns;
mod combination_json_processor;
mod combination_lambda_calculus;
mod combination_lexer_patterns;
mod combination_linked_list_ops;
mod combination_list_algorithms;
mod combination_logic_engine;
mod combination_logic_puzzles;
mod combination_macro_patterns;
mod combination_markup_parser;
mod combination_mathematical_structures;
mod combination_matrix_math;
mod combination_matrix_operations;
mod combination_memo_table;
mod combination_metaprogramming;
mod combination_mini_languages;
mod combination_monad_patterns;
mod combination_numeric_algorithms;
mod combination_numeric_patterns;
mod combination_object_system;
mod combination_oop_patterns;
mod combination_parser_combinators;
mod combination_parser_recursive_descent;
mod combination_parsing;
mod combination_pattern_matching;
mod combination_patterns;
mod combination_persistent_data;
mod combination_problem_solving;
mod combination_promise_patterns;
mod combination_protocol_fsm;
mod combination_protocol_implementations;
mod combination_query_language;
mod combination_queue_stack;
mod combination_property_list_patterns;
mod combination_reactive_patterns;
mod combination_red_black_tree;
mod combination_regex_engine;
mod combination_ring_buffer;
mod combination_real_world;
mod combination_real_world_elisp;
mod combination_recursion;
mod combination_rope_datastructure;
mod combination_scheduling;
mod combination_serialization;
mod combination_set_operations;
mod combination_signal_processing;
mod combination_simulation;
mod combination_sorting;
mod combination_sparse_matrix;
mod combination_state_machines;
mod combination_symbolic_math;
mod combination_text_analysis;
mod combination_text_formatting;
mod combination_text_templating;
mod combination_text_processing;
mod combination_tree_algorithms;
mod combination_trie_datastructure;
mod combination_type_checker;
mod combination_type_inference;
mod combination_type_systems;
mod combination_unification;
mod combination_undo_system;
mod combination_validation;
mod combination_workflow;
mod combination_zipper_datastructure;
pub(crate) mod common;
#[path = "compare-strings.rs"]
mod compare_strings;
#[path = "compare-strings-advanced.rs"]
mod compare_strings_advanced;
mod comparison;
#[path = "comparison-advanced.rs"]
mod comparison_advanced;
mod cond;
#[path = "cond-advanced.rs"]
mod cond_advanced;
#[path = "condition-case.rs"]
mod condition_case;
#[path = "condition-case-advanced2.rs"]
mod condition_case_advanced2;
#[path = "condition-case-extended.rs"]
mod condition_case_extended;
#[path = "condition-case-patterns.rs"]
mod condition_case_patterns;
#[path = "copy-alist.rs"]
mod copy_alist;
#[path = "copy-alist-advanced.rs"]
mod copy_alist_advanced;
#[path = "copy-keymap-advanced.rs"]
mod copy_keymap_advanced;
mod coverage;
mod coverage_manifest;
#[path = "count-lines-advanced.rs"]
mod count_lines_advanced;
#[path = "current-buffer.rs"]
mod current_buffer;
#[path = "current-column-advanced.rs"]
mod current_column_advanced;
#[path = "decode-char-encode-char-advanced.rs"]
mod decode_char_encode_char_advanced;
#[path = "defalias-advanced.rs"]
mod defalias_advanced;
#[path = "defalias-fset-patterns.rs"]
mod defalias_fset_patterns;
#[path = "defmacro-advanced.rs"]
mod defmacro_advanced;
#[path = "defmacro-macroexpand.rs"]
mod defmacro_macroexpand;
#[path = "defmacro-patterns.rs"]
mod defmacro_patterns;
mod defvar;
#[path = "defvar-advanced.rs"]
mod defvar_advanced;
#[path = "delete-and-extract-advanced.rs"]
mod delete_and_extract_advanced;
#[path = "delete-operations.rs"]
mod delete_operations;
#[path = "delete-operations-advanced.rs"]
mod delete_operations_advanced;
#[path = "delete-region.rs"]
mod delete_region;
#[path = "delete-region-advanced.rs"]
mod delete_region_advanced;
mod delq;
#[path = "dolist-dotimes-advanced.rs"]
mod dolist_dotimes_advanced;
mod dolist;
mod dotimes;
#[path = "dynamic-binding.rs"]
mod dynamic_binding;
#[path = "dynamic-binding-advanced.rs"]
mod dynamic_binding_advanced;
#[path = "elt-aref-aset-patterns.rs"]
mod elt_aref_aset_patterns;
#[path = "end-of-line.rs"]
mod end_of_line;
mod equality;
#[path = "equality-advanced.rs"]
mod equality_advanced;
#[path = "erase-buffer-advanced.rs"]
mod erase_buffer_advanced;
#[path = "erase-buffer-patterns.rs"]
mod erase_buffer_patterns;
#[path = "error-handling-patterns.rs"]
mod error_handling_patterns;
#[path = "error-handling-patterns-advanced.rs"]
mod error_handling_patterns_advanced;
mod eval;
#[path = "eval-advanced.rs"]
mod eval_advanced;
#[path = "eval-advanced-2.rs"]
mod eval_advanced_2;
mod event_convert_list;
#[path = "event-convert-advanced.rs"]
mod event_convert_advanced;
#[path = "expt-sqrt-log-patterns.rs"]
mod expt_sqrt_log_patterns;
#[path = "fillarray-advanced.rs"]
mod fillarray_advanced;
#[path = "fillarray-operations.rs"]
mod fillarray_operations;
#[path = "following-char-operations.rs"]
mod following_char_operations;
mod format;
#[path = "format-advanced.rs"]
mod format_advanced;
#[path = "format-extended.rs"]
mod format_extended;
#[path = "format-extended-advanced.rs"]
mod format_extended_advanced;
#[path = "format-message-patterns.rs"]
mod format_message_patterns;
#[path = "format-patterns.rs"]
mod format_patterns;
#[path = "forward-char.rs"]
mod forward_char;
#[path = "forward-comment.rs"]
mod forward_comment;
#[path = "forward-line.rs"]
mod forward_line;
#[path = "forward-line-advanced.rs"]
mod forward_line_advanced;
#[path = "fset-symbol-function.rs"]
mod fset_symbol_function;
mod funcall;
mod r#get;
#[path = "goto-char.rs"]
mod goto_char;
#[path = "goto-char-advanced.rs"]
mod goto_char_advanced;
#[path = "hash-table.rs"]
mod hash_table;
#[path = "hash-table-advanced.rs"]
mod hash_table_advanced;
#[path = "hash-table-extended.rs"]
mod hash_table_extended;
#[path = "hash-table-patterns.rs"]
mod hash_table_patterns;
#[path = "hash-table-operations-extended.rs"]
mod hash_table_operations_extended;
mod r#if;
#[path = "if-advanced.rs"]
mod if_advanced;
#[path = "identity-operations.rs"]
mod identity_operations;
#[path = "indirect-function.rs"]
mod indirect_function;
mod insert;
#[path = "insert-advanced.rs"]
mod insert_advanced;
#[path = "insert-char-operations.rs"]
mod insert_char_operations;
#[path = "interactive-patterns.rs"]
mod interactive_patterns;
#[path = "interactive-patterns-advanced.rs"]
mod interactive_patterns_advanced;
#[path = "intern-soft-advanced.rs"]
mod intern_soft_advanced;
#[path = "internal-event-symbol-advanced.rs"]
mod internal_event_symbol_advanced;
#[path = "kbd-event-advanced.rs"]
mod kbd_event_advanced;
#[path = "key-description.rs"]
mod key_description;
mod keymap;
#[path = "keymap-advanced.rs"]
mod keymap_advanced;
#[path = "keymap-operations-extended.rs"]
mod keymap_operations_extended;
#[path = "lambda-anonymous.rs"]
mod lambda_anonymous;
#[path = "lambda-anonymous-advanced.rs"]
mod lambda_anonymous_advanced;
mod last;
#[path = "length-operations.rs"]
mod length_operations;
#[path = "line-position-advanced.rs"]
mod line_position_advanced;
mod r#let;
#[path = "let-advanced.rs"]
mod let_advanced;
#[path = "let-dynamic.rs"]
mod let_dynamic;
#[path = "let-star.rs"]
mod let_star;
#[path = "let-star-advanced.rs"]
mod let_star_advanced;
#[path = "let-star-advanced-2.rs"]
mod let_star_advanced_2;
#[path = "let-binding-patterns.rs"]
mod let_binding_patterns;
mod list;
#[path = "list-operations-advanced.rs"]
mod list_operations_advanced;
#[path = "looking-at-advanced.rs"]
mod looking_at_advanced;
#[path = "macroexpand-advanced.rs"]
mod macroexpand_advanced;
#[path = "make-list.rs"]
mod make_list;
#[path = "make-string.rs"]
mod make_string;
#[path = "make-string-advanced.rs"]
mod make_string_advanced;
#[path = "make-symbol.rs"]
mod make_symbol;
#[path = "make-vector-advanced.rs"]
mod make_vector_advanced;
#[path = "make-vector-patterns.rs"]
mod make_vector_patterns;
#[path = "map-operations.rs"]
mod map_operations;
#[path = "map-operations-advanced.rs"]
mod map_operations_advanced;
#[path = "mapc-operations.rs"]
mod mapc_operations;
mod mapcar;
#[path = "mapconcat-advanced.rs"]
mod mapconcat_advanced;
#[path = "marker-operations.rs"]
mod marker_operations;
#[path = "match-beginning.rs"]
mod match_beginning;
#[path = "match-data.rs"]
mod match_data;
#[path = "match-data-advanced.rs"]
mod match_data_advanced;
#[path = "match-end.rs"]
mod match_end;
#[path = "match-string-advanced.rs"]
mod match_string_advanced;
#[path = "matching-paren-advanced.rs"]
mod matching_paren_advanced;
#[path = "math-functions.rs"]
mod math_functions;
mod max;
#[path = "max-char-operations.rs"]
mod max_char_operations;
mod member;
mod memq;
mod min;
#[path = "modify-syntax-entry.rs"]
mod modify_syntax_entry;
#[path = "move-to-column-advanced.rs"]
mod move_to_column_advanced;
#[path = "move-to-column-patterns.rs"]
mod move_to_column_patterns;
#[path = "narrow-advanced.rs"]
mod narrow_advanced;
#[path = "narrow-widen-patterns.rs"]
mod narrow_widen_patterns;
#[path = "nbutlast-butlast-advanced.rs"]
mod nbutlast_butlast_advanced;
mod nconc;
#[path = "nconc-advanced.rs"]
mod nconc_advanced;
#[path = "nconc-nreverse-patterns.rs"]
mod nconc_nreverse_patterns;
#[path = "next-property-change-advanced.rs"]
mod next_property_change_advanced;
mod r#not;
mod nreverse;
mod nthcdr;
#[path = "nthcdr-advanced.rs"]
mod nthcdr_advanced;
#[path = "number-predicates.rs"]
mod number_predicates;
#[path = "number-predicates-advanced.rs"]
mod number_predicates_advanced;
#[path = "number-sequence-operations.rs"]
mod number_sequence_operations;
#[path = "number-to-string.rs"]
mod number_to_string;
#[path = "number-to-string-advanced.rs"]
mod number_to_string_advanced;
#[path = "obarray-symbol-interning.rs"]
mod obarray_symbol_interning;
mod oclosure;
#[path = "oclosure-advanced.rs"]
mod oclosure_advanced;
mod r#or;
mod plist;
#[path = "plist-advanced.rs"]
mod plist_advanced;
mod point;
#[path = "point-max.rs"]
mod point_max;
#[path = "point-min.rs"]
mod point_min;
mod predicates;
#[path = "prin1-to-string-advanced.rs"]
mod prin1_to_string_advanced;
mod prog1;
mod progn;
#[path = "progn-advanced.rs"]
mod progn_advanced;
mod progn_ast;
#[path = "property-list-advanced.rs"]
mod property_list_advanced;
#[path = "propertize-advanced.rs"]
mod propertize_advanced;
#[path = "proper-list-predicates.rs"]
mod proper_list_predicates;
mod put;
#[path = "re-search-backward-advanced.rs"]
mod re_search_backward_advanced;
#[path = "re-search-forward.rs"]
mod re_search_forward;
#[path = "re-search-patterns.rs"]
mod re_search_patterns;
#[path = "read-from-string-advanced.rs"]
mod read_from_string_advanced;
#[path = "read-from-string-patterns.rs"]
mod read_from_string_patterns;
#[path = "read-print.rs"]
mod read_print;
#[path = "read-print-advanced.rs"]
mod read_print_advanced;
mod recursion;
#[path = "recursion-advanced.rs"]
mod recursion_advanced;
#[path = "regexp-advanced.rs"]
mod regexp_advanced;
#[path = "regexp-operations.rs"]
mod regexp_operations;
#[path = "regexp-operations-advanced.rs"]
mod regexp_operations_advanced;
#[path = "regexp-quote-advanced.rs"]
mod regexp_quote_advanced;
#[path = "regexp-replace-advanced.rs"]
mod regexp_replace_advanced;
#[path = "replace-match-advanced.rs"]
mod replace_match_advanced;
#[path = "replace-regexp-advanced.rs"]
mod replace_regexp_advanced;
mod reverse;
#[path = "safe-length-operations.rs"]
mod safe_length_operations;
#[path = "safe-length-patterns.rs"]
mod safe_length_patterns;
#[path = "save-excursion.rs"]
mod save_excursion;
#[path = "save-excursion-advanced.rs"]
mod save_excursion_advanced;
#[path = "save-excursion-patterns.rs"]
mod save_excursion_patterns;
#[path = "save-restriction-advanced.rs"]
mod save_restriction_advanced;
#[path = "search-backward-advanced.rs"]
mod search_backward_advanced;
#[path = "search-operations.rs"]
mod search_operations;
#[path = "sequence-operations.rs"]
mod sequence_operations;
#[path = "seq-operations-advanced.rs"]
mod seq_operations_advanced;
#[path = "seq-operations-extended.rs"]
mod seq_operations_extended;
mod sequencep;
#[path = "set-buffer.rs"]
mod set_buffer;
mod setcar;
#[path = "setcar-setcdr-advanced.rs"]
mod setcar_setcdr_advanced;
mod setcdr;
mod setq;
#[path = "setq-advanced.rs"]
mod setq_advanced;
mod signal;
#[path = "signal-advanced.rs"]
mod signal_advanced;
#[path = "single-key-description-advanced.rs"]
mod single_key_description_advanced;
#[path = "skip-chars.rs"]
mod skip_chars;
#[path = "skip-chars-advanced.rs"]
mod skip_chars_advanced;
#[path = "skip-syntax-advanced.rs"]
mod skip_syntax_advanced;
mod sort;
#[path = "sort-algorithms.rs"]
mod sort_algorithms;
#[path = "sort-extended.rs"]
mod sort_extended;
#[path = "split-string-advanced.rs"]
mod split_string_advanced;
mod string;
#[path = "string-distance.rs"]
mod string_distance;
#[path = "string-distance-advanced.rs"]
mod string_distance_advanced;
#[path = "string-distance-patterns.rs"]
mod string_distance_patterns;
#[path = "string-equal.rs"]
mod string_equal;
#[path = "string-lessp.rs"]
mod string_lessp;
#[path = "string-manipulation.rs"]
mod string_manipulation;
#[path = "string-manipulation-advanced.rs"]
mod string_manipulation_advanced;
#[path = "string-match.rs"]
mod string_match;
#[path = "string-processing.rs"]
mod string_processing;
#[path = "string-processing-advanced.rs"]
mod string_processing_advanced;
#[path = "string-replace.rs"]
mod string_replace;
#[path = "string-search-advanced.rs"]
mod string_search_advanced;
#[path = "string-match-p.rs"]
mod string_match_p;
#[path = "string-to-char-advanced.rs"]
mod string_to_char_advanced;
#[path = "string-to-number.rs"]
mod string_to_number;
#[path = "string-to-number-advanced.rs"]
mod string_to_number_advanced;
#[path = "string-trim-patterns.rs"]
mod string_trim_patterns;
#[path = "string-version-lessp.rs"]
mod string_version_lessp;
#[path = "string-version-lessp-advanced.rs"]
mod string_version_lessp_advanced;
#[path = "string-width-advanced.rs"]
mod string_width_advanced;
#[path = "subr-arity-advanced.rs"]
mod subr_arity_advanced;
#[path = "subr-predicates.rs"]
mod subr_predicates;
mod substring;
mod symbol;
#[path = "symbol-advanced.rs"]
mod symbol_advanced;
#[path = "symbol-plist-patterns.rs"]
mod symbol_plist_patterns;
#[path = "symbol-properties-advanced.rs"]
mod symbol_properties_advanced;
#[path = "syntax-table.rs"]
mod syntax_table;
#[path = "syntax-table-advanced.rs"]
mod syntax_table_advanced;
#[path = "syntax-table-operations.rs"]
mod syntax_table_operations;
mod take;
#[path = "text-properties.rs"]
mod text_properties;
#[path = "text-properties-advanced.rs"]
mod text_properties_advanced;
#[path = "text-properties-patterns.rs"]
mod text_properties_patterns;
#[path = "text-property-manipulation.rs"]
mod text_property_manipulation;
mod trigonometry;
#[path = "trigonometry-advanced.rs"]
mod trigonometry_advanced;
mod r#throw;
#[path = "type-of.rs"]
mod type_of;
#[path = "type-of-advanced.rs"]
mod type_of_advanced;
#[path = "type-predicates.rs"]
mod type_predicates;
#[path = "type-predicates-advanced.rs"]
mod type_predicates_advanced;
mod unless;
#[path = "unwind-protect.rs"]
mod unwind_protect;
#[path = "unwind-protect-advanced.rs"]
mod unwind_protect_advanced;
#[path = "upcase-downcase.rs"]
mod upcase_downcase;
#[path = "upcase-downcase-advanced.rs"]
mod upcase_downcase_advanced;
#[path = "upcase-downcase-patterns.rs"]
mod upcase_downcase_patterns;
mod vector;
#[path = "vconcat-operations.rs"]
mod vconcat_operations;
#[path = "vector-advanced.rs"]
mod vector_advanced;
#[path = "vector-operations.rs"]
mod vector_operations;
#[path = "vector-or-char-table-operations.rs"]
mod vector_or_char_table_operations;
mod when;
mod r#while;
#[path = "while-advanced.rs"]
mod while_advanced;
#[path = "while-loop-patterns.rs"]
mod while_loop_patterns;
#[path = "while-patterns.rs"]
mod while_patterns;
#[path = "with-temp-buffer.rs"]
mod with_temp_buffer;
