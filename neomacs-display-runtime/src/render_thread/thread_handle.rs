use std::thread::{self, JoinHandle};

use super::{SharedImageDimensions, SharedMonitorInfo, run_render_loop};
use crate::thread_comm::RenderComms;

/// Render thread state.
pub struct RenderThread {
    handle: Option<JoinHandle<()>>,
}

impl RenderThread {
    /// Spawn the render thread.
    pub fn spawn(
        comms: RenderComms,
        width: u32,
        height: u32,
        title: String,
        image_dimensions: SharedImageDimensions,
        shared_monitors: SharedMonitorInfo,
        #[cfg(feature = "neo-term")] shared_terminals: crate::terminal::SharedTerminals,
    ) -> Self {
        let handle = thread::spawn(move || {
            run_render_loop(
                comms,
                width,
                height,
                title,
                image_dimensions,
                shared_monitors,
                #[cfg(feature = "neo-term")]
                shared_terminals,
            );
        });

        Self {
            handle: Some(handle),
        }
    }

    /// Wait for render thread to finish.
    pub fn join(mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
