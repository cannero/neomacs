//! Video cache with GStreamer backend and optional VA-API hardware acceleration.
//!
//! Provides async video decoding with DMA-BUF zero-copy when available,
//! falling back to CPU decode + copy otherwise.
#![allow(dead_code)]

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::thread;

use gstreamer as gst;
use gstreamer::prelude::*;
#[cfg(target_os = "linux")]
use gstreamer_allocators as gst_allocators;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

/// Video playback state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoState {
    /// Video is loading/buffering
    Loading,
    /// Video is playing
    Playing,
    /// Video is paused
    Paused,
    /// Video playback stopped
    Stopped,
    /// Video reached end
    EndOfStream,
    /// Error occurred
    Error,
}

impl VideoState {
    /// Whether this state should keep the render loop actively redrawing.
    ///
    /// `Loading` must be considered active so initial decoded frames get
    /// uploaded promptly, even before state transitions to `Playing`.
    #[inline]
    fn keeps_render_loop_active(self) -> bool {
        matches!(self, VideoState::Loading | VideoState::Playing)
    }
}

/// DMA-BUF information for zero-copy path.
///
/// Owns the file descriptor — Drop closes it automatically.
/// The fd is either dup'd from GStreamer memory or created by vaExportSurfaceHandle().
#[cfg(target_os = "linux")]
pub struct DmaBufInfo {
    /// File descriptor (owned — will be closed on drop)
    pub fd: RawFd,
    /// Stride (bytes per row)
    pub stride: u32,
    /// DRM fourcc format code
    pub fourcc: u32,
    /// DRM modifier
    pub modifier: u64,
}

#[cfg(target_os = "linux")]
impl Drop for DmaBufInfo {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe {
                libc::close(self.fd);
            }
            tracing::trace!("Closed DMA-BUF fd {}", self.fd);
        }
    }
}

/// Decoded video frame ready for rendering
pub struct DecodedFrame {
    /// Frame ID
    pub id: u32,
    /// Video ID this frame belongs to
    pub video_id: u32,
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// RGBA pixel data (CPU path) - empty if using DMA-BUF
    pub data: Vec<u8>,
    /// DMA-BUF info for zero-copy (Linux only)
    #[cfg(target_os = "linux")]
    pub dmabuf: Option<DmaBufInfo>,
    /// Presentation timestamp in nanoseconds
    pub pts: u64,
    /// Duration in nanoseconds
    pub duration: u64,
}

/// Cached video with GStreamer pipeline
pub struct CachedVideo {
    /// Video ID
    pub id: u32,
    /// Video dimensions
    pub width: u32,
    pub height: u32,
    /// Current state
    pub state: VideoState,
    /// Current wgpu texture (updated each frame)
    pub texture: Option<wgpu::Texture>,
    pub texture_view: Option<wgpu::TextureView>,
    pub bind_group: Option<wgpu::BindGroup>,
    /// Frame count
    pub frame_count: u64,
    /// Loop count (-1 = infinite), shared with decode thread
    pub loop_count: Arc<AtomicI32>,
}

/// Request to load a video
struct LoadRequest {
    id: u32,
    path: String,
    loop_count: Arc<AtomicI32>,
}

/// Video pipeline with frame extraction
struct VideoPipeline {
    pipeline: gst::Pipeline,
    appsink: gst_video::VideoSink,
}

/// Video cache managing multiple videos with async decoding
pub struct VideoCache {
    /// Cached videos by ID
    videos: HashMap<u32, CachedVideo>,
    /// Next video ID
    next_id: u32,
    /// Channel to send load requests
    load_tx: mpsc::Sender<LoadRequest>,
    /// Channel to receive decoded frames
    frame_rx: mpsc::Receiver<DecodedFrame>,
    /// Bind group layout for video textures (created in init_gpu)
    bind_group_layout: Option<wgpu::BindGroupLayout>,
    /// Sampler for video textures (created in init_gpu)
    sampler: Option<wgpu::Sampler>,
}

