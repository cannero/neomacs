use super::*;
use crate::buffer::BufferText;
use crate::buffer::buffer::{Buffer, BufferId};
use crate::emacs_core::value::read_cons;

/// Helper: create a buffer with given text, point at start, full accessible range.
fn buf_with_text(text: &str) -> Buffer {
    let mut buf = Buffer::new(BufferId(99), "test-syntax".into());
    buf.text = BufferText::from_str(text);
    buf.zv = buf.text.len();
    buf.pt = 0;
    buf
}

// -----------------------------------------------------------------------
// SyntaxClass parsing
// -----------------------------------------------------------------------

#[test]
fn syntax_class_roundtrip() {
    let classes = [
        (' ', SyntaxClass::Whitespace),
        ('w', SyntaxClass::Word),
        ('_', SyntaxClass::Symbol),
        ('.', SyntaxClass::Punctuation),
        ('(', SyntaxClass::Open),
        (')', SyntaxClass::Close),
        ('\'', SyntaxClass::Prefix),
        ('"', SyntaxClass::StringDelim),
        ('$', SyntaxClass::MathDelim),
        ('\\', SyntaxClass::Escape),
        ('/', SyntaxClass::CharQuote),
        ('<', SyntaxClass::Comment),
        ('>', SyntaxClass::EndComment),
        ('@', SyntaxClass::InheritStandard),
        ('!', SyntaxClass::Generic),
    ];
    for (ch, class) in &classes {
        assert_eq!(SyntaxClass::from_char(*ch), Some(*class));
        assert_eq!(class.to_char(), *ch);
    }
}

#[test]
fn syntax_class_dash_is_whitespace() {
    assert_eq!(SyntaxClass::from_char('-'), Some(SyntaxClass::Whitespace));
}

// -----------------------------------------------------------------------
// string-to-syntax parser
// -----------------------------------------------------------------------

#[test]
fn string_to_syntax_whitespace() {
    let entry = string_to_syntax(" ").unwrap();
    assert_eq!(entry.class, SyntaxClass::Whitespace);
    assert_eq!(entry.matching_char, None);
    assert!(entry.flags.is_empty());
}

#[test]
fn string_to_syntax_word() {
    let entry = string_to_syntax("w").unwrap();
    assert_eq!(entry.class, SyntaxClass::Word);
}

#[test]
fn string_to_syntax_open_paren() {
    let entry = string_to_syntax("()").unwrap();
    assert_eq!(entry.class, SyntaxClass::Open);
    assert_eq!(entry.matching_char, Some(')'));
}

#[test]
fn string_to_syntax_close_paren() {
    let entry = string_to_syntax(")(").unwrap();
    assert_eq!(entry.class, SyntaxClass::Close);
    assert_eq!(entry.matching_char, Some('('));
}

#[test]
fn string_to_syntax_string_delim() {
    let entry = string_to_syntax("\"").unwrap();
    assert_eq!(entry.class, SyntaxClass::StringDelim);
}

#[test]
fn string_to_syntax_prefix_class() {
    let mut heap = crate::gc::heap::LispHeap::new();
    crate::emacs_core::value::set_current_heap(&mut heap);

    let entry = string_to_syntax("'").unwrap();
    assert_eq!(entry.class, SyntaxClass::Prefix);
    let value = syntax_entry_to_value(&entry);
    if let Value::Cons(cell) = &value {
        let cell = read_cons(*cell);
        assert!(matches!(cell.car, Value::Int(6)));
    } else {
        panic!("Expected cons cell");
    }
}

#[test]
fn builtin_string_to_syntax_at_returns_nil() {
    let out = builtin_string_to_syntax(vec![Value::string("@")]).unwrap();
    assert_eq!(out, Value::Nil);
}

#[test]
fn string_to_syntax_with_flags() {
    let entry = string_to_syntax(". 12").unwrap();
    assert_eq!(entry.class, SyntaxClass::Punctuation);
    assert_eq!(entry.matching_char, None);
    assert!(entry.flags.contains(SyntaxFlags::COMMENT_START_FIRST));
    assert!(entry.flags.contains(SyntaxFlags::COMMENT_START_SECOND));
}

