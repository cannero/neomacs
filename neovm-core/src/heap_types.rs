//! Shared heap payload types used by both the tagged runtime and legacy dump code.
//!
//! These types are not specific to the old `gc::heap` implementation. Keeping
//! them behind a neutral module boundary lets the tagged runtime depend on them
//! without importing the legacy GC namespace.

use crate::buffer::BufferId;
use std::sync::{Arc, OnceLock};

const MAX_CONCAT_PARTS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LispStringPart {
    backing: Arc<String>,
    start: usize,
    end: usize,
}

impl LispStringPart {
    fn new(backing: Arc<String>, start: usize, end: usize) -> Self {
        Self {
            backing,
            start,
            end,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.backing[self.start..self.end]
    }

    pub(crate) fn len(&self) -> usize {
        self.end - self.start
    }

    fn slice(&self, start: usize, end: usize) -> Option<Self> {
        self.as_str().get(start..end)?;
        Some(Self::new(
            self.backing.clone(),
            self.start + start,
            self.start + end,
        ))
    }
}

#[derive(Debug)]
enum LispStringStorage {
    Flat(Arc<String>),
    Slice {
        backing: Arc<String>,
        start: usize,
        end: usize,
    },
    Concat {
        parts: Vec<LispStringPart>,
        flattened: OnceLock<Arc<String>>,
    },
}

pub struct LispString {
    storage: LispStringStorage,
    pub multibyte: bool,
}

impl Clone for LispStringStorage {
    fn clone(&self) -> Self {
        match self {
            Self::Flat(text) => Self::Flat(text.clone()),
            Self::Slice {
                backing,
                start,
                end,
            } => Self::Slice {
                backing: backing.clone(),
                start: *start,
                end: *end,
            },
            Self::Concat { parts, flattened } => {
                let cloned = Self::Concat {
                    parts: parts.clone(),
                    flattened: OnceLock::new(),
                };
                if let Self::Concat {
                    flattened: cloned_flattened,
                    ..
                } = &cloned
                    && let Some(text) = flattened.get()
                {
                    let _ = cloned_flattened.set(text.clone());
                }
                cloned
            }
        }
    }
}

impl Clone for LispString {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            multibyte: self.multibyte,
        }
    }
}

impl std::fmt::Debug for LispString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LispString")
            .field("text", &self.as_str())
            .field("multibyte", &self.multibyte)
            .finish()
    }
}

impl PartialEq for LispString {
    fn eq(&self, other: &Self) -> bool {
        self.multibyte == other.multibyte && self.as_str() == other.as_str()
    }
}

impl Eq for LispString {}

impl LispString {
    pub fn new(text: String, multibyte: bool) -> Self {
        Self {
            storage: LispStringStorage::Flat(Arc::new(text)),
            multibyte,
        }
    }

    pub(crate) fn from_parts(parts: Vec<LispStringPart>, multibyte: bool) -> Self {
        match parts.len() {
            0 => Self::new(String::new(), multibyte),
            1 => {
                let part = &parts[0];
                if part.start == 0 && part.end == part.backing.len() {
                    Self {
                        storage: LispStringStorage::Flat(part.backing.clone()),
                        multibyte,
                    }
                } else {
                    Self {
                        storage: LispStringStorage::Slice {
                            backing: part.backing.clone(),
                            start: part.start,
                            end: part.end,
                        },
                        multibyte,
                    }
                }
            }
            n if n > MAX_CONCAT_PARTS => {
                let total_len: usize = parts.iter().map(LispStringPart::len).sum();
                let mut text = String::with_capacity(total_len);
                for part in &parts {
                    text.push_str(part.as_str());
                }
                Self::new(text, multibyte)
            }
            _ => Self {
                storage: LispStringStorage::Concat {
                    parts,
                    flattened: OnceLock::new(),
                },
                multibyte,
            },
        }
    }

    fn flattened_arc(&self) -> Arc<String> {
        match &self.storage {
            LispStringStorage::Flat(text) => text.clone(),
            LispStringStorage::Slice {
                backing,
                start,
                end,
            } => Arc::new(backing[*start..*end].to_owned()),
            LispStringStorage::Concat { parts, flattened } => flattened
                .get_or_init(|| {
                    let total_len: usize = parts.iter().map(LispStringPart::len).sum();
                    let mut text = String::with_capacity(total_len);
                    for part in parts {
                        text.push_str(part.as_str());
                    }
                    Arc::new(text)
                })
                .clone(),
        }
    }

