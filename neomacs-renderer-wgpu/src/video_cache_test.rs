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
