//! Scene graph for display rendering.

use crate::face::Face;
use crate::types::{Color, Rect, Transform};
use std::collections::HashMap;

/// Scene graph node types
#[derive(Debug, Clone)]
pub enum NodeKind {
    /// Container with children
    Container { children: Vec<Node> },

    /// Text run with shaped glyphs
    TextRun {
        text: String,
        face_id: u32,
        x: f32,
        y: f32,
    },

    /// Solid color rectangle
    ColorRect { color: Color },

    /// Image texture
    Image { image_id: u32 },

    /// Video frame
    Video { video_id: u32 },

    /// WPE WebKit view
    Wpe { view_id: u32 },

    /// Cursor
    Cursor {
        style: SceneCursorStyle,
        color: Color,
        blink_on: bool,
    },
}

/// Cursor style
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneCursorStyle {
    Box,
    Bar,
    Underline,
    Hollow,
}

impl Default for SceneCursorStyle {
    fn default() -> Self {
        Self::Box
    }
}

/// A node in the scene graph
#[derive(Debug, Clone)]
pub struct Node {
    /// Kind of node
    pub kind: NodeKind,

    /// Bounding rectangle
    pub bounds: Rect,

    /// Opacity (0.0 - 1.0)
    pub opacity: f32,

    /// Optional transform
    pub transform: Option<Transform>,

    /// Optional clip rectangle
    pub clip: Option<Rect>,
}

impl Node {
    /// Create a container node
    pub fn container(children: Vec<Node>) -> Self {
        Self {
            kind: NodeKind::Container { children },
            bounds: Rect::ZERO,
            opacity: 1.0,
            transform: None,
            clip: None,
        }
    }

    /// Create a container with transform
    pub fn container_with_transform(children: Vec<Node>, transform: Transform) -> Self {
        Self {
            kind: NodeKind::Container { children },
            bounds: Rect::ZERO,
            opacity: 1.0,
            transform: Some(transform),
            clip: None,
        }
    }

    /// Create a color rectangle node
    pub fn color_rect(bounds: Rect, color: Color) -> Self {
        Self {
            kind: NodeKind::ColorRect { color },
            bounds,
            opacity: 1.0,
            transform: None,
            clip: None,
        }
    }

    /// Create a text run node
    pub fn text_run(text: String, face_id: u32, x: f32, y: f32, bounds: Rect) -> Self {
        Self {
            kind: NodeKind::TextRun {
                text,
                face_id,
                x,
                y,
            },
            bounds,
            opacity: 1.0,
            transform: None,
            clip: None,
        }
    }

    /// Create an image node
    pub fn image(image_id: u32, bounds: Rect) -> Self {
        Self {
            kind: NodeKind::Image { image_id },
            bounds,
            opacity: 1.0,
            transform: None,
            clip: None,
        }
    }

    /// Create a video node
    pub fn video(video_id: u32, bounds: Rect) -> Self {
        Self {
            kind: NodeKind::Video { video_id },
            bounds,
            opacity: 1.0,
            transform: None,
            clip: None,
        }
    }

    /// Create a cursor node
    pub fn cursor(style: SceneCursorStyle, color: Color, bounds: Rect) -> Self {
        Self {
            kind: NodeKind::Cursor {
                style,
                color,
                blink_on: true,
            },
            bounds,
            opacity: 1.0,
            transform: None,
            clip: None,
        }
    }

    /// Set opacity
    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    /// Set transform
    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = Some(transform);
        self
    }

    /// Set clip
    pub fn with_clip(mut self, clip: Rect) -> Self {
        self.clip = Some(clip);
        self
    }
}

/// Represents a window in the scene
#[derive(Debug, Clone)]
pub struct WindowScene {
    /// Window ID (from Emacs window pointer)
    pub window_id: i32,

    /// Position and size
    pub bounds: Rect,

    /// Background color
    pub background: Color,

    /// Cursor position and style
    pub cursor: Option<CursorState>,

    /// Scroll offset (for smooth scrolling)
    pub scroll_offset: f32,

    /// Is this the selected (focused) window?
    pub selected: bool,

    /// Mode line height
    pub mode_line_height: i32,

    /// Header line height
    pub header_line_height: i32,

