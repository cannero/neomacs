//! Child frame rendering methods for WgpuRenderer.

use super::super::glyph_atlas::WgpuGlyphAtlas;
use super::super::vertex::{RectVertex, RoundedRectVertex, Uniforms};
use super::WgpuRenderer;
use neomacs_display_protocol::face::Face;
use neomacs_display_protocol::frame_glyphs::FrameGlyphBuffer;
use neomacs_display_protocol::types::{AnimatedCursor, Color};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

impl WgpuRenderer {
    /// Render a child frame as a floating overlay on top of the parent frame.
    ///
    /// Draws shadow, background fill, rounded border, then delegates all glyph
    /// rendering (text, cursors, images, etc.) to `render_frame_content()`.
    /// Uses LoadOp::Load to composite on top of whatever was rendered before.
    #[allow(clippy::too_many_arguments)]
    pub fn render_child_frame(
        &self,
        view: &wgpu::TextureView,
        child: &FrameGlyphBuffer,
        offset_x: f32,
        offset_y: f32,
        glyph_atlas: &mut WgpuGlyphAtlas,
        faces: &HashMap<u32, Face>,
        surface_width: u32,
        surface_height: u32,
        cursor_visible: bool,
        animated_cursor: Option<AnimatedCursor>,
        corner_radius: f32,
        shadow_enabled: bool,
        shadow_layers: u32,
        shadow_offset: f32,
        shadow_opacity: f32,
    ) {
        let logical_w = surface_width as f32 / self.scale_factor;
        let logical_h = surface_height as f32 / self.scale_factor;
        let uniforms = Uniforms {
            screen_size: [logical_w, logical_h],
            time: 0.0,
            _padding: 0.0,
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));

        let bw = child.border_width;
        let frame_w = child.width;
        let frame_h = child.height;
        let bg_alpha = child.background_alpha;

        tracing::debug!(
            "render_child_frame: size={:.0}x{:.0} offset=({:.1},{:.1}) border={:.1} glyphs={}",
            frame_w,
            frame_h,
            offset_x,
            offset_y,
            bw,
            child.glyphs.len(),
        );