#[test]
fn string_to_syntax_comment_style_b() {
    let entry = string_to_syntax(". 12b").unwrap();
    assert!(entry.flags.contains(SyntaxFlags::COMMENT_STYLE_B));
}

#[test]
fn string_to_syntax_comment_style_c() {
    let entry = string_to_syntax(". c").unwrap();
    assert!(entry.flags.contains(SyntaxFlags::COMMENT_STYLE_C));
}

#[test]
fn string_to_syntax_prefix_flag() {
    let entry = string_to_syntax(". p").unwrap();
    assert_eq!(entry.class, SyntaxClass::Punctuation);
    assert!(entry.flags.contains(SyntaxFlags::PREFIX));
}

#[test]
fn string_to_syntax_empty_errors() {
    assert!(string_to_syntax("").is_err());
}

#[test]
fn string_to_syntax_invalid_class() {
    assert!(string_to_syntax("Z").is_err());
}

// -----------------------------------------------------------------------
// SyntaxTable
// -----------------------------------------------------------------------

#[test]
fn standard_table_word_chars() {
    let table = SyntaxTable::new_standard();
    assert_eq!(table.char_syntax('a'), SyntaxClass::Word);
    assert_eq!(table.char_syntax('Z'), SyntaxClass::Word);
    assert_eq!(table.char_syntax('5'), SyntaxClass::Word);
    assert_eq!(table.char_syntax('$'), SyntaxClass::Word);
    assert_eq!(table.char_syntax('%'), SyntaxClass::Word);
}

#[test]
fn standard_table_whitespace() {
    let table = SyntaxTable::new_standard();
    assert_eq!(table.char_syntax(' '), SyntaxClass::Whitespace);
    assert_eq!(table.char_syntax('\t'), SyntaxClass::Whitespace);
    assert_eq!(table.char_syntax('\n'), SyntaxClass::Whitespace);
}

#[test]
fn standard_table_parens() {
    let table = SyntaxTable::new_standard();
    assert_eq!(table.char_syntax('('), SyntaxClass::Open);
    assert_eq!(table.char_syntax(')'), SyntaxClass::Close);
    assert_eq!(table.char_syntax('['), SyntaxClass::Open);
    assert_eq!(table.char_syntax(']'), SyntaxClass::Close);
}

#[test]
fn standard_table_string_delim() {
    let table = SyntaxTable::new_standard();
    assert_eq!(table.char_syntax('"'), SyntaxClass::StringDelim);
}

#[test]
fn standard_table_escape() {
    let table = SyntaxTable::new_standard();
    assert_eq!(table.char_syntax('\\'), SyntaxClass::Escape);
}

#[test]
fn standard_table_punctuation() {
    let table = SyntaxTable::new_standard();
    assert_eq!(table.char_syntax('\u{0001}'), SyntaxClass::Punctuation);
    assert_eq!(table.char_syntax('\u{007f}'), SyntaxClass::Punctuation);
    assert_eq!(table.char_syntax(';'), SyntaxClass::Punctuation);
    assert_eq!(table.char_syntax('?'), SyntaxClass::Punctuation);
    assert_eq!(table.char_syntax('.'), SyntaxClass::Punctuation);
}

#[test]
fn standard_table_symbol_constituents() {
    let table = SyntaxTable::new_standard();
    assert_eq!(table.char_syntax('_'), SyntaxClass::Symbol);
    assert_eq!(table.char_syntax('-'), SyntaxClass::Symbol);
    assert_eq!(table.char_syntax('+'), SyntaxClass::Symbol);
    assert_eq!(table.char_syntax('/'), SyntaxClass::Symbol);
    assert_eq!(table.char_syntax('='), SyntaxClass::Symbol);
}

#[test]
fn modify_syntax_entry_overrides() {
    let mut table = SyntaxTable::new_standard();
    assert_eq!(table.char_syntax('+'), SyntaxClass::Symbol);
    table.modify_syntax_entry('+', SyntaxEntry::simple(SyntaxClass::Word));
    assert_eq!(table.char_syntax('+'), SyntaxClass::Word);
}

