//! Asset and embedded-content render commands.

use super::RenderApp;
use crate::thread_comm::{InputEvent, RenderCommand};

#[cfg(feature = "wpe-webkit")]
use crate::backend::wpe::WpeWebView;

impl RenderApp {
    pub(super) fn handle_asset_command(&mut self, cmd: RenderCommand) -> Result<(), RenderCommand> {
        match cmd {
            RenderCommand::ImageLoadFile {
                id,
                path,
                max_width,
                max_height,
                fg_color,
                bg_color,
            } => {
                tracing::info!(
                    "Loading image {}: {} (max {}x{})",
                    id,
                    path,
                    max_width,
                    max_height
                );
                if let Some(ref mut renderer) = self.renderer {
                    renderer.load_image_file_with_id(
                        id, &path, max_width, max_height, fg_color, bg_color,
                    );
                    if let Some((w, h)) = renderer.get_image_size(id) {
                        let (lock, cvar) = &*self.image_dimensions;
                        if let Ok(mut dims) = lock.lock() {
                            dims.insert(id, (w, h));
                            cvar.notify_all();
                        }
                        self.comms.send_input(InputEvent::ImageDimensionsReady {
                            id,
                            width: w,
                            height: h,
                        });
                        tracing::debug!("Sent ImageDimensionsReady for image {}: {}x{}", id, w, h);
                    }
                } else {
                    tracing::warn!("Renderer not initialized, cannot load image {}", id);
                }
                Ok(())
            }
            RenderCommand::ImageLoadData {
                id,
                data,
                max_width,
                max_height,
                fg_color,
                bg_color,
            } => {
                tracing::info!(
                    "Loading image data {}: {} bytes (max {}x{})",
                    id,
                    data.len(),
                    max_width,
                    max_height
                );
                if let Some(ref mut renderer) = self.renderer {
                    renderer.load_image_data_with_id(
                        id, &data, max_width, max_height, fg_color, bg_color,
                    );
                    if let Some((w, h)) = renderer.get_image_size(id) {
                        let (lock, cvar) = &*self.image_dimensions;
                        if let Ok(mut dims) = lock.lock() {
                            dims.insert(id, (w, h));
                            cvar.notify_all();
                        }
                        self.comms.send_input(InputEvent::ImageDimensionsReady {
                            id,
                            width: w,
                            height: h,
                        });
                        tracing::debug!(
                            "Sent ImageDimensionsReady for image data {}: {}x{}",
                            id,
                            w,
                            h
                        );
                    }
                } else {
                    tracing::warn!("Renderer not initialized, cannot load image data {}", id);
                }
                Ok(())
            }
            RenderCommand::ImageLoadArgb32 {
                id,
                data,
                width,
                height,
                stride,
            } => {
                tracing::debug!(
                    "Loading ARGB32 image {}: {}x{} stride={}",
                    id,
                    width,
                    height,
                    stride
                );
                if let Some(ref mut renderer) = self.renderer {
                    renderer.load_image_argb32_with_id(id, &data, width, height, stride);
                    if let Some((w, h)) = renderer.get_image_size(id) {
                        let (lock, cvar) = &*self.image_dimensions;
                        if let Ok(mut dims) = lock.lock() {
                            dims.insert(id, (w, h));
                            cvar.notify_all();
                        }
                    }
                }
                Ok(())
            }
            RenderCommand::ImageLoadRgb24 {
                id,
                data,
                width,
                height,
                stride,
            } => {
                tracing::debug!(
                    "Loading RGB24 image {}: {}x{} stride={}",
                    id,
                    width,
                    height,
                    stride
                );
                if let Some(ref mut renderer) = self.renderer {
                    renderer.load_image_rgb24_with_id(id, &data, width, height, stride);
                    if let Some((w, h)) = renderer.get_image_size(id) {
                        let (lock, cvar) = &*self.image_dimensions;
                        if let Ok(mut dims) = lock.lock() {
                            dims.insert(id, (w, h));
                            cvar.notify_all();
                        }
                    }
                }
                Ok(())
            }
            RenderCommand::ImageFree { id } => {
                tracing::debug!("Freeing image {}", id);
                if let Some(ref mut renderer) = self.renderer {
                    renderer.free_image(id);
                }
                Ok(())
            }
            RenderCommand::WebKitCreate { id, width, height } => {
                tracing::info!("Creating WebKit view: id={}, {}x{}", id, width, height);
                #[cfg(feature = "wpe-webkit")]
                if let Some(ref backend) = self.wpe_backend {
                    if let Some(platform_display) = backend.platform_display() {
                        match WpeWebView::new(id, platform_display, width, height) {
                            Ok(view) => {
                                self.webkit_views.insert(id, view);
                                tracing::info!("WebKit view {} created successfully", id);
                            }
                            Err(e) => {
                                tracing::error!("Failed to create WebKit view {}: {:?}", id, e)
                            }
                        }
                    } else {
                        tracing::error!("WPE platform display not available");
                    }
                } else {
                    tracing::warn!("WPE backend not initialized, cannot create WebKit view");
                }
                Ok(())
            }
            RenderCommand::WebKitLoadUri { id, url } => {
                tracing::info!("Loading URL in WebKit view {}: {}", id, url);
                #[cfg(feature = "wpe-webkit")]
                if let Some(view) = self.webkit_views.get_mut(&id) {
                    if let Err(e) = view.load_uri(&url) {
                        tracing::error!("Failed to load URL in view {}: {:?}", id, e);
                    }
                } else {
                    tracing::warn!("WebKit view {} not found", id);
                }
                Ok(())
            }
            RenderCommand::WebKitResize { id, width, height } => {
                tracing::debug!("Resizing WebKit view {}: {}x{}", id, width, height);
                #[cfg(feature = "wpe-webkit")]
                if let Some(view) = self.webkit_views.get_mut(&id) {
                    view.resize(width, height);
                }
                Ok(())
            }
            RenderCommand::WebKitDestroy { id } => {
                tracing::info!("Destroying WebKit view {}", id);
                #[cfg(feature = "wpe-webkit")]
                {
                    self.webkit_views.remove(&id);
                    if let Some(ref mut renderer) = self.renderer {
                        renderer.remove_webkit_view(id);
                    }
                }
                Ok(())
            }
            RenderCommand::WebKitClick { id, x, y, button } => {
                tracing::debug!(
                    "WebKit click view {} at ({}, {}), button {}",
                    id,
                    x,
                    y,
                    button
                );
                #[cfg(feature = "wpe-webkit")]
                if let Some(view) = self.webkit_views.get(&id) {
                    view.click(x, y, button);
                }
                Ok(())
            }
            RenderCommand::WebKitPointerEvent {
                id,
                event_type,
                x,
                y,
                button,
                state,
                modifiers,
            } => {
                tracing::trace!(
                    "WebKit pointer event view {} type {} at ({}, {})",
                    id,
                    event_type,
                    x,
                    y
                );
                #[cfg(feature = "wpe-webkit")]
                if let Some(view) = self.webkit_views.get(&id) {
                    view.send_pointer_event(event_type, x, y, button, state, modifiers);
                }
                Ok(())
            }
            RenderCommand::WebKitScroll {
                id,
                x,
                y,
                delta_x,
                delta_y,
            } => {
                tracing::debug!(
                    "WebKit scroll view {} at ({}, {}), delta ({}, {})",
                    id,
                    x,
                    y,
                    delta_x,
                    delta_y
                );
                #[cfg(feature = "wpe-webkit")]
                if let Some(view) = self.webkit_views.get(&id) {
                    view.scroll(x, y, delta_x, delta_y);
                }
                Ok(())
            }
            RenderCommand::WebKitKeyEvent {
                id,
                keyval,
                keycode,
                pressed,
                modifiers,
            } => {
                tracing::debug!(
                    "WebKit key event view {} keyval {} pressed {}",
                    id,
                    keyval,
                    pressed
                );
                #[cfg(feature = "wpe-webkit")]
                if let Some(view) = self.webkit_views.get(&id) {
                    view.send_keyboard_event(keyval, keycode, pressed, modifiers);
                }
                Ok(())
            }
            RenderCommand::WebKitGoBack { id } => {
                tracing::info!("WebKit go back view {}", id);
                #[cfg(feature = "wpe-webkit")]
                if let Some(view) = self.webkit_views.get_mut(&id) {
                    let _ = view.go_back();
                }
                Ok(())
            }
            RenderCommand::WebKitGoForward { id } => {
                tracing::info!("WebKit go forward view {}", id);
                #[cfg(feature = "wpe-webkit")]
                if let Some(view) = self.webkit_views.get_mut(&id) {
                    let _ = view.go_forward();
                }
                Ok(())
            }
            RenderCommand::WebKitReload { id } => {
                tracing::info!("WebKit reload view {}", id);
                #[cfg(feature = "wpe-webkit")]
                if let Some(view) = self.webkit_views.get_mut(&id) {
                    let _ = view.reload();
                }
                Ok(())
            }
            RenderCommand::WebKitExecuteJavaScript { id, script } => {
                tracing::debug!("WebKit execute JS view {}", id);
                #[cfg(feature = "wpe-webkit")]
                if let Some(view) = self.webkit_views.get(&id) {
                    let _ = view.execute_javascript(&script);
                }
                Ok(())
            }
            RenderCommand::WebKitSetFloating {
                id,
                x,
                y,
                width,
                height,
            } => {
                tracing::info!(
                    "WebKit set floating: id={} at ({},{}) {}x{}",
                    id,
                    x,
                    y,
                    width,
                    height
                );
                #[cfg(feature = "wpe-webkit")]
                {
                    self.floating_webkits.retain(|w| w.webkit_id != id);
                    self.floating_webkits
                        .push(crate::core::scene::FloatingWebKit {
                            webkit_id: id,
                            x,
                            y,
                            width,
                            height,
                        });
                    self.frame_dirty = true;
                }
                Ok(())
            }
            RenderCommand::WebKitRemoveFloating { id } => {
                tracing::info!("WebKit remove floating: id={}", id);
                #[cfg(feature = "wpe-webkit")]
                {
                    self.floating_webkits.retain(|w| w.webkit_id != id);
                    self.frame_dirty = true;
                }
                Ok(())
            }
            RenderCommand::VideoCreate { id, path } => {
                tracing::info!("Loading video {}: {}", id, path);
                #[cfg(feature = "video")]
                if let Some(ref mut renderer) = self.renderer {
                    let video_id = renderer.load_video_file(&path);
                    tracing::info!(
                        "Video loaded with id {} (requested id was {})",
                        video_id,
                        id
                    );
                }
                Ok(())
            }
            RenderCommand::VideoPlay { id } => {
                tracing::debug!("Playing video {}", id);
                #[cfg(feature = "video")]
                if let Some(ref mut renderer) = self.renderer {
                    renderer.video_play(id);
                }
                Ok(())
            }
            RenderCommand::VideoPause { id } => {
                tracing::debug!("Pausing video {}", id);
                #[cfg(feature = "video")]
                if let Some(ref mut renderer) = self.renderer {
                    renderer.video_pause(id);
                }
                Ok(())
            }
            RenderCommand::VideoDestroy { id } => {
                tracing::info!("Destroying video {}", id);
                #[cfg(feature = "video")]
                if let Some(ref mut renderer) = self.renderer {
                    renderer.video_stop(id);
                }
                Ok(())
            }
            other => Err(other),
        }
    }
}