        // Child-frame-specific rendering: shadow + background + border.
        {
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Child Frame Chrome Encoder"),
                });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Child Frame Chrome Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });

                if shadow_enabled && shadow_layers > 0 {
                    let mut shadow_verts: Vec<RectVertex> = Vec::new();
                    let total_w = frame_w + 2.0 * bw;
                    let total_h = frame_h + 2.0 * bw;
                    let sx = offset_x - bw;
                    let sy = offset_y - bw;
                    for layer in (1..=shadow_layers).rev() {
                        let off = layer as f32 * shadow_offset;
                        let alpha =
                            shadow_opacity * (1.0 - (layer - 1) as f32 / shadow_layers as f32);
                        let c = Color::new(0.0, 0.0, 0.0, alpha);
                        self.add_rect(&mut shadow_verts, sx + off, sy + total_h, total_w, off, &c);
                        self.add_rect(&mut shadow_verts, sx + total_w, sy + off, off, total_h, &c);
                        self.add_rect(&mut shadow_verts, sx + total_w, sy + total_h, off, off, &c);
                    }
                    if !shadow_verts.is_empty() {
                        let buffer =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Child Frame Shadow Buffer"),
                                    contents: bytemuck::cast_slice(&shadow_verts),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });
                        pass.set_pipeline(&self.rect_pipeline);
                        pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                        pass.set_vertex_buffer(0, buffer.slice(..));
                        pass.draw(0..shadow_verts.len() as u32, 0..1);
                    }
                }
            }

            {
                let bg = Color::new(
                    child.background.r,
                    child.background.g,
                    child.background.b,
                    bg_alpha,
                );
                if corner_radius > 0.0 {
                    let mut bg_verts: Vec<RoundedRectVertex> = Vec::new();
                    self.add_rounded_rect(
                        &mut bg_verts,
                        offset_x,
                        offset_y,
                        frame_w,
                        frame_h,
                        0.0,
                        corner_radius,
                        &bg,
                    );
                    if !bg_verts.is_empty() {
                        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("Child Frame BG Pass"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Load,
                                    store: wgpu::StoreOp::Store,
                                },
                                depth_slice: None,
                            })],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                            multiview_mask: None,
                        });
                        let buffer =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Child Frame BG Buffer"),
                                    contents: bytemuck::cast_slice(&bg_verts),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });
                        pass.set_pipeline(&self.rounded_rect_pipeline);
                        pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                        pass.set_vertex_buffer(0, buffer.slice(..));
                        pass.draw(0..bg_verts.len() as u32, 0..1);
                    }
                } else {
                    let mut bg_verts: Vec<RectVertex> = Vec::new();
                    self.add_rect(&mut bg_verts, offset_x, offset_y, frame_w, frame_h, &bg);
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Child Frame BG Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    let buffer =
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("Child Frame BG Buffer"),
                                contents: bytemuck::cast_slice(&bg_verts),
                                usage: wgpu::BufferUsages::VERTEX,
                            });
                    pass.set_pipeline(&self.rect_pipeline);
                    pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.draw(0..bg_verts.len() as u32, 0..1);
                }
            }

            if bw > 0.0 || corner_radius > 0.0 {
                let mut border_verts: Vec<RoundedRectVertex> = Vec::new();
                let bc = if bw > 0.0 {
                    child.border_color
                } else {
                    Color::new(0.5, 0.5, 0.5, 0.3).srgb_to_linear()
                };
                let effective_bw = if bw > 0.0 { bw } else { 1.0 };
                self.add_rounded_rect(
                    &mut border_verts,
                    offset_x - effective_bw,
                    offset_y - effective_bw,
                    frame_w + 2.0 * effective_bw,
                    frame_h + 2.0 * effective_bw,
                    effective_bw,
                    corner_radius,
                    &bc,
                );
                if !border_verts.is_empty() {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Child Frame Border Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    let buffer =
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("Child Frame Border Buffer"),
                                contents: bytemuck::cast_slice(&border_verts),
                                usage: wgpu::BufferUsages::VERTEX,
                            });
                    pass.set_pipeline(&self.rounded_rect_pipeline);
                    pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.draw(0..border_verts.len() as u32, 0..1);
                }
            }

            self.queue.submit(std::iter::once(encoder.finish()));
        }

        // Stencil-write pass: write rounded rect shape into stencil buffer
        // so content rendering clips to the rounded corners.
        if corner_radius > 0.0 {
            let mut stencil_verts: Vec<RoundedRectVertex> = Vec::new();
            self.add_rounded_rect(
                &mut stencil_verts,
                offset_x,
                offset_y,
                frame_w,
                frame_h,
                0.0, // filled (no border)
                corner_radius,
                &Color::new(1.0, 1.0, 1.0, 1.0), // color irrelevant, writes disabled
            );
            if !stencil_verts.is_empty() {
                let mut encoder = self
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Stencil Write Encoder"),
                    });
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Stencil Write Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &self.stencil_view,
                            depth_ops: None,
                            stencil_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(0),
                                store: wgpu::StoreOp::Store,
                            }),
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    let buffer =
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("Stencil Write Buffer"),
                                contents: bytemuck::cast_slice(&stencil_verts),
                                usage: wgpu::BufferUsages::VERTEX,
                            });
                    pass.set_pipeline(&self.stencil_write_pipeline);
                    pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.set_stencil_reference(1);
                    pass.draw(0..stencil_verts.len() as u32, 0..1);
                }
                self.queue.submit(std::iter::once(encoder.finish()));
            }
        }

        self.render_frame_content(
            view,
            child,
            glyph_atlas,
            faces,
            surface_width,
            surface_height,
            offset_x,
            offset_y,
            cursor_visible,
            animated_cursor,
            corner_radius,
        );
    }
}
