use super::RenderApp;
use crate::core::frame_glyphs::{DisplaySlotId, FrameGlyph, GlyphRowRole};
use crate::thread_comm::InputEvent;

#[cfg(all(feature = "wpe-webkit", wpe_platform_available))]
use crate::backend::wpe::sys::platform as plat;
#[cfg(feature = "wpe-webkit")]
use crate::render_thread::state::WebKitImportPolicy;
#[cfg(all(feature = "wpe-webkit", target_os = "linux"))]
use neomacs_renderer_wgpu::WgpuRenderer;

impl RenderApp {
    #[cfg(all(feature = "wpe-webkit", wpe_platform_available))]
    pub(super) fn pump_glib(&mut self) {
        unsafe {
            // WPEViewHeadless attaches to thread-default context.
            // Do NOT fall back to g_main_context_default() — the Emacs main
            // thread dispatches that via xg_select(), and iterating it here
            // races with pselect() causing EBADF crashes.
            let thread_ctx = plat::g_main_context_get_thread_default();
            if !thread_ctx.is_null() {
                while plat::g_main_context_iteration(thread_ctx, 0) != 0 {}
            }
        }

        // Update all webkit views and send state change events
        for (id, view) in self.webkit_views.iter_mut() {
            let old_title = view.title.clone();
            let old_url = view.url.clone();
            let old_progress = view.progress;

            view.update();

            // Send state change events
            if view.title != old_title {
                if let Some(ref title) = view.title {
                    self.comms.send_input(InputEvent::WebKitTitleChanged {
                        id: *id,
                        title: title.clone(),
                    });
                }
            }
            if view.url != old_url {
                self.comms.send_input(InputEvent::WebKitUrlChanged {
                    id: *id,
                    url: view.url.clone(),
                });
            }
            if (view.progress - old_progress).abs() > 0.01 {
                self.comms.send_input(InputEvent::WebKitProgressChanged {
                    id: *id,
                    progress: view.progress,
                });
            }
        }
    }

    #[cfg(not(all(feature = "wpe-webkit", wpe_platform_available)))]
    pub(super) fn pump_glib(&mut self) {}

