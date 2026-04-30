//! Memory-mapped pdump image primitives.
//!
//! GNU Emacs's pdumper does not read a serialized payload and rebuild the Lisp
//! heap.  It maps a dump image, validates the build fingerprint, then applies
//! relocations to sections that already contain heap-shaped objects.  This
//! module is the Neomacs image-format boundary for that design: a fixed header,
//! section table, fingerprint, checksum, and an mmap-backed owner that exposes
//! section bytes directly from the mapped file.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use bytemuck::{Pod, Zeroable};
use memmap2::{MmapMut, MmapOptions};
use sha2::{Digest, Sha256};

use super::{DumpError, fingerprint_bytes, hex_string};

const MMAP_MAGIC: [u8; 16] = *b"NEOMMAPDUMP\0\0\0\0\0";
const MMAP_FORMAT_VERSION: u32 = 5;
const SECTION_ALIGN: u64 = 8;

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DumpSectionKind {
    Metadata = 1,
    HeapImage = 2,
    Roots = 3,
    Relocations = 4,
    ObjectStarts = 5,
    EmacsRelocations = 6,
    RuntimeState = 7,
    SymbolTable = 8,
    Obarray = 10,
    Autoloads = 11,
    CharsetRegistry = 12,
    CodingSystems = 13,
    FaceTable = 14,
    Buffers = 15,
    RuntimeManagers = 16,
    ObjectExtra = 17,
    ValueRelocations = 18,
}