impl VideoCache {
    /// Create a new video cache
    pub fn new() -> Self {
        // Initialize GStreamer
        if let Err(e) = gst::init() {
            tracing::error!("Failed to initialize GStreamer: {}", e);
        }

        let (load_tx, load_rx) = mpsc::channel::<LoadRequest>();
        // Bounded channel: caps the number of decoded frames (and their open
        // DMA-BUF fds) waiting for the render thread.  When the channel is full,
        // the decode thread blocks — providing natural backpressure instead of
        // accumulating hundreds of open fds that exhaust the process limit.
        let (frame_tx, frame_rx) = mpsc::sync_channel::<DecodedFrame>(4);

        // Spawn decoder thread
        thread::spawn(move || {
            Self::decoder_thread(load_rx, frame_tx);
        });

        Self {
            videos: HashMap::new(),
            next_id: 1,
            load_tx,
            frame_rx,
            bind_group_layout: None,
            sampler: None,
        }
    }

    /// Initialize GPU resources
    pub fn init_gpu(&mut self, device: &wgpu::Device) {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Video Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Video Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        self.bind_group_layout = Some(bind_group_layout);
        self.sampler = Some(sampler);
        tracing::info!("VideoCache: GPU resources initialized");
    }

    /// Load a video file
    pub fn load_file(&mut self, path: &str) -> u32 {
        let id = self.next_id;
        self.next_id += 1;

        // Create placeholder entry
        let loop_count = Arc::new(AtomicI32::new(0));
        self.videos.insert(
            id,
            CachedVideo {
                id,
                width: 0,
                height: 0,
                state: VideoState::Loading,
                texture: None,
                texture_view: None,
                bind_group: None,
                frame_count: 0,
                loop_count: Arc::clone(&loop_count),
            },
        );

        // Send load request
        let _ = self.load_tx.send(LoadRequest {
            id,
            path: path.to_string(),
            loop_count,
        });

        tracing::info!("VideoCache: queued video {} for loading: {}", id, path);
        id
    }

    /// Get video state
    pub fn get_state(&self, id: u32) -> Option<VideoState> {
        self.videos.get(&id).map(|v| v.state)
    }

    /// Get video dimensions
    pub fn get_dimensions(&self, id: u32) -> Option<(u32, u32)> {
        self.videos.get(&id).map(|v| (v.width, v.height))
    }

    /// Get video for rendering
    pub fn get(&self, id: u32) -> Option<&CachedVideo> {
        self.videos.get(&id)
    }

    /// Play video
    pub fn play(&mut self, id: u32) {
        if let Some(video) = self.videos.get_mut(&id) {
            video.state = VideoState::Playing;
            tracing::debug!("VideoCache: play video {}", id);
        }
    }

    /// Pause video
    pub fn pause(&mut self, id: u32) {
        if let Some(video) = self.videos.get_mut(&id) {
            video.state = VideoState::Paused;
            tracing::debug!("VideoCache: pause video {}", id);
        }
    }

    /// Stop video
    pub fn stop(&mut self, id: u32) {
        if let Some(video) = self.videos.get_mut(&id) {
            video.state = VideoState::Stopped;
            tracing::debug!("VideoCache: stop video {}", id);
        }
    }

    /// Set loop count (-1 for infinite)
    pub fn set_loop(&mut self, id: u32, count: i32) {
        if let Some(video) = self.videos.get(&id) {
            video.loop_count.store(count, Ordering::Relaxed);
        }
    }

    /// Remove video from cache
    pub fn remove(&mut self, id: u32) {
        self.videos.remove(&id);
        tracing::debug!("VideoCache: removed video {}", id);
    }

    /// Check if any video is currently in Playing state
    pub fn has_playing_videos(&self) -> bool {
        self.videos
            .values()
            .any(|v| v.state.keeps_render_loop_active())
    }