#[test]
fn inherited_table_falls_back() {
    let table = SyntaxTable::make_syntax_table();
    // Should inherit from standard.
    assert_eq!(table.char_syntax('a'), SyntaxClass::Word);
    assert_eq!(table.char_syntax(' '), SyntaxClass::Whitespace);
}

#[test]
fn inherited_table_override() {
    let mut table = SyntaxTable::make_syntax_table();
    table.modify_syntax_entry('a', SyntaxEntry::simple(SyntaxClass::Punctuation));
    assert_eq!(table.char_syntax('a'), SyntaxClass::Punctuation);
    // Other inherited entries still work.
    assert_eq!(table.char_syntax('b'), SyntaxClass::Word);
}

#[test]
fn copy_syntax_table_is_independent() {
    let original = SyntaxTable::new_standard();
    let mut copy = original.copy_syntax_table();
    copy.modify_syntax_entry('a', SyntaxEntry::simple(SyntaxClass::Punctuation));
    assert_eq!(original.char_syntax('a'), SyntaxClass::Word);
    assert_eq!(copy.char_syntax('a'), SyntaxClass::Punctuation);
}

#[test]
fn non_ascii_defaults_to_word() {
    let table = SyntaxTable::new_standard();
    // A random Unicode character not in the table.
    assert_eq!(table.char_syntax('\u{1F600}'), SyntaxClass::Word);
}

// -----------------------------------------------------------------------
// forward_word / backward_word
// -----------------------------------------------------------------------

#[test]
fn forward_word_basic() {
    let mut buf = buf_with_text("hello world");
    buf.pt = 0;
    let table = SyntaxTable::new_standard();
    let pos = forward_word(&buf, &table, 1);
    // "hello" ends at byte 5.
    assert_eq!(pos, 5);
}

#[test]
fn forward_word_two() {
    let mut buf = buf_with_text("hello world");
    buf.pt = 0;
    let table = SyntaxTable::new_standard();
    let pos = forward_word(&buf, &table, 2);
    // Past "hello world" = byte 11.
    assert_eq!(pos, 11);
}

#[test]
fn forward_word_from_middle() {
    let mut buf = buf_with_text("hello world");
    buf.pt = 3; // inside "hello"
    let table = SyntaxTable::new_standard();
    let pos = forward_word(&buf, &table, 1);
    assert_eq!(pos, 5); // end of "hello"
}

#[test]
fn backward_word_basic() {
    let mut buf = buf_with_text("hello world");
    buf.pt = 11; // end of text
    let table = SyntaxTable::new_standard();
    let pos = backward_word(&buf, &table, 1);
    assert_eq!(pos, 6); // start of "world"
}

#[test]
fn backward_word_two() {
    let mut buf = buf_with_text("hello world");
    buf.pt = 11;
    let table = SyntaxTable::new_standard();
    let pos = backward_word(&buf, &table, 2);
    assert_eq!(pos, 0); // start of "hello"
}

#[test]
fn forward_word_negative_goes_backward() {
    let mut buf = buf_with_text("hello world");
    buf.pt = 11;
    let table = SyntaxTable::new_standard();
    let pos = forward_word(&buf, &table, -1);
    assert_eq!(pos, 6);
}

// -----------------------------------------------------------------------
// skip_syntax_forward / skip_syntax_backward
// -----------------------------------------------------------------------

#[test]
fn skip_syntax_forward_word() {
    let mut buf = buf_with_text("hello world");
    buf.pt = 0;
    let table = SyntaxTable::new_standard();
    let pos = skip_syntax_forward(&buf, &table, "w", None);
    assert_eq!(pos, 5); // end of "hello"
}

#[test]
fn skip_syntax_forward_whitespace_and_word() {
    let mut buf = buf_with_text("  hello");
    buf.pt = 0;
    let table = SyntaxTable::new_standard();
    let pos = skip_syntax_forward(&buf, &table, " w", None);
    assert_eq!(pos, 7); // end of "  hello"
}