    /// Process webkit frames and import to wgpu textures
    #[cfg(all(feature = "wpe-webkit", target_os = "linux"))]
    pub(super) fn process_webkit_frames(&mut self) {
        use crate::backend::wpe::DmaBufData;
        use neomacs_renderer_wgpu::DmaBufBuffer;

        // Get mutable reference to renderer - we need to update its internal webkit cache
        let renderer = match &mut self.renderer {
            Some(r) => r,
            None => {
                tracing::trace!("process_webkit_frames: no renderer available");
                return;
            }
        };

        if self.webkit_views.is_empty() {
            tracing::trace!("process_webkit_frames: no webkit views");
            return;
        }

        let policy = self.webkit_import_policy.effective();

        let try_upload_dmabuf =
            |renderer: &mut WgpuRenderer, view_id: u32, dmabuf: DmaBufData| -> bool {
                let num_planes = dmabuf.fds.len().min(4) as u32;
                let mut fds = [-1i32; 4];
                let mut strides = [0u32; 4];
                let mut offsets = [0u32; 4];

                for i in 0..num_planes as usize {
                    fds[i] = dmabuf.fds[i];
                    strides[i] = dmabuf.strides[i];
                    offsets[i] = dmabuf.offsets[i];
                }

                let buffer = DmaBufBuffer::new(
                    fds,
                    strides,
                    offsets,
                    num_planes,
                    dmabuf.width,
                    dmabuf.height,
                    dmabuf.fourcc,
                    dmabuf.modifier,
                );

                renderer.update_webkit_view_dmabuf(view_id, buffer)
            };

        for (view_id, view) in &self.webkit_views {
            match policy {
                WebKitImportPolicy::DmaBufFirst => {
                    if let Some(dmabuf) = view.take_latest_dmabuf() {
                        if try_upload_dmabuf(renderer, *view_id, dmabuf) {
                            // Discard pending pixel fallback when DMA-BUF succeeds.
                            let _ = view.take_latest_pixels();
                            tracing::debug!(
                                "Imported DMA-BUF for webkit view {} (dmabuf-first)",
                                view_id
                            );
                        } else if let Some(raw_pixels) = view.take_latest_pixels() {
                            if renderer.update_webkit_view_pixels(
                                *view_id,
                                raw_pixels.width,
                                raw_pixels.height,
                                &raw_pixels.pixels,
                            ) {
                                tracing::debug!(
                                    "Uploaded pixels for webkit view {} (dmabuf-first fallback)",
                                    view_id
                                );
                            } else {
                                tracing::warn!(
                                    "Both DMA-BUF and pixel upload failed for webkit view {}",
                                    view_id
                                );
                            }
                        } else {
                            tracing::warn!(
                                "Both DMA-BUF import and pixel fallback unavailable for webkit view {}",
                                view_id
                            );
                        }
                    } else if let Some(raw_pixels) = view.take_latest_pixels() {
                        if renderer.update_webkit_view_pixels(
                            *view_id,
                            raw_pixels.width,
                            raw_pixels.height,
                            &raw_pixels.pixels,
                        ) {
                            tracing::debug!(
                                "Uploaded pixels for webkit view {} (dmabuf-first: no dmabuf frame)",
                                view_id
                            );
                        }
                    }
                }
                WebKitImportPolicy::PixelsFirst | WebKitImportPolicy::Auto => {
                    // Prefer pixel upload over DMA-BUF zero-copy.
                    //
                    // wgpu's create_texture_from_hal() always inserts textures with
                    // UNINITIALIZED tracking state, causing a second UNDEFINED layout
                    // transition that discards DMA-BUF content on AMD RADV (and
                    // potentially other drivers with compressed modifiers like DCC/CCS).
                    // Until wgpu supports pre-initialized HAL textures, pixel upload
                    // via wpe_buffer_import_to_pixels() is the reliable path.
                    if let Some(raw_pixels) = view.take_latest_pixels() {
                        // Drain any pending DMA-BUF so it doesn't accumulate
                        let _ = view.take_latest_dmabuf();
                        if renderer.update_webkit_view_pixels(
                            *view_id,
                            raw_pixels.width,
                            raw_pixels.height,
                            &raw_pixels.pixels,
                        ) {
                            tracing::debug!("Uploaded pixels for webkit view {}", view_id);
                        }
                    }
                    // DMA-BUF zero-copy fallback (only if no pixel data available)
                    else if let Some(dmabuf) = view.take_latest_dmabuf() {
                        if try_upload_dmabuf(renderer, *view_id, dmabuf) {
                            tracing::debug!(
                                "Imported DMA-BUF for webkit view {} (pixels-first fallback)",
                                view_id
                            );
                        } else if let Some(raw_pixels) = view.take_latest_pixels() {
                            if renderer.update_webkit_view_pixels(
                                *view_id,
                                raw_pixels.width,
                                raw_pixels.height,
                                &raw_pixels.pixels,
                            ) {
                                tracing::debug!(
                                    "Uploaded pixels for webkit view {} (pixels-first second fallback)",
                                    view_id
                                );
                            } else {
                                tracing::warn!(
                                    "Both pixel and DMA-BUF import failed for webkit view {}",
                                    view_id
                                );
                            }
                        } else {
                            tracing::warn!(
                                "Both pixel and DMA-BUF import failed for webkit view {}",
                                view_id
                            );
                        }
                    }
                }
            }
        }
    }

    #[cfg(not(all(feature = "wpe-webkit", target_os = "linux")))]
    pub(super) fn process_webkit_frames(&mut self) {}

    /// Process pending video frames
    #[cfg(feature = "video")]
    pub(super) fn process_video_frames(&mut self) {
        tracing::trace!("process_video_frames called");
        if let Some(ref mut renderer) = self.renderer {
            renderer.process_pending_videos();
        }
    }

    #[cfg(not(feature = "video"))]
    pub(super) fn process_video_frames(&mut self) {}

    /// Check if any video is currently playing (needs continuous rendering)
    #[cfg(feature = "video")]
    pub(super) fn has_playing_videos(&self) -> bool {
        self.renderer
            .as_ref()
            .map_or(false, |r| r.has_playing_videos())
    }

    #[cfg(not(feature = "video"))]
    pub(super) fn has_playing_videos(&self) -> bool {
        false
    }

    /// Check if any WebKit view needs redraw
    #[cfg(feature = "wpe-webkit")]
    pub(super) fn has_webkit_needing_redraw(&self) -> bool {
        self.webkit_views.values().any(|v| v.needs_redraw())
    }

    #[cfg(not(feature = "wpe-webkit"))]
    pub(super) fn has_webkit_needing_redraw(&self) -> bool {
        false
    }

    /// Check if any terminal has pending content from PTY reader threads.
    #[cfg(feature = "neo-term")]
    pub(super) fn has_terminal_activity(&self) -> bool {
        for view in self.terminal_manager.terminals.values() {
            if view.event_proxy.peek_wakeup() || view.dirty {
                return true;
            }
        }
        false
    }

    #[cfg(not(feature = "neo-term"))]
    pub(super) fn has_terminal_activity(&self) -> bool {
        false
    }

    /// Process pending image uploads (decode → GPU texture)
    pub(super) fn process_pending_images(&mut self) {
        if let Some(ref mut renderer) = self.renderer {
            renderer.process_pending_images();
        }
    }