impl DumpSectionKind {
    fn from_raw(raw: u32) -> Result<Self, DumpError> {
        match raw {
            1 => Ok(Self::Metadata),
            2 => Ok(Self::HeapImage),
            3 => Ok(Self::Roots),
            4 => Ok(Self::Relocations),
            5 => Ok(Self::ObjectStarts),
            6 => Ok(Self::EmacsRelocations),
            7 => Ok(Self::RuntimeState),
            8 => Ok(Self::SymbolTable),
            10 => Ok(Self::Obarray),
            11 => Ok(Self::Autoloads),
            12 => Ok(Self::CharsetRegistry),
            13 => Ok(Self::CodingSystems),
            14 => Ok(Self::FaceTable),
            15 => Ok(Self::Buffers),
            16 => Ok(Self::RuntimeManagers),
            17 => Ok(Self::ObjectExtra),
            18 => Ok(Self::ValueRelocations),
            other => Err(DumpError::ImageFormatError(format!(
                "unknown section kind {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ImageSection<'a> {
    pub kind: DumpSectionKind,
    pub flags: u32,
    pub bytes: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ImageRelocation {
    pub location_section: DumpSectionKind,
    pub location_offset: u64,
    pub target_section: DumpSectionKind,
    pub target_offset: u64,
    pub addend: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct DumpImageHeader {
    magic: [u8; 16],
    version: u32,
    header_size: u32,
    section_count: u32,
    reserved0: u32,
    fingerprint: [u8; 32],
    checksum: [u8; 32],
    file_len: u64,
    section_table_offset: u64,
    section_table_len: u64,
    payload_offset: u64,
    flags: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct DumpImageSection {
    kind: u32,
    flags: u32,
    offset: u64,
    len: u64,
    reserved: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct DumpImageRelocation {
    location_section: u32,
    target_section: u32,
    location_offset: u64,
    target_offset: u64,
    addend: u64,
}

const HEADER_SIZE: usize = std::mem::size_of::<DumpImageHeader>();
const SECTION_SIZE: usize = std::mem::size_of::<DumpImageSection>();
const RELOCATION_SIZE: usize = std::mem::size_of::<DumpImageRelocation>();

pub(crate) struct LoadedMmapImage {
    mmap: MmapMut,
    sections: Vec<DumpImageSection>,
}

impl LoadedMmapImage {
    pub(crate) fn section(&self, kind: DumpSectionKind) -> Option<&[u8]> {
        let section = self
            .sections
            .iter()
            .find(|section| section.kind == kind as u32)?;
        Some(&self.mmap[section.offset as usize..section.offset as usize + section.len as usize])
    }

    pub(crate) fn section_mut(&mut self, kind: DumpSectionKind) -> Option<&mut [u8]> {
        let section = self
            .sections
            .iter()
            .find(|section| section.kind == kind as u32)?;
        let start = section.offset as usize;
        let end = start + section.len as usize;
        Some(&mut self.mmap[start..end])
    }

    pub(crate) fn section_mut_ptr(&self, kind: DumpSectionKind) -> Option<(*mut u8, usize)> {
        let section = self
            .sections
            .iter()
            .find(|section| section.kind == kind as u32)?;
        let start = section.offset as usize;
        let len = section.len as usize;
        Some((unsafe { self.mmap.as_ptr().cast_mut().add(start) }, len))
    }

    pub(crate) fn apply_relocations(&mut self) -> Result<(), DumpError> {
        let Ok(reloc_section) = self.section_bounds(DumpSectionKind::Relocations) else {
            return Ok(());
        };
        if !reloc_section.len().is_multiple_of(RELOCATION_SIZE) {
            return Err(DumpError::ImageFormatError(format!(
                "relocation section length {} is not a multiple of {RELOCATION_SIZE}",
                reloc_section.len()
            )));
        }
        let heap_image_bounds = self
            .section_bounds(DumpSectionKind::HeapImage)
            .ok()
            .map(|range| (range.start, range.end));
        let heap_image_kind = DumpSectionKind::HeapImage as u32;

        for relocation_offset in (reloc_section.start..reloc_section.end).step_by(RELOCATION_SIZE) {
            let relocation = *bytemuck::from_bytes::<DumpImageRelocation>(
                &self.mmap[relocation_offset..relocation_offset + RELOCATION_SIZE],
            );
            if relocation.location_section == heap_image_kind
                && relocation.target_section == heap_image_kind
            {
                let (heap_start, heap_end) = heap_image_bounds.ok_or_else(|| {
                    DumpError::ImageFormatError(
                        "heap-image relocation requires HeapImage section".into(),
                    )
                })?;
                self.apply_heap_image_relocation(relocation, heap_start, heap_end)?;
            } else {
                self.apply_relocation(relocation)?;
            }
        }
        Ok(())
    }

    fn apply_heap_image_relocation(
        &mut self,
        relocation: DumpImageRelocation,
        heap_start: usize,
        heap_end: usize,
    ) -> Result<(), DumpError> {
        let location_start =
            checked_end(heap_start, relocation.location_offset as usize, heap_end)?;
        let location_end = checked_end(location_start, std::mem::size_of::<usize>(), heap_end)?;
        let target_start = checked_end(heap_start, relocation.target_offset as usize, heap_end)?;
        let addend = usize::try_from(relocation.addend)
            .map_err(|_| DumpError::ImageFormatError("relocation addend overflows usize".into()))?;
        let target_ptr = (self.mmap.as_mut_ptr() as usize)
            .checked_add(target_start)
            .and_then(|ptr| ptr.checked_add(addend))
            .ok_or_else(|| {
                DumpError::ImageFormatError("relocation target pointer overflow".into())
            })?;

        debug_assert_eq!(location_end - location_start, std::mem::size_of::<usize>());
        unsafe {
            self.mmap
                .as_mut_ptr()
                .add(location_start)
                .cast::<usize>()
                .write_unaligned(target_ptr);
        }
        Ok(())
    }

    fn apply_relocation(&mut self, relocation: DumpImageRelocation) -> Result<(), DumpError> {
        let location_kind = DumpSectionKind::from_raw(relocation.location_section)?;
        let target_kind = DumpSectionKind::from_raw(relocation.target_section)?;
        let location = self.section_bounds(location_kind)?;
        let target = self.section_bounds(target_kind)?;

        let location_start = checked_end(
            location.start,
            relocation.location_offset as usize,
            location.end,
        )?;
        let location_end = checked_end(location_start, std::mem::size_of::<usize>(), location.end)?;
        let target_start =
            checked_end(target.start, relocation.target_offset as usize, target.end)?;
        let addend = usize::try_from(relocation.addend)
            .map_err(|_| DumpError::ImageFormatError("relocation addend overflows usize".into()))?;
        let target_ptr = (self.mmap.as_mut_ptr() as usize)
            .checked_add(target_start)
            .and_then(|ptr| ptr.checked_add(addend))
            .ok_or_else(|| {
                DumpError::ImageFormatError("relocation target pointer overflow".into())
            })?;

        debug_assert_eq!(location_end - location_start, std::mem::size_of::<usize>());
        unsafe {
            self.mmap
                .as_mut_ptr()
                .add(location_start)
                .cast::<usize>()
                .write_unaligned(target_ptr);
        }
        Ok(())
    }

    fn section_bounds(&self, kind: DumpSectionKind) -> Result<std::ops::Range<usize>, DumpError> {
        let section = self
            .sections
            .iter()
            .find(|section| section.kind == kind as u32)
            .ok_or_else(|| DumpError::ImageFormatError(format!("missing {kind:?} section")))?;
        let start = section.offset as usize;
        let end = start + section.len as usize;
        Ok(start..end)
    }

    pub(crate) fn contains_ptr(&self, ptr: *const u8) -> bool {
        let ptr = ptr as usize;
        self.mapped_range().contains(&ptr)
    }

    fn mapped_range(&self) -> std::ops::Range<usize> {
        let start = self.mmap.as_ptr() as usize;
        start..start + self.mmap.len()
    }
}

pub(crate) fn write_image(path: &Path, sections: &[ImageSection<'_>]) -> Result<(), DumpError> {
    if sections.is_empty() {
        return Err(DumpError::ImageFormatError(
            "mmap pdump image must contain at least one section".to_string(),
        ));
    }

    let section_table_offset = HEADER_SIZE as u64;
    let section_table_len = (sections.len() * SECTION_SIZE) as u64;
    let payload_offset = align_up(section_table_offset + section_table_len, SECTION_ALIGN);

    let mut section_headers = Vec::with_capacity(sections.len());
    let mut cursor = payload_offset;
    for section in sections {
        cursor = align_up(cursor, SECTION_ALIGN);
        section_headers.push(DumpImageSection {
            kind: section.kind as u32,
            flags: section.flags,
            offset: cursor,
            len: section.bytes.len() as u64,
            reserved: 0,
        });
        cursor = cursor
            .checked_add(section.bytes.len() as u64)
            .ok_or_else(|| DumpError::ImageFormatError("pdump image length overflow".into()))?;
    }

    let file_len = cursor as usize;
    let mut bytes = vec![0u8; file_len];

    for (idx, section_header) in section_headers.iter().enumerate() {
        let start = section_table_offset as usize + idx * SECTION_SIZE;
        bytes[start..start + SECTION_SIZE].copy_from_slice(bytemuck::bytes_of(section_header));
    }
    for (section, section_header) in sections.iter().zip(section_headers.iter()) {
        let start = section_header.offset as usize;
        let end = start + section_header.len as usize;
        bytes[start..end].copy_from_slice(section.bytes);
    }

    let checksum = checksum_body(&bytes);
    let header = DumpImageHeader {
        magic: MMAP_MAGIC,
        version: MMAP_FORMAT_VERSION,
        header_size: HEADER_SIZE as u32,
        section_count: sections.len() as u32,
        reserved0: 0,
        fingerprint: fingerprint_bytes(),
        checksum,
        file_len: file_len as u64,
        section_table_offset,
        section_table_len,
        payload_offset,
        flags: 0,
    };
    bytes[..HEADER_SIZE].copy_from_slice(bytemuck::bytes_of(&header));

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&bytes)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|err| DumpError::Io(err.error))?;
    Ok(())
}

pub(crate) fn relocation_section_bytes(relocations: &[ImageRelocation]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(relocations.len() * RELOCATION_SIZE);
    for relocation in relocations {
        let raw = DumpImageRelocation {
            location_section: relocation.location_section as u32,
            target_section: relocation.target_section as u32,
            location_offset: relocation.location_offset,
            target_offset: relocation.target_offset,
            addend: relocation.addend,
        };
        bytes.extend_from_slice(bytemuck::bytes_of(&raw));
    }
    bytes
}

pub(crate) fn load_image(path: &Path) -> Result<LoadedMmapImage, DumpError> {
    let file = File::open(path)?;
    let mmap = unsafe { MmapOptions::new().map_copy(&file)? };
    validate_image(mmap)
}

fn validate_image(mmap: MmapMut) -> Result<LoadedMmapImage, DumpError> {
    if mmap.len() < HEADER_SIZE {
        return Err(DumpError::BadMagic);
    }

    let header = *bytemuck::from_bytes::<DumpImageHeader>(&mmap[..HEADER_SIZE]);
    if header.magic != MMAP_MAGIC {
        return Err(DumpError::BadMagic);
    }
    if header.version != MMAP_FORMAT_VERSION {
        return Err(DumpError::UnsupportedVersion(header.version));
    }
    if header.header_size != HEADER_SIZE as u32 {
        return Err(DumpError::ImageFormatError(format!(
            "header size {} does not match runtime header size {HEADER_SIZE}",
            header.header_size
        )));
    }
    if header.file_len as usize != mmap.len() {
        return Err(DumpError::ImageFormatError(format!(
            "header file length {} does not match mapped length {}",
            header.file_len,
            mmap.len()
        )));
    }

    let expected_fingerprint = fingerprint_bytes();
    if header.fingerprint != expected_fingerprint {
        return Err(DumpError::FingerprintMismatch {
            expected: hex_string(&expected_fingerprint),
            found: hex_string(&header.fingerprint),
        });
    }

    // GNU pdumper validates the fixed header and build fingerprint on the
    // startup path; it does not hash the full mapped image before relocation.
    // Keep the checksum in the writer format for offline/debug validation, but
    // do not make normal pdump startup walk the entire file.

    let section_table_start = header.section_table_offset as usize;
    let section_table_len = header.section_table_len as usize;
    let section_table_end = checked_end(section_table_start, section_table_len, mmap.len())?;
    let expected_table_len = header.section_count as usize * SECTION_SIZE;
    if section_table_len != expected_table_len {
        return Err(DumpError::ImageFormatError(format!(
            "section table length {section_table_len} does not match section count {}",
            header.section_count
        )));
    }
    if section_table_start < HEADER_SIZE {
        return Err(DumpError::ImageFormatError(
            "section table overlaps header".to_string(),
        ));
    }
    if header.payload_offset < section_table_end as u64 {
        return Err(DumpError::ImageFormatError(
            "payload starts before section table ends".to_string(),
        ));
    }

    let mut sections = Vec::with_capacity(header.section_count as usize);
    for idx in 0..header.section_count as usize {
        let start = section_table_start + idx * SECTION_SIZE;
        let raw = *bytemuck::from_bytes::<DumpImageSection>(&mmap[start..start + SECTION_SIZE]);
        DumpSectionKind::from_raw(raw.kind)?;
        if raw.reserved != 0 {
            return Err(DumpError::ImageFormatError(format!(
                "section {idx} reserved field is nonzero"
            )));
        }
        if raw.offset % SECTION_ALIGN != 0 {
            return Err(DumpError::ImageFormatError(format!(
                "section {idx} offset {} is not {SECTION_ALIGN}-byte aligned",
                raw.offset
            )));
        }
        if raw.offset < header.payload_offset {
            return Err(DumpError::ImageFormatError(format!(
                "section {idx} starts before payload offset"
            )));
        }
        checked_end(raw.offset as usize, raw.len as usize, mmap.len())?;
        sections.push(raw);
    }

    let mut ranges: Vec<_> = sections
        .iter()
        .map(|section| {
            (
                section.offset,
                section.offset.saturating_add(section.len),
                section.kind,
            )
        })
        .collect();
    ranges.sort_unstable_by_key(|range| range.0);
    for pair in ranges.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(DumpError::ImageFormatError(format!(
                "sections {} and {} overlap",
                pair[0].2, pair[1].2
            )));
        }
    }

    Ok(LoadedMmapImage { mmap, sections })
}

fn checked_end(start: usize, len: usize, file_len: usize) -> Result<usize, DumpError> {
    let end = start
        .checked_add(len)
        .ok_or_else(|| DumpError::ImageFormatError("pdump image offset overflow".into()))?;
    if end > file_len {
        return Err(DumpError::ImageFormatError(format!(
            "section range {start}..{end} exceeds image length {file_len}"
        )));
    }
    Ok(end)
}

fn align_up(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

fn checksum_body(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(&bytes[HEADER_SIZE..]);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Seek, SeekFrom, Write};

    use super::*;

    #[test]
    fn write_and_load_sections_from_mmap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.pdump");

        write_image(
            &path,
            &[
                ImageSection {
                    kind: DumpSectionKind::Metadata,
                    flags: 0,
                    bytes: b"metadata",
                },
                ImageSection {
                    kind: DumpSectionKind::HeapImage,
                    flags: 7,
                    bytes: b"heap bytes",
                },
            ],
        )
        .unwrap();

        let image = load_image(&path).unwrap();
        assert_eq!(
            image.section(DumpSectionKind::Metadata),
            Some(&b"metadata"[..])
        );
        assert_eq!(
            image.section(DumpSectionKind::HeapImage),
            Some(&b"heap bytes"[..])
        );

        let mapped = image.mapped_range();
        let section_ptr = image.section(DumpSectionKind::HeapImage).unwrap().as_ptr() as usize;
        assert!(
            mapped.contains(&section_ptr),
            "section bytes must be borrowed from the mmap, not copied"
        );
    }

    #[test]
    fn load_image_does_not_hash_payload_corruption_on_startup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.pdump");
        write_image(
            &path,
            &[ImageSection {
                kind: DumpSectionKind::HeapImage,
                flags: 0,
                bytes: b"heap bytes",
            }],
        )
        .unwrap();

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::End(-1)).unwrap();
        let mut byte = [0u8; 1];
        file.read_exact(&mut byte).unwrap();
        byte[0] ^= 0x55;
        file.seek(SeekFrom::End(-1)).unwrap();
        file.write_all(&byte).unwrap();
        file.sync_all().unwrap();

        let image = load_image(&path).unwrap();
        assert_ne!(
            image.section(DumpSectionKind::HeapImage),
            Some(&b"heap bytes"[..])
        );
    }

    #[test]
    fn rejects_bad_section_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.pdump");
        write_image(
            &path,
            &[ImageSection {
                kind: DumpSectionKind::HeapImage,
                flags: 0,
                bytes: b"heap bytes",
            }],
        )
        .unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        let section_start = HEADER_SIZE;
        let offset_start = section_start + 8;
        let bogus_offset = (bytes.len() as u64 + 128).to_le_bytes();
        bytes[offset_start..offset_start + 8].copy_from_slice(&bogus_offset);

        let checksum = checksum_body(&bytes);
        let checksum_start = 16 + 4 + 4 + 4 + 4 + 32;
        bytes[checksum_start..checksum_start + 32].copy_from_slice(&checksum);
        std::fs::write(&path, bytes).unwrap();

        assert!(matches!(
            load_image(&path),
            Err(DumpError::ImageFormatError(_))
        ));
    }

    #[test]
    fn relocations_patch_mapped_pointers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.pdump");
        let pointer_bytes = vec![0u8; std::mem::size_of::<usize>()];
        let relocations = relocation_section_bytes(&[ImageRelocation {
            location_section: DumpSectionKind::HeapImage,
            location_offset: 0,
            target_section: DumpSectionKind::Metadata,
            target_offset: 2,
            addend: 0,
        }]);

        write_image(
            &path,
            &[
                ImageSection {
                    kind: DumpSectionKind::HeapImage,
                    flags: 0,
                    bytes: &pointer_bytes,
                },
                ImageSection {
                    kind: DumpSectionKind::Metadata,
                    flags: 0,
                    bytes: b"abcdef",
                },
                ImageSection {
                    kind: DumpSectionKind::Relocations,
                    flags: 0,
                    bytes: &relocations,
                },
            ],
        )
        .unwrap();

        let mut image = load_image(&path).unwrap();
        let before = image.section(DumpSectionKind::HeapImage).unwrap();
        assert_eq!(before, pointer_bytes.as_slice());

        image.apply_relocations().unwrap();

        let heap = image.section(DumpSectionKind::HeapImage).unwrap();
        let patched =
            usize::from_ne_bytes(heap[..std::mem::size_of::<usize>()].try_into().unwrap());
        let metadata = image.section(DumpSectionKind::Metadata).unwrap();
        let expected = unsafe { metadata.as_ptr().add(2) as usize };
        assert_eq!(patched, expected);
        assert!(image.mapped_range().contains(&patched));
    }

    #[test]
    fn relocations_can_patch_tagged_pointer_addends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.pdump");
        let pointer_bytes = vec![0u8; std::mem::size_of::<usize>()];
        let relocations = relocation_section_bytes(&[ImageRelocation {
            location_section: DumpSectionKind::HeapImage,
            location_offset: 0,
            target_section: DumpSectionKind::Metadata,
            target_offset: 0,
            addend: 0b011,
        }]);

        write_image(
            &path,
            &[
                ImageSection {
                    kind: DumpSectionKind::HeapImage,
                    flags: 0,
                    bytes: &pointer_bytes,
                },
                ImageSection {
                    kind: DumpSectionKind::Metadata,
                    flags: 0,
                    bytes: b"target",
                },
                ImageSection {
                    kind: DumpSectionKind::Relocations,
                    flags: 0,
                    bytes: &relocations,
                },
            ],
        )
        .unwrap();

        let mut image = load_image(&path).unwrap();
        image.apply_relocations().unwrap();

        let heap = image.section(DumpSectionKind::HeapImage).unwrap();
        let patched =
            usize::from_ne_bytes(heap[..std::mem::size_of::<usize>()].try_into().unwrap());
        let metadata = image.section(DumpSectionKind::Metadata).unwrap();
        let expected = metadata.as_ptr() as usize + 0b011;
        assert_eq!(patched, expected);
    }