#[test]
fn skip_syntax_backward_word() {
    let mut buf = buf_with_text("hello world");
    buf.pt = 11;
    let table = SyntaxTable::new_standard();
    let pos = skip_syntax_backward(&buf, &table, "w", None);
    assert_eq!(pos, 6); // start of "world"
}

#[test]
fn skip_syntax_forward_with_limit() {
    let mut buf = buf_with_text("helloworld");
    buf.pt = 0;
    let table = SyntaxTable::new_standard();
    let pos = skip_syntax_forward(&buf, &table, "w", Some(3));
    assert_eq!(pos, 3);
}

#[test]
fn builtin_skip_syntax_forward_limit_uses_char_positions_for_multibyte_text() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("éézz");
        buf.goto_char(buf.point_min());
    }

    let moved =
        builtin_skip_syntax_forward(&mut eval, vec![Value::string("w"), Value::Int(3)]).unwrap();
    assert_eq!(moved, Value::Int(2));
    assert_eq!(
        eval.buffers
            .current_buffer()
            .expect("current buffer")
            .point_char() as i64
            + 1,
        3
    );
}

// -----------------------------------------------------------------------
// scan_sexps (balanced expressions)
// -----------------------------------------------------------------------

#[test]
fn scan_sexps_forward_parens() {
    let buf = buf_with_text("(hello world)");
    let table = SyntaxTable::new_standard();
    let pos = scan_sexps(&buf, &table, 0, 1).unwrap();
    assert_eq!(pos, 13); // past closing paren
}

#[test]
fn scan_sexps_forward_nested() {
    let buf = buf_with_text("(a (b c) d)");
    let table = SyntaxTable::new_standard();
    let pos = scan_sexps(&buf, &table, 0, 1).unwrap();
    assert_eq!(pos, 11);
}

#[test]
fn scan_sexps_forward_word() {
    let buf = buf_with_text("hello world");
    let table = SyntaxTable::new_standard();
    let pos = scan_sexps(&buf, &table, 0, 1).unwrap();
    assert_eq!(pos, 5); // end of "hello"
}

#[test]
fn scan_sexps_forward_string() {
    let buf = buf_with_text("\"hello\" world");
    let table = SyntaxTable::new_standard();
    let pos = scan_sexps(&buf, &table, 0, 1).unwrap();
    assert_eq!(pos, 7); // past closing quote
}

#[test]
fn scan_sexps_backward_parens() {
    let buf = buf_with_text("(hello world)");
    let table = SyntaxTable::new_standard();
    // Start after closing paren.
    let pos = scan_sexps(&buf, &table, 13, -1).unwrap();
    assert_eq!(pos, 0); // back to opening paren
}

#[test]
fn scan_sexps_forward_unbalanced() {
    let buf = buf_with_text("(hello");
    let table = SyntaxTable::new_standard();
    assert!(scan_sexps(&buf, &table, 0, 1).is_err());
}

#[test]
fn scan_sexps_backward_unbalanced() {
    let buf = buf_with_text("hello)");
    let table = SyntaxTable::new_standard();
    assert!(scan_sexps(&buf, &table, 6, -1).is_err());
}

#[test]
fn scan_sexps_zero_count() {
    let buf = buf_with_text("(hello)");
    let table = SyntaxTable::new_standard();
    let pos = scan_sexps(&buf, &table, 3, 0).unwrap();
    assert_eq!(pos, 3); // unchanged
}

#[test]
fn scan_sexps_forward_brackets() {
    let buf = buf_with_text("[a b c]");
    let table = SyntaxTable::new_standard();
    let pos = scan_sexps(&buf, &table, 0, 1).unwrap();
    assert_eq!(pos, 7);
}

#[test]
fn scan_sexps_string_with_escape() {
    let buf = buf_with_text("\"he\\\"llo\" world");
    let table = SyntaxTable::new_standard();
    let pos = scan_sexps(&buf, &table, 0, 1).unwrap();
    assert_eq!(pos, 9); // past the closing quote
}

