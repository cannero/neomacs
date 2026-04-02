//! Centralized tagged-heap mutation helpers.
//!
//! These functions are the single place to hook future generational or
//! incremental write barriers into the tagged runtime.

use crate::buffer::text_props::TextPropertyTable;
use crate::emacs_core::bytecode::ByteCodeFunction;
use crate::emacs_core::value::LispHashTable;
use crate::heap_types::{LispString, MarkerData, OverlayData};

use super::gc::note_heap_write;
use super::header::{
    ByteCodeObj, ConsCell, HashTableObj, LambdaObj, MacroObj, MarkerObj, OverlayObj, RecordObj,
    StringObj, VecLikeType, VectorObj,
};
use super::value::TaggedValue;

#[inline]
pub fn set_cons_car(cell: TaggedValue, value: TaggedValue) -> bool {
    if !cell.is_cons() {
        return false;
    }
    note_heap_write(cell);
    unsafe {
        (*(cell.xcons_ptr() as *mut ConsCell)).car = value;
    }
    true
}

#[inline]
pub fn set_cons_cdr(cell: TaggedValue, value: TaggedValue) -> bool {
    if !cell.is_cons() {
        return false;
    }
    note_heap_write(cell);
    unsafe {
        (*(cell.xcons_ptr() as *mut ConsCell)).cdr = value;
    }
    true
}

#[inline]
pub fn with_vector_data_mut<R>(
    value: TaggedValue,
    f: impl FnOnce(&mut Vec<TaggedValue>) -> R,
) -> Option<R> {
    if value.veclike_type()? != VecLikeType::Vector {
        return None;
    }
    note_heap_write(value);
    let ptr = value.as_veclike_ptr().unwrap() as *mut VectorObj;
    Some(f(unsafe { &mut (*ptr).data }))
}

#[inline]
pub fn replace_vector_data(value: TaggedValue, items: Vec<TaggedValue>) -> bool {
    with_vector_data_mut(value, |data| *data = items).is_some()
}

#[inline]
pub fn set_vector_slot(value: TaggedValue, index: usize, item: TaggedValue) -> bool {
    with_vector_data_mut(value, |data| {
        let slot = match data.get_mut(index) {
            Some(slot) => slot,
            None => return false,
        };
        *slot = item;
        true
    })
    .unwrap_or(false)
}

#[inline]
pub fn with_record_data_mut<R>(
    value: TaggedValue,
    f: impl FnOnce(&mut Vec<TaggedValue>) -> R,
) -> Option<R> {
    if value.veclike_type()? != VecLikeType::Record {
        return None;
    }
    note_heap_write(value);
    let ptr = value.as_veclike_ptr().unwrap() as *mut RecordObj;
    Some(f(unsafe { &mut (*ptr).data }))
}

#[inline]
pub fn replace_record_data(value: TaggedValue, items: Vec<TaggedValue>) -> bool {
    with_record_data_mut(value, |data| *data = items).is_some()
}

#[inline]
pub fn set_record_slot(value: TaggedValue, index: usize, item: TaggedValue) -> bool {
    with_record_data_mut(value, |data| {
        let slot = match data.get_mut(index) {
            Some(slot) => slot,
            None => return false,
        };
        *slot = item;
        true
    })
    .unwrap_or(false)
}

#[inline]
pub fn with_closure_slots_mut<R>(
    value: TaggedValue,
    f: impl FnOnce(&mut Vec<TaggedValue>) -> R,
) -> Option<R> {
    note_heap_write(value);
    match value.veclike_type()? {
        VecLikeType::Lambda => {
            let ptr = value.as_veclike_ptr().unwrap() as *mut LambdaObj;
            unsafe {
                let obj = &mut *ptr;
                let _ = obj.parsed_params.take();
                Some(f(&mut obj.data))
            }
        }
        VecLikeType::Macro => {
            let ptr = value.as_veclike_ptr().unwrap() as *mut MacroObj;
            unsafe {
                let obj = &mut *ptr;
                let _ = obj.parsed_params.take();
                Some(f(&mut obj.data))
            }
        }
        _ => None,
    }
}

#[inline]
pub fn replace_closure_slots(value: TaggedValue, slots: Vec<TaggedValue>) -> bool {
    with_closure_slots_mut(value, |data| *data = slots).is_some()
}

#[inline]
pub fn set_closure_slot(value: TaggedValue, index: usize, item: TaggedValue) -> bool {
    with_closure_slots_mut(value, |data| {
        let slot = match data.get_mut(index) {
            Some(slot) => slot,
            None => return false,
        };
        *slot = item;
        true
    })
    .unwrap_or(false)
}

#[inline]
pub fn with_string_text_props_mut<R>(
    value: TaggedValue,
    f: impl FnOnce(&mut TextPropertyTable) -> R,
) -> Option<R> {
    let ptr = value.as_string_ptr()? as *mut StringObj;
    note_heap_write(value);
    Some(f(unsafe { &mut (*ptr).text_props }))
}

#[inline]
pub fn with_lisp_string_mut<R>(
    value: TaggedValue,
    f: impl FnOnce(&mut LispString) -> R,
) -> Option<R> {
    let ptr = value.as_string_ptr()? as *mut StringObj;
    note_heap_write(value);
    Some(f(unsafe { &mut (*ptr).data }))
}

#[inline]
pub fn with_hash_table_mut<R>(
    value: TaggedValue,
    f: impl FnOnce(&mut LispHashTable) -> R,
) -> Option<R> {
    if value.veclike_type()? != VecLikeType::HashTable {
        return None;
    }
    note_heap_write(value);
    let ptr = value.as_veclike_ptr().unwrap() as *mut HashTableObj;
    Some(f(unsafe { &mut (*ptr).table }))
}

#[inline]
pub fn with_bytecode_data_mut<R>(
    value: TaggedValue,
    f: impl FnOnce(&mut ByteCodeFunction) -> R,
) -> Option<R> {
    if value.veclike_type()? != VecLikeType::ByteCode {
        return None;
    }
    note_heap_write(value);
    let ptr = value.as_veclike_ptr().unwrap() as *mut ByteCodeObj;
    Some(f(unsafe { &mut (*ptr).data }))
}

#[inline]
pub fn with_marker_data_mut<R>(
    value: TaggedValue,
    f: impl FnOnce(&mut MarkerData) -> R,
) -> Option<R> {
    if value.veclike_type()? != VecLikeType::Marker {
        return None;
    }
    note_heap_write(value);
    let ptr = value.as_veclike_ptr().unwrap() as *mut MarkerObj;
    Some(f(unsafe { &mut (*ptr).data }))
}

#[inline]
pub fn with_overlay_data_mut<R>(
    value: TaggedValue,
    f: impl FnOnce(&mut OverlayData) -> R,
) -> Option<R> {
    if value.veclike_type()? != VecLikeType::Overlay {
        return None;
    }
    note_heap_write(value);
    let ptr = value.as_veclike_ptr().unwrap() as *mut OverlayObj;
    Some(f(unsafe { &mut (*ptr).data }))
}