    /// Update terminal content and expand Terminal glyphs into renderable cells.
    #[cfg(feature = "neo-term")]
    pub(super) fn update_terminals(&mut self) {
        use crate::terminal::TerminalMode;

        // Get frame font metrics for terminal cell sizing.
        // These come from FRAME_COLUMN_WIDTH / FRAME_LINE_HEIGHT / FRAME_FONT->pixel_size.
        let (cell_w, cell_h, font_size, frame_w, frame_h) =
            if let Some(ref frame) = self.current_frame {
                (
                    frame.char_width,
                    frame.char_height,
                    frame.font_pixel_size,
                    frame.width,
                    frame.height,
                )
            } else {
                (8.0, 16.0, 14.0, self.width as f32, self.height as f32)
            };
        let ascent = cell_h * 0.8;

        // Auto-resize Window-mode terminals to fit the frame area.
        // Reserve space for mode-line (~1 row) and echo area (~1 row).
        let term_area_height = (frame_h - cell_h * 2.0).max(cell_h);
        let target_cols = (frame_w / cell_w).floor() as u16;
        let target_rows = (term_area_height / cell_h).floor() as u16;

        if target_cols > 0 && target_rows > 0 {
            for id in self.terminal_manager.ids() {
                if let Some(view) = self.terminal_manager.get_mut(id) {
                    if view.mode != TerminalMode::Window {
                        continue;
                    }
                    // Resize if grid dimensions changed
                    if let Some(content) = view.content() {
                        if content.cols as u16 != target_cols || content.rows as u16 != target_rows
                        {
                            view.resize(target_cols, target_rows);
                        }
                    }
                }
            }
        }

        // Update all terminal content (check for PTY data)
        self.terminal_manager.update_all();

        // Check for exited terminals and notify Emacs
        for id in self.terminal_manager.ids() {
            if let Some(view) = self.terminal_manager.get_mut(id) {
                if view.event_proxy.is_exited() && !view.exit_notified {
                    view.exit_notified = true;
                    self.comms.send_input(InputEvent::TerminalExited { id });
                }
            }
        }

        // Expand FrameGlyph::Terminal entries (placed by C redisplay) into cells
        if let Some(ref mut frame) = self.current_frame {
            let mut extra_glyphs = Vec::new();

            for glyph in &frame.glyphs {
                if let FrameGlyph::Terminal {
                    terminal_id,
                    x,
                    y,
                    width,
                    height,
                } = glyph
                {
                    if let Some(view) = self.terminal_manager.get(*terminal_id) {
                        if let Some(content) = view.content() {
                            extra_glyphs.push(FrameGlyph::Stretch {
                                window_id: 0,
                                row_role: GlyphRowRole::Text,
                                clip_rect: None,
                                slot_id: DisplaySlotId::from_pixels(0, *x, *y, cell_w, cell_h),
                                x: *x,
                                y: *y,
                                width: *width,
                                height: *height,
                                bg: content.default_bg,
                                face_id: 0,
                                stipple_id: 0,
                                stipple_fg: None,
                            });

                            Self::expand_terminal_cells(
                                content,
                                *x,
                                *y,
                                cell_w,
                                cell_h,
                                ascent,
                                font_size,
                                false,
                                1.0,
                                &mut extra_glyphs,
                            );
                        }
                    }
                }
            }

            if !extra_glyphs.is_empty() {
                frame.glyphs.extend(extra_glyphs);
                self.frame_dirty = true;
            }
        }

        // Render Window-mode terminals as overlays covering the frame body.
        if let Some(ref mut frame) = self.current_frame {
            let mut win_glyphs = Vec::new();
            for id in self.terminal_manager.ids() {
                if let Some(view) = self.terminal_manager.get(id) {
                    if view.mode != TerminalMode::Window {
                        continue;
                    }
                    if let Some(content) = view.content() {
                        let x = 0.0_f32;
                        let y = 0.0_f32;
                        let width = content.cols as f32 * cell_w;
                        let height = content.rows as f32 * cell_h;

                        // Terminal background
                        win_glyphs.push(FrameGlyph::Stretch {
                            window_id: 0,
                            row_role: GlyphRowRole::ModeLine,
                            clip_rect: None,
                            slot_id: DisplaySlotId::from_pixels(0, x, y, cell_w, cell_h),
                            x,
                            y,
                            width,
                            height,
                            bg: content.default_bg,
                            face_id: 0,
                            stipple_id: 0,
                            stipple_fg: None,
                        });

                        Self::expand_terminal_cells(
                            content,
                            x,
                            y,
                            cell_w,
                            cell_h,
                            ascent,
                            font_size,
                            true,
                            1.0,
                            &mut win_glyphs,
                        );
                    }
                }
            }

            if !win_glyphs.is_empty() {
                frame.glyphs.extend(win_glyphs);
                self.frame_dirty = true;
            }
        }

        // Render floating terminals
        if let Some(ref mut frame) = self.current_frame {
            let mut float_glyphs = Vec::new();
            for id in self.terminal_manager.ids() {
                if let Some(view) = self.terminal_manager.get(id) {
                    if view.mode != TerminalMode::Floating {
                        continue;
                    }
                    if let Some(content) = view.content() {
                        let x = view.float_x;
                        let y = view.float_y;
                        let width = content.cols as f32 * cell_w;
                        let height = content.rows as f32 * cell_h;

                        let mut bg = content.default_bg;
                        bg.a = view.float_opacity;
                        float_glyphs.push(FrameGlyph::Stretch {
                            window_id: 0,
                            row_role: GlyphRowRole::ModeLine,
                            clip_rect: None,
                            slot_id: DisplaySlotId::from_pixels(0, x, y, cell_w, cell_h),
                            x,
                            y,
                            width,
                            height,
                            bg,
                            face_id: 0,
                            stipple_id: 0,
                            stipple_fg: None,
                        });

                        Self::expand_terminal_cells(
                            content,
                            x,
                            y,
                            cell_w,
                            cell_h,
                            ascent,
                            font_size,
                            true,
                            view.float_opacity,
                            &mut float_glyphs,
                        );
                    }
                }
            }

            if !float_glyphs.is_empty() {
                frame.glyphs.extend(float_glyphs);
                self.frame_dirty = true;
            }
        }
    }

