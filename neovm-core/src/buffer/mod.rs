pub mod buffer;
pub mod buffer_text;
pub mod gap_buffer;
pub mod overlay;
pub mod shared;
pub mod text_props;
pub mod undo;

pub use buffer::{
    Buffer, BufferId, BufferManager, InsertionType, LabeledRestriction, LabeledRestrictionLabel,
    SavedRestrictionKind, SavedRestrictionState,
};
pub use buffer_text::BufferText;
pub use overlay::{Overlay, OverlayList};
pub use shared::SharedUndoState;
pub use text_props::TextPropertyTable;
pub use undo::{
    truncate_undo_list, undo_list_boundary, undo_list_contains_boundary,
    undo_list_has_trailing_boundary, undo_list_is_disabled, undo_list_is_empty,
    undo_list_pop_group, undo_list_record_delete, undo_list_record_first_change,
    undo_list_record_insert, undo_list_record_point, undo_list_record_property_change,
};
