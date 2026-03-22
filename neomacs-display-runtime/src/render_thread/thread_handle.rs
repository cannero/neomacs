use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::bootstrap::{build_render_event_loop, run_render_loop_with_event_loop};
use super::{SharedImageDimensions, SharedMonitorInfo};
use crate::thread_comm::RenderComms;

/// Render thread state.
pub struct RenderThread {
    handle: Option<JoinHandle<()>>,
}

impl RenderThread {
    fn finish_spawn(
        handle: JoinHandle<()>,
        startup_rx: mpsc::Receiver<Result<(), String>>,
        startup_timeout: Duration,
    ) -> Result<Self, String> {
        match startup_rx.recv_timeout(startup_timeout) {
            Ok(Ok(())) => Ok(Self {
                handle: Some(handle),
            }),
            Ok(Err(err)) => {
                let _ = handle.join();
                Err(err)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(format!(
                "Timed out waiting {:?} for render thread startup",
                startup_timeout
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = handle.join();
                Err("Render thread terminated before reporting startup status".to_string())
            }
        }
    }

    fn spawn_with_state_timeout<T, S, R>(
        startup: S,
        runner: R,
        startup_timeout: Duration,
    ) -> Result<Self, String>
    where
        T: Send + 'static,
        S: FnOnce() -> Result<T, String> + Send + 'static,
        R: FnOnce(T) -> Result<(), String> + Send + 'static,
    {
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let handle = thread::spawn(move || match startup() {
            Ok(state) => {
                let _ = startup_tx.send(Ok(()));
                if let Err(err) = runner(state) {
                    tracing::error!("Render thread exited with error: {}", err);
                }
            }
            Err(err) => {
                let _ = startup_tx.send(Err(err));
            }
        });

        Self::finish_spawn(handle, startup_rx, startup_timeout)
    }

    fn spawn_with_state<T, S, R>(startup: S, runner: R) -> Result<Self, String>
    where
        T: Send + 'static,
        S: FnOnce() -> Result<T, String> + Send + 'static,
        R: FnOnce(T) -> Result<(), String> + Send + 'static,
    {
        Self::spawn_with_state_timeout(startup, runner, Duration::from_secs(10))
    }

    /// Spawn the render thread.
    pub fn spawn(
        comms: RenderComms,
        width: u32,
        height: u32,
        title: String,
        image_dimensions: SharedImageDimensions,
        shared_monitors: SharedMonitorInfo,
        #[cfg(feature = "neo-term")] shared_terminals: crate::terminal::SharedTerminals,
    ) -> Result<Self, String> {
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let handle = thread::spawn(move || match build_render_event_loop() {
            Ok(event_loop) => {
                let _ = startup_tx.send(Ok(()));
                if let Err(err) = run_render_loop_with_event_loop(
                    event_loop,
                    comms,
                    width,
                    height,
                    title,
                    image_dimensions,
                    shared_monitors,
                    #[cfg(feature = "neo-term")]
                    shared_terminals,
                ) {
                    tracing::error!("Render thread exited with error: {}", err);
                }
            }
            Err(err) => {
                let _ = startup_tx.send(Err(err));
            }
        });

        Self::finish_spawn(handle, startup_rx, Duration::from_secs(10))
    }

    /// Wait for render thread to finish.
    pub fn join(mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RenderThread;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn spawn_with_state_returns_error_when_startup_fails() {
        let err = match RenderThread::spawn_with_state::<(), _, _>(
            || Err("boom".to_string()),
            |_| Ok(()),
        ) {
            Ok(_) => panic!("startup failure should be surfaced to caller"),
            Err(err) => err,
        };

        assert_eq!(err, "boom");
    }

    #[test]
    fn spawn_with_state_returns_after_successful_startup() {
        let (done_tx, done_rx) = mpsc::sync_channel(0);
        let handle = RenderThread::spawn_with_state(
            || Ok(()),
            move |_| {
                done_rx
                    .recv()
                    .expect("test should unblock render-thread runner");
                Ok(())
            },
        )
        .expect("startup success should return render handle");

        done_tx
            .send(())
            .expect("spawn should return before runner exits");
        handle.join();
    }

    #[test]
    fn spawn_with_state_returns_timeout_when_startup_never_reports() {
        let err = match RenderThread::spawn_with_state_timeout(
            || {
                std::thread::sleep(Duration::from_millis(50));
                Ok(())
            },
            |_| Ok(()),
            Duration::from_millis(5),
        ) {
            Ok(_) => panic!("startup timeout should be surfaced to caller"),
            Err(err) => err,
        };

        assert!(err.contains("Timed out waiting"));
    }
}
