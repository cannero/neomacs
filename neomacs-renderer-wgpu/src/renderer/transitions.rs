//! Transitions methods for WgpuRenderer.

use super::super::vertex::GlyphVertex;
use super::WgpuRenderer;
use wgpu::util::DeviceExt;

impl WgpuRenderer {
    /// Render a crossfade transition within a scissor region
    /// Uses the image_pipeline to blend old and new textures
    pub fn render_crossfade(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        blend_t: f32,
        bounds: &neomacs_display_protocol::types::Rect,
        surface_width: u32,
        surface_height: u32,
    ) {
        // We render two passes: old texture with alpha (1-t), new texture with alpha t
        // Using scissor rect to constrain to the window bounds

        // Scissor rects operate in physical framebuffer pixels; bounds from Emacs are logical
        let sf = self.scale_factor;
        let sx = (bounds.x.max(0.0) * sf) as u32;
        let sy = (bounds.y.max(0.0) * sf) as u32;
        let sw = ((bounds.width * sf) as u32).min(surface_width.saturating_sub(sx));
        let sh = ((bounds.height * sf) as u32).min(surface_height.saturating_sub(sy));

        if sw == 0 || sh == 0 {
            return;
        }

        // Use logical dimensions for vertex positions since screen_size uniform is logical
        let w = surface_width as f32 / sf;
        let h = surface_height as f32 / sf;

        // Fullscreen quad with UV mapping
        let vertices = [
            GlyphVertex {
                position: [0.0, 0.0],
                tex_coords: [0.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            GlyphVertex {
                position: [w, 0.0],
                tex_coords: [1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            GlyphVertex {
                position: [w, h],
                tex_coords: [1.0, 1.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            GlyphVertex {
                position: [0.0, 0.0],
                tex_coords: [0.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            GlyphVertex {
                position: [w, h],
                tex_coords: [1.0, 1.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            GlyphVertex {
                position: [0.0, h],
                tex_coords: [0.0, 1.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
        ];

        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Crossfade Vertex Buffer"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Crossfade Encoder"),
            });

        {
            // Pass 1: Draw old texture with alpha (1 - blend_t)
            let old_alpha = 1.0 - blend_t;
            let old_vertices = [
                GlyphVertex {
                    position: [0.0, 0.0],
                    tex_coords: [0.0, 0.0],
                    color: [1.0, 1.0, 1.0, old_alpha],
                },
                GlyphVertex {
                    position: [w, 0.0],
                    tex_coords: [1.0, 0.0],
                    color: [1.0, 1.0, 1.0, old_alpha],
                },
                GlyphVertex {
                    position: [w, h],
                    tex_coords: [1.0, 1.0],
                    color: [1.0, 1.0, 1.0, old_alpha],
                },
                GlyphVertex {
                    position: [0.0, 0.0],
                    tex_coords: [0.0, 0.0],
                    color: [1.0, 1.0, 1.0, old_alpha],
                },
                GlyphVertex {
                    position: [w, h],
                    tex_coords: [1.0, 1.0],
                    color: [1.0, 1.0, 1.0, old_alpha],
                },
                GlyphVertex {
                    position: [0.0, h],
                    tex_coords: [0.0, 1.0],
                    color: [1.0, 1.0, 1.0, old_alpha],
                },
            ];
            let old_vb = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Crossfade Old VB"),
                    contents: bytemuck::cast_slice(&old_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });

            // New texture with alpha blend_t
            let new_vertices = [
                GlyphVertex {
                    position: [0.0, 0.0],
                    tex_coords: [0.0, 0.0],
                    color: [1.0, 1.0, 1.0, blend_t],
                },
                GlyphVertex {
                    position: [w, 0.0],
                    tex_coords: [1.0, 0.0],
                    color: [1.0, 1.0, 1.0, blend_t],
                },
                GlyphVertex {
                    position: [w, h],
                    tex_coords: [1.0, 1.0],
                    color: [1.0, 1.0, 1.0, blend_t],
                },
                GlyphVertex {
                    position: [0.0, 0.0],
                    tex_coords: [0.0, 0.0],
                    color: [1.0, 1.0, 1.0, blend_t],
                },
                GlyphVertex {
                    position: [w, h],
                    tex_coords: [1.0, 1.0],
                    color: [1.0, 1.0, 1.0, blend_t],
                },
                GlyphVertex {
                    position: [0.0, h],
                    tex_coords: [0.0, 1.0],
                    color: [1.0, 1.0, 1.0, blend_t],
                },
            ];
            let new_vb = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Crossfade New VB"),
                    contents: bytemuck::cast_slice(&new_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });

            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Crossfade Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: surface_view,
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

            render_pass.set_scissor_rect(sx, sy, sw, sh);
            render_pass.set_pipeline(&self.image_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

            // Draw old with fading alpha
            render_pass.set_bind_group(1, old_bind_group, &[]);
            render_pass.set_vertex_buffer(0, old_vb.slice(..));
            render_pass.draw(0..6, 0..1);

            // Draw new with increasing alpha
            render_pass.set_bind_group(1, new_bind_group, &[]);
            render_pass.set_vertex_buffer(0, new_vb.slice(..));
            render_pass.draw(0..6, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Render a scroll slide transition within a scissor region
    ///
    /// Uses content-region UV mapping so only the content area of each offscreen
    /// texture is sampled — the mode-line is never included in the sliding quads.
    pub fn render_scroll_slide(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        // Ease-out quadratic
        let eased_t = 1.0 - (1.0 - t).powi(2);
        let offset = scroll_distance * eased_t;

        // Scissor rects operate in physical framebuffer pixels; bounds from Emacs are logical
        let sf = self.scale_factor;
        let sx = (bounds.x.max(0.0) * sf) as u32;
        let sy = (bounds.y.max(0.0) * sf) as u32;
        let sw = ((bounds.width * sf) as u32).min(surface_width.saturating_sub(sx));
        let sh = ((bounds.height * sf) as u32).min(surface_height.saturating_sub(sy));

        if sw == 0 || sh == 0 {
            return;
        }

        // Use logical dimensions for vertex positions since screen_size uniform is logical
        let w = surface_width as f32 / sf;
        let h = surface_height as f32 / sf;
        let dir = direction as f32;

        // UV coordinates for the content region within the full-frame texture.
        // bounds is already content-only (mode-line excluded by caller).
        let uv_left = bounds.x / w;
        let uv_top = bounds.y / h;
        let uv_right = (bounds.x + bounds.width) / w;
        let uv_bottom = (bounds.y + bounds.height) / h;

        // Old texture slides out by offset in direction
        let old_y_offset = -dir * offset;
        // New texture slides in from opposite side
        let new_y_offset = dir * (scroll_distance - offset);

        // Build a content-region quad: position covers the content bounds shifted
        // by y_off, UV maps to exactly the content region in the full-frame texture.
        let make_quad = |y_off: f32| -> [GlyphVertex; 6] {
            let x0 = bounds.x;
            let x1 = bounds.x + bounds.width;
            let y0 = bounds.y + y_off;
            let y1 = bounds.y + bounds.height + y_off;
            [
                GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_left, uv_top],
                    color: [1.0, 1.0, 1.0, 1.0],
                },
                GlyphVertex {
                    position: [x1, y0],
                    tex_coords: [uv_right, uv_top],
                    color: [1.0, 1.0, 1.0, 1.0],
                },
                GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_right, uv_bottom],
                    color: [1.0, 1.0, 1.0, 1.0],
                },
                GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_left, uv_top],
                    color: [1.0, 1.0, 1.0, 1.0],
                },
                GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_right, uv_bottom],
                    color: [1.0, 1.0, 1.0, 1.0],
                },
                GlyphVertex {
                    position: [x0, y1],
                    tex_coords: [uv_left, uv_bottom],
                    color: [1.0, 1.0, 1.0, 1.0],
                },
            ]
        };

        let old_vertices = make_quad(old_y_offset);
        let new_vertices = make_quad(new_y_offset);

        let old_vb = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Scroll Old VB"),
                contents: bytemuck::cast_slice(&old_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let new_vb = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Scroll New VB"),
                contents: bytemuck::cast_slice(&new_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Scroll Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Scroll Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: surface_view,
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

            render_pass.set_scissor_rect(sx, sy, sw, sh);
            render_pass.set_pipeline(&self.image_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

            // Draw old texture sliding out
            render_pass.set_bind_group(1, old_bind_group, &[]);
            render_pass.set_vertex_buffer(0, old_vb.slice(..));
            render_pass.draw(0..6, 0..1);

            // Draw new texture sliding in
            render_pass.set_bind_group(1, new_bind_group, &[]);
            render_pass.set_vertex_buffer(0, new_vb.slice(..));
            render_pass.draw(0..6, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Dispatch to the appropriate scroll effect renderer.
    ///
    /// This is the main entry point called by `render_transitions()` for each
    /// active scroll transition. It applies the easing function to `raw_t`,
    /// then delegates to the specific effect renderer.
    pub fn render_scroll_effect(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        raw_t: f32,
        elapsed_secs: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        scroll_distance: f32,
        effect: neomacs_display_protocol::scroll_animation::ScrollEffect,
        easing: neomacs_display_protocol::scroll_animation::ScrollEasing,
        surface_width: u32,
        surface_height: u32,
    ) {
        use neomacs_display_protocol::scroll_animation::ScrollEffect;

        let eased_t = easing.apply(raw_t);

        match effect {
            ScrollEffect::Slide => {
                // Use existing slide renderer (it has its own easing, pass raw_t)
                self.render_scroll_slide(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    raw_t,
                    direction,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::Crossfade => {
                self.render_scroll_crossfade(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::ScaleZoom => {
                self.render_scroll_scale_zoom(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::FadeEdges => {
                self.render_scroll_fade_edges(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    direction,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::Cascade => {
                self.render_scroll_cascade(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    elapsed_secs,
                    direction,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::Parallax => {
                self.render_scroll_parallax(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    direction,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::Tilt => {
                self.render_scroll_tilt(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    direction,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::PageCurl => {
                self.render_scroll_page_curl(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    direction,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::CardFlip => {
                self.render_scroll_card_flip(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    direction,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::CylinderRoll => {
                self.render_scroll_cylinder_roll(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    direction,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::Wobbly => {
                self.render_scroll_wobbly(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    elapsed_secs,
                    direction,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::Wave => {
                self.render_scroll_wave(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    elapsed_secs,
                    direction,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::PerLineSpring => {
                self.render_scroll_per_line_spring(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    elapsed_secs,
                    direction,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::Liquid => {
                self.render_scroll_liquid(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    elapsed_secs,
                    direction,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }

            // Post-processing effects: render slide first, then apply post-process
            ScrollEffect::MotionBlur
            | ScrollEffect::ChromaticAberration
            | ScrollEffect::GhostTrails
            | ScrollEffect::ColorTemperature
            | ScrollEffect::CRTScanlines
            | ScrollEffect::DepthOfField => {
                self.render_scroll_with_post_process(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    raw_t,
                    eased_t,
                    elapsed_secs,
                    direction,
                    bounds,
                    scroll_distance,
                    effect,
                    surface_width,
                    surface_height,
                );
            }

            ScrollEffect::TypewriterReveal => {
                self.render_scroll_typewriter(
                    surface_view,
                    old_bind_group,
                    new_bind_group,
                    eased_t,
                    elapsed_secs,
                    direction,
                    bounds,
                    scroll_distance,
                    surface_width,
                    surface_height,
                );
            }
        }
    }

    /// Helper: compute scissor rect and content UV from bounds.
    fn scroll_scissor_and_uv(
        &self,
        bounds: &neomacs_display_protocol::types::Rect,
        surface_width: u32,
        surface_height: u32,
    ) -> Option<(u32, u32, u32, u32, f32, f32, f32, f32, f32, f32)> {
        let sf = self.scale_factor;
        let sx = (bounds.x.max(0.0) * sf) as u32;
        let sy = (bounds.y.max(0.0) * sf) as u32;
        let sw = ((bounds.width * sf) as u32).min(surface_width.saturating_sub(sx));
        let sh = ((bounds.height * sf) as u32).min(surface_height.saturating_sub(sy));
        if sw == 0 || sh == 0 {
            return None;
        }
        let w = surface_width as f32 / sf;
        let h = surface_height as f32 / sf;
        let uv_left = bounds.x / w;
        let uv_top = bounds.y / h;
        let uv_right = (bounds.x + bounds.width) / w;
        let uv_bottom = (bounds.y + bounds.height) / h;
        Some((sx, sy, sw, sh, w, h, uv_left, uv_top, uv_right, uv_bottom))
    }

    /// Helper: create a vertex buffer from GlyphVertex slice.
    fn create_scroll_vb(&self, vertices: &[GlyphVertex]) -> wgpu::Buffer {
        use wgpu::util::DeviceExt;
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Scroll VB"),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            })
    }

    /// Helper: submit a two-quad scroll render pass (old + new textures).
    fn submit_scroll_two_quad_pass(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        old_vertices: &[GlyphVertex],
        new_vertices: &[GlyphVertex],
        sx: u32,
        sy: u32,
        sw: u32,
        sh: u32,
    ) {
        let old_vb = self.create_scroll_vb(old_vertices);
        let new_vb = self.create_scroll_vb(new_vertices);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Scroll Effect Encoder"),
            });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Scroll Effect Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: surface_view,
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
            rp.set_scissor_rect(sx, sy, sw, sh);
            rp.set_pipeline(&self.image_pipeline);
            rp.set_bind_group(0, &self.uniform_bind_group, &[]);

            rp.set_bind_group(1, old_bind_group, &[]);
            rp.set_vertex_buffer(0, old_vb.slice(..));
            rp.draw(0..old_vertices.len() as u32, 0..1);

            rp.set_bind_group(1, new_bind_group, &[]);
            rp.set_vertex_buffer(0, new_vb.slice(..));
            rp.draw(0..new_vertices.len() as u32, 0..1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Crossfade scroll: alpha blend old → new within content bounds.
    fn render_scroll_crossfade(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        bounds: &neomacs_display_protocol::types::Rect,
        _scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };
        let x0 = bounds.x;
        let y0 = bounds.y;
        let x1 = bounds.x + bounds.width;
        let y1 = bounds.y + bounds.height;
        let old_a = 1.0 - t;

        let old_verts = [
            GlyphVertex {
                position: [x0, y0],
                tex_coords: [uv_l, uv_t],
                color: [1.0, 1.0, 1.0, old_a],
            },
            GlyphVertex {
                position: [x1, y0],
                tex_coords: [uv_r, uv_t],
                color: [1.0, 1.0, 1.0, old_a],
            },
            GlyphVertex {
                position: [x1, y1],
                tex_coords: [uv_r, uv_b],
                color: [1.0, 1.0, 1.0, old_a],
            },
            GlyphVertex {
                position: [x0, y0],
                tex_coords: [uv_l, uv_t],
                color: [1.0, 1.0, 1.0, old_a],
            },
            GlyphVertex {
                position: [x1, y1],
                tex_coords: [uv_r, uv_b],
                color: [1.0, 1.0, 1.0, old_a],
            },
            GlyphVertex {
                position: [x0, y1],
                tex_coords: [uv_l, uv_b],
                color: [1.0, 1.0, 1.0, old_a],
            },
        ];
        let new_verts = [
            GlyphVertex {
                position: [x0, y0],
                tex_coords: [uv_l, uv_t],
                color: [1.0, 1.0, 1.0, t],
            },
            GlyphVertex {
                position: [x1, y0],
                tex_coords: [uv_r, uv_t],
                color: [1.0, 1.0, 1.0, t],
            },
            GlyphVertex {
                position: [x1, y1],
                tex_coords: [uv_r, uv_b],
                color: [1.0, 1.0, 1.0, t],
            },
            GlyphVertex {
                position: [x0, y0],
                tex_coords: [uv_l, uv_t],
                color: [1.0, 1.0, 1.0, t],
            },
            GlyphVertex {
                position: [x1, y1],
                tex_coords: [uv_r, uv_b],
                color: [1.0, 1.0, 1.0, t],
            },
            GlyphVertex {
                position: [x0, y1],
                tex_coords: [uv_l, uv_b],
                color: [1.0, 1.0, 1.0, t],
            },
        ];
        self.submit_scroll_two_quad_pass(
            surface_view,
            old_bind_group,
            new_bind_group,
            &old_verts,
            &new_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// ScaleZoom: old shrinks to 95% and fades; new zooms from 95% to 100%.
    fn render_scroll_scale_zoom(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        bounds: &neomacs_display_protocol::types::Rect,
        _scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };
        let cx = bounds.x + bounds.width / 2.0;
        let cy = bounds.y + bounds.height / 2.0;

        // Old: scale from 1.0 → 0.92, fade out
        let old_scale = 1.0 - t * 0.08;
        let old_a = 1.0 - t;
        let old_hw = bounds.width / 2.0 * old_scale;
        let old_hh = bounds.height / 2.0 * old_scale;

        // New: scale from 0.92 → 1.0, fade in
        let new_scale = 0.92 + t * 0.08;
        let new_hw = bounds.width / 2.0 * new_scale;
        let new_hh = bounds.height / 2.0 * new_scale;

        let make_quad = |hw: f32, hh: f32, alpha: f32| -> [GlyphVertex; 6] {
            let x0 = cx - hw;
            let y0 = cy - hh;
            let x1 = cx + hw;
            let y1 = cy + hh;
            [
                GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, uv_t],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x1, y0],
                    tex_coords: [uv_r, uv_t],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, uv_b],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, uv_t],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, uv_b],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x0, y1],
                    tex_coords: [uv_l, uv_b],
                    color: [1.0, 1.0, 1.0, alpha],
                },
            ]
        };

        let old_verts = make_quad(old_hw, old_hh, old_a);
        let new_verts = make_quad(new_hw, new_hh, t);
        self.submit_scroll_two_quad_pass(
            surface_view,
            old_bind_group,
            new_bind_group,
            &old_verts,
            &new_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// FadeEdges: slide with soft fade at viewport top/bottom edges.
    fn render_scroll_fade_edges(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };

        let dir = direction as f32;
        let offset = scroll_distance * t;
        let num_strips = 16;
        let strip_h = bounds.height / num_strips as f32;
        let uv_strip_h = (uv_b - uv_t) / num_strips as f32;
        let fade_zone = 0.15; // fade over 15% of height at each edge

        let make_strips = |y_off: f32, is_old: bool| -> Vec<GlyphVertex> {
            let mut verts = Vec::with_capacity(num_strips * 6);
            for i in 0..num_strips {
                let rel_y0 = bounds.y + i as f32 * strip_h + y_off;
                let rel_y1 = bounds.y + (i + 1) as f32 * strip_h + y_off;
                let u0 = uv_t + i as f32 * uv_strip_h;
                let u1 = uv_t + (i + 1) as f32 * uv_strip_h;

                // Alpha based on distance from edge
                let center_y = (rel_y0 + rel_y1) / 2.0;
                let in_bounds_t = ((center_y - bounds.y) / bounds.height).clamp(0.0, 1.0);
                let edge_alpha = if in_bounds_t < fade_zone {
                    in_bounds_t / fade_zone
                } else if in_bounds_t > 1.0 - fade_zone {
                    (1.0 - in_bounds_t) / fade_zone
                } else {
                    1.0
                };
                let base_alpha = if is_old { 1.0 - t } else { t };
                let alpha = (base_alpha * edge_alpha).clamp(0.0, 1.0);

                let x0 = bounds.x;
                let x1 = bounds.x + bounds.width;
                let c = [1.0, 1.0, 1.0, alpha];
                verts.push(GlyphVertex {
                    position: [x0, rel_y0],
                    tex_coords: [uv_l, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x1, rel_y0],
                    tex_coords: [uv_r, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x1, rel_y1],
                    tex_coords: [uv_r, u1],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x0, rel_y0],
                    tex_coords: [uv_l, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x1, rel_y1],
                    tex_coords: [uv_r, u1],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x0, rel_y1],
                    tex_coords: [uv_l, u1],
                    color: c,
                });
            }
            verts
        };

        let old_y_off = -dir * offset;
        let new_y_off = dir * (scroll_distance - offset);
        let old_verts = make_strips(old_y_off, true);
        let new_verts = make_strips(new_y_off, false);
        self.submit_scroll_two_quad_pass(
            surface_view,
            old_bind_group,
            new_bind_group,
            &old_verts,
            &new_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// Cascade: lines drop in with staggered delay (waterfall).
    fn render_scroll_cascade(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        elapsed_secs: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };

        let num_strips = 20;
        let strip_h = bounds.height / num_strips as f32;
        let uv_strip_h = (uv_b - uv_t) / num_strips as f32;
        let dir = direction as f32;
        let stagger = 0.06; // 60ms stagger per line

        let make_cascade_strips = |bind: &wgpu::BindGroup, is_new: bool| -> Vec<GlyphVertex> {
            let mut verts = Vec::with_capacity(num_strips * 6);
            for i in 0..num_strips {
                let line_delay = i as f32 * stagger;
                let line_t =
                    ((t - line_delay / 1.0).max(0.0) / (1.0 - line_delay).max(0.01)).min(1.0);
                let eased = 1.0 - (1.0 - line_t).powi(2);

                let base_y = bounds.y + i as f32 * strip_h;
                let u0 = uv_t + i as f32 * uv_strip_h;
                let u1 = uv_t + (i + 1) as f32 * uv_strip_h;

                let (y_off, alpha) = if is_new {
                    (dir * (scroll_distance * (1.0 - eased)), eased)
                } else {
                    (-dir * (scroll_distance * eased), 1.0 - eased)
                };

                let y0 = base_y + y_off;
                let y1 = base_y + strip_h + y_off;
                let x0 = bounds.x;
                let x1 = bounds.x + bounds.width;
                let c = [1.0, 1.0, 1.0, alpha];

                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x1, y0],
                    tex_coords: [uv_r, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x0, y1],
                    tex_coords: [uv_l, u1],
                    color: c,
                });
            }
            verts
        };

        let old_verts = make_cascade_strips(old_bind_group, false);
        let new_verts = make_cascade_strips(new_bind_group, true);
        self.submit_scroll_two_quad_pass(
            surface_view,
            old_bind_group,
            new_bind_group,
            &old_verts,
            &new_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// Parallax: layers scroll at different speeds for depth illusion.
    fn render_scroll_parallax(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };

        let dir = direction as f32;
        // Foreground scrolls at normal speed, "background" slower
        // We simulate by having old content move slower (0.7x) and new content normal
        let slow_t = t * 0.7;
        let slow_offset = scroll_distance * slow_t;
        let fast_offset = scroll_distance * t;

        let make_quad = |y_off: f32, alpha: f32| -> [GlyphVertex; 6] {
            let x0 = bounds.x;
            let y0 = bounds.y + y_off;
            let x1 = bounds.x + bounds.width;
            let y1 = bounds.y + bounds.height + y_off;
            [
                GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, uv_t],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x1, y0],
                    tex_coords: [uv_r, uv_t],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, uv_b],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, uv_t],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, uv_b],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x0, y1],
                    tex_coords: [uv_l, uv_b],
                    color: [1.0, 1.0, 1.0, alpha],
                },
            ]
        };

        let old_verts = make_quad(-dir * slow_offset, 1.0 - t);
        let new_verts = make_quad(dir * (scroll_distance - fast_offset), t);
        self.submit_scroll_two_quad_pass(
            surface_view,
            old_bind_group,
            new_bind_group,
            &old_verts,
            &new_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// Tilt: subtle perspective tilt during scroll.
    fn render_scroll_tilt(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };

        let dir = direction as f32;
        let offset = scroll_distance * t;
        let tilt_strength = (1.0 - t) * dir; // Tilt decays as animation settles
        let max_tilt = bounds.height * 0.03; // 3% of height
        let num_strips = 12;
        let strip_h = bounds.height / num_strips as f32;
        let uv_strip_h = (uv_b - uv_t) / num_strips as f32;

        let make_tilted = |y_base_off: f32| -> Vec<GlyphVertex> {
            let mut verts = Vec::with_capacity(num_strips * 6);
            for i in 0..num_strips {
                let nt0 = i as f32 / num_strips as f32;
                let nt1 = (i + 1) as f32 / num_strips as f32;

                // Tilt: center stays, edges deflect
                let tilt0 = max_tilt * tilt_strength * (nt0 - 0.5) * 2.0;
                let tilt1 = max_tilt * tilt_strength * (nt1 - 0.5) * 2.0;

                // Horizontal squeeze at edges (perspective)
                let squeeze0 = 1.0 - (nt0 - 0.5).abs() * 0.02 * tilt_strength.abs();
                let squeeze1 = 1.0 - (nt1 - 0.5).abs() * 0.02 * tilt_strength.abs();

                let cx = bounds.x + bounds.width / 2.0;
                let hw0 = bounds.width / 2.0 * squeeze0;
                let hw1 = bounds.width / 2.0 * squeeze1;

                let y0 = bounds.y + i as f32 * strip_h + y_base_off + tilt0;
                let y1 = bounds.y + (i + 1) as f32 * strip_h + y_base_off + tilt1;
                let u0 = uv_t + i as f32 * uv_strip_h;
                let u1 = uv_t + (i + 1) as f32 * uv_strip_h;

                verts.push(GlyphVertex {
                    position: [cx - hw0, y0],
                    tex_coords: [uv_l, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [cx + hw0, y0],
                    tex_coords: [uv_r, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [cx + hw1, y1],
                    tex_coords: [uv_r, u1],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [cx - hw0, y0],
                    tex_coords: [uv_l, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [cx + hw1, y1],
                    tex_coords: [uv_r, u1],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [cx - hw1, y1],
                    tex_coords: [uv_l, u1],
                    color: [1.0; 4],
                });
            }
            verts
        };

        let old_verts = make_tilted(-dir * offset);
        let new_verts = make_tilted(dir * (scroll_distance - offset));
        self.submit_scroll_two_quad_pass(
            surface_view,
            old_bind_group,
            new_bind_group,
            &old_verts,
            &new_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// PageCurl: page curls away revealing new content underneath.
    fn render_scroll_page_curl(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        _scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        use neomacs_display_protocol::scroll_animation::page_curl_transform;
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };

        let num_strips = 24;
        let strip_h = bounds.height / num_strips as f32;
        let uv_strip_h = (uv_b - uv_t) / num_strips as f32;

        // New content: flat, full opacity (drawn first, underneath)
        let new_verts: Vec<GlyphVertex> = {
            let x0 = bounds.x;
            let x1 = bounds.x + bounds.width;
            let y0 = bounds.y;
            let y1 = bounds.y + bounds.height;
            vec![
                GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, uv_t],
                    color: [1.0; 4],
                },
                GlyphVertex {
                    position: [x1, y0],
                    tex_coords: [uv_r, uv_t],
                    color: [1.0; 4],
                },
                GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, uv_b],
                    color: [1.0; 4],
                },
                GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, uv_t],
                    color: [1.0; 4],
                },
                GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, uv_b],
                    color: [1.0; 4],
                },
                GlyphVertex {
                    position: [x0, y1],
                    tex_coords: [uv_l, uv_b],
                    color: [1.0; 4],
                },
            ]
        };

        // Old content: curling away from bottom (or top for scroll up)
        let old_verts: Vec<GlyphVertex> = {
            let mut verts = Vec::with_capacity(num_strips * 6);
            for i in 0..num_strips {
                let nt = i as f32 / num_strips as f32;
                let nt1 = (i + 1) as f32 / num_strips as f32;

                let curl_pos = if direction > 0 { nt } else { 1.0 - nt };
                let (x_off, y_off, alpha) = page_curl_transform(curl_pos, t, bounds.height);

                let x0 = bounds.x + x_off;
                let x1 = bounds.x + bounds.width + x_off;
                let y0 = bounds.y + i as f32 * strip_h + y_off;
                let y1 = bounds.y + (i + 1) as f32 * strip_h + y_off;
                let u0 = uv_t + i as f32 * uv_strip_h;
                let u1 = uv_t + (i + 1) as f32 * uv_strip_h;
                let c = [1.0, 1.0, 1.0, alpha];

                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x1, y0],
                    tex_coords: [uv_r, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x0, y1],
                    tex_coords: [uv_l, u1],
                    color: c,
                });
            }
            verts
        };

        // Draw new first (underneath), then old (curling on top)
        self.submit_scroll_two_quad_pass(
            surface_view,
            new_bind_group,
            old_bind_group,
            &new_verts,
            &old_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// CardFlip: screenful flips like a card around X-axis.
    fn render_scroll_card_flip(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        _scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };

        let cx = bounds.x + bounds.width / 2.0;
        let cy = bounds.y + bounds.height / 2.0;

        // Card flip: shrinks height to 0 at midpoint, then expands
        let angle = t * std::f32::consts::PI;
        let scale_y = angle.cos().abs().max(0.02);
        let hh = bounds.height / 2.0 * scale_y;
        let hw = bounds.width / 2.0;

        let (bind_group, alpha) = if t < 0.5 {
            (old_bind_group, 1.0)
        } else {
            (new_bind_group, 1.0)
        };

        let verts = [
            GlyphVertex {
                position: [cx - hw, cy - hh],
                tex_coords: [uv_l, uv_t],
                color: [1.0, 1.0, 1.0, alpha],
            },
            GlyphVertex {
                position: [cx + hw, cy - hh],
                tex_coords: [uv_r, uv_t],
                color: [1.0, 1.0, 1.0, alpha],
            },
            GlyphVertex {
                position: [cx + hw, cy + hh],
                tex_coords: [uv_r, uv_b],
                color: [1.0, 1.0, 1.0, alpha],
            },
            GlyphVertex {
                position: [cx - hw, cy - hh],
                tex_coords: [uv_l, uv_t],
                color: [1.0, 1.0, 1.0, alpha],
            },
            GlyphVertex {
                position: [cx + hw, cy + hh],
                tex_coords: [uv_r, uv_b],
                color: [1.0, 1.0, 1.0, alpha],
            },
            GlyphVertex {
                position: [cx - hw, cy + hh],
                tex_coords: [uv_l, uv_b],
                color: [1.0, 1.0, 1.0, alpha],
            },
        ];

        // Single texture pass
        let empty: [GlyphVertex; 0] = [];
        self.submit_scroll_two_quad_pass(
            surface_view,
            bind_group,
            bind_group,
            &verts,
            &empty,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// CylinderRoll: content wraps around a vertical cylinder.
    fn render_scroll_cylinder_roll(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };

        let num_strips = 16;
        let strip_h = bounds.height / num_strips as f32;
        let uv_strip_h = (uv_b - uv_t) / num_strips as f32;
        let dir = direction as f32;
        let offset = scroll_distance * t;
        let pi = std::f32::consts::PI;

        let make_cylinder = |y_base_off: f32, is_old: bool| -> Vec<GlyphVertex> {
            let mut verts = Vec::with_capacity(num_strips * 6);
            let cx = bounds.x + bounds.width / 2.0;

            for i in 0..num_strips {
                let nt = (i as f32 + 0.5) / num_strips as f32;
                // Angle on the cylinder surface
                let angle = (nt - 0.5) * pi * 0.4 + dir * (1.0 - t) * pi * 0.2;

                let cos_a = angle.cos();
                let squeeze = cos_a.abs().max(0.4);
                let hw = bounds.width / 2.0 * squeeze;

                // Brightness based on angle (facing = bright, edge = dim)
                let brightness = (cos_a * 0.4 + 0.6).clamp(0.3, 1.0);
                let alpha = if is_old {
                    (1.0 - t) * brightness
                } else {
                    t * brightness
                };

                let y0 = bounds.y + i as f32 * strip_h + y_base_off;
                let y1 = bounds.y + (i + 1) as f32 * strip_h + y_base_off;
                let u0 = uv_t + i as f32 * uv_strip_h;
                let u1 = uv_t + (i + 1) as f32 * uv_strip_h;
                let c = [brightness, brightness, brightness, alpha];

                verts.push(GlyphVertex {
                    position: [cx - hw, y0],
                    tex_coords: [uv_l, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [cx + hw, y0],
                    tex_coords: [uv_r, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [cx + hw, y1],
                    tex_coords: [uv_r, u1],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [cx - hw, y0],
                    tex_coords: [uv_l, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [cx + hw, y1],
                    tex_coords: [uv_r, u1],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [cx - hw, y1],
                    tex_coords: [uv_l, u1],
                    color: c,
                });
            }
            verts
        };

        let old_verts = make_cylinder(-dir * offset, true);
        let new_verts = make_cylinder(dir * (scroll_distance - offset), false);
        self.submit_scroll_two_quad_pass(
            surface_view,
            old_bind_group,
            new_bind_group,
            &old_verts,
            &new_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// Wobbly/jelly: content deforms elastically during scroll.
    fn render_scroll_wobbly(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        elapsed_secs: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        use neomacs_display_protocol::scroll_animation::wobbly_deform;
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };

        let dir = direction as f32;
        let offset = scroll_distance * t;
        let num_strips = 20;
        let strip_h = bounds.height / num_strips as f32;
        let uv_strip_h = (uv_b - uv_t) / num_strips as f32;
        let amplitude = bounds.width * 0.03; // 3% of width

        let make_wobbly = |y_base_off: f32| -> Vec<GlyphVertex> {
            let mut verts = Vec::with_capacity(num_strips * 6);
            for i in 0..num_strips {
                let nt = i as f32 / num_strips as f32;
                let (dx, _dy) = wobbly_deform(i, num_strips, nt, t, dir, amplitude);

                let x0 = bounds.x + dx;
                let x1 = bounds.x + bounds.width + dx;
                let y0 = bounds.y + i as f32 * strip_h + y_base_off;
                let y1 = bounds.y + (i + 1) as f32 * strip_h + y_base_off;
                let u0 = uv_t + i as f32 * uv_strip_h;
                let u1 = uv_t + (i + 1) as f32 * uv_strip_h;

                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x1, y0],
                    tex_coords: [uv_r, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x0, y1],
                    tex_coords: [uv_l, u1],
                    color: [1.0; 4],
                });
            }
            verts
        };

        let old_verts = make_wobbly(-dir * offset);
        let new_verts = make_wobbly(dir * (scroll_distance - offset));
        self.submit_scroll_two_quad_pass(
            surface_view,
            old_bind_group,
            new_bind_group,
            &old_verts,
            &new_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// Wave: horizontal sine-wave displacement during scroll.
    fn render_scroll_wave(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        elapsed_secs: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };

        let dir = direction as f32;
        let offset = scroll_distance * t;
        let num_strips = 20;
        let strip_h = bounds.height / num_strips as f32;
        let uv_strip_h = (uv_b - uv_t) / num_strips as f32;
        let amplitude = bounds.width * 0.025;
        let damping = 1.0 - t;

        let make_wave = |y_base_off: f32| -> Vec<GlyphVertex> {
            let mut verts = Vec::with_capacity(num_strips * 6);
            for i in 0..num_strips {
                let nt = i as f32 / num_strips as f32;
                let phase = nt * std::f32::consts::PI * 4.0 + elapsed_secs * 8.0;
                let dx = amplitude * phase.sin() * damping;

                let x0 = bounds.x + dx;
                let x1 = bounds.x + bounds.width + dx;
                let y0 = bounds.y + i as f32 * strip_h + y_base_off;
                let y1 = bounds.y + (i + 1) as f32 * strip_h + y_base_off;
                let u0 = uv_t + i as f32 * uv_strip_h;
                let u1 = uv_t + (i + 1) as f32 * uv_strip_h;

                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x1, y0],
                    tex_coords: [uv_r, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x0, y1],
                    tex_coords: [uv_l, u1],
                    color: [1.0; 4],
                });
            }
            verts
        };

        let old_verts = make_wave(-dir * offset);
        let new_verts = make_wave(dir * (scroll_distance - offset));
        self.submit_scroll_two_quad_pass(
            surface_view,
            old_bind_group,
            new_bind_group,
            &old_verts,
            &new_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// PerLineSpring: each line on own spring with stagger delay.
    fn render_scroll_per_line_spring(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        elapsed_secs: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };

        let dir = direction as f32;
        let num_strips = 20;
        let strip_h = bounds.height / num_strips as f32;
        let uv_strip_h = (uv_b - uv_t) / num_strips as f32;
        let stagger = 0.015; // 15ms stagger per line

        let make_spring = |is_new: bool| -> Vec<GlyphVertex> {
            let mut verts = Vec::with_capacity(num_strips * 6);
            for i in 0..num_strips {
                let line_start = i as f32 * stagger;
                let line_t = ((elapsed_secs - line_start).max(0.0) * 8.0).min(1.0);

                // Spring overshoot: goes past 1.0 then settles
                let omega = 10.0;
                let spring_t = if line_t >= 1.0 {
                    1.0
                } else {
                    let et = (-omega * line_t).exp();
                    1.0 - (1.0 + omega * line_t) * et
                };

                let line_offset = scroll_distance * spring_t;
                let y_off = if is_new {
                    dir * (scroll_distance - line_offset)
                } else {
                    -dir * line_offset
                };

                let x0 = bounds.x;
                let x1 = bounds.x + bounds.width;
                let y0 = bounds.y + i as f32 * strip_h + y_off;
                let y1 = bounds.y + (i + 1) as f32 * strip_h + y_off;
                let u0 = uv_t + i as f32 * uv_strip_h;
                let u1 = uv_t + (i + 1) as f32 * uv_strip_h;

                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x1, y0],
                    tex_coords: [uv_r, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x0, y1],
                    tex_coords: [uv_l, u1],
                    color: [1.0; 4],
                });
            }
            verts
        };

        let old_verts = make_spring(false);
        let new_verts = make_spring(true);
        self.submit_scroll_two_quad_pass(
            surface_view,
            old_bind_group,
            new_bind_group,
            &old_verts,
            &new_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// Liquid: noise-based UV warping, text ripples like water.
    fn render_scroll_liquid(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        elapsed_secs: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        use neomacs_display_protocol::scroll_animation::liquid_deform;
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };

        let dir = direction as f32;
        let offset = scroll_distance * t;
        let num_strips = 20;
        let strip_h = bounds.height / num_strips as f32;
        let uv_strip_h = (uv_b - uv_t) / num_strips as f32;
        let amplitude = bounds.width * 0.04;

        let make_liquid = |y_base_off: f32| -> Vec<GlyphVertex> {
            let mut verts = Vec::with_capacity(num_strips * 6);
            for i in 0..num_strips {
                let nt = i as f32 / num_strips as f32;
                let (dx, dy) = liquid_deform(i, num_strips, nt, t, elapsed_secs, amplitude);

                let x0 = bounds.x + dx;
                let x1 = bounds.x + bounds.width + dx;
                let y0 = bounds.y + i as f32 * strip_h + y_base_off + dy;
                let y1 = bounds.y + (i + 1) as f32 * strip_h + y_base_off + dy;
                let u0 = uv_t + i as f32 * uv_strip_h;
                let u1 = uv_t + (i + 1) as f32 * uv_strip_h;

                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x1, y0],
                    tex_coords: [uv_r, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: [1.0; 4],
                });
                verts.push(GlyphVertex {
                    position: [x0, y1],
                    tex_coords: [uv_l, u1],
                    color: [1.0; 4],
                });
            }
            verts
        };

        let old_verts = make_liquid(-dir * offset);
        let new_verts = make_liquid(dir * (scroll_distance - offset));
        self.submit_scroll_two_quad_pass(
            surface_view,
            old_bind_group,
            new_bind_group,
            &old_verts,
            &new_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// Post-processing scroll effects: render slide, then tint/distort via color manipulation.
    ///
    /// Since we don't have a separate post-process shader pipeline yet, we approximate
    /// post-processing effects by manipulating vertex colors during the slide transition.
    fn render_scroll_with_post_process(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        raw_t: f32,
        eased_t: f32,
        elapsed_secs: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        scroll_distance: f32,
        effect: neomacs_display_protocol::scroll_animation::ScrollEffect,
        surface_width: u32,
        surface_height: u32,
    ) {
        use neomacs_display_protocol::scroll_animation::ScrollEffect;

        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };

        let dir = direction as f32;
        let offset = scroll_distance * eased_t;
        let speed = (1.0 - eased_t); // High at start, low at end
        let num_strips = 20;
        let strip_h = bounds.height / num_strips as f32;
        let uv_strip_h = (uv_b - uv_t) / num_strips as f32;

        let make_postprocess = |y_base_off: f32, is_old: bool| -> Vec<GlyphVertex> {
            let mut verts = Vec::with_capacity(num_strips * 6);
            for i in 0..num_strips {
                let nt = i as f32 / num_strips as f32;
                let nt_center = (nt - 0.5).abs() * 2.0; // 0 at center, 1 at edges

                let mut r = 1.0_f32;
                let mut g = 1.0_f32;
                let mut b = 1.0_f32;
                let mut alpha = 1.0_f32;
                let mut dx = 0.0_f32;

                match effect {
                    ScrollEffect::MotionBlur => {
                        // Simulate blur by reducing alpha at edges proportional to speed
                        let blur = speed * 0.4;
                        alpha = 1.0 - nt_center * blur;
                    }
                    ScrollEffect::ChromaticAberration => {
                        // Shift color channels based on position and speed
                        let shift = speed * 0.08;
                        r = 1.0 + shift * (nt - 0.5);
                        b = 1.0 - shift * (nt - 0.5);
                    }
                    ScrollEffect::GhostTrails => {
                        // Reduced alpha creates ghost-like transparency
                        let ghost = speed * 0.3;
                        alpha = 1.0 - ghost * nt_center;
                    }
                    ScrollEffect::ColorTemperature => {
                        // Warm (orange) scrolling down, cool (blue) scrolling up
                        let temp = dir * speed * 0.06;
                        r = (1.0 + temp).clamp(0.9, 1.1);
                        b = (1.0 - temp).clamp(0.9, 1.1);
                    }
                    ScrollEffect::CRTScanlines => {
                        // Scanline brightness modulation
                        let scanline = ((nt * num_strips as f32 * 2.0 + elapsed_secs * 20.0).sin()
                            * 0.5
                            + 0.5);
                        let intensity = 1.0 - speed * 0.15 * scanline;
                        r = intensity;
                        g = intensity;
                        b = intensity;
                    }
                    ScrollEffect::DepthOfField => {
                        // Edges get dimmer (simulating blur)
                        let dof = speed * 0.3;
                        let brightness = 1.0 - nt_center * dof;
                        r = brightness;
                        g = brightness;
                        b = brightness;
                    }
                    _ => {}
                }

                let x0 = bounds.x + dx;
                let x1 = bounds.x + bounds.width + dx;
                let y0 = bounds.y + i as f32 * strip_h + y_base_off;
                let y1 = bounds.y + (i + 1) as f32 * strip_h + y_base_off;
                let u0 = uv_t + i as f32 * uv_strip_h;
                let u1 = uv_t + (i + 1) as f32 * uv_strip_h;
                let c = [r, g, b, alpha];

                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x1, y0],
                    tex_coords: [uv_r, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, u0],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, u1],
                    color: c,
                });
                verts.push(GlyphVertex {
                    position: [x0, y1],
                    tex_coords: [uv_l, u1],
                    color: c,
                });
            }
            verts
        };

        let old_verts = make_postprocess(-dir * offset, true);
        let new_verts = make_postprocess(dir * (scroll_distance - offset), false);
        self.submit_scroll_two_quad_pass(
            surface_view,
            old_bind_group,
            new_bind_group,
            &old_verts,
            &new_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }

    /// TypewriterReveal: new lines appear character-by-character (simulated with strips).
    fn render_scroll_typewriter(
        &self,
        surface_view: &wgpu::TextureView,
        old_bind_group: &wgpu::BindGroup,
        new_bind_group: &wgpu::BindGroup,
        t: f32,
        elapsed_secs: f32,
        direction: i32,
        bounds: &neomacs_display_protocol::types::Rect,
        _scroll_distance: f32,
        surface_width: u32,
        surface_height: u32,
    ) {
        let (sx, sy, sw, sh, _w, _h, uv_l, uv_t, uv_r, uv_b) =
            match self.scroll_scissor_and_uv(bounds, surface_width, surface_height) {
                Some(v) => v,
                None => return,
            };

        let dir = direction as f32;
        let num_strips = 20;
        let strip_h = bounds.height / num_strips as f32;
        let uv_strip_h = (uv_b - uv_t) / num_strips as f32;
        let stagger = 0.04; // 40ms per line

        // Old content fades out quickly
        let old_verts: Vec<GlyphVertex> = {
            let alpha = (1.0 - t * 2.0).max(0.0);
            let x0 = bounds.x;
            let x1 = bounds.x + bounds.width;
            let y0 = bounds.y;
            let y1 = bounds.y + bounds.height;
            vec![
                GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, uv_t],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x1, y0],
                    tex_coords: [uv_r, uv_t],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, uv_b],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x0, y0],
                    tex_coords: [uv_l, uv_t],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x1, y1],
                    tex_coords: [uv_r, uv_b],
                    color: [1.0, 1.0, 1.0, alpha],
                },
                GlyphVertex {
                    position: [x0, y1],
                    tex_coords: [uv_l, uv_b],
                    color: [1.0, 1.0, 1.0, alpha],
                },
            ]
        };

        // New content: each line reveals left-to-right with stagger
        let new_verts: Vec<GlyphVertex> = {
            let mut verts = Vec::with_capacity(num_strips * 6);
            for i in 0..num_strips {
                let line_delay = i as f32 * stagger;
                let line_t = ((t - line_delay).max(0.0) / (1.0 - line_delay).max(0.01)).min(1.0);

                // Reveal from left: only show portion of UV
                let reveal = line_t;
                let uv_reveal_right = uv_l + (uv_r - uv_l) * reveal;
                let x_right = bounds.x + bounds.width * reveal;

                let y0 = bounds.y + i as f32 * strip_h;
                let y1 = bounds.y + (i + 1) as f32 * strip_h;
                let u0 = uv_t + i as f32 * uv_strip_h;
                let u1 = uv_t + (i + 1) as f32 * uv_strip_h;
                let alpha = line_t;

                verts.push(GlyphVertex {
                    position: [bounds.x, y0],
                    tex_coords: [uv_l, u0],
                    color: [1.0, 1.0, 1.0, alpha],
                });
                verts.push(GlyphVertex {
                    position: [x_right, y0],
                    tex_coords: [uv_reveal_right, u0],
                    color: [1.0, 1.0, 1.0, alpha],
                });
                verts.push(GlyphVertex {
                    position: [x_right, y1],
                    tex_coords: [uv_reveal_right, u1],
                    color: [1.0, 1.0, 1.0, alpha],
                });
                verts.push(GlyphVertex {
                    position: [bounds.x, y0],
                    tex_coords: [uv_l, u0],
                    color: [1.0, 1.0, 1.0, alpha],
                });
                verts.push(GlyphVertex {
                    position: [x_right, y1],
                    tex_coords: [uv_reveal_right, u1],
                    color: [1.0, 1.0, 1.0, alpha],
                });
                verts.push(GlyphVertex {
                    position: [bounds.x, y1],
                    tex_coords: [uv_l, u1],
                    color: [1.0, 1.0, 1.0, alpha],
                });
            }
            verts
        };

        self.submit_scroll_two_quad_pass(
            surface_view,
            old_bind_group,
            new_bind_group,
            &old_verts,
            &new_verts,
            sx,
            sy,
            sw,
            sh,
        );
    }
}
