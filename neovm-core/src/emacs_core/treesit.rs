use std::collections::{BTreeMap, HashMap};

use libloading::Library;
use tree_sitter::{InputEdit, Language, Parser, Point, Query, Tree};

use super::value::Value;
use crate::buffer::BufferId;

pub(crate) const TREESIT_PARSER_TAG: &str = "treesit-parser";
pub(crate) const TREESIT_NODE_TAG: &str = "treesit-node";
pub(crate) const TREESIT_COMPILED_QUERY_TAG: &str = "treesit-compiled-query";

pub(crate) const PARSER_SLOT_TYPE: usize = 0;
pub(crate) const PARSER_SLOT_ID: usize = 1;
pub(crate) const PARSER_SLOT_LANGUAGE: usize = 2;
pub(crate) const PARSER_SLOT_BUFFER: usize = 3;
pub(crate) const PARSER_SLOT_TAG: usize = 4;
pub(crate) const PARSER_SLOT_EMBED_LEVEL: usize = 5;
pub(crate) const PARSER_SLOT_NOTIFIERS: usize = 6;
pub(crate) const PARSER_SLOT_INCLUDED_RANGES: usize = 7;

pub(crate) const NODE_SLOT_TYPE: usize = 0;
pub(crate) const NODE_SLOT_ID: usize = 1;
pub(crate) const NODE_SLOT_PARSER: usize = 2;

pub(crate) const QUERY_SLOT_TYPE: usize = 0;
pub(crate) const QUERY_SLOT_ID: usize = 1;
pub(crate) const QUERY_SLOT_LANGUAGE: usize = 2;
pub(crate) const QUERY_SLOT_SOURCE: usize = 3;

pub(crate) struct LoadedLanguage {
    pub(crate) language: Language,
    pub(crate) filename: Option<String>,
    pub(crate) _library: Option<Library>,
}

#[derive(Clone, Copy)]
pub(crate) struct LineColCache {
    pub(crate) line: i64,
    pub(crate) col: i64,
    pub(crate) bytepos: usize,
}

pub(crate) struct ParserEntry {
    pub(crate) value: Value,
    pub(crate) orig_buffer_id: BufferId,
    pub(crate) root_buffer_id: BufferId,
    pub(crate) language_name: String,
    pub(crate) tag: Value,
    pub(crate) parser: Parser,
    pub(crate) tree: Option<Tree>,
    pub(crate) last_source: Option<String>,
    pub(crate) generation: u64,
    pub(crate) need_to_gc_buffer: bool,
    pub(crate) deleted: bool,
    pub(crate) tracking_linecol: bool,
    pub(crate) last_changed_ranges: Vec<(usize, usize)>,
}

pub(crate) struct NodeEntry {
    pub(crate) parser_id: u64,
    pub(crate) raw: tree_sitter::ffi::TSNode,
    pub(crate) generation: u64,
}

pub(crate) struct QueryEntry {
    pub(crate) language_name: String,
    pub(crate) compiled: Option<Query>,
}

#[derive(Clone, Copy)]
struct PendingBufferEdit {
    start_byte: usize,
    old_end_byte: usize,
    start_position: Point,
    old_end_position: Point,
}

#[derive(Default)]
pub(crate) struct TreeSitterManager {
    next_parser_id: u64,
    next_node_id: u64,
    next_query_id: u64,
    loaded_languages: BTreeMap<String, LoadedLanguage>,
    parsers: BTreeMap<u64, ParserEntry>,
    nodes: BTreeMap<u64, NodeEntry>,
    queries: BTreeMap<u64, QueryEntry>,
    linecol_caches: HashMap<BufferId, LineColCache>,
    pending_edits: HashMap<BufferId, PendingBufferEdit>,
}

impl TreeSitterManager {
    pub(crate) fn new() -> Self {
        Self {
            next_parser_id: 1,
            next_node_id: 1,
            next_query_id: 1,
            loaded_languages: BTreeMap::new(),
            parsers: BTreeMap::new(),
            nodes: BTreeMap::new(),
            queries: BTreeMap::new(),
            linecol_caches: HashMap::new(),
            pending_edits: HashMap::new(),
        }
    }

    pub(crate) fn roots(&self) -> Vec<Value> {
        self.parsers.values().map(|entry| entry.value).collect()
    }

    pub(crate) fn loaded_language(&self, key: &str) -> Option<(Language, Option<String>)> {
        self.loaded_languages
            .get(key)
            .map(|loaded| (loaded.language.clone(), loaded.filename.clone()))
    }

    pub(crate) fn cache_loaded_language(&mut self, key: String, loaded: LoadedLanguage) {
        self.loaded_languages.entry(key).or_insert(loaded);
    }

