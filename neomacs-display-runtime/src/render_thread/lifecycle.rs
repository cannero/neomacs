use super::RenderApp;
use super::state::{effective_window_scale_factor, window_size_from_emacs_pixels};
use crate::thread_comm::InputEvent;
use std::sync::Arc;
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::Window;

impl RenderApp {
    fn collect_monitor_snapshot(
        event_loop: &ActiveEventLoop,
    ) -> Vec<crate::thread_comm::MonitorInfo> {
        let mut monitors = Vec::new();
        for monitor in event_loop.available_monitors() {
            let pos = monitor.position();
            let size = monitor.size();
            let scale = monitor.scale_factor();
            let name = monitor.name();
            let width_mm = if scale > 0.0 {
                (size.width as f64 * 25.4 / (96.0 * scale)) as i32
            } else {
                0
            };
            let height_mm = if scale > 0.0 {
                (size.height as f64 * 25.4 / (96.0 * scale)) as i32
            } else {
                0
            };
            monitors.push(crate::thread_comm::MonitorInfo {
                x: pos.x,
                y: pos.y,
                width: size.width as i32,
                height: size.height as i32,
                scale,
                width_mm,
                height_mm,
                name,
            });
        }
        monitors
    }

    fn refresh_monitor_snapshot(&mut self, event_loop: &ActiveEventLoop, emit_change_event: bool) {
        let snapshot = Self::collect_monitor_snapshot(event_loop);
        let had_snapshot = self.monitors_populated;
        let changed = !had_snapshot || self.last_monitor_snapshot != snapshot;

        if !changed {
            return;
        }

        self.last_monitor_snapshot = snapshot.clone();
        self.monitors_populated = true;

        for monitor in &snapshot {
            tracing::info!(
                "Monitor: {:?} pos=({},{}) size={}x{} scale={} mm={}x{}",
                monitor.name,
                monitor.x,
                monitor.y,
                monitor.width,
                monitor.height,
                monitor.scale,
                monitor.width_mm,
                monitor.height_mm
            );
        }

        if let Some(ref shared) = self.shared_monitors {
            let (ref lock, ref cvar) = **shared;
            if let Ok(mut shared) = lock.lock() {
                *shared = snapshot.clone();
                cvar.notify_all();
            }
        }

        if emit_change_event && had_snapshot {
            self.comms
                .send_input(InputEvent::MonitorsChanged { monitors: snapshot });
        }
    }

    pub(super) fn handle_resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.resumed_seen {
            tracing::info!(
                "Render thread resumed: primary_window_exists={} size={}x{} title={:?}",
                self.window.is_some(),
                self.width,
                self.height,
                self.title
            );
            self.resumed_seen = true;
        }
        if self.window.is_none() {
            let attrs = Window::default_attributes()
                .with_title(&self.title)
                .with_inner_size(window_size_from_emacs_pixels(self.width, self.height))
                .with_transparent(true);

            tracing::info!(
                "Render thread creating primary window: emacs_pixels={}x{} title={:?}",
                self.width,
                self.height,
                self.title
            );
            match event_loop.create_window(attrs) {
                Ok(window) => {
                    let window = Arc::new(window);

                    let raw_scale_factor = window.scale_factor();
                    self.scale_factor = effective_window_scale_factor(raw_scale_factor);
                    tracing::info!(
                        "Display scale factor: raw={} effective={}",
                        raw_scale_factor,
                        self.scale_factor
                    );

                    let phys = window.inner_size();
                    self.width = phys.width;
                    self.height = phys.height;
                    tracing::info!(
                        "Render thread: window created (physical {}x{})",
                        self.width,
                        self.height
                    );

                    // Initialize wgpu with the window
                    self.init_wgpu(window.clone());

                    // Enable IME input for CJK and compose support
                    window.set_ime_allowed(true);

                    // Set window icon from project SVG.
                    crate::window_icon::apply_window_icon(&window);

                    self.window = Some(window);
                }
                Err(e) => {
                    tracing::error!("Failed to create window: {:?}", e);
                }
            }
        }

