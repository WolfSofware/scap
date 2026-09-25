//! End-to-end capture tests. They need a graphical session and, on macOS,
//! the screen recording permission, so they are ignored by default:
//!
//! ```sh
//! cargo test --test capture -- --ignored --test-threads=1
//! ```
//!
//! On Wayland the video tests open the ScreenCast portal dialog.

use std::{
    collections::HashSet,
    sync::mpsc,
    time::{Duration, Instant},
};

use scap::{
    capturer::{Capturer, Options},
    frame::{AudioData, AudioFrame, Frame, FrameType, NativeFrame, VideoFrame},
    Subscription, SubscriptionError, TargetId,
};

const NEEDS_SESSION: &str = "needs a graphical session and screen capture permission";
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct VideoSummary {
    display_time: u64,
    width: u32,
    height: u32,
    zero_copy: bool,
}

fn summarize(frame: &VideoFrame<'_>) -> VideoSummary {
    VideoSummary {
        display_time: frame.display_time,
        width: frame.width,
        height: frame.height,
        zero_copy: is_zero_copy(&frame.native),
    }
}

/// Checks the frame points at the capture API's own buffer
fn is_zero_copy(native: &NativeFrame<'_>) -> bool {
    match native {
        #[cfg(target_os = "macos")]
        NativeFrame::CoreVideo(sample) => {
            use screencapturekit::prelude::CMSampleBufferExt;
            sample
                .image_buffer()
                .is_some_and(|buffer| buffer.is_backed_by_io_surface())
        }
        #[cfg(target_os = "windows")]
        NativeFrame::Direct3D11 { texture, .. } => {
            let mut desc = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
            unsafe { texture.GetDesc(&mut desc) };
            desc.Width > 0 && desc.Height > 0
        }
        #[cfg(target_os = "linux")]
        NativeFrame::DmaBuf(dmabuf) => !dmabuf.planes().is_empty(),
        #[cfg(target_os = "linux")]
        NativeFrame::Memory { data, stride } => !data.is_empty() && *stride > 0,
    }
}

fn collect_video(target: TargetId, count: usize) -> Vec<VideoSummary> {
    let (sender, receiver) = mpsc::channel();
    let subscription = Subscription::builder(target)
        .fps(30)
        .on_video(move |frame| {
            let _ = sender.send(summarize(frame));
        })
        .start()
        .unwrap_or_else(|error| panic!("subscribing to {target:?} failed: {error}"));

    let deadline = Instant::now() + TIMEOUT;
    let mut frames = Vec::new();
    while frames.len() < count {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(frame) => frames.push(frame),
            Err(_) => break,
        }
    }
    subscription.stop().expect("stopping the subscription failed");
    frames
}

fn assert_frames(frames: &[VideoSummary]) {
    assert!(!frames.is_empty(), "no video frames within {TIMEOUT:?}");
    for frame in frames {
        assert!(frame.width > 0 && frame.height > 0, "empty frame {frame:?}");
        assert!(frame.zero_copy, "frame is not backed by a native buffer: {frame:?}");
    }
    for pair in frames.windows(2) {
        assert!(
            pair[1].display_time >= pair[0].display_time,
            "timestamps went backwards: {pair:?}"
        );
    }
}

#[test]
#[ignore = "needs a graphical session and screen capture permission"]
fn lists_displays_with_ids_and_titles() {
    let displays = scap::get_all_displays().expect(NEEDS_SESSION);
    assert!(!displays.is_empty(), "no displays found");

    let ids = displays.iter().map(|display| display.id).collect::<HashSet<_>>();
    assert_eq!(ids.len(), displays.len(), "display ids are not unique");
    for display in &displays {
        assert!(!display.title.is_empty(), "display {} has no title", display.id);
        let found = scap::targets::find_display(display.id)
            .unwrap()
            .expect("display id does not round trip");
        assert_eq!(found.title, display.title);
    }
}

#[test]
#[ignore = "needs a graphical session and screen capture permission"]
fn lists_windows_and_applications() {
    let windows = scap::get_all_windows().expect(NEEDS_SESSION);
    let ids = windows.iter().map(|window| window.id).collect::<HashSet<_>>();
    assert_eq!(ids.len(), windows.len(), "window ids are not unique");

    let applications = scap::get_all_applications().unwrap();
    let pids = applications
        .iter()
        .map(|application| application.pid)
        .collect::<HashSet<_>>();
    assert_eq!(pids.len(), applications.len(), "application pids are not unique");
    assert!(applications.iter().all(|application| !application.name.is_empty()));

    // Every window with a known owner belongs to a listed application
    for window in &windows {
        if let Some(pid) = window.process_id {
            assert!(pids.contains(&pid), "{window:?} has no application");
        }
    }
}

#[test]
#[ignore = "needs a graphical session and screen capture permission"]
fn subscribes_to_display_video() {
    let display = scap::get_all_displays().expect(NEEDS_SESSION).remove(0);
    assert_frames(&collect_video(TargetId::Display(display.id), 5));
}

