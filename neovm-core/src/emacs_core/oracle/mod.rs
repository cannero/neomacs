//! Oracle-backed Elisp parity tests.

mod abs;
mod advice;
#[path = "alist-get.rs"]
mod alist_get;
mod r#and;
mod apply;
mod arithmetic;
mod assoc;
mod assq;
mod backquote;
#[path = "beginning-of-line.rs"]
mod beginning_of_line;
#[path = "buffer-name.rs"]
mod buffer_name;
#[path = "buffer-string.rs"]
mod buffer_string;
#[path = "buffer-substring.rs"]
mod buffer_substring;
#[path = "car-safe.rs"]
mod car_safe;
mod r#catch;
#[path = "char-after.rs"]
mod char_after;
#[path = "char-literal.rs"]
mod char_literal;
#[path = "char-table.rs"]
mod char_table;
#[path = "char-table-extra-slot.rs"]
mod char_table_extra_slot;
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
mod combination_complex;
pub(crate) mod common;
#[path = "compare-strings.rs"]
mod compare_strings;
mod comparison;
mod cond;
#[path = "condition-case.rs"]
mod condition_case;
#[path = "copy-alist.rs"]
mod copy_alist;
mod coverage;
mod coverage_manifest;
#[path = "current-buffer.rs"]
mod current_buffer;
#[path = "defmacro-macroexpand.rs"]
mod defmacro_macroexpand;
mod defvar;
#[path = "delete-region.rs"]
mod delete_region;
mod delq;
mod dolist;
mod dotimes;
#[path = "end-of-line.rs"]
mod end_of_line;
mod equality;
mod eval;
mod event_convert_list;
mod format;
#[path = "forward-char.rs"]
mod forward_char;
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
mod r#if;
mod insert;
#[path = "interactive-patterns.rs"]
mod interactive_patterns;
#[path = "key-description.rs"]
mod key_description;
mod keymap;
#[path = "lambda-anonymous.rs"]
mod lambda_anonymous;
mod last;
mod r#let;
#[path = "let-dynamic.rs"]
mod let_dynamic;
#[path = "let-star.rs"]
mod let_star;
mod list;
#[path = "make-list.rs"]
mod make_list;
#[path = "make-string.rs"]
mod make_string;
mod mapcar;
#[path = "math-functions.rs"]
mod math_functions;
#[path = "match-beginning.rs"]
mod match_beginning;
#[path = "match-end.rs"]
mod match_end;
mod max;
mod member;
mod memq;
mod min;
#[path = "modify-syntax-entry.rs"]
mod modify_syntax_entry;
mod nconc;
mod r#not;
mod nreverse;
mod nthcdr;
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
mod progn_ast;
mod put;
#[path = "re-search-forward.rs"]
mod re_search_forward;
mod recursion;
mod reverse;
#[path = "set-buffer.rs"]
mod set_buffer;
mod sequencep;
mod setcar;
mod setcdr;
mod signal;
mod setq;
mod sort;
mod string;
#[path = "string-manipulation.rs"]
mod string_manipulation;
#[path = "string-distance.rs"]
mod string_distance;
#[path = "string-equal.rs"]
mod string_equal;
#[path = "string-lessp.rs"]
mod string_lessp;
#[path = "string-match.rs"]
mod string_match;
#[path = "string-to-number.rs"]
mod string_to_number;
#[path = "string-version-lessp.rs"]
mod string_version_lessp;
mod substring;
mod symbol;
#[path = "syntax-table.rs"]
mod syntax_table;
mod take;
#[path = "text-properties.rs"]
mod text_properties;
mod r#throw;
#[path = "type-of.rs"]
mod type_of;
mod unless;
#[path = "unwind-protect.rs"]
mod unwind_protect;
#[path = "upcase-downcase.rs"]
mod upcase_downcase;
mod vector;
mod when;
mod r#while;
#[path = "with-temp-buffer.rs"]
mod with_temp_buffer;