// -----------------------------------------------------------------------
// syntax_entry_to_value
// -----------------------------------------------------------------------

#[test]
fn syntax_entry_to_value_simple() {
    let mut heap = crate::gc::heap::LispHeap::new();
    crate::emacs_core::value::set_current_heap(&mut heap);

    let entry = SyntaxEntry::simple(SyntaxClass::Word);
    let val = syntax_entry_to_value(&entry);
    // Should be (2 . nil) since Word code = 2
    if let Value::Cons(cell) = &val {
        let cell = read_cons(*cell);
        assert!(matches!(cell.car, Value::Int(2)));
        assert!(matches!(cell.cdr, Value::Nil));
    } else {
        panic!("Expected cons cell");
    }
}

#[test]
fn syntax_entry_to_value_with_match() {
    let mut heap = crate::gc::heap::LispHeap::new();
    crate::emacs_core::value::set_current_heap(&mut heap);

    let entry = SyntaxEntry::with_match(SyntaxClass::Open, ')');
    let val = syntax_entry_to_value(&entry);
    if let Value::Cons(cell) = &val {
        let cell = read_cons(*cell);
        assert!(matches!(cell.car, Value::Int(4))); // Open code = 4
        assert!(matches!(cell.cdr, Value::Int(41))); // ')' = 41
    } else {
        panic!("Expected cons cell");
    }
}

#[test]
fn syntax_entry_to_value_with_flags() {
    let mut heap = crate::gc::heap::LispHeap::new();
    crate::emacs_core::value::set_current_heap(&mut heap);

    let entry = SyntaxEntry {
        class: SyntaxClass::Punctuation,
        matching_char: None,
        flags: SyntaxFlags::COMMENT_START_FIRST | SyntaxFlags::COMMENT_START_SECOND,
    };
    let val = syntax_entry_to_value(&entry);
    if let Value::Cons(cell) = &val {
        let cell = read_cons(*cell);
        // code = 1 (punctuation) | (0x03 << 16) = 1 | 196608 = 196609
        assert!(matches!(cell.car, Value::Int(196609)));
    } else {
        panic!("Expected cons cell");
    }
}

#[test]
fn make_syntax_table_returns_syntax_char_table() {
    let table = builtin_make_syntax_table(vec![]).unwrap();
    let is_ct = crate::emacs_core::chartable::builtin_char_table_p(vec![table]).unwrap();
    assert_eq!(is_ct, Value::True);
    let subtype = crate::emacs_core::chartable::builtin_char_table_subtype(vec![table]).unwrap();
    assert_eq!(subtype, Value::symbol("syntax-table"));
}