#[test]
#[ignore = "needs a graphical session and screen capture permission"]
fn subscribes_to_window_video() {
    let windows = scap::get_all_windows().expect(NEEDS_SESSION);
    let Some(window) = windows.iter().find(|window| !window.title.is_empty()) else {
        eprintln!("skipped: no titled windows are open");
        return;
    };
    assert_frames(&collect_video(TargetId::Window(window.id), 3));
}

#[test]
#[ignore = "needs a graphical session and screen capture permission"]
fn subscribes_to_application_video() {
    let windows = scap::get_all_windows().expect(NEEDS_SESSION);
    let Some(pid) = windows.iter().find_map(|window| window.process_id) else {
        eprintln!("skipped: no window reports its process");
        return;
    };
    assert_frames(&collect_video(TargetId::Application(pid), 3));
}

#[test]
#[ignore = "needs a graphical session and screen capture permission"]
fn subscribes_to_display_audio() {
    let display = scap::get_all_displays().expect(NEEDS_SESSION).remove(0);
    let (sender, receiver) = mpsc::channel();
    let subscription = Subscription::builder(TargetId::Display(display.id))
        .on_audio(move |audio: &AudioFrame<'_>| {
            let bytes = match audio.data {
                AudioData::Interleaved(data) => data.len(),
                AudioData::Planar(planes) => planes.iter().map(|plane| plane.len()).sum(),
            };
            let _ = sender.send((
                audio.sample_rate,
                audio.channels,
                audio.frames,
                bytes,
                audio.format.bytes_per_sample(),
            ));
        })
        .start()
        .expect("subscribing to display audio failed");

    // Silence produces no packets on some platforms, so only check what arrives
    if let Ok((sample_rate, channels, frames, bytes, sample_size)) =
        receiver.recv_timeout(Duration::from_secs(3))
    {
        assert!(sample_rate > 0 && channels > 0);
        assert_eq!(bytes, frames * channels as usize * sample_size);
    }
    subscription.stop().expect("stopping audio failed");
}

/// Set `SCAP_TEST_AUDIO_PID` to a process that is playing sound
#[test]
#[ignore = "needs a graphical session and screen capture permission"]
fn subscribes_to_application_audio() {
    let Some(pid) = std::env::var("SCAP_TEST_AUDIO_PID")
        .ok()
        .and_then(|pid| pid.parse().ok())
    else {
        eprintln!("skipped: SCAP_TEST_AUDIO_PID is not set");
        return;
    };

    let (sender, receiver) = mpsc::channel();
    let subscription = Subscription::builder(TargetId::Application(pid))
        .on_audio(move |audio| {
            let _ = sender.send((audio.frames, audio.display_time));
        })
        .start()
        .expect("subscribing to application audio failed");

    let (frames, _) = receiver
        .recv_timeout(TIMEOUT)
        .expect("no audio from the playing application");
    assert!(frames > 0);
    subscription.stop().expect("stopping audio failed");
}

#[test]
#[ignore = "needs a graphical session and screen capture permission"]
fn unknown_target_is_reported() {
    let result = Subscription::builder(TargetId::Display(u32::MAX - 7))
        .on_video(|_| {})
        .start();
    assert!(
        matches!(result, Err(SubscriptionError::TargetNotFound(_))),
        "expected TargetNotFound, got {:?}",
        result.err()
    );
}

#[test]
#[ignore = "needs a graphical session and screen capture permission"]
fn capturer_returns_frames_of_the_reported_size() {
    let mut capturer = Capturer::build(Options {
        fps: 30,
        output_type: FrameType::BGRAFrame,
        ..Default::default()
    })
    .expect(NEEDS_SESSION);
    let [width, height] = capturer.get_output_frame_size().unwrap();
    capturer.start_capture().unwrap();

    let deadline = Instant::now() + TIMEOUT;
    let frame = loop {
        assert!(Instant::now() < deadline, "no frame within {TIMEOUT:?}");
        match capturer.get_next_frame().unwrap() {
            // Idle frames on macOS are empty
            Frame::BGRA(frame) if frame.width == 0 => continue,
            frame => break frame,
        }
    };
    capturer.stop_capture().unwrap();

    match frame {
        Frame::BGRA(frame) => {
            assert_eq!(frame.data.len(), frame.width as usize * frame.height as usize * 4);
            if width > 0 {
                assert_eq!([frame.width as u32, frame.height as u32], [width, height]);
            }
        }
        // PipeWire picks the pixel layout on Linux
        #[cfg(target_os = "linux")]
        Frame::BGRx(_) | Frame::RGBx(_) | Frame::XBGR(_) => {}
        frame => panic!("unexpected frame type {frame:?}"),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn lists_installed_display_servers() {
    for server in scap::get_installed_display_servers() {
        assert!(!server.name.is_empty(), "{server:?} has no name");
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some() {
        assert!(scap::get_current_display_protocol().is_some());
    }
}