    /// Frame counter when this window was last touched (for stale window removal)
    pub last_frame_touched: u64,
}

/// Cursor state in a window
#[derive(Debug, Clone)]
pub struct CursorState {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub style: SceneCursorStyle,
    pub color: Color,
    pub visible: bool,
}

/// Complete scene for a frame
#[derive(Debug, Clone)]
pub struct Scene {
    /// Frame dimensions
    pub width: f32,
    pub height: f32,

    /// Background color
    pub background: Color,

    /// Windows in this frame
    pub windows: Vec<WindowScene>,

    /// Root node of the scene graph
    pub root: Option<Node>,

    /// Dirty region (needs redraw)
    pub dirty: Option<Rect>,

    /// Faces used in this scene (face_id -> Face)
    pub faces: HashMap<u32, Face>,

    /// Floating videos at screen positions
    pub floating_videos: Vec<FloatingVideo>,

    /// Floating images at screen positions
    pub floating_images: Vec<FloatingImage>,

    /// Floating WebKit views at screen positions
    pub floating_webkits: Vec<FloatingWebKit>,

    /// Vertical/horizontal window borders
    pub borders: Vec<BorderRect>,
}

/// Border rectangle for window dividers
#[derive(Debug, Clone)]
pub struct BorderRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: Color,
}

/// Floating video layer for rendering video at a specific screen position
#[derive(Debug, Clone)]
pub struct FloatingVideo {
    pub video_id: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Floating image layer for rendering image at a specific screen position
#[derive(Debug, Clone)]
pub struct FloatingImage {
    pub image_id: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Floating WebKit view for rendering web content at a specific screen position
#[derive(Debug, Clone)]
pub struct FloatingWebKit {
    pub webkit_id: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Scene {
    /// Create a new empty scene
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            background: Color::BLACK,
            windows: Vec::new(),
            root: None,
            dirty: None,
            faces: HashMap::new(),
            floating_videos: Vec::new(),
            floating_images: Vec::new(),
            floating_webkits: Vec::new(),
            borders: Vec::new(),
        }
    }

    /// Mark the entire scene as dirty
    pub fn mark_dirty(&mut self) {
        self.dirty = Some(Rect::new(0.0, 0.0, self.width, self.height));
    }

    /// Mark a region as dirty
    pub fn mark_region_dirty(&mut self, region: Rect) {
        self.dirty = Some(match self.dirty {
            Some(existing) => {
                // Union of existing and new dirty region
                let x = existing.x.min(region.x);
                let y = existing.y.min(region.y);
                let right = existing.right().max(region.right());
                let bottom = existing.bottom().max(region.bottom());
                Rect::new(x, y, right - x, bottom - y)
            }
            None => region,
        });
    }

    /// Clear dirty region
    pub fn clear_dirty(&mut self) {
        self.dirty = None;
    }

    /// Clear all windows and content from the scene (preserves faces)
    pub fn clear(&mut self) {
        self.windows.clear();
        self.root = None;
        self.mark_dirty();
    }

    /// Add or update a face in the scene
    pub fn set_face(&mut self, face: Face) {
        self.faces.insert(face.id, face);
    }

    /// Get a face by ID
    pub fn get_face(&self, id: u32) -> Option<&Face> {
        self.faces.get(&id)
    }

    /// Add a floating video at screen position
    pub fn add_floating_video(&mut self, video_id: u32, x: f32, y: f32, width: f32, height: f32) {
        self.floating_videos.push(FloatingVideo {
            video_id,
            x,
            y,
            width,
            height,
        });
        self.mark_dirty();
    }

    /// Remove floating video by video ID
    pub fn remove_floating_video(&mut self, video_id: u32) {
        self.floating_videos.retain(|v| v.video_id != video_id);
        self.mark_dirty();
    }

    /// Clear all floating videos
    pub fn clear_floating_videos(&mut self) {
        self.floating_videos.clear();
        self.mark_dirty();
    }

    /// Add a floating image at screen position
    pub fn add_floating_image(&mut self, image_id: u32, x: f32, y: f32, width: f32, height: f32) {
        self.floating_images.push(FloatingImage {
            image_id,
            x,
            y,
            width,
            height,
        });
        self.mark_dirty();
    }

