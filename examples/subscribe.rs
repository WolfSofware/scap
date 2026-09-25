//! Lists capture targets, then streams one of them for five seconds.
//!
//! ```sh
//! cargo run --example subscribe             # main display, video + audio
//! cargo run --example subscribe -- window 1234
//! cargo run --example subscribe -- app 5678
//! ```

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use scap::{frame::NativeFrame, Subscription, TargetId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !scap::has_permission() && !scap::request_permission() {
        return Err("screen capture permission denied".into());
    }

    #[cfg(target_os = "linux")]
    for server in scap::get_installed_display_servers() {
        println!("display server: {:?} {} ({:?})", server.protocol, server.name, server.source);
    }

    let displays = scap::get_all_displays()?;
    for display in &displays {
        println!("display {:>10}  {}", display.id, display.title);
    }
    for window in scap::get_all_windows()? {
        println!("window  {:>10}  {} [{:?}]", window.id, window.title, window.app_name);
    }
    for application in scap::get_all_applications()? {
        println!("app     {:>10}  {}", application.pid, application.name);
    }

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let target = match args.as_slice() {
        [kind, id] if kind == "window" => TargetId::Window(id.parse()?),
        [kind, id] if kind == "app" => TargetId::Application(id.parse()?),
        [kind, id] if kind == "display" => TargetId::Display(id.parse()?),
        _ => TargetId::Display(displays.first().ok_or("no displays")?.id),
    };
    println!("subscribing to {target:?}");

    let video_frames = Arc::new(AtomicUsize::new(0));
    let audio_frames = Arc::new(AtomicUsize::new(0));
    let (video_count, audio_count) = (Arc::clone(&video_frames), Arc::clone(&audio_frames));

    let subscription = Subscription::builder(target)
        .fps(60)
        .on_video(move |frame| {
            if video_count.fetch_add(1, Ordering::Relaxed) == 0 {
                let kind = match &frame.native {
                    #[cfg(target_os = "macos")]
                    NativeFrame::CoreVideo(_) => "IOSurface",
                    #[cfg(target_os = "windows")]
                    NativeFrame::Direct3D11 { .. } => "ID3D11Texture2D",
                    #[cfg(target_os = "linux")]
                    NativeFrame::DmaBuf(_) => "DMA-BUF",
                    #[cfg(target_os = "linux")]
                    NativeFrame::Memory { .. } => "shared memory",
                };
                println!("first frame: {}x{} {:?} in {kind}", frame.width, frame.height, frame.format);
            }
        })
        .on_audio(move |audio| {
            audio_count.fetch_add(audio.frames, Ordering::Relaxed);
        })
        .start()?;

    std::thread::sleep(Duration::from_secs(5));
    subscription.stop()?;

    println!(
        "{} video frames, {} audio samples per channel",
        video_frames.load(Ordering::Relaxed),
        audio_frames.load(Ordering::Relaxed)
    );
    Ok(())
}