    #[test]
    fn heap_to_heap_relocations_patch_mapped_pointers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.pdump");
        let mut heap_bytes = vec![0u8; 2 * std::mem::size_of::<usize>()];
        heap_bytes[std::mem::size_of::<usize>()..].copy_from_slice(&0xfeedusize.to_ne_bytes());
        let relocations = relocation_section_bytes(&[ImageRelocation {
            location_section: DumpSectionKind::HeapImage,
            location_offset: 0,
            target_section: DumpSectionKind::HeapImage,
            target_offset: std::mem::size_of::<usize>() as u64,
            addend: 0b011,
        }]);

        write_image(
            &path,
            &[
                ImageSection {
                    kind: DumpSectionKind::HeapImage,
                    flags: 0,
                    bytes: &heap_bytes,
                },
                ImageSection {
                    kind: DumpSectionKind::Relocations,
                    flags: 0,
                    bytes: &relocations,
                },
            ],
        )
        .unwrap();

        let mut image = load_image(&path).unwrap();
        image.apply_relocations().unwrap();

        let heap = image.section(DumpSectionKind::HeapImage).unwrap();
        let patched =
            usize::from_ne_bytes(heap[..std::mem::size_of::<usize>()].try_into().unwrap());
        let expected = unsafe { heap.as_ptr().add(std::mem::size_of::<usize>()) as usize } + 0b011;
        assert_eq!(patched, expected);
    }

    #[test]
    fn rejects_malformed_relocation_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.pdump");

        write_image(
            &path,
            &[
                ImageSection {
                    kind: DumpSectionKind::HeapImage,
                    flags: 0,
                    bytes: &[0u8; std::mem::size_of::<usize>()],
                },
                ImageSection {
                    kind: DumpSectionKind::Relocations,
                    flags: 0,
                    bytes: &[0u8; RELOCATION_SIZE - 1],
                },
            ],
        )
        .unwrap();

        let mut image = load_image(&path).unwrap();
        assert!(matches!(
            image.apply_relocations(),
            Err(DumpError::ImageFormatError(_))
        ));
    }

    #[test]
    fn rejects_relocation_outside_location_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.pdump");
        let relocations = relocation_section_bytes(&[ImageRelocation {
            location_section: DumpSectionKind::HeapImage,
            location_offset: 1,
            target_section: DumpSectionKind::Metadata,
            target_offset: 0,
            addend: 0,
        }]);

        write_image(
            &path,
            &[
                ImageSection {
                    kind: DumpSectionKind::HeapImage,
                    flags: 0,
                    bytes: &[0u8; std::mem::size_of::<usize>()],
                },
                ImageSection {
                    kind: DumpSectionKind::Metadata,
                    flags: 0,
                    bytes: b"target",
                },
                ImageSection {
                    kind: DumpSectionKind::Relocations,
                    flags: 0,
                    bytes: &relocations,
                },
            ],
        )
        .unwrap();

        let mut image = load_image(&path).unwrap();
        assert!(matches!(
            image.apply_relocations(),
            Err(DumpError::ImageFormatError(_))
        ));
    }
}
