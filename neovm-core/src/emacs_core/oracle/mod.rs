//! Oracle-backed Elisp parity tests.

mod abs;
mod advice;
#[path = "alist-get.rs"]
mod alist_get;
#[path = "alist-operations.rs"]
mod alist_operations;
mod r#and;
mod apply;
#[path = "apply-advanced.rs"]
mod apply_advanced;
mod arithmetic;
mod assoc;
mod assq;
mod backquote;
#[path = "beginning-of-line.rs"]
mod beginning_of_line;
mod bitwise;
#[path = "buffer-operations.rs"]
mod buffer_operations;
#[path = "buffer-name.rs"]
mod buffer_name;
#[path = "buffer-position.rs"]
mod buffer_position;
#[path = "buffer-string.rs"]
mod buffer_string;
#[path = "buffer-substring.rs"]
mod buffer_substring;
#[path = "aref-aset.rs"]
mod aref_aset;
#[path = "car-safe.rs"]
mod car_safe;
#[path = "car-cdr-combinations.rs"]
mod car_cdr_combinations;
mod r#catch;
#[path = "catch-throw-advanced.rs"]
mod catch_throw_advanced;
#[path = "char-after.rs"]
mod char_after;
#[path = "char-literal.rs"]
mod char_literal;
#[path = "char-table.rs"]
mod char_table;
#[path = "char-table-extra-slot.rs"]
mod char_table_extra_slot;
#[path = "char-operations.rs"]
mod char_operations;
#[path = "char-to-string.rs"]
mod char_to_string;
mod charset;
#[path = "cl-lib-patterns.rs"]
mod cl_lib_patterns;
mod closure;
mod coding;
#[path = "concat-extended.rs"]
mod concat_extended;
#[path = "copy-sequence.rs"]
mod copy_sequence;
#[path = "coding-metadata.rs"]
mod coding_metadata;
#[path = "coding-string.rs"]
mod coding_string;
mod combination;
mod combination_advanced;
mod combination_algorithms;
mod combination_buffer_algorithms;
mod combination_buffer_processing;
mod combination_closures;
mod combination_complex;
mod combination_control_flow;
mod combination_data_structures;
mod combination_elisp_idioms;
mod combination_string_algorithms;
mod combination_functional;
mod combination_interpreters;
mod combination_iterative_algorithms;
mod combination_macro_patterns;
mod combination_patterns;
mod combination_real_world;
mod combination_recursion;
mod combination_text_processing;
pub(crate) mod common;
#[path = "compare-strings.rs"]
mod compare_strings;
mod comparison;
mod cond;
#[path = "cond-advanced.rs"]
mod cond_advanced;
#[path = "condition-case.rs"]
mod condition_case;
#[path = "condition-case-extended.rs"]
mod condition_case_extended;
#[path = "copy-alist.rs"]
mod copy_alist;
mod coverage;
mod coverage_manifest;
#[path = "current-buffer.rs"]
mod current_buffer;
#[path = "defmacro-advanced.rs"]
mod defmacro_advanced;
#[path = "defmacro-macroexpand.rs"]
mod defmacro_macroexpand;
mod defvar;
#[path = "delete-operations.rs"]
mod delete_operations;
#[path = "delete-region.rs"]
mod delete_region;
mod delq;
#[path = "dolist-dotimes-advanced.rs"]
mod dolist_dotimes_advanced;
mod dolist;
mod dotimes;
#[path = "dynamic-binding.rs"]
mod dynamic_binding;
#[path = "end-of-line.rs"]
mod end_of_line;
mod equality;
#[path = "error-handling-patterns.rs"]
mod error_handling_patterns;
mod eval;
#[path = "eval-advanced.rs"]
mod eval_advanced;
#[path = "eval-advanced-2.rs"]
mod eval_advanced_2;
mod event_convert_list;
mod format;
#[path = "format-advanced.rs"]
mod format_advanced;
#[path = "format-extended.rs"]
mod format_extended;
#[path = "forward-char.rs"]
mod forward_char;
#[path = "forward-comment.rs"]
mod forward_comment;
#[path = "forward-line.rs"]
mod forward_line;
#[path = "fset-symbol-function.rs"]
mod fset_symbol_function;
mod funcall;
mod r#get;
#[path = "goto-char.rs"]
mod goto_char;
#[path = "hash-table.rs"]
mod hash_table;
#[path = "hash-table-extended.rs"]
mod hash_table_extended;
#[path = "hash-table-patterns.rs"]
mod hash_table_patterns;
mod r#if;
#[path = "indirect-function.rs"]
mod indirect_function;
mod insert;
#[path = "insert-advanced.rs"]
mod insert_advanced;
#[path = "interactive-patterns.rs"]
mod interactive_patterns;
#[path = "key-description.rs"]
mod key_description;
mod keymap;
#[path = "lambda-anonymous.rs"]
mod lambda_anonymous;
mod last;
#[path = "length-operations.rs"]
mod length_operations;
mod r#let;
#[path = "let-dynamic.rs"]
mod let_dynamic;
#[path = "let-star.rs"]
mod let_star;
#[path = "let-star-advanced.rs"]
mod let_star_advanced;
#[path = "let-star-advanced-2.rs"]
mod let_star_advanced_2;
mod list;
#[path = "make-list.rs"]
mod make_list;
#[path = "make-string.rs"]
mod make_string;
#[path = "make-symbol.rs"]
mod make_symbol;
#[path = "map-operations.rs"]
mod map_operations;
mod mapcar;
#[path = "marker-operations.rs"]
mod marker_operations;
#[path = "match-beginning.rs"]
mod match_beginning;
#[path = "match-data.rs"]
mod match_data;
#[path = "match-end.rs"]
mod match_end;
#[path = "math-functions.rs"]
mod math_functions;
mod max;
mod member;
mod memq;
mod min;
#[path = "modify-syntax-entry.rs"]
mod modify_syntax_entry;
#[path = "narrow-advanced.rs"]
mod narrow_advanced;
mod nconc;
mod r#not;
mod nreverse;
mod nthcdr;
#[path = "nthcdr-advanced.rs"]
mod nthcdr_advanced;
#[path = "number-predicates.rs"]
mod number_predicates;
#[path = "number-to-string.rs"]
mod number_to_string;
mod oclosure;
mod r#or;
mod plist;
mod point;
#[path = "point-max.rs"]
mod point_max;
#[path = "point-min.rs"]
mod point_min;
mod predicates;
mod prog1;
mod progn;
#[path = "progn-advanced.rs"]
mod progn_advanced;
mod progn_ast;
#[path = "property-list-advanced.rs"]
mod property_list_advanced;
mod put;
#[path = "re-search-forward.rs"]
mod re_search_forward;
#[path = "read-print.rs"]
mod read_print;
mod recursion;
#[path = "regexp-advanced.rs"]
mod regexp_advanced;
#[path = "regexp-operations.rs"]
mod regexp_operations;
mod reverse;
#[path = "save-excursion.rs"]
mod save_excursion;
#[path = "search-operations.rs"]
mod search_operations;
#[path = "sequence-operations.rs"]
mod sequence_operations;
mod sequencep;
#[path = "set-buffer.rs"]
mod set_buffer;
mod setcar;
mod setcdr;
mod setq;
#[path = "setq-advanced.rs"]
mod setq_advanced;
mod signal;
#[path = "skip-chars.rs"]
mod skip_chars;
mod sort;
#[path = "sort-extended.rs"]
mod sort_extended;
mod string;
#[path = "string-distance.rs"]
mod string_distance;
#[path = "string-equal.rs"]
mod string_equal;
#[path = "string-lessp.rs"]
mod string_lessp;
#[path = "string-manipulation.rs"]
mod string_manipulation;
#[path = "string-match.rs"]
mod string_match;
#[path = "string-processing.rs"]
mod string_processing;
#[path = "string-replace.rs"]
mod string_replace;
#[path = "string-match-p.rs"]
mod string_match_p;
#[path = "string-to-number.rs"]
mod string_to_number;
#[path = "string-version-lessp.rs"]
mod string_version_lessp;
#[path = "subr-predicates.rs"]
mod subr_predicates;
mod substring;
mod symbol;
#[path = "symbol-advanced.rs"]
mod symbol_advanced;
#[path = "syntax-table.rs"]
mod syntax_table;
mod take;
#[path = "text-properties.rs"]
mod text_properties;
mod trigonometry;
mod r#throw;
#[path = "type-of.rs"]
mod type_of;
#[path = "type-predicates.rs"]
mod type_predicates;
mod unless;
#[path = "unwind-protect.rs"]
mod unwind_protect;
#[path = "upcase-downcase.rs"]
mod upcase_downcase;
mod vector;
#[path = "vector-operations.rs"]
mod vector_operations;
mod when;
mod r#while;
#[path = "while-advanced.rs"]
mod while_advanced;
#[path = "with-temp-buffer.rs"]
mod with_temp_buffer;