#[test]
fn make_syntax_table_parent_must_be_char_table() {
    match builtin_make_syntax_table(vec![Value::Int(1)]) {
        Err(crate::emacs_core::error::Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data.first(), Some(&Value::symbol("char-table-p")));
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn standard_syntax_table_returns_char_table() {
    let table = builtin_standard_syntax_table(vec![]).unwrap();
    let is_ct = crate::emacs_core::chartable::builtin_char_table_p(vec![table]).unwrap();
    assert_eq!(is_ct, Value::True);
    let subtype = crate::emacs_core::chartable::builtin_char_table_subtype(vec![table]).unwrap();
    assert_eq!(subtype, Value::symbol("syntax-table"));
}

#[test]
fn copy_syntax_table_returns_fresh_syntax_table() {
    let source = builtin_make_syntax_table(vec![]).unwrap();
    let copied = builtin_copy_syntax_table(vec![source]).unwrap();

    let is_ct = crate::emacs_core::chartable::builtin_char_table_p(vec![copied]).unwrap();
    assert_eq!(is_ct, Value::True);
    let subtype = crate::emacs_core::chartable::builtin_char_table_subtype(vec![copied]).unwrap();
    assert_eq!(subtype, Value::symbol("syntax-table"));

    match (source, copied) {
        (Value::Vector(a), Value::Vector(b)) => assert_ne!(a, b),
        other => panic!("expected vector-backed char tables, got {other:?}"),
    }
}

#[test]
fn copy_syntax_table_validates_arity_and_type() {
    match builtin_copy_syntax_table(vec![Value::Int(1)]) {
        Err(crate::emacs_core::error::Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data.first(), Some(&Value::symbol("syntax-table-p")));
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }

    match builtin_copy_syntax_table(vec![Value::Nil, Value::Nil]) {
        Err(crate::emacs_core::error::Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(sig.data.first(), Some(&Value::symbol("copy-syntax-table")));
        }
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn syntax_class_to_char_basics_and_errors() {
    assert_eq!(
        builtin_syntax_class_to_char(vec![Value::Int(0)]).unwrap(),
        Value::Char(' ')
    );
    assert_eq!(
        builtin_syntax_class_to_char(vec![Value::Int(15)]).unwrap(),
        Value::Char('|')
    );

    match builtin_syntax_class_to_char(vec![Value::Int(-1)]) {
        Err(crate::emacs_core::error::Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::Int(15), Value::Int(-1)]);
        }
        other => panic!("expected args-out-of-range signal, got {other:?}"),
    }

    match builtin_syntax_class_to_char(vec![Value::string("x")]) {
        Err(crate::emacs_core::error::Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data.first(), Some(&Value::symbol("fixnump")));
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn matching_paren_basics_and_errors() {
    assert_eq!(
        builtin_matching_paren(vec![Value::Int('(' as i64)]).unwrap(),
        Value::Char(')')
    );
    assert_eq!(
        builtin_matching_paren(vec![Value::Int(']' as i64)]).unwrap(),
        Value::Char('[')
    );
    assert_eq!(
        builtin_matching_paren(vec![Value::Int('a' as i64)]).unwrap(),
        Value::Nil
    );

    match builtin_matching_paren(vec![Value::string("(")]) {
        Err(crate::emacs_core::error::Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data.first(), Some(&Value::symbol("characterp")));
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }

    match builtin_matching_paren(vec![]) {
        Err(crate::emacs_core::error::Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(sig.data.first(), Some(&Value::symbol("matching-paren")));
        }
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn syntax_table_eval_returns_char_table() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let table = builtin_syntax_table(&mut eval, vec![]).unwrap();
    let is_ct = crate::emacs_core::chartable::builtin_char_table_p(vec![table]).unwrap();
    assert_eq!(is_ct, Value::True);
    let subtype = crate::emacs_core::chartable::builtin_char_table_subtype(vec![table]).unwrap();
    assert_eq!(subtype, Value::symbol("syntax-table"));
}

#[test]
fn syntax_table_p_recognizes_syntax_tables() {
    let syntax_table = builtin_make_syntax_table(vec![]).unwrap();
    let is_syntax = builtin_syntax_table_p(vec![syntax_table]).unwrap();
    assert_eq!(is_syntax, Value::True);

    let char_table =
        crate::emacs_core::chartable::make_char_table_value(Value::symbol("foo"), Value::Nil);
    let not_syntax = builtin_syntax_table_p(vec![char_table]).unwrap();
    assert_eq!(not_syntax, Value::Nil);

    let atom = builtin_syntax_table_p(vec![Value::Int(1)]).unwrap();
    assert_eq!(atom, Value::Nil);
}

#[test]
fn set_syntax_table_validates_and_returns_table() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let table = builtin_make_syntax_table(vec![]).unwrap();
    let out = builtin_set_syntax_table(&mut eval, vec![table]).unwrap();
    assert_eq!(out, table);

    match builtin_set_syntax_table(&mut eval, vec![Value::Int(1)]) {
        Err(crate::emacs_core::error::Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data.first(), Some(&Value::symbol("syntax-table-p")));
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn syntax_table_and_standard_default_to_same_object() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let current = builtin_syntax_table(&mut eval, vec![]).unwrap();
    let standard = builtin_standard_syntax_table(vec![]).unwrap();
    match (current, standard) {
        (Value::Vector(a), Value::Vector(b)) => assert_eq!(a, b),
        other => panic!("expected syntax-table vectors, got {other:?}"),
    }
}

#[test]
fn set_syntax_table_updates_current_buffer_only() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let custom = builtin_make_syntax_table(vec![]).unwrap();
    builtin_modify_syntax_entry(
        &mut eval,
        vec![Value::Int(';' as i64), Value::string("<"), custom],
    )
    .unwrap();
    builtin_modify_syntax_entry(
        &mut eval,
        vec![Value::Int('\n' as i64), Value::string(">"), custom],
    )
    .unwrap();
    let current_id = eval.buffers.current_buffer().expect("current buffer").id;
    let other_id = eval.buffers.create_buffer("*syntax-other*");

    let out = builtin_set_syntax_table(&mut eval, vec![custom]).unwrap();
    assert_eq!(out, custom);
    let current = builtin_syntax_table(&mut eval, vec![]).unwrap();
    assert_eq!(current, custom);

    eval.buffers.set_current(other_id);
    let other = builtin_syntax_table(&mut eval, vec![]).unwrap();
    match (&other, &custom) {
        (Value::Vector(a), Value::Vector(b)) => assert_ne!(a, b),
        pair => panic!("expected syntax-table vectors, got {pair:?}"),
    }

    eval.buffers.set_current(current_id);
    let restored = builtin_syntax_table(&mut eval, vec![]).unwrap();
    assert_eq!(restored, custom);
    assert_eq!(
        builtin_char_syntax(&mut eval, vec![Value::Int(';' as i64)]).unwrap(),
        Value::Char('<')
    );
    assert_eq!(
        builtin_char_syntax(&mut eval, vec![Value::Int('\n' as i64)]).unwrap(),
        Value::Char('>')
    );
}

#[test]
fn forward_comment_skips_whitespace_and_returns_nil() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("  foo");
        buf.goto_char(buf.point_min());
    }

    let out = builtin_forward_comment(&mut eval, vec![Value::Int(1)]).unwrap();
    assert_eq!(out, Value::Nil);
    let point_1 = eval
        .buffers
        .current_buffer()
        .expect("current buffer")
        .point_char() as i64
        + 1;
    assert_eq!(point_1, 3);
}

#[test]
fn forward_comment_validates_arity_and_type() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    match builtin_forward_comment(&mut eval, vec![]) {
        Err(crate::emacs_core::error::Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(sig.data.first(), Some(&Value::symbol("forward-comment")));
        }
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    match builtin_forward_comment(&mut eval, vec![Value::symbol("x")]) {
        Err(crate::emacs_core::error::Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data.first(), Some(&Value::symbol("integerp")));
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn backward_prefix_chars_default_is_noop() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("''foo");
        buf.goto_char(buf.text.char_to_byte(2));
    }

    let out = builtin_backward_prefix_chars(&mut eval, vec![]).unwrap();
    assert_eq!(out, Value::Nil);
    let point_1 = eval
        .buffers
        .current_buffer()
        .expect("current buffer")
        .point_char() as i64
        + 1;
    assert_eq!(point_1, 3);
}

#[test]
fn backward_prefix_chars_moves_over_prefix_flag_chars() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("''foo");
        buf.goto_char(buf.point_min());
        let entry = string_to_syntax(". p").unwrap();
        buf.syntax_table.modify_syntax_entry('\'', entry);
        buf.goto_char(buf.text.char_to_byte(2));
    }

    builtin_backward_prefix_chars(&mut eval, vec![]).unwrap();
    let point_1 = eval
        .buffers
        .current_buffer()
        .expect("current buffer")
        .point_char() as i64
        + 1;
    assert_eq!(point_1, 1);
}