    pub(crate) fn find_reusable_parser(
        &self,
        orig_buffer_id: BufferId,
        language_name: &str,
        tag: Value,
    ) -> Option<Value> {
        self.parsers
            .values()
            .find(|entry| {
                !entry.deleted
                    && entry.orig_buffer_id == orig_buffer_id
                    && entry.language_name == language_name
                    && entry.tag == tag
            })
            .map(|entry| entry.value)
    }

    pub(crate) fn insert_parser(
        &mut self,
        value: Value,
        orig_buffer_id: BufferId,
        root_buffer_id: BufferId,
        language_name: String,
        tag: Value,
        parser: Parser,
        tracking_linecol: bool,
    ) -> u64 {
        let id = self.next_parser_id;
        self.next_parser_id += 1;
        self.parsers.insert(
            id,
            ParserEntry {
                value,
                orig_buffer_id,
                root_buffer_id,
                language_name,
                tag,
                parser,
                tree: None,
                last_source: None,
                generation: 0,
                need_to_gc_buffer: false,
                deleted: false,
                tracking_linecol,
                last_changed_ranges: Vec::new(),
            },
        );
        id
    }

    pub(crate) fn parser(&self, id: u64) -> Option<&ParserEntry> {
        self.parsers.get(&id)
    }

    pub(crate) fn parser_mut(&mut self, id: u64) -> Option<&mut ParserEntry> {
        self.parsers.get_mut(&id)
    }

    pub(crate) fn parser_values_for(
        &self,
        root_buffer_id: BufferId,
        orig_buffer_id: BufferId,
        language_name: Option<&str>,
        tag_filter: ParserTagFilter,
    ) -> Vec<Value> {
        let mut items = self
            .parsers
            .iter()
            .rev()
            .filter_map(|(_, entry)| {
                if entry.root_buffer_id != root_buffer_id || entry.orig_buffer_id != orig_buffer_id
                {
                    return None;
                }
                if entry.deleted {
                    return None;
                }
                if let Some(language_name) = language_name {
                    if entry.language_name != language_name {
                        return None;
                    }
                }
                if !tag_filter.matches(entry.tag) {
                    return None;
                }
                Some(entry.value)
            })
            .collect::<Vec<_>>();
        items.shrink_to_fit();
        items
    }

    pub(crate) fn mark_parser_deleted(&mut self, id: u64) -> bool {
        let Some(entry) = self.parsers.get_mut(&id) else {
            return false;
        };
        entry.deleted = true;
        true
    }

    pub(crate) fn insert_node(
        &mut self,
        parser_id: u64,
        raw: tree_sitter::ffi::TSNode,
        generation: u64,
    ) -> u64 {
        let id = self.next_node_id;
        self.next_node_id += 1;
        self.nodes.insert(
            id,
            NodeEntry {
                parser_id,
                raw,
                generation,
            },
        );
        id
    }

    pub(crate) fn node(&self, id: u64) -> Option<&NodeEntry> {
        self.nodes.get(&id)
    }

    pub(crate) fn clear_nodes_for_parser(&mut self, parser_id: u64) {
        self.nodes.retain(|_, entry| entry.parser_id != parser_id);
    }

    pub(crate) fn insert_query(&mut self, language_name: String) -> u64 {
        let id = self.next_query_id;
        self.next_query_id += 1;
        self.queries.insert(
            id,
            QueryEntry {
                language_name,
                compiled: None,
            },
        );
        id
    }

    pub(crate) fn query(&self, id: u64) -> Option<&QueryEntry> {
        self.queries.get(&id)
    }

    pub(crate) fn query_mut(&mut self, id: u64) -> Option<&mut QueryEntry> {
        self.queries.get_mut(&id)
    }

    pub(crate) fn linecol_cache(&self, buffer_id: BufferId) -> Option<LineColCache> {
        self.linecol_caches.get(&buffer_id).copied()
    }

    pub(crate) fn set_linecol_cache(&mut self, buffer_id: BufferId, cache: LineColCache) {
        self.linecol_caches.insert(buffer_id, cache);
        for parser in self.parsers.values_mut() {
            if parser.orig_buffer_id == buffer_id && !parser.deleted {
                parser.tracking_linecol = true;
            }
        }
    }

    pub(crate) fn enable_linecol_tracking(&mut self, buffer_id: BufferId) {
        self.linecol_caches
            .entry(buffer_id)
            .or_insert(LineColCache {
                line: 1,
                col: 1,
                bytepos: 0,
            });
        for parser in self.parsers.values_mut() {
            if parser.orig_buffer_id == buffer_id && !parser.deleted {
                parser.tracking_linecol = true;
            }
        }
    }