        self.refresh_monitor_snapshot(event_loop, false);
    }

    pub(super) fn handle_about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if !self.about_to_wait_seen {
            tracing::info!(
                "Render thread entered about_to_wait: primary_window_exists={} multi_windows={}",
                self.window.is_some(),
                self.multi_windows.count()
            );
            self.about_to_wait_seen = true;
        }
        self.refresh_monitor_snapshot(event_loop, true);
        // Check for shutdown
        if self.process_commands() {
            event_loop.exit();
            return;
        }

        // Process multi-window creates/destroys
        if let (Some(device), Some(adapter)) = (&self.device, &self.adapter) {
            self.multi_windows
                .process_creates(event_loop, device, adapter);
        }
        self.multi_windows.process_destroys();

        // Get latest frame from Emacs
        self.poll_frame();

        // Pump GLib for WebKit
        self.pump_glib();

        // Update cursor blink state
        if self.tick_cursor_blink() {
            self.frame_dirty = true;
        }

        // Tick cursor animation
        if self.cursor.tick_animation() {
            self.frame_dirty = true;
        }

        // Tick cursor size transition (runs after position animation, overrides w/h)
        if self.cursor.tick_size_animation() {
            self.frame_dirty = true;
        }

        // Tick idle dimming
        if self.effects.idle_dim.enabled {
            let idle_time = self.last_activity_time.elapsed();
            let target_alpha = if idle_time >= self.effects.idle_dim.delay {
                self.effects.idle_dim.opacity
            } else {
                0.0
            };
            let diff = target_alpha - self.idle_dim_current_alpha;
            if diff.abs() > 0.001 {
                let fade_speed = if self.effects.idle_dim.fade_duration.as_secs_f32() > 0.0 {
                    1.0 / self.effects.idle_dim.fade_duration.as_secs_f32() * 0.016
                } else {
                    1.0
                };
                if diff > 0.0 {
                    self.idle_dim_current_alpha = (self.idle_dim_current_alpha
                        + fade_speed * self.effects.idle_dim.opacity)
                        .min(target_alpha);
                } else {
                    self.idle_dim_current_alpha = (self.idle_dim_current_alpha
                        - fade_speed * self.effects.idle_dim.opacity)
                        .max(0.0);
                }
                self.idle_dim_active = true;
                self.frame_dirty = true;
            } else if self.idle_dim_current_alpha > 0.001 {
                self.idle_dim_active = true;
                self.frame_dirty = true;
            } else {
                self.idle_dim_active = false;
            }
        }

        // Keep dirty if cursor pulse is active (needs continuous redraw)
        if self.effects.cursor_pulse.enabled && self.effects.cursor_glow.enabled {
            self.frame_dirty = true;
        }

        // Keep dirty if renderer signals need for continuous redraws (dim fade, animated borders)
        if let Some(ref renderer) = self.renderer {
            if renderer.needs_continuous_redraw || renderer.has_animated_borders {
                self.frame_dirty = true;
            }
        }

        // Keep dirty if transitions are active
        if self.transitions.has_active() {
            self.frame_dirty = true;
        }

        // Check for terminal PTY activity
        if self.has_terminal_activity() {
            self.frame_dirty = true;
        }

        // Determine if continuous rendering is needed
        let has_active_content = self.has_webkit_needing_redraw() || self.has_playing_videos();

        // Request redraw when we have new frame data, cursor blink toggled,
        // or webkit/video content changed
        if self.frame_dirty || has_active_content {
            if let Some(ref window) = self.window {
                window.request_redraw();
            }
        }

        // Use WaitUntil with smart timeouts instead of Poll to save CPU.
        // Window events (key, mouse, resize) still wake immediately.
        let now = std::time::Instant::now();
        let next_wake = if self.frame_dirty
            || has_active_content
            || self.cursor.animating
            || self.cursor.size_animating
            || self.idle_dim_active
            || self.transitions.has_active()
        {
            // Active rendering: cap at ~240fps to avoid spinning
            now + std::time::Duration::from_millis(4)
        } else if self.cursor.blink_enabled {
            // Idle with cursor blink: wake at next toggle time
            self.cursor.last_blink_toggle + self.cursor.blink_interval
        } else {
            // Fully idle: poll for new Emacs frames at 60fps
            now + std::time::Duration::from_millis(16)
        };
        event_loop.set_control_flow(ControlFlow::WaitUntil(next_wake));
    }
    pub(super) fn handle_exiting(&mut self) {
        // Explicitly drop wgpu resources while the Wayland connection is still alive.
        // Without this, RenderApp's implicit drop happens AFTER the event loop's
        // Wayland display is torn down, causing SEGV in eglTerminate → dri2_teardown_wayland.
        //
        // wgpu uses internal Arc reference counting: the Adapter holds Arc<Instance>,
        // and Device/Surface/Texture objects hold indirect Arc references back to it.
        // Even after .take()'ing all Option fields, other RenderApp fields (transition
        // textures, child frames, etc.) may still hold transitive Arc references that
        // keep the EGL Instance alive until the final implicit drop of RenderApp —
        // at which point the Wayland connection is already torn down.
        //
        // Solution: leak the adapter to prevent eglTerminate from ever running.
        // The OS reclaims all GPU resources on process exit anyway.
        tracing::info!("Event loop exiting, cleaning up GPU resources");

        // Drop WebKit views and WPE backend (hold EGL contexts)
        #[cfg(feature = "wpe-webkit")]
        {
            self.webkit_views.clear();
            self.wpe_backend = None;
        }
        // Drop renderer (holds device/queue references, textures, pipelines)
        drop(self.renderer.take());
        // Drop glyph atlas (holds device reference)
        drop(self.glyph_atlas.take());
        // Drop surface (holds wl_surface proxy if on Wayland)
        drop(self.surface.take());
        self.surface_config = None;
        // Drop device and queue
        drop(self.device.take());
        drop(self.queue.take());
        // Drop multi-window state (secondary surfaces)
        self.multi_windows.destroy_all();
        // Leak the adapter to prevent eglTerminate crash on Wayland.
        // The adapter's Drop triggers eglTerminate → dri2_teardown_wayland which
        // SEGVs if the Wayland connection is already gone. Since we're exiting,
        // the OS will reclaim all GPU/EGL resources.
        if let Some(adapter) = self.adapter.take() {
            std::mem::forget(adapter);
        }

        tracing::info!("GPU resources cleaned up");
    }
}
