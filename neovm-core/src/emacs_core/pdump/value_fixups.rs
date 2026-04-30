//! Raw mapped-value fixups for HeapImage words.
//!
//! GNU pdumper writes heap-shaped objects and records relocation work for Lisp
//! value fields that cannot be represented as a plain intra-dump pointer.  This
//! section is the Neomacs equivalent for mapped HeapImage words: each entry
//! names a HeapImage word offset and the logical DumpValue that should be
//! written there after the dump-local symbol table has been restored.

use bytemuck::{Pod, Zeroable};

use super::DumpError;
use super::object_value_codec;
use super::types::DumpValue;

const VALUE_FIXUPS_MAGIC: [u8; 16] = *b"NEOVALUEFIXUPS\0\0";
const VALUE_FIXUPS_FORMAT_VERSION: u32 = 2;
const FIXUP_KIND_BITS: u64 = 2;
const FIXUP_KIND_MASK: u64 = (1 << FIXUP_KIND_BITS) - 1;
const FIXUP_OFFSET_ALIGN_BITS: u64 = 3;
const FIXUP_SYMBOL: u64 = 0;
const FIXUP_VALUE: u64 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct ValueFixupsHeader {
    magic: [u8; 16],
    version: u32,
    header_size: u32,
    fixup_count: u64,
    payload_offset: u64,
    payload_len: u64,
}

const HEADER_SIZE: usize = std::mem::size_of::<ValueFixupsHeader>();

#[derive(Clone, Debug)]
pub(crate) enum RawValueFixup {
    Symbol {
        location_offset: u64,
    },
    Value {
        location_offset: u64,
        value: DumpValue,
    },
}

impl RawValueFixup {
    pub(crate) fn location_offset(&self) -> u64 {
        match self {
            Self::Symbol { location_offset }
            | Self::Value {
                location_offset, ..
            } => *location_offset,
        }
    }
}

pub(crate) fn value_fixups_section_bytes(fixups: &[RawValueFixup]) -> Result<Vec<u8>, DumpError> {
    let mut bytes = vec![0u8; HEADER_SIZE];
    for fixup in fixups {
        match fixup {
            RawValueFixup::Symbol { location_offset } => {
                object_value_codec::write_u64(
                    &mut bytes,
                    pack_fixup_location(*location_offset, FIXUP_SYMBOL)?,
                );
            }
            RawValueFixup::Value {
                location_offset,
                value,
            } => {
                object_value_codec::write_u64(
                    &mut bytes,
                    pack_fixup_location(*location_offset, FIXUP_VALUE)?,
                );
                object_value_codec::write_value(&mut bytes, value)?;
            }
        }
    }

    let payload_len = bytes.len() - HEADER_SIZE;
    let header = ValueFixupsHeader {
        magic: VALUE_FIXUPS_MAGIC,
        version: VALUE_FIXUPS_FORMAT_VERSION,
        header_size: HEADER_SIZE as u32,
        fixup_count: fixups.len() as u64,
        payload_offset: HEADER_SIZE as u64,
        payload_len: payload_len as u64,
    };
    bytes[..HEADER_SIZE].copy_from_slice(bytemuck::bytes_of(&header));
    Ok(bytes)
}

pub(crate) fn load_value_fixups_section(section: &[u8]) -> Result<Vec<RawValueFixup>, DumpError> {
    let (fixup_count, payload) = value_fixups_payload(section)?;
    let mut cursor = object_value_codec::Cursor::new(payload);
    let mut fixups = Vec::with_capacity(fixup_count);
    for _ in 0..fixup_count {
        let fixup = read_value_fixup(&mut cursor)?;
        fixups.push(fixup);
    }
    ensure_fixup_cursor_empty(&cursor)?;
    Ok(fixups)
}

pub(crate) fn for_each_value_fixup(
    section: &[u8],
    mut f: impl FnMut(RawValueFixup) -> Result<(), DumpError>,
) -> Result<(), DumpError> {
    let (fixup_count, payload) = value_fixups_payload(section)?;
    let mut cursor = object_value_codec::Cursor::new(payload);
    for _ in 0..fixup_count {
        f(read_value_fixup(&mut cursor)?)?;
    }
    ensure_fixup_cursor_empty(&cursor)
}

fn value_fixups_payload(section: &[u8]) -> Result<(usize, &[u8]), DumpError> {
    if section.len() < HEADER_SIZE {
        return Err(DumpError::ImageFormatError(
            "value-fixups section too small for header".into(),
        ));
    }

    let header = *bytemuck::from_bytes::<ValueFixupsHeader>(&section[..HEADER_SIZE]);
    if header.magic != VALUE_FIXUPS_MAGIC {
        return Err(DumpError::ImageFormatError(
            "value-fixups section has bad magic".into(),
        ));
    }
    if header.version != VALUE_FIXUPS_FORMAT_VERSION {
        return Err(DumpError::UnsupportedVersion(header.version));
    }
    if header.header_size != HEADER_SIZE as u32 {
        return Err(DumpError::ImageFormatError(format!(
            "value-fixups header size {} does not match runtime header size {HEADER_SIZE}",
            header.header_size
        )));
    }

    let payload_start = usize::try_from(header.payload_offset).map_err(|_| {
        DumpError::ImageFormatError("value-fixups payload offset overflows usize".into())
    })?;
    let payload_len = usize::try_from(header.payload_len).map_err(|_| {
        DumpError::ImageFormatError("value-fixups payload length overflows usize".into())
    })?;
    let payload_end = payload_start.checked_add(payload_len).ok_or_else(|| {
        DumpError::ImageFormatError("value-fixups payload range overflows".into())
    })?;
    if payload_start < HEADER_SIZE || payload_end > section.len() {
        return Err(DumpError::ImageFormatError(
            "value-fixups payload range is outside section".into(),
        ));
    }

    let fixup_count = usize::try_from(header.fixup_count)
        .map_err(|_| DumpError::ImageFormatError("value-fixups count overflows usize".into()))?;
    Ok((fixup_count, &section[payload_start..payload_end]))
}

