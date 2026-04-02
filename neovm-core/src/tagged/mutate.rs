//! Centralized tagged-heap mutation helpers.
//!
//! These functions are the single place to hook future generational or
//! incremental write barriers into the tagged runtime.

use crate::buffer::text_props::TextPropertyTable;

use super::gc::note_heap_write;
use super::header::{ConsCell, LambdaObj, MacroObj, RecordObj, StringObj, VecLikeType, VectorObj};
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
pub fn vector_data_mut_ref(value: TaggedValue) -> Option<&'static mut Vec<TaggedValue>> {
    if value.veclike_type()? != VecLikeType::Vector {
        return None;
    }
    note_heap_write(value);
    let ptr = value.as_veclike_ptr().unwrap() as *mut VectorObj;
    Some(unsafe { &mut (*ptr).data })
}

#[inline]
pub fn record_data_mut_ref(value: TaggedValue) -> Option<&'static mut Vec<TaggedValue>> {
    if value.veclike_type()? != VecLikeType::Record {
        return None;
    }
    note_heap_write(value);
    let ptr = value.as_veclike_ptr().unwrap() as *mut RecordObj;
    Some(unsafe { &mut (*ptr).data })
}

#[inline]
pub fn closure_slots_mut_ref(value: TaggedValue) -> Option<&'static mut Vec<TaggedValue>> {
    note_heap_write(value);
    match value.veclike_type()? {
        VecLikeType::Lambda => {
            let ptr = value.as_veclike_ptr().unwrap() as *mut LambdaObj;
            unsafe {
                let obj = &mut *ptr;
                let _ = obj.parsed_params.take();
                Some(&mut obj.data)
            }
        }
        VecLikeType::Macro => {
            let ptr = value.as_veclike_ptr().unwrap() as *mut MacroObj;
            unsafe {
                let obj = &mut *ptr;
                let _ = obj.parsed_params.take();
                Some(&mut obj.data)
            }
        }
        _ => None,
    }
}

#[inline]
pub fn string_text_props_mut_ref(value: TaggedValue) -> Option<&'static mut TextPropertyTable> {
    let ptr = value.as_string_ptr()? as *mut StringObj;
    note_heap_write(value);
    Some(unsafe { &mut (*ptr).text_props })
}