    pub(crate) fn byte_len(&self) -> usize {
        match &self.storage {
            LispStringStorage::Flat(text) => text.len(),
            LispStringStorage::Slice { start, end, .. } => end - start,
            LispStringStorage::Concat { parts, .. } => parts.iter().map(LispStringPart::len).sum(),
        }
    }

    pub(crate) fn is_ascii(&self) -> bool {
        match &self.storage {
            LispStringStorage::Flat(text) => text.is_ascii(),
            LispStringStorage::Slice {
                backing,
                start,
                end,
            } => backing[*start..*end].is_ascii(),
            LispStringStorage::Concat { parts, .. } => {
                parts.iter().all(|part| part.as_str().is_ascii())
            }
        }
    }

    pub(crate) fn append_parts_to(&self, out: &mut Vec<LispStringPart>) {
        match &self.storage {
            LispStringStorage::Flat(backing) => {
                out.push(LispStringPart::new(backing.clone(), 0, backing.len()));
            }
            LispStringStorage::Slice {
                backing,
                start,
                end,
            } => out.push(LispStringPart::new(backing.clone(), *start, *end)),
            LispStringStorage::Concat { parts, .. } => out.extend(parts.iter().cloned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match &self.storage {
            LispStringStorage::Flat(text) => text.as_str(),
            LispStringStorage::Slice {
                backing,
                start,
                end,
            } => &backing[*start..*end],
            LispStringStorage::Concat { parts, flattened } => flattened
                .get_or_init(|| {
                    let total_len: usize = parts.iter().map(LispStringPart::len).sum();
                    let mut text = String::with_capacity(total_len);
                    for part in parts {
                        text.push_str(part.as_str());
                    }
                    Arc::new(text)
                })
                .as_str(),
        }
    }

    pub fn slice(&self, start: usize, end: usize) -> Option<Self> {
        if start > end || end > self.byte_len() {
            return None;
        }

        let result = match &self.storage {
            LispStringStorage::Flat(backing) => Self::from_parts(
                vec![LispStringPart::new(backing.clone(), start, end)],
                self.multibyte,
            ),
            LispStringStorage::Slice {
                backing,
                start: base_start,
                ..
            } => Self::from_parts(
                vec![LispStringPart::new(
                    backing.clone(),
                    base_start + start,
                    base_start + end,
                )],
                self.multibyte,
            ),
            LispStringStorage::Concat { parts, .. } => {
                let mut remaining_start = start;
                let mut remaining_end = end;
                let mut sliced_parts = Vec::new();

                for part in parts {
                    let part_len = part.len();
                    if remaining_start >= part_len {
                        remaining_start -= part_len;
                        remaining_end -= part_len;
                        continue;
                    }

                    let local_start = remaining_start;
                    let local_end = remaining_end.min(part_len);
                    if let Some(slice) = part.slice(local_start, local_end) {
                        sliced_parts.push(slice);
                    } else {
                        return None;
                    }

                    if remaining_end <= part_len {
                        break;
                    }

                    remaining_start = 0;
                    remaining_end -= part_len;
                }

                Self::from_parts(sliced_parts, self.multibyte)
            }
        };

        result.as_str().get(..)?;
        Some(result)
    }

    pub fn make_mut(&mut self) -> &mut String {
        if !matches!(self.storage, LispStringStorage::Flat(_)) {
            self.storage = LispStringStorage::Flat(self.flattened_arc());
        }
        match &mut self.storage {
            LispStringStorage::Flat(text) => Arc::make_mut(text),
            LispStringStorage::Slice { .. } | LispStringStorage::Concat { .. } => {
                unreachable!("non-flat strings are detached above")
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct OverlayData {
    pub plist: crate::emacs_core::value::Value,
    pub buffer: Option<BufferId>,
    pub start: usize,
    pub end: usize,
    pub front_advance: bool,
    pub rear_advance: bool,
}

#[derive(Clone, Debug)]
pub struct MarkerData {
    pub buffer: Option<BufferId>,
    pub position: Option<i64>,
    pub insertion_type: bool,
    pub marker_id: Option<u64>,
}