    /// Expand terminal content cells into FrameGlyph entries.
    #[cfg(feature = "neo-term")]
    fn expand_terminal_cells(
        content: &crate::terminal::content::TerminalContent,
        origin_x: f32,
        origin_y: f32,
        cell_w: f32,
        cell_h: f32,
        ascent: f32,
        font_size: f32,
        is_overlay: bool,
        opacity: f32,
        out: &mut Vec<FrameGlyph>,
    ) {
        use alacritty_terminal::term::cell::Flags as CellFlags;
        let row_role = if is_overlay {
            GlyphRowRole::ModeLine
        } else {
            GlyphRowRole::Text
        };

        for cell in &content.cells {
            let cx = origin_x + cell.col as f32 * cell_w;
            let cy = origin_y + cell.row as f32 * cell_h;

            if cell.bg != content.default_bg {
                let mut bg = cell.bg;
                bg.a *= opacity;
                out.push(FrameGlyph::Stretch {
                    window_id: 0,
                    row_role,
                    clip_rect: None,
                    slot_id: DisplaySlotId::from_pixels(0, cx, cy, cell_w, cell_h),
                    x: cx,
                    y: cy,
                    width: cell_w,
                    height: cell_h,
                    bg,
                    face_id: 0,
                    stipple_id: 0,
                    stipple_fg: None,
                });
            }

            if cell.c != ' ' && cell.c != '\0' {
                let mut fg = cell.fg;
                fg.a *= opacity;
                out.push(FrameGlyph::Char {
                    window_id: 0,
                    row_role,
                    clip_rect: None,
                    slot_id: DisplaySlotId::from_pixels(0, cx, cy, cell_w, cell_h),
                    char: cell.c,
                    composed: None,
                    x: cx,
                    y: cy,
                    baseline: cy + ascent,
                    width: cell_w,
                    height: cell_h,
                    ascent,
                    fg,
                    bg: None,
                    face_id: 0,
                    font_weight: if cell.flags.contains(CellFlags::BOLD) {
                        700
                    } else {
                        400
                    },
                    italic: cell.flags.contains(CellFlags::ITALIC),
                    font_size,
                    underline: if cell.flags.contains(CellFlags::UNDERLINE) {
                        1
                    } else {
                        0
                    },
                    underline_color: None,
                    strike_through: if cell.flags.contains(CellFlags::STRIKEOUT) {
                        1
                    } else {
                        0
                    },
                    strike_through_color: None,
                    overline: 0,
                    overline_color: None,
                    overstrike: false,
                });
            }
        }

        // Terminal cursor
        if content.cursor.visible {
            let cx = origin_x + content.cursor.col as f32 * cell_w;
            let cy = origin_y + content.cursor.row as f32 * cell_h;
            let mut fg = content.default_fg;
            fg.a *= opacity;
            out.push(FrameGlyph::Border {
                window_id: 0,
                row_role,
                clip_rect: None,
                x: cx,
                y: cy,
                width: cell_w,
                height: cell_h,
                color: fg,
            });
        }
    }
}