#[test]
fn backward_prefix_chars_validates_arity() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    match builtin_backward_prefix_chars(&mut eval, vec![Value::Int(1)]) {
        Err(crate::emacs_core::error::Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data.first(),
                Some(&Value::symbol("backward-prefix-chars"))
            );
        }
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn modify_syntax_entry_at_descriptor_inherits_parent_or_default() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    builtin_modify_syntax_entry(&mut eval, vec![Value::Int('x' as i64), Value::string("@")])
        .unwrap();

    let out = builtin_char_syntax(&mut eval, vec![Value::Int('x' as i64)]).unwrap();
    assert_eq!(out, Value::Char(' '));
}

#[test]
fn syntax_ppss_flush_cache_contract() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();

    assert_eq!(
        builtin_syntax_ppss_flush_cache(&mut eval, vec![Value::Int(1)]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        builtin_syntax_ppss_flush_cache(
            &mut eval,
            vec![Value::Int(1), Value::symbol("ignored"), Value::Int(3)],
        )
        .unwrap(),
        Value::Nil
    );

    match builtin_syntax_ppss_flush_cache(&mut eval, vec![]) {
        Err(crate::emacs_core::error::Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data.first(),
                Some(&Value::symbol("syntax-ppss-flush-cache"))
            );
        }
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    match builtin_syntax_ppss_flush_cache(&mut eval, vec![Value::Nil]) {
        Err(crate::emacs_core::error::Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data.first(), Some(&Value::symbol("number-or-marker-p")));
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn scan_lists_basic_and_backward_nil() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("(a b)");
    }

    let forward =
        builtin_scan_lists(&mut eval, vec![Value::Int(1), Value::Int(1), Value::Int(0)]).unwrap();
    assert_eq!(forward, Value::Int(6));

    let backward = builtin_scan_lists(
        &mut eval,
        vec![Value::Int(1), Value::Int(-1), Value::Int(0)],
    )
    .unwrap();
    assert_eq!(backward, Value::Nil);
}