    /// Process pending decoded frames using stored GPU resources (call each frame)
    pub fn process_pending_frames(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        // Take resources temporarily to avoid borrow conflict
        let layout = match self.bind_group_layout.take() {
            Some(l) => l,
            None => {
                tracing::warn!(
                    "VideoCache: GPU resources not initialized, skipping frame processing"
                );
                return;
            }
        };
        let sampler = match self.sampler.take() {
            Some(s) => s,
            None => {
                self.bind_group_layout = Some(layout);
                tracing::warn!(
                    "VideoCache: GPU resources not initialized, skipping frame processing"
                );
                return;
            }
        };

        // Process frames
        self.process_pending(device, queue, &layout, &sampler);

        // Put resources back
        self.bind_group_layout = Some(layout);
        self.sampler = Some(sampler);
    }

    /// Process pending decoded frames (call each frame)
    /// Uses the provided bind_group_layout and sampler from image_cache
    /// to ensure compatibility with the shared image/video rendering pipeline.
    pub fn process_pending(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bind_group_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
    ) {
        // Drain queue quickly, then keep only the latest frame per video ID.
        // This bounds upload work per tick and avoids long stalls when decode
        // threads outpace rendering (e.g., two concurrent 4K videos).
        let mut drained_frames = Vec::new();
        while let Ok(frame) = self.frame_rx.try_recv() {
            drained_frames.push(frame);
        }
        if drained_frames.is_empty() {
            return;
        }

        let drained_count = drained_frames.len();
        let latest_frames = Self::coalesce_latest_frames(drained_frames);
        if drained_count > latest_frames.len() {
            tracing::debug!(
                "VideoCache::process_pending coalesced {} queued frames into {} uploads",
                drained_count,
                latest_frames.len()
            );
        }

        for frame in latest_frames.into_values() {
            self.process_single_frame(device, queue, bind_group_layout, sampler, frame);
        }
    }

    fn coalesce_latest_frames<I>(frames: I) -> HashMap<u32, DecodedFrame>
    where
        I: IntoIterator<Item = DecodedFrame>,
    {
        let mut latest = HashMap::new();
        for frame in frames {
            latest.insert(frame.video_id, frame);
        }
        latest
    }