    /// Remove floating image by image ID
    pub fn remove_floating_image(&mut self, image_id: u32) {
        self.floating_images.retain(|i| i.image_id != image_id);
        self.mark_dirty();
    }

    /// Clear all floating images
    pub fn clear_floating_images(&mut self) {
        self.floating_images.clear();
        self.mark_dirty();
    }

    /// Add a floating WebKit view at screen position
    pub fn add_floating_webkit(&mut self, webkit_id: u32, x: f32, y: f32, width: f32, height: f32) {
        self.floating_webkits.push(FloatingWebKit {
            webkit_id,
            x,
            y,
            width,
            height,
        });
        self.mark_dirty();
    }

    /// Remove floating WebKit view by ID
    pub fn remove_floating_webkit(&mut self, webkit_id: u32) {
        self.floating_webkits.retain(|w| w.webkit_id != webkit_id);
        self.mark_dirty();
    }

    /// Clear all floating WebKit views
    pub fn clear_floating_webkits(&mut self) {
        self.floating_webkits.clear();
        self.mark_dirty();
    }

    /// Add a border rectangle
    pub fn add_border(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        self.borders.push(BorderRect {
            x,
            y,
            width,
            height,
            color,
        });
        self.mark_dirty();
    }

    /// Clear all borders
    pub fn clear_borders(&mut self) {
        self.borders.clear();
    }

    /// Build the scene graph from windows
    pub fn build(&mut self) {
        let mut children = Vec::new();

        // Background
        children.push(Node::color_rect(
            Rect::new(0.0, 0.0, self.width, self.height),
            self.background,
        ));

        // Each window
        for window in &self.windows {
            children.push(self.build_window_node(window));
        }

        // Borders (drawn on top of windows)
        for border in &self.borders {
            children.push(Node::color_rect(
                Rect::new(border.x, border.y, border.width, border.height),
                border.color,
            ));
        }

        self.root = Some(Node::container(children));
    }

