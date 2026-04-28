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
const VALUE_FIXUPS_FORMAT_VERSION: u32 = 1;

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
pub(crate) struct RawValueFixup {
    pub location_offset: u64,
    pub value: DumpValue,
}

pub(crate) fn value_fixups_section_bytes(fixups: &[RawValueFixup]) -> Result<Vec<u8>, DumpError> {
    let mut bytes = vec![0u8; HEADER_SIZE];
    for fixup in fixups {
        object_value_codec::write_u64(&mut bytes, fixup.location_offset);
        object_value_codec::write_value(&mut bytes, &fixup.value)?;
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
    let mut cursor = object_value_codec::Cursor::new(&section[payload_start..payload_end]);
    let mut fixups = Vec::with_capacity(fixup_count);
    for _ in 0..fixup_count {
        let location_offset = cursor.read_u64("value-fixup location offset")?;
        let value = cursor.read_value()?;
        fixups.push(RawValueFixup {
            location_offset,
            value,
        });
    }
    if !cursor.is_empty() {
        return Err(DumpError::ImageFormatError(format!(
            "value-fixups section has {} trailing payload bytes",
            cursor.remaining()
        )));
    }
    Ok(fixups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs_core::pdump::types::{DumpHeapRef, DumpNameId, DumpSymId};

    #[test]
    fn value_fixups_round_trip_representative_values() {
        let fixups = vec![
            RawValueFixup {
                location_offset: 8,
                value: DumpValue::Symbol(DumpSymId(3)),
            },
            RawValueFixup {
                location_offset: 16,
                value: DumpValue::Subr(DumpNameId(4)),
            },
            RawValueFixup {
                location_offset: 24,
                value: DumpValue::HashTable(DumpHeapRef { index: 5 }),
            },
        ];

        let bytes = value_fixups_section_bytes(&fixups).expect("encode value fixups");
        let decoded = load_value_fixups_section(&bytes).expect("decode value fixups");

        assert_eq!(decoded.len(), fixups.len());
        assert_eq!(decoded[0].location_offset, 8);
        assert!(matches!(decoded[0].value, DumpValue::Symbol(DumpSymId(3))));
        assert_eq!(decoded[1].location_offset, 16);
        assert!(matches!(decoded[1].value, DumpValue::Subr(DumpNameId(4))));
        assert_eq!(decoded[2].location_offset, 24);
        assert!(matches!(
            decoded[2].value,
            DumpValue::HashTable(DumpHeapRef { index: 5 })
        ));
    }
}
