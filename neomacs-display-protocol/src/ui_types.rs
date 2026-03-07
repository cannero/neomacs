//! Shared UI menu/toolbar/popup item types.

/// A single item in a popup menu.
#[derive(Debug, Clone)]
pub struct PopupMenuItem {
    /// Display label for the item
    pub label: String,
    /// Keyboard shortcut text (e.g., "C-x C-s"), or empty
    pub shortcut: String,
    /// Whether the item is enabled (selectable)
    pub enabled: bool,
    /// Whether this is a separator line
    pub separator: bool,
    /// Whether this is a submenu header (has children)
    pub submenu: bool,
    /// Nesting depth (0 = top-level, 1 = first submenu, etc.)
    pub depth: u32,
}

/// A top-level menu bar item (e.g., "File", "Edit", "Tools").
#[derive(Clone, Debug)]
pub struct MenuBarItem {
    pub index: u32,
    pub label: String,
    pub key: String,
}

/// A single toolbar item.
#[derive(Clone, Debug)]
pub struct ToolBarItem {
    pub index: u32,
    pub icon_name: String,
    pub label: String,
    pub help: String,
    pub enabled: bool,
    pub selected: bool,
    pub is_separator: bool,
}

/// A single tab bar item.
#[derive(Clone, Debug)]
pub struct TabBarItem {
    pub index: u32,
    pub label: String,
    pub help: String,
    pub enabled: bool,
    pub selected: bool,
    pub is_separator: bool,
}