    pub(crate) fn note_buffer_change(&mut self, buffer_id: BufferId, start_byte: usize) {
        if let Some(cache) = self.linecol_caches.get_mut(&buffer_id)
            && cache.bytepos > start_byte
        {
            *cache = LineColCache {
                line: 1,
                col: 1,
                bytepos: 0,
            };
        }
    }

    pub(crate) fn begin_buffer_edit(
        &mut self,
        buffer_id: BufferId,
        source: &str,
        start_byte: usize,
        old_end_byte: usize,
    ) {
        self.pending_edits.insert(
            buffer_id,
            PendingBufferEdit {
                start_byte,
                old_end_byte,
                start_position: point_for_byte(source, start_byte),
                old_end_position: point_for_byte(source, old_end_byte),
            },
        );
    }

    pub(crate) fn finish_buffer_edit(
        &mut self,
        buffer_id: BufferId,
        source: &str,
        new_end_byte: usize,
    ) {
        let Some(edit) = self.pending_edits.remove(&buffer_id) else {
            return;
        };
        let input_edit = InputEdit {
            start_byte: edit.start_byte,
            old_end_byte: edit.old_end_byte,
            new_end_byte,
            start_position: edit.start_position,
            old_end_position: edit.old_end_position,
            new_end_position: point_for_byte(source, new_end_byte),
        };

        let mut edited_parser_ids = Vec::new();
        for (parser_id, parser) in &mut self.parsers {
            if parser.deleted || parser.orig_buffer_id != buffer_id {
                continue;
            }
            if let Some(tree) = parser.tree.as_mut() {
                tree.edit(&input_edit);
                parser.generation = parser.generation.saturating_add(1);
                parser.last_changed_ranges.clear();
                edited_parser_ids.push(*parser_id);
            }
        }

        for parser_id in edited_parser_ids {
            self.clear_nodes_for_parser(parser_id);
        }
    }
}

fn point_for_byte(source: &str, byte_offset: usize) -> Point {
    let target = byte_offset.min(source.len());
    let mut row = 0usize;
    let mut last_newline = 0usize;
    for (idx, byte) in source.as_bytes().iter().enumerate().take(target) {
        if *byte == b'\n' {
            row += 1;
            last_newline = idx + 1;
        }
    }
    Point {
        row,
        column: target.saturating_sub(last_newline),
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ParserTagFilter {
    Any,
    Exact(Value),
}

impl ParserTagFilter {
    fn matches(self, candidate: Value) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(expected) => candidate == expected,
        }
    }
}

pub(crate) fn make_parser_value(
    id: u64,
    language_symbol: Value,
    buffer: Value,
    tag: Value,
) -> Value {
    Value::make_record(vec![
        Value::symbol(TREESIT_PARSER_TAG),
        Value::fixnum(id as i64),
        language_symbol,
        buffer,
        tag,
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ])
}

pub(crate) fn make_node_value(id: u64, parser: Value) -> Value {
    Value::make_record(vec![
        Value::symbol(TREESIT_NODE_TAG),
        Value::fixnum(id as i64),
        parser,
    ])
}

pub(crate) fn make_query_value(id: u64, language_symbol: Value, source: Value) -> Value {
    Value::make_record(vec![
        Value::symbol(TREESIT_COMPILED_QUERY_TAG),
        Value::fixnum(id as i64),
        language_symbol,
        source,
    ])
}

pub(crate) fn parser_id(value: Value) -> Option<u64> {
    record_id_with_tag(value, TREESIT_PARSER_TAG, PARSER_SLOT_ID)
}

pub(crate) fn node_id(value: Value) -> Option<u64> {
    record_id_with_tag(value, TREESIT_NODE_TAG, NODE_SLOT_ID)
}

pub(crate) fn query_id(value: Value) -> Option<u64> {
    record_id_with_tag(value, TREESIT_COMPILED_QUERY_TAG, QUERY_SLOT_ID)
}

pub(crate) fn is_parser(value: Value) -> bool {
    record_tag_is(value, TREESIT_PARSER_TAG)
}

pub(crate) fn is_node(value: Value) -> bool {
    record_tag_is(value, TREESIT_NODE_TAG)
}

pub(crate) fn is_compiled_query(value: Value) -> bool {
    record_tag_is(value, TREESIT_COMPILED_QUERY_TAG)
}

fn record_id_with_tag(value: Value, expected_tag: &str, id_slot: usize) -> Option<u64> {
    let items = value.as_record_data()?;
    let tag = items.first()?.as_symbol_name()?;
    if tag != expected_tag {
        return None;
    }
    let id = items.get(id_slot)?.as_fixnum()?;
    (id >= 0).then_some(id as u64)
}

fn record_tag_is(value: Value, expected_tag: &str) -> bool {
    value
        .as_record_data()
        .and_then(|items| items.first().copied())
        .and_then(|tag| tag.as_symbol_name())
        == Some(expected_tag)
}