fn read_value_fixup(
    cursor: &mut object_value_codec::Cursor<'_>,
) -> Result<RawValueFixup, DumpError> {
    let packed = cursor.read_u64("value-fixup location")?;
    let location_offset = unpack_fixup_location(packed)?;
    match packed & FIXUP_KIND_MASK {
        FIXUP_SYMBOL => Ok(RawValueFixup::Symbol { location_offset }),
        FIXUP_VALUE => Ok(RawValueFixup::Value {
            location_offset,
            value: cursor.read_value()?,
        }),
        other => Err(DumpError::ImageFormatError(format!(
            "unknown value-fixup kind {other}"
        ))),
    }
}

fn pack_fixup_location(location_offset: u64, kind: u64) -> Result<u64, DumpError> {
    if kind > FIXUP_KIND_MASK {
        return Err(DumpError::SerializationError(format!(
            "value-fixup kind {kind} exceeds kind mask"
        )));
    }
    let alignment_mask = (1 << FIXUP_OFFSET_ALIGN_BITS) - 1;
    if location_offset & alignment_mask != 0 {
        return Err(DumpError::SerializationError(format!(
            "value-fixup location offset {location_offset} is not word-aligned"
        )));
    }
    let raw_offset = location_offset >> FIXUP_OFFSET_ALIGN_BITS;
    if raw_offset > (u64::MAX >> FIXUP_KIND_BITS) {
        return Err(DumpError::SerializationError(
            "value-fixup location offset is out of range".into(),
        ));
    }
    Ok((raw_offset << FIXUP_KIND_BITS) | kind)
}

fn unpack_fixup_location(packed: u64) -> Result<u64, DumpError> {
    let raw_offset = packed >> FIXUP_KIND_BITS;
    raw_offset
        .checked_shl(FIXUP_OFFSET_ALIGN_BITS as u32)
        .ok_or_else(|| DumpError::ImageFormatError("value-fixup location overflow".into()))
}

fn ensure_fixup_cursor_empty(cursor: &object_value_codec::Cursor<'_>) -> Result<(), DumpError> {
    if !cursor.is_empty() {
        return Err(DumpError::ImageFormatError(format!(
            "value-fixups section has {} trailing payload bytes",
            cursor.remaining()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs_core::pdump::types::{DumpHeapRef, DumpNameId, DumpSymId};

    #[test]
    fn value_fixups_round_trip_representative_values() {
        let fixups = vec![
            RawValueFixup::Value {
                location_offset: 8,
                value: DumpValue::Symbol(DumpSymId(3)),
            },
            RawValueFixup::Value {
                location_offset: 16,
                value: DumpValue::Subr(DumpNameId(4)),
            },
            RawValueFixup::Value {
                location_offset: 24,
                value: DumpValue::HashTable(DumpHeapRef { index: 5 }),
            },
        ];

        let bytes = value_fixups_section_bytes(&fixups).expect("encode value fixups");
        let decoded = load_value_fixups_section(&bytes).expect("decode value fixups");

        assert_eq!(decoded.len(), fixups.len());
        assert_eq!(decoded[0].location_offset(), 8);
        assert!(matches!(
            decoded[0],
            RawValueFixup::Value {
                value: DumpValue::Symbol(DumpSymId(3)),
                ..
            }
        ));
        assert_eq!(decoded[1].location_offset(), 16);
        assert!(matches!(
            decoded[1],
            RawValueFixup::Value {
                value: DumpValue::Subr(DumpNameId(4)),
                ..
            }
        ));
        assert_eq!(decoded[2].location_offset(), 24);
        assert!(matches!(
            decoded[2],
            RawValueFixup::Value {
                value: DumpValue::HashTable(DumpHeapRef { index: 5 }),
                ..
            }
        ));
    }

    #[test]
    fn symbol_value_fixups_encode_as_single_aligned_word() {
        let fixups = vec![RawValueFixup::Symbol {
            location_offset: 16,
        }];

        let bytes = value_fixups_section_bytes(&fixups).expect("encode symbol fixup");
        let decoded = load_value_fixups_section(&bytes).expect("decode symbol fixup");

        assert_eq!(bytes.len(), HEADER_SIZE + 8);
        assert!(matches!(
            decoded.as_slice(),
            [RawValueFixup::Symbol {
                location_offset: 16
            }]
        ));
    }
}