    fn process_single_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bind_group_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        mut frame: DecodedFrame,
    ) {
        let total = self
            .videos
            .get(&frame.video_id)
            .map(|v| v.frame_count)
            .unwrap_or(0)
            + 1;
        tracing::trace!(
            "VideoCache::process_pending received frame #{} for video {}, pts={}ms, size={}x{}",
            total,
            frame.video_id,
            frame.pts / 1_000_000,
            frame.width,
            frame.height
        );

        if let Some(video) = self.videos.get_mut(&frame.video_id) {
            // Check if we need to create new texture (first frame or size changed)
            let need_new_texture = video.texture.is_none()
                || video.width != frame.width
                || video.height != frame.height;

            if need_new_texture {
                // Update dimensions
                video.width = frame.width;
                video.height = frame.height;
                if video.state == VideoState::Loading {
                    video.state = VideoState::Playing;
                }

                // Create new texture (only when dimensions change)
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("Video Frame Texture"),
                    size: wgpu::Extent3d {
                        width: frame.width,
                        height: frame.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });

                let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());

                // Create bind group
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Video Bind Group"),
                    layout: bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&texture_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                    ],
                });

                video.texture = Some(texture);
                video.texture_view = Some(texture_view);
                video.bind_group = Some(bind_group);
            }

            // Update texture data
            // Try DMA-BUF zero-copy path first, fall back to CPU copy
            #[cfg(target_os = "linux")]
            let dmabuf_imported = if let Some(dmabuf) = frame.dmabuf.take() {
                // Import DMA-BUF directly as the video texture (zero-copy).
                use crate::external_buffer::DmaBufBuffer;

                let dmabuf_buffer = DmaBufBuffer::single_plane(
                    dmabuf.fd,
                    frame.width,
                    frame.height,
                    dmabuf.stride,
                    dmabuf.fourcc,
                    dmabuf.modifier,
                );

                if let Some(imported_texture) = dmabuf_buffer.to_wgpu_texture(device, queue) {
                    tracing::debug!(
                        "DMA-BUF zero-copy import successful for video {}",
                        frame.video_id
                    );

                    // Drop old resources in dependency order so wgpu can
                    // schedule deferred destruction.  Do NOT call
                    // device.poll(Wait) here — that forces a full GPU idle
                    // per video frame, and each sync cycle creates RADV
                    // syncobj fds that accumulate and exhaust the fd limit.
                    // wgpu will process the deferred destruction during the
                    // main render loop's normal queue.submit() / poll() calls.
                    drop(video.bind_group.take());
                    drop(video.texture_view.take());
                    drop(video.texture.take());

                    // Install new DMA-BUF texture
                    let texture_view =
                        imported_texture.create_view(&wgpu::TextureViewDescriptor::default());
                    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("Video DMA-BUF Bind Group"),
                        layout: bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&texture_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(sampler),
                            },
                        ],
                    });

                    video.texture = Some(imported_texture);
                    video.texture_view = Some(texture_view);
                    video.bind_group = Some(bind_group);
                    true
                } else {
                    tracing::debug!("DMA-BUF import failed, falling back to CPU copy");
                    false
                }
            } else {
                false
            };

            #[cfg(not(target_os = "linux"))]
            let dmabuf_imported = false;

            // Fall back to CPU copy if DMA-BUF import failed or not available
            if !dmabuf_imported && !frame.data.is_empty() {
                // If the existing texture was a zero-copy DMA-BUF import, it
                // lacks COPY_DST usage and cannot be written to.  Recreate a
                // CPU-writable texture in that case.
                let needs_cpu_texture = match video.texture {
                    Some(ref t) => !t.usage().contains(wgpu::TextureUsages::COPY_DST),
                    None => true,
                };
                if needs_cpu_texture {
                    let texture = device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("Video Frame Texture"),
                        size: wgpu::Extent3d {
                            width: frame.width,
                            height: frame.height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("Video CPU Fallback Bind Group"),
                        layout: bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&texture_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(sampler),
                            },
                        ],
                    });
                    video.texture = Some(texture);
                    video.texture_view = Some(texture_view);
                    video.bind_group = Some(bind_group);
                }
                if let Some(ref texture) = video.texture {
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        &frame.data,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(frame.width * 4),
                            rows_per_image: Some(frame.height),
                        },
                        wgpu::Extent3d {
                            width: frame.width,
                            height: frame.height,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }

            video.frame_count += 1;
            tracing::trace!(
                "VideoCache: updated video {} frame {}",
                frame.video_id,
                video.frame_count
            );
        }
    }

    /// Try to extract DMA-BUF info from a GStreamer buffer
    ///
    /// Supports multiple memory types:
    /// - DmaBufMemory: Direct DMA-BUF allocator
    /// - FdMemory: Generic fd-backed memory (includes VA-API memory)
    #[cfg(target_os = "linux")]
    fn try_extract_dmabuf(
        buffer: &gst::BufferRef,
        info: &gst_video::VideoInfo,
    ) -> Option<DmaBufInfo> {
        // Get the first memory block from the buffer
        let n_memory = buffer.n_memory();
        if n_memory == 0 {
            return None;
        }

        let memory = buffer.memory(0)?;

        // Debug: log allocator info
        let allocator_type = if let Some(allocator) = memory.allocator() {
            use gst::prelude::*;
            let type_name = allocator.type_().name();
            tracing::debug!("Memory allocator type: {}", type_name);
            type_name.to_string()
        } else {
            tracing::debug!("Memory has no allocator");
            String::new()
        };

        // Try DmaBufMemory first (explicit DMA-BUF allocator)
        // IMPORTANT: dup() GStreamer fds so DmaBufInfo owns a copy — the original
        // fd is owned by GStreamer and becomes invalid when the sample is dropped.
        let fd = if let Some(dmabuf_mem) =
            memory.downcast_memory_ref::<gst_allocators::DmaBufMemory>()
        {
            tracing::debug!("Found DmaBufMemory");
            let raw_fd = dmabuf_mem.fd();
            let duped = unsafe { libc::dup(raw_fd) };
            if duped < 0 {
                tracing::warn!("Failed to dup DmaBufMemory fd {}", raw_fd);
                return None;
            }
            duped
        } else if let Some(fd_mem) = memory.downcast_memory_ref::<gst_allocators::FdMemory>() {
            // FdMemory: generic fd-backed memory (VA-API uses this)
            tracing::debug!("Found FdMemory (VA-API or other fd-backed)");
            let raw_fd = fd_mem.fd();
            let duped = unsafe { libc::dup(raw_fd) };
            if duped < 0 {
                tracing::warn!("Failed to dup FdMemory fd {}", raw_fd);
                return None;
            }
            duped
        } else if allocator_type == "GstVaAllocator" {
            // VA-API memory - try to export via vaExportSurfaceHandle
            tracing::debug!("Attempting VA-API DMA-BUF export...");
            return Self::try_export_va_surface(buffer, &memory, info);
        } else {
            tracing::debug!("Buffer memory is not fd-backed (DmaBuf or Fd)");
            return None;
        };

        // Validate fd
        if fd < 0 {
            tracing::warn!("Invalid fd from memory: {}", fd);
            return None;
        }

        // Get stride from video info
        let stride = info.stride()[0] as u32;

        // Determine fourcc from format
        let format = info.format();
        let fourcc = match format {
            gst_video::VideoFormat::Rgba => 0x34324142, // AB24 - actually RGBA maps to ABGR in DRM
            gst_video::VideoFormat::Bgra => 0x34324241, // AR24 - BGRA maps to ARGB in DRM
            gst_video::VideoFormat::Argb => 0x34325241, // RA24
            gst_video::VideoFormat::Abgr => 0x34324152, // RA24
            gst_video::VideoFormat::Rgbx => 0x34325842, // XB24
            gst_video::VideoFormat::Bgrx => 0x34325258, // XR24
            gst_video::VideoFormat::Nv12 => 0x3231564e, // NV12
            _ => {
                tracing::debug!("Unsupported video format for DMA-BUF: {:?}", format);
                return None;
            }
        };

        tracing::info!(
            "Extracted DMA-BUF: fd={}, stride={}, format={:?}, fourcc={:#x}",
            fd,
            stride,
            format,
            fourcc
        );

        Some(DmaBufInfo {
            fd,
            stride,
            fourcc,
            modifier: 0, // Linear modifier - VA-API typically uses linear
        })
    }

    /// Try to export VA-API surface as DMA-BUF
    #[cfg(target_os = "linux")]
    fn try_export_va_surface(
        buffer: &gst::BufferRef,
        memory: &gst::Memory,
        info: &gst_video::VideoInfo,
    ) -> Option<DmaBufInfo> {
        use crate::va_dmabuf_export::{get_va_display_from_memory, try_export_va_dmabuf};

        // Get VA display from allocator
        let va_display = get_va_display_from_memory(memory)?;

        // Export surface as DMA-BUF
        let mut export = try_export_va_dmabuf(buffer, va_display)?;

        // Use the first fd and plane info
        if export.num_planes == 0 || export.fds[0] < 0 {
            tracing::warn!("VA export returned no valid planes");
            // Drop will close all valid fds automatically
            return None;
        }

        tracing::info!(
            "VA-API DMA-BUF export: fd={}, pitch={}, fourcc={:#x}, modifier={:#x}",
            export.fds[0],
            export.pitches[0],
            export.fourcc,
            export.modifier
        );

        // Take ownership of fds[0] — set to -1 so VaDmaBufExport::drop skips it.
        // VaDmaBufExport::drop will close any remaining fds (fds[1..]) automatically.
        let fd = export.fds[0];
        export.fds[0] = -1;

        Some(DmaBufInfo {
            fd,
            stride: export.pitches[0],
            fourcc: export.fourcc,
            modifier: export.modifier,
        })
    }

    /// Background decoder thread — dispatches each video to its own thread
    fn decoder_thread(rx: mpsc::Receiver<LoadRequest>, tx: mpsc::SyncSender<DecodedFrame>) {
        tracing::debug!("Video decoder thread started");

        while let Ok(request) = rx.recv() {
            tracing::info!(
                "Decoder thread: dispatching video {}: {}",
                request.id,
                request.path
            );
            let tx_clone = tx.clone();
            let loop_count = request.loop_count;
            // Spawn a dedicated thread per video so multiple videos load/play concurrently
            thread::spawn(move || {
                Self::decode_single_video(request.id, &request.path, tx_clone, loop_count);
            });
        }

        tracing::debug!("Video decoder thread exiting");
    }

    /// Decode a single video: create pipeline, pull frames, wait for EOS, cleanup.
    fn decode_single_video(
        video_id: u32,
        raw_path: &str,
        tx: mpsc::SyncSender<DecodedFrame>,
        loop_count: Arc<AtomicI32>,
    ) {
        tracing::info!("Video thread: loading video {}: {}", video_id, raw_path);

        // Strip file:// prefix if present (filesrc needs raw paths)
        let path = if raw_path.starts_with("file://") {
            &raw_path[7..]
        } else {
            raw_path
        };

        // Check if VA-API hardware acceleration is available
        let has_vapostproc = gst::ElementFactory::find("vapostproc").is_some();

        // Create GStreamer pipeline
        // NOTE: vapostproc does YUV→RGB conversion but doesn't respect downstream
        // colorimetry caps (GitLab issue #80). For BT.2020 content (10-bit VP9/AV1),
        // colors may be slightly off.
        let pipeline_str = if has_vapostproc {
            tracing::info!("Using VA-API hardware acceleration pipeline with CPU upload");
            // VA-API decodes on GPU, vapostproc does YUV→RGB on GPU,
            // then the result is downloaded to system memory for CPU upload
            // via queue.write_texture().  This avoids per-frame Vulkan HAL
            // imports (DMA-BUF → VkImage → wgpu texture) which leak sync fds
            // on AMD RADV (~0.3 fds per import, exhausting ulimit within
            // seconds of 4K video playback).
            format!(
                "filesrc location=\"{}\" ! decodebin ! \
                 queue max-size-buffers=3 ! vapostproc ! \
                 video/x-raw,format=RGBA ! appsink name=sink",
                path.replace("\"", "\\\"")
            )
        } else {
            tracing::info!("VA-API not available, using software decoding");
            format!(
                "filesrc location=\"{}\" ! decodebin ! \
                 queue ! videoconvert ! video/x-raw,format=RGBA ! appsink name=sink",
                path.replace("\"", "\\\"")
            )
        };

        tracing::debug!("Creating GStreamer pipeline: {}", pipeline_str);

        let pipeline = match gst::parse::launch(&pipeline_str) {
            Ok(elem) => match elem.dynamic_cast::<gst::Pipeline>() {
                Ok(p) => p,
                Err(_) => {
                    tracing::error!("Failed to cast pipeline element for video {}", video_id);
                    return;
                }
            },
            Err(e) => {
                tracing::error!("Failed to create pipeline for video {}: {}", video_id, e);
                return;
            }
        };

        // Get appsink
        let sink_element = match pipeline.by_name("sink") {
            Some(e) => e,
            None => {
                tracing::error!("Could not get appsink element for video {}", video_id);
                let _ = pipeline.set_state(gst::State::Null);
                return;
            }
        };
        let appsink = match sink_element.dynamic_cast::<gst_app::AppSink>() {
            Ok(s) => s,
            Err(_) => {
                tracing::error!("Could not cast sink to AppSink for video {}", video_id);
                let _ = pipeline.set_state(gst::State::Null);
                return;
            }
        };

        // Configure appsink for pull mode
        appsink.set_max_buffers(2);
        appsink.set_drop(true);

        // Start playing
        tracing::debug!("Setting pipeline to Playing state for video {}", video_id);
        if let Err(e) = pipeline.set_state(gst::State::Playing) {
            tracing::error!("Failed to start pipeline for video {}: {:?}", video_id, e);
            let _ = pipeline.set_state(gst::State::Null);
            return;
        }
        tracing::info!("Pipeline started successfully for video {}", video_id);

        // Spawn frame pulling thread
        let appsink_clone = appsink.clone();
        let pipeline_weak = pipeline.downgrade();
        let tx_puller = tx.clone();
        let loop_count_puller = Arc::clone(&loop_count);
        thread::spawn(move || {
            tracing::info!("Frame puller thread started for video {}", video_id);

            // Wait for pipeline to reach PLAYING state
            if let Some(pipeline) = pipeline_weak.upgrade() {
                let (res, state, _) = pipeline.state(gst::ClockTime::from_seconds(5));
                tracing::info!(
                    "Video {} pipeline state: {:?}, result: {:?}",
                    video_id,
                    state,
                    res
                );
            }
            let mut frame_count = 0u64;
            let mut timeout_count = 0u64;

            loop {
                match appsink_clone.try_pull_sample(gst::ClockTime::from_mseconds(100)) {
                    Some(sample) => {
                        timeout_count = 0;
                        frame_count += 1;
                        if let Some(buffer) = sample.buffer() {
                            if let Some(caps) = sample.caps() {
                                if let Ok(info) = gst_video::VideoInfo::from_caps(caps) {
                                    let width = info.width();
                                    let height = info.height();

                                    // DMA-BUF extraction is disabled: the Vulkan HAL
                                    // import path (DmaBufBuffer → VkImage → wgpu texture)
                                    // creates per-frame sync fds on AMD RADV that
                                    // accumulate and exhaust the process fd limit.
                                    // CPU upload via queue.write_texture() is used instead.
                                    #[cfg(target_os = "linux")]
                                    let dmabuf_info: Option<
                                        DmaBufInfo,
                                    > = None;
                                    #[cfg(not(target_os = "linux"))]
                                    let dmabuf_info: Option<()> = None;

                                    let has_dmabuf = dmabuf_info.is_some();
                                    if frame_count <= 5 || frame_count % 60 == 0 {
                                        tracing::info!(
                                            "Frame #{} for video {}, {}x{}, format={:?}, DMA-BUF: {}",
                                            frame_count,
                                            video_id,
                                            width,
                                            height,
                                            info.format(),
                                            has_dmabuf
                                        );
                                    }

                                    let data = if let Ok(map) = buffer.map_readable() {
                                        map.as_slice().to_vec()
                                    } else if has_dmabuf {
                                        tracing::debug!(
                                            "DMA-BUF memory not mappable (expected for zero-copy)"
                                        );
                                        Vec::new()
                                    } else {
                                        tracing::warn!(
                                            "Failed to map buffer and no DMA-BUF available"
                                        );
                                        Vec::new()
                                    };

                                    if tx_puller
                                        .send(DecodedFrame {
                                            id: frame_count as u32,
                                            video_id,
                                            width,
                                            height,
                                            data,
                                            #[cfg(target_os = "linux")]
                                            dmabuf: dmabuf_info,
                                            pts: buffer.pts().map(|p| p.nseconds()).unwrap_or(0),
                                            duration: buffer
                                                .duration()
                                                .map(|d| d.nseconds())
                                                .unwrap_or(0),
                                        })
                                        .is_err()
                                    {
                                        tracing::debug!("Frame receiver dropped, stopping puller");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    None => {
                        timeout_count += 1;
                        if appsink_clone.is_eos() {
                            let lc = loop_count_puller.load(Ordering::Relaxed);
                            if lc == -1 || lc > 0 {
                                // Seek back to start for looping
                                if let Some(pipeline) = pipeline_weak.upgrade() {
                                    if let Err(e) = pipeline.seek_simple(
                                        gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                                        gst::ClockTime::ZERO,
                                    ) {
                                        tracing::error!("Video {} seek failed: {}", video_id, e);
                                        break;
                                    }
                                } else {
                                    tracing::error!(
                                        "Video {} pipeline dropped during loop seek",
                                        video_id
                                    );
                                    break;
                                }
                                if lc > 0 {
                                    loop_count_puller.fetch_sub(1, Ordering::Relaxed);
                                }
                                tracing::info!(
                                    "Video {} looping (remaining={})",
                                    video_id,
                                    if lc == -1 {
                                        "infinite".to_string()
                                    } else {
                                        (lc - 1).to_string()
                                    }
                                );
                                timeout_count = 0;
                                continue;
                            }
                            tracing::info!(
                                "Video {} reached EOS after {} frames",
                                video_id,
                                frame_count
                            );
                            break;
                        }
                        if timeout_count == 1 || timeout_count % 50 == 0 {
                            tracing::debug!(
                                "Video {} pull timeout #{}, frames so far: {}",
                                video_id,
                                timeout_count,
                                frame_count
                            );
                        }
                    }
                }
            }
            tracing::debug!("Frame puller thread exiting for video {}", video_id);
        });

        // Wait for EOS or error on bus (this thread is dedicated to this video,
        // so blocking here doesn't prevent other videos from loading)
        let bus = match pipeline.bus() {
            Some(b) => b,
            None => {
                tracing::error!("Could not get bus from pipeline for video {}", video_id);
                let _ = pipeline.set_state(gst::State::Null);
                return;
            }
        };
        for msg in bus.iter_timed(gst::ClockTime::NONE) {
            match msg.view() {
                gst::MessageView::Eos(..) => {
                    let lc = loop_count.load(Ordering::Relaxed);
                    if lc == 0 {
                        tracing::debug!("Video {} bus: end of stream (no loop)", video_id);
                        break;
                    }
                    // Frame puller will handle seek, just continue waiting
                    tracing::debug!("Video {} bus: EOS but looping (count={})", video_id, lc);
                }
                gst::MessageView::Error(err) => {
                    tracing::error!(
                        "Video {} error: {} ({:?})",
                        video_id,
                        err.error(),
                        err.debug()
                    );
                    break;
                }
                _ => {}
            }
        }

        // Cleanup
        let _ = pipeline.set_state(gst::State::Null);
        tracing::debug!("Video {} pipeline cleaned up", video_id);
    }
}

impl Default for VideoCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodedFrame, VideoCache, VideoState};

    fn frame(video_id: u32, id: u32, pts: u64) -> DecodedFrame {
        DecodedFrame {
            id,
            video_id,
            width: 320,
            height: 180,
            data: Vec::new(),
            #[cfg(target_os = "linux")]
            dmabuf: None,
            pts,
            duration: 16_666_667,
        }
    }

    #[test]
    fn loading_and_playing_states_keep_render_loop_active() {
        assert!(VideoState::Loading.keeps_render_loop_active());
        assert!(VideoState::Playing.keeps_render_loop_active());
        assert!(!VideoState::Paused.keeps_render_loop_active());
        assert!(!VideoState::Stopped.keeps_render_loop_active());
        assert!(!VideoState::EndOfStream.keeps_render_loop_active());
        assert!(!VideoState::Error.keeps_render_loop_active());
    }

    #[test]
    fn coalesce_latest_frames_keeps_only_most_recent_per_video() {
        let latest = VideoCache::coalesce_latest_frames(vec![
            frame(1, 1, 100),
            frame(2, 1, 120),
            frame(1, 2, 140),
            frame(2, 2, 160),
            frame(1, 3, 180),
        ]);

        assert_eq!(latest.len(), 2);
        assert_eq!(latest.get(&1).map(|f| f.id), Some(3));
        assert_eq!(latest.get(&2).map(|f| f.id), Some(2));
        assert_eq!(latest.get(&1).map(|f| f.pts), Some(180));
        assert_eq!(latest.get(&2).map(|f| f.pts), Some(160));
    }
}