#[test]
fn syntax_after_returns_descriptor_and_nil_out_of_range() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("a(");
    }

    let word = builtin_syntax_after(&mut eval, vec![Value::Int(1)]).unwrap();
    assert_eq!(
        word,
        syntax_entry_to_value(&SyntaxEntry::simple(SyntaxClass::Word))
    );

    let open = builtin_syntax_after(&mut eval, vec![Value::Int(2)]).unwrap();
    assert_eq!(
        open,
        syntax_entry_to_value(&SyntaxEntry::with_match(SyntaxClass::Open, ')'))
    );

    let oob = builtin_syntax_after(&mut eval, vec![Value::Int(3)]).unwrap();
    assert_eq!(oob, Value::Nil);
}

#[test]
fn scan_sexps_basic_and_backward_nil() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("(a b)");
    }

    let forward = builtin_scan_sexps(&mut eval, vec![Value::Int(1), Value::Int(1)]).unwrap();
    assert_eq!(forward, Value::Int(6));

    let backward = builtin_scan_sexps(&mut eval, vec![Value::Int(1), Value::Int(-1)]).unwrap();
    assert_eq!(backward, Value::Nil);
}

#[test]
fn parse_partial_sexp_baseline_shapes() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("abc");
    }
    let state = builtin_parse_partial_sexp(&mut eval, vec![Value::Int(1), Value::Int(4)]).unwrap();
    assert_eq!(
        state,
        Value::list(vec![
            Value::Int(0),
            Value::Nil,
            Value::Int(1),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Int(0),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ])
    );

    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("(a)");
    }
    let nested = builtin_parse_partial_sexp(&mut eval, vec![Value::Int(1), Value::Int(3)]).unwrap();
    assert_eq!(
        nested,
        Value::list(vec![
            Value::Int(1),
            Value::Int(1),
            Value::Int(2),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Int(0),
            Value::Nil,
            Value::Nil,
            Value::list(vec![Value::Int(1)]),
            Value::Nil,
        ])
    );
}

#[test]
fn syntax_ppss_baseline_shape() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("(a)");
    }

    let state = builtin_syntax_ppss(&mut eval, vec![Value::Int(3)]).unwrap();
    assert_eq!(
        state,
        Value::list(vec![
            Value::Int(1),
            Value::Int(1),
            Value::Int(2),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Int(0),
            Value::Nil,
            Value::Nil,
            Value::list(vec![Value::Int(1)]),
            Value::Nil,
        ])
    );
}
