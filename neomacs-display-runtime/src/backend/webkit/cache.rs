//! Cache for managing multiple WebKit views.

use super::view::{WebKitState, WebKitView};
use crate::core::error::{DisplayError, DisplayResult};
use std::collections::HashMap;

#[cfg(feature = "wpe-webkit")]
use crate::backend::wpe::WpeBackend;

/// Cache for managing multiple WebKit browser views
pub struct WebKitCache {
    views: HashMap<u32, WebKitView>,
    next_id: u32,
}

impl Default for WebKitCache {
    fn default() -> Self {
        Self::new()
    }
}

impl WebKitCache {
    /// Create a new WebKit cache
    pub fn new() -> Self {
        Self {
            views: HashMap::new(),
            next_id: 1,
        }
    }

    /// Create a new WebKit view with WPE backend
    #[cfg(feature = "wpe-webkit")]
    pub fn create_with_backend(
        &mut self,
        backend: &WpeBackend,
        width: i32,
        height: i32,
    ) -> DisplayResult<u32> {
        let id = self.next_id;
        self.next_id += 1;
        let view = WebKitView::new(id, backend, width, height)?;
        self.views.insert(id, view);
        tracing::info!("Created WebKit view {} ({}x{})", id, width, height);
        Ok(id)
    }

    /// Create a new WebKit view (stub for non-wpe builds)
    #[cfg(not(feature = "wpe-webkit"))]
    pub fn create(&mut self, _width: i32, _height: i32) -> DisplayResult<u32> {
        Err(DisplayError::WebKit(
            "WPE WebKit support not compiled".into(),
        ))
    }

    /// Get a view by ID
    pub fn get(&self, id: u32) -> Option<&WebKitView> {
        self.views.get(&id)
    }

    /// Get a view by ID (mutable)
    pub fn get_mut(&mut self, id: u32) -> Option<&mut WebKitView> {
        self.views.get_mut(&id)
    }

    /// Remove a view
    pub fn remove(&mut self, id: u32) -> bool {
        self.views.remove(&id).is_some()
    }

    /// Update all views
    pub fn update_all(&mut self) {
        for view in self.views.values_mut() {
            view.update();
        }
    }

    /// Load URI in a view
    pub fn load_uri(&mut self, id: u32, uri: &str) -> DisplayResult<()> {
        let view = self
            .views
            .get_mut(&id)
            .ok_or_else(|| DisplayError::WebKit(format!("View {} not found", id)))?;
        view.load_uri(uri);
        Ok(())
    }

    /// Load HTML in a view
    pub fn load_html(&mut self, id: u32, html: &str, base_uri: Option<&str>) -> DisplayResult<()> {
        let view = self
            .views
            .get_mut(&id)
            .ok_or_else(|| DisplayError::WebKit(format!("View {} not found", id)))?;
        view.load_html(html, base_uri);
        Ok(())
    }

    /// Execute JavaScript in a view
    pub fn execute_javascript(&mut self, id: u32, script: &str) -> DisplayResult<()> {
        let view = self
            .views
            .get_mut(&id)
            .ok_or_else(|| DisplayError::WebKit(format!("View {} not found", id)))?;
        view.execute_javascript(script);
        Ok(())
    }

    /// Get number of views
    pub fn len(&self) -> usize {
        self.views.len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }

    /// Iterate over all views
    pub fn iter(&self) -> impl Iterator<Item = (u32, &WebKitView)> {
        self.views.iter().map(|(&id, view)| (id, view))
    }

    /// Get all views as a HashMap reference
    pub fn views(&self) -> &HashMap<u32, WebKitView> {
        &self.views
    }

    /// Send keyboard event to a view
    pub fn send_keyboard_event(
        &self,
        id: u32,
        key_code: u32,
        hardware_key_code: u32,
        pressed: bool,
        modifiers: u32,
    ) -> DisplayResult<()> {
        let view = self
            .views
            .get(&id)
            .ok_or_else(|| DisplayError::WebKit(format!("View {} not found", id)))?;
        view.send_keyboard_event(key_code, hardware_key_code, pressed, modifiers);
        Ok(())
    }

    /// Send pointer event to a view
    pub fn send_pointer_event(
        &self,
        id: u32,
        event_type: u32,
        x: i32,
        y: i32,
        button: u32,
        state: u32,
        modifiers: u32,
    ) -> DisplayResult<()> {
        let view = self
            .views
            .get(&id)
            .ok_or_else(|| DisplayError::WebKit(format!("View {} not found", id)))?;
        view.send_pointer_event(event_type, x, y, button, state, modifiers);
        Ok(())
    }

    /// Send scroll event to a view
    pub fn send_scroll_event(
        &self,
        id: u32,
        x: i32,
        y: i32,
        delta_x: i32,
        delta_y: i32,
    ) -> DisplayResult<()> {
        let view = self
            .views
            .get(&id)
            .ok_or_else(|| DisplayError::WebKit(format!("View {} not found", id)))?;
        view.scroll(x, y, delta_x, delta_y);
        Ok(())
    }

    /// Click in a view
    pub fn click(&self, id: u32, x: i32, y: i32, button: u32) -> DisplayResult<()> {
        let view = self
            .views
            .get(&id)
            .ok_or_else(|| DisplayError::WebKit(format!("View {} not found", id)))?;
        view.click(x, y, button);
        Ok(())
    }

    /// Get view title
    pub fn get_title(&self, id: u32) -> Option<String> {
        self.views
            .get(&id)
            .and_then(|v| v.title().map(|s| s.to_string()))
    }

    /// Get view URL
    pub fn get_url(&self, id: u32) -> Option<String> {
        self.views.get(&id).map(|v| v.url().to_string())
    }

    /// Get view loading progress (0.0 - 1.0)
    pub fn get_progress(&self, id: u32) -> Option<f64> {
        self.views.get(&id).map(|v| v.progress())
    }

    /// Check if view is loading
    pub fn is_loading(&self, id: u32) -> Option<bool> {
        self.views
            .get(&id)
            .map(|v| v.state() == WebKitState::Loading)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_creation() {
        let cache = WebKitCache::new();
        assert!(cache.is_empty());
    }
}