    fn build_window_node(&self, window: &WindowScene) -> Node {
        let mut children = Vec::new();

        // Window background
        children.push(Node::color_rect(
            Rect::new(0.0, 0.0, window.bounds.width, window.bounds.height),
            window.background,
        ));

        // TODO: Build text nodes from glyph rows

        // Cursor
        if let Some(cursor) = &window.cursor {
            if cursor.visible {
                children.push(Node::cursor(
                    cursor.style,
                    cursor.color,
                    Rect::new(cursor.x, cursor.y, cursor.width, cursor.height),
                ));
            }
        }

        // Apply window position and scroll offset
        let transform =
            Transform::translate(window.bounds.x, window.bounds.y - window.scroll_offset);

        Node::container_with_transform(children, transform).with_clip(window.bounds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scene_creation() {
        let scene = Scene::new(800.0, 600.0);
        assert_eq!(scene.width, 800.0);
        assert_eq!(scene.height, 600.0);
    }

    #[test]
    fn test_dirty_region() {
        let mut scene = Scene::new(800.0, 600.0);
        scene.mark_region_dirty(Rect::new(10.0, 10.0, 100.0, 100.0));
        scene.mark_region_dirty(Rect::new(50.0, 50.0, 100.0, 100.0));

        let dirty = scene.dirty.unwrap();
        assert_eq!(dirty.x, 10.0);
        assert_eq!(dirty.y, 10.0);
        assert_eq!(dirty.right(), 150.0);
        assert_eq!(dirty.bottom(), 150.0);
    }

    // ---------------------------------------------------------------
    // Scene creation and modification
    // ---------------------------------------------------------------

    #[test]
    fn test_scene_defaults() {
        let scene = Scene::new(1920.0, 1080.0);
        assert_eq!(scene.background, Color::BLACK);
        assert!(scene.windows.is_empty());
        assert!(scene.root.is_none());
        assert!(scene.dirty.is_none());
        assert!(scene.faces.is_empty());
        assert!(scene.floating_videos.is_empty());
        assert!(scene.floating_images.is_empty());
        assert!(scene.floating_webkits.is_empty());
        assert!(scene.borders.is_empty());
    }

    #[test]
    fn test_scene_clear_resets_windows_and_root_but_preserves_faces() {
        let mut scene = Scene::new(800.0, 600.0);
        // Add a face
        let face = Face::new(1);
        scene.set_face(face);
        // Add a window
        scene.windows.push(make_window(42, 0.0, 0.0, 400.0, 300.0));
        // Build the scene graph
        scene.build();
        assert!(scene.root.is_some());
        assert_eq!(scene.windows.len(), 1);

        scene.clear();

        assert!(scene.windows.is_empty());
        assert!(scene.root.is_none());
        // Faces should be preserved
        assert!(scene.get_face(1).is_some());
        // clear marks the scene dirty
        assert!(scene.dirty.is_some());
    }

    #[test]
    fn test_scene_set_and_get_face() {
        let mut scene = Scene::new(800.0, 600.0);
        let mut face = Face::new(5);
        face.font_size = 16.0;
        face.font_family = "Iosevka".to_string();
        scene.set_face(face);

        let retrieved = scene.get_face(5).unwrap();
        assert_eq!(retrieved.id, 5);
        assert_eq!(retrieved.font_size, 16.0);
        assert_eq!(retrieved.font_family, "Iosevka");
        // Non-existent face
        assert!(scene.get_face(99).is_none());
    }

    #[test]
    fn test_scene_set_face_overwrites() {
        let mut scene = Scene::new(800.0, 600.0);
        let mut face = Face::new(1);
        face.font_size = 12.0;
        scene.set_face(face);

        let mut updated = Face::new(1);
        updated.font_size = 20.0;
        scene.set_face(updated);

        let f = scene.get_face(1).unwrap();
        assert_eq!(f.font_size, 20.0);
    }

    // ---------------------------------------------------------------
    // Node creation (text, rect, image, etc.)
    // ---------------------------------------------------------------

    #[test]
    fn test_node_color_rect() {
        let bounds = Rect::new(10.0, 20.0, 100.0, 50.0);
        let node = Node::color_rect(bounds, Color::RED);
        assert_eq!(node.bounds, bounds);
        assert_eq!(node.opacity, 1.0);
        assert!(node.transform.is_none());
        assert!(node.clip.is_none());
        match node.kind {
            NodeKind::ColorRect { color } => assert_eq!(color, Color::RED),
            _ => panic!("Expected ColorRect"),
        }
    }

    #[test]
    fn test_node_text_run() {
        let bounds = Rect::new(0.0, 0.0, 200.0, 16.0);
        let node = Node::text_run("hello world".into(), 3, 5.0, 10.0, bounds);
        match &node.kind {
            NodeKind::TextRun {
                text,
                face_id,
                x,
                y,
            } => {
                assert_eq!(text, "hello world");
                assert_eq!(*face_id, 3);
                assert_eq!(*x, 5.0);
                assert_eq!(*y, 10.0);
            }
            _ => panic!("Expected TextRun"),
        }
        assert_eq!(node.bounds, bounds);
    }

    #[test]
    fn test_node_image() {
        let bounds = Rect::new(0.0, 0.0, 64.0, 64.0);
        let node = Node::image(42, bounds);
        match &node.kind {
            NodeKind::Image { image_id } => assert_eq!(*image_id, 42),
            _ => panic!("Expected Image"),
        }
    }

    #[test]
    fn test_node_video() {
        let bounds = Rect::new(0.0, 0.0, 320.0, 240.0);
        let node = Node::video(7, bounds);
        match &node.kind {
            NodeKind::Video { video_id } => assert_eq!(*video_id, 7),
            _ => panic!("Expected Video"),
        }
    }

    #[test]
    fn test_node_cursor_creation() {
        let bounds = Rect::new(100.0, 50.0, 8.0, 16.0);
        let node = Node::cursor(SceneCursorStyle::Bar, Color::WHITE, bounds);
        match &node.kind {
            NodeKind::Cursor {
                style,
                color,
                blink_on,
            } => {
                assert_eq!(*style, SceneCursorStyle::Bar);
                assert_eq!(*color, Color::WHITE);
                assert!(*blink_on);
            }
            _ => panic!("Expected Cursor"),
        }
        assert_eq!(node.bounds, bounds);
    }

    // ---------------------------------------------------------------
    // Node builder methods (with_opacity, with_transform, with_clip)
    // ---------------------------------------------------------------

    #[test]
    fn test_node_builder_methods() {
        let bounds = Rect::new(0.0, 0.0, 50.0, 50.0);
        let clip = Rect::new(5.0, 5.0, 40.0, 40.0);
        let transform = Transform::translate(10.0, 20.0);
        let node = Node::color_rect(bounds, Color::GREEN)
            .with_opacity(0.5)
            .with_transform(transform)
            .with_clip(clip);

        assert_eq!(node.opacity, 0.5);
        assert_eq!(node.transform, Some(transform));
        assert_eq!(node.clip, Some(clip));
    }

    // ---------------------------------------------------------------
    // Node tree building and traversal
    // ---------------------------------------------------------------

    #[test]
    fn test_container_with_children() {
        let child1 = Node::color_rect(Rect::new(0.0, 0.0, 10.0, 10.0), Color::RED);
        let child2 = Node::color_rect(Rect::new(10.0, 0.0, 10.0, 10.0), Color::BLUE);
        let container = Node::container(vec![child1, child2]);

        match &container.kind {
            NodeKind::Container { children } => {
                assert_eq!(children.len(), 2);
                // First child is red
                match &children[0].kind {
                    NodeKind::ColorRect { color } => assert_eq!(*color, Color::RED),
                    _ => panic!("Expected ColorRect"),
                }
                // Second child is blue
                match &children[1].kind {
                    NodeKind::ColorRect { color } => assert_eq!(*color, Color::BLUE),
                    _ => panic!("Expected ColorRect"),
                }
            }
            _ => panic!("Expected Container"),
        }
    }

    #[test]
    fn test_container_with_transform() {
        let child = Node::color_rect(Rect::new(0.0, 0.0, 10.0, 10.0), Color::RED);
        let transform = Transform::translate(100.0, 200.0);
        let container = Node::container_with_transform(vec![child], transform);

        assert_eq!(container.transform, Some(transform));
        match &container.kind {
            NodeKind::Container { children } => assert_eq!(children.len(), 1),
            _ => panic!("Expected Container"),
        }
    }

    #[test]
    fn test_nested_containers() {
        let leaf = Node::color_rect(Rect::new(0.0, 0.0, 5.0, 5.0), Color::WHITE);
        let inner = Node::container(vec![leaf]);
        let outer = Node::container(vec![inner]);

        match &outer.kind {
            NodeKind::Container { children } => {
                assert_eq!(children.len(), 1);
                match &children[0].kind {
                    NodeKind::Container {
                        children: inner_children,
                    } => {
                        assert_eq!(inner_children.len(), 1);
                        match &inner_children[0].kind {
                            NodeKind::ColorRect { color } => assert_eq!(*color, Color::WHITE),
                            _ => panic!("Expected ColorRect leaf"),
                        }
                    }
                    _ => panic!("Expected inner Container"),
                }
            }
            _ => panic!("Expected outer Container"),
        }
    }

    // ---------------------------------------------------------------
    // Dirty region tracking and clearing
    // ---------------------------------------------------------------

    #[test]
    fn test_mark_dirty_covers_full_scene() {
        let mut scene = Scene::new(800.0, 600.0);
        scene.mark_dirty();
        let dirty = scene.dirty.unwrap();
        assert_eq!(dirty.x, 0.0);
        assert_eq!(dirty.y, 0.0);
        assert_eq!(dirty.width, 800.0);
        assert_eq!(dirty.height, 600.0);
    }

    #[test]
    fn test_clear_dirty() {
        let mut scene = Scene::new(800.0, 600.0);
        scene.mark_dirty();
        assert!(scene.dirty.is_some());
        scene.clear_dirty();
        assert!(scene.dirty.is_none());
    }

    #[test]
    fn test_mark_region_dirty_first_region() {
        let mut scene = Scene::new(800.0, 600.0);
        assert!(scene.dirty.is_none());
        scene.mark_region_dirty(Rect::new(50.0, 60.0, 70.0, 80.0));
        let dirty = scene.dirty.unwrap();
        assert_eq!(dirty.x, 50.0);
        assert_eq!(dirty.y, 60.0);
        assert_eq!(dirty.width, 70.0);
        assert_eq!(dirty.height, 80.0);
    }

    #[test]
    fn test_mark_region_dirty_disjoint_union() {
        let mut scene = Scene::new(800.0, 600.0);
        scene.mark_region_dirty(Rect::new(0.0, 0.0, 10.0, 10.0));
        scene.mark_region_dirty(Rect::new(200.0, 300.0, 50.0, 50.0));
        let dirty = scene.dirty.unwrap();
        // Union bounding box
        assert_eq!(dirty.x, 0.0);
        assert_eq!(dirty.y, 0.0);
        assert_eq!(dirty.right(), 250.0);
        assert_eq!(dirty.bottom(), 350.0);
    }

    // ---------------------------------------------------------------
    // Window / scene relationship
    // ---------------------------------------------------------------

    /// Helper to create a basic WindowScene
    fn make_window(id: i32, x: f32, y: f32, w: f32, h: f32) -> WindowScene {
        WindowScene {
            window_id: id,
            bounds: Rect::new(x, y, w, h),
            background: Color::BLACK,
            cursor: None,
            scroll_offset: 0.0,
            selected: false,
            mode_line_height: 20,
            header_line_height: 0,
            last_frame_touched: 0,
        }
    }

    #[test]
    fn test_scene_build_with_no_windows() {
        let mut scene = Scene::new(800.0, 600.0);
        scene.background = Color::WHITE;
        scene.build();

        let root = scene.root.as_ref().unwrap();
        match &root.kind {
            NodeKind::Container { children } => {
                // Only the background rect
                assert_eq!(children.len(), 1);
                match &children[0].kind {
                    NodeKind::ColorRect { color } => assert_eq!(*color, Color::WHITE),
                    _ => panic!("Expected background ColorRect"),
                }
            }
            _ => panic!("Expected root Container"),
        }
    }

    #[test]
    fn test_scene_build_with_windows() {
        let mut scene = Scene::new(800.0, 600.0);
        scene.windows.push(make_window(1, 0.0, 0.0, 400.0, 600.0));
        scene.windows.push(make_window(2, 400.0, 0.0, 400.0, 600.0));
        scene.build();

        let root = scene.root.as_ref().unwrap();
        match &root.kind {
            NodeKind::Container { children } => {
                // 1 background + 2 windows = 3 children
                assert_eq!(children.len(), 3);
                // children[1] and children[2] should be window containers with transforms
                for i in 1..=2 {
                    assert!(
                        children[i].transform.is_some(),
                        "Window node must have transform"
                    );
                    assert!(children[i].clip.is_some(), "Window node must have clip");
                }
            }
            _ => panic!("Expected root Container"),
        }
    }

    #[test]
    fn test_scene_build_window_transform_and_clip() {
        let mut scene = Scene::new(800.0, 600.0);
        scene.windows.push(make_window(1, 50.0, 30.0, 300.0, 200.0));
        scene.build();

        let root = scene.root.as_ref().unwrap();
        match &root.kind {
            NodeKind::Container { children } => {
                let win_node = &children[1]; // first window node (after background)
                // Transform should translate to window position
                let transform = win_node.transform.unwrap();
                // translate(50.0, 30.0 - scroll_offset=0.0)
                assert_eq!(transform, Transform::translate(50.0, 30.0));
                // Clip should match window bounds
                let clip = win_node.clip.unwrap();
                assert_eq!(clip, Rect::new(50.0, 30.0, 300.0, 200.0));
            }
            _ => panic!("Expected root Container"),
        }
    }

    #[test]
    fn test_scene_build_window_with_scroll_offset() {
        let mut scene = Scene::new(800.0, 600.0);
        let mut win = make_window(1, 0.0, 0.0, 400.0, 300.0);
        win.scroll_offset = 25.0;
        scene.windows.push(win);
        scene.build();

        let root = scene.root.as_ref().unwrap();
        match &root.kind {
            NodeKind::Container { children } => {
                let transform = children[1].transform.unwrap();
                // translate(0.0, 0.0 - 25.0) = translate(0.0, -25.0)
                assert_eq!(transform, Transform::translate(0.0, -25.0));
            }
            _ => panic!("Expected root Container"),
        }
    }

    // ---------------------------------------------------------------
    // Cursor scene updates
    // ---------------------------------------------------------------

    #[test]
    fn test_scene_build_window_with_visible_cursor() {
        let mut scene = Scene::new(800.0, 600.0);
        let mut win = make_window(1, 0.0, 0.0, 400.0, 300.0);
        win.cursor = Some(CursorState {
            x: 100.0,
            y: 50.0,
            width: 8.0,
            height: 16.0,
            style: SceneCursorStyle::Box,
            color: Color::GREEN,
            visible: true,
        });
        scene.windows.push(win);
        scene.build();

        let root = scene.root.as_ref().unwrap();
        match &root.kind {
            NodeKind::Container { children } => {
                let win_node = &children[1];
                match &win_node.kind {
                    NodeKind::Container {
                        children: win_children,
                    } => {
                        // background rect + cursor = 2
                        assert_eq!(win_children.len(), 2);
                        match &win_children[1].kind {
                            NodeKind::Cursor {
                                style,
                                color,
                                blink_on,
                            } => {
                                assert_eq!(*style, SceneCursorStyle::Box);
                                assert_eq!(*color, Color::GREEN);
                                assert!(*blink_on);
                            }
                            _ => panic!("Expected Cursor node"),
                        }
                        // Cursor bounds
                        assert_eq!(win_children[1].bounds, Rect::new(100.0, 50.0, 8.0, 16.0));
                    }
                    _ => panic!("Expected window Container"),
                }
            }
            _ => panic!("Expected root Container"),
        }
    }

    #[test]
    fn test_scene_build_window_with_invisible_cursor() {
        let mut scene = Scene::new(800.0, 600.0);
        let mut win = make_window(1, 0.0, 0.0, 400.0, 300.0);
        win.cursor = Some(CursorState {
            x: 100.0,
            y: 50.0,
            width: 8.0,
            height: 16.0,
            style: SceneCursorStyle::Bar,
            color: Color::WHITE,
            visible: false,
        });
        scene.windows.push(win);
        scene.build();

        let root = scene.root.as_ref().unwrap();
        match &root.kind {
            NodeKind::Container { children } => {
                let win_node = &children[1];
                match &win_node.kind {
                    NodeKind::Container {
                        children: win_children,
                    } => {
                        // Only background rect, no cursor
                        assert_eq!(win_children.len(), 1);
                    }
                    _ => panic!("Expected window Container"),
                }
            }
            _ => panic!("Expected root Container"),
        }
    }

    #[test]
    fn test_cursor_style_default() {
        assert_eq!(SceneCursorStyle::default(), SceneCursorStyle::Box);
    }

    #[test]
    fn test_all_cursor_styles() {
        let styles = [
            SceneCursorStyle::Box,
            SceneCursorStyle::Bar,
            SceneCursorStyle::Underline,
            SceneCursorStyle::Hollow,
        ];
        let bounds = Rect::new(0.0, 0.0, 8.0, 16.0);
        for style in &styles {
            let node = Node::cursor(*style, Color::WHITE, bounds);
            match &node.kind {
                NodeKind::Cursor { style: s, .. } => assert_eq!(s, style),
                _ => panic!("Expected Cursor"),
            }
        }
    }

    // ---------------------------------------------------------------
    // Layer ordering (borders on top)
    // ---------------------------------------------------------------

    #[test]
    fn test_scene_build_borders_on_top_of_windows() {
        let mut scene = Scene::new(800.0, 600.0);
        scene.windows.push(make_window(1, 0.0, 0.0, 400.0, 600.0));
        scene.add_border(400.0, 0.0, 2.0, 600.0, Color::WHITE);
        scene.build();

        let root = scene.root.as_ref().unwrap();
        match &root.kind {
            NodeKind::Container { children } => {
                // 1 background + 1 window + 1 border = 3
                assert_eq!(children.len(), 3);
                // Last child must be the border
                match &children[2].kind {
                    NodeKind::ColorRect { color } => assert_eq!(*color, Color::WHITE),
                    _ => panic!("Expected border ColorRect as last child"),
                }
                assert_eq!(children[2].bounds, Rect::new(400.0, 0.0, 2.0, 600.0));
            }
            _ => panic!("Expected root Container"),
        }
    }

    #[test]
    fn test_scene_clear_borders() {
        let mut scene = Scene::new(800.0, 600.0);
        scene.add_border(0.0, 0.0, 2.0, 600.0, Color::WHITE);
        scene.add_border(400.0, 0.0, 2.0, 600.0, Color::RED);
        assert_eq!(scene.borders.len(), 2);

        scene.clear_borders();
        assert!(scene.borders.is_empty());
    }

    // ---------------------------------------------------------------
    // Floating video / image / webkit management
    // ---------------------------------------------------------------

    #[test]
    fn test_floating_video_add_remove_clear() {
        let mut scene = Scene::new(800.0, 600.0);
        scene.add_floating_video(1, 10.0, 20.0, 320.0, 240.0);
        scene.add_floating_video(2, 400.0, 20.0, 320.0, 240.0);
        assert_eq!(scene.floating_videos.len(), 2);

        scene.remove_floating_video(1);
        assert_eq!(scene.floating_videos.len(), 1);
        assert_eq!(scene.floating_videos[0].video_id, 2);

        scene.clear_floating_videos();
        assert!(scene.floating_videos.is_empty());
    }

    #[test]
    fn test_floating_image_add_remove_clear() {
        let mut scene = Scene::new(800.0, 600.0);
        scene.add_floating_image(10, 0.0, 0.0, 128.0, 128.0);
        assert_eq!(scene.floating_images.len(), 1);
        assert_eq!(scene.floating_images[0].image_id, 10);

        scene.remove_floating_image(10);
        assert!(scene.floating_images.is_empty());

        scene.add_floating_image(20, 0.0, 0.0, 64.0, 64.0);
        scene.add_floating_image(21, 64.0, 0.0, 64.0, 64.0);
        scene.clear_floating_images();
        assert!(scene.floating_images.is_empty());
    }

    #[test]
    fn test_floating_webkit_add_remove_clear() {
        let mut scene = Scene::new(800.0, 600.0);
        scene.add_floating_webkit(100, 0.0, 0.0, 800.0, 400.0);
        assert_eq!(scene.floating_webkits.len(), 1);
        assert_eq!(scene.floating_webkits[0].webkit_id, 100);

        scene.remove_floating_webkit(100);
        assert!(scene.floating_webkits.is_empty());

        scene.add_floating_webkit(200, 0.0, 0.0, 400.0, 300.0);
        scene.clear_floating_webkits();
        assert!(scene.floating_webkits.is_empty());
    }

    #[test]
    fn test_remove_nonexistent_floating_is_noop() {
        let mut scene = Scene::new(800.0, 600.0);
        scene.add_floating_video(1, 0.0, 0.0, 100.0, 100.0);
        // Removing a non-existent ID should not panic or change existing entries
        scene.remove_floating_video(999);
        assert_eq!(scene.floating_videos.len(), 1);
        scene.remove_floating_image(999);
        scene.remove_floating_webkit(999);
    }

    // ---------------------------------------------------------------
    // Mutations mark dirty
    // ---------------------------------------------------------------

    #[test]
    fn test_mutations_mark_dirty() {
        let mut scene = Scene::new(800.0, 600.0);

        // add_floating_video marks dirty
        scene.add_floating_video(1, 0.0, 0.0, 100.0, 100.0);
        assert!(scene.dirty.is_some());
        scene.clear_dirty();

        // remove_floating_video marks dirty
        scene.remove_floating_video(1);
        assert!(scene.dirty.is_some());
        scene.clear_dirty();

        // add_floating_image marks dirty
        scene.add_floating_image(1, 0.0, 0.0, 100.0, 100.0);
        assert!(scene.dirty.is_some());
        scene.clear_dirty();

        // add_border marks dirty
        scene.add_border(0.0, 0.0, 2.0, 600.0, Color::WHITE);
        assert!(scene.dirty.is_some());
        scene.clear_dirty();

        // clear marks dirty
        scene.clear();
        assert!(scene.dirty.is_some());
    }

    // ---------------------------------------------------------------
    // WindowScene selected field
    // ---------------------------------------------------------------

    #[test]
    fn test_window_selected_flag() {
        let mut win = make_window(1, 0.0, 0.0, 400.0, 300.0);
        assert!(!win.selected);
        win.selected = true;
        assert!(win.selected);
    }
}
