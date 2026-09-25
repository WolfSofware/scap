use std::{
    error::Error,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
    time::Duration,
};

use wasapi::{
    AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode, WasapiError, WaveFormat,
};
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    frame::Frame as WCFrame,
    graphics_capture_api::{GraphicsCaptureApi, InternalCaptureControl},
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
    window::Window,
};

use super::{AudioCallback, Config, SubscriptionError, TargetId, VideoCallback};
use crate::frame::{
    AudioData, AudioFrame, AudioSampleFormat, NativeFrame, PixelFormat, VideoFrame,
};

type BoxError = Box<dyn Error + Send + Sync>;

const AUDIO_SAMPLE_RATE: usize = 48_000;
const AUDIO_CHANNELS: usize = 2;
// 20 ms, in 100 ns units
const AUDIO_BUFFER_DURATION: i64 = 200_000;

pub struct Subscription {
    video: Option<CaptureControl<VideoHandler, BoxError>>,
    audio: Option<AudioThread>,
}

impl Subscription {
    pub fn stop(self) -> Result<(), SubscriptionError> {
        let video = match self.video {
            Some(control) => control.stop().map_err(SubscriptionError::platform),
            None => Ok(()),
        };
        let audio = match self.audio {
            Some(audio) => audio.stop(),
            None => Ok(()),
        };
        video.and(audio)
    }
}

enum VideoItem {
    Monitor(Monitor),
    Window(Window),
}

enum AudioSource {
    System,
    Process(u32),
}

pub fn start(config: Config) -> Result<Subscription, SubscriptionError> {
    let (item, audio_source) = resolve(config.target, config.video.is_some())?;

    let video = match (item, config.video) {
        (Some(item), Some(callback)) => Some(start_video(
            item,
            callback,
            config.fps,
            config.show_cursor,
        )?),
        _ => None,
    };

    let audio = match config.audio {
        Some(callback) => match AudioThread::spawn(audio_source, callback) {
            Ok(audio) => Some(audio),
            Err(error) => {
                if let Some(video) = video {
                    let _ = video.stop();
                }
                return Err(error);
            }
        },
        None => None,
    };

    Ok(Subscription { video, audio })
}

fn resolve(
    target: TargetId,
    needs_video: bool,
) -> Result<(Option<VideoItem>, AudioSource), SubscriptionError> {
    let not_found = || SubscriptionError::TargetNotFound(target);
    let windows = || Window::enumerate().map_err(SubscriptionError::platform);

    match target {
        TargetId::Display(id) => {
            let monitor = Monitor::enumerate()
                .map_err(SubscriptionError::platform)?
                .into_iter()
                .find(|monitor| monitor.as_raw_hmonitor() as usize as u32 == id)
                .ok_or_else(not_found)?;
            Ok((Some(VideoItem::Monitor(monitor)), AudioSource::System))
        }
        TargetId::Window(id) => {
            let window = windows()?
                .into_iter()
                .find(|window| window.as_raw_hwnd() as usize as u32 == id)
                .ok_or_else(not_found)?;
            let pid = window.process_id().map_err(SubscriptionError::platform)?;
            Ok((Some(VideoItem::Window(window)), AudioSource::Process(pid)))
        }
        TargetId::Application(pid) => {
            // Windows.Graphics.Capture captures windows, not processes, so
            // video follows the application's largest titled window
            let window = windows()?
                .into_iter()
                .filter(|window| {
                    window.process_id().ok() == Some(pid)
                        && window.title().is_ok_and(|title| !title.is_empty())
                })
                .max_by_key(|window| {
                    window.rect().map_or(0, |rect| {
                        i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top)
                    })
                });
            if window.is_none() && needs_video {
                return Err(not_found());
            }
            Ok((window.map(VideoItem::Window), AudioSource::Process(pid)))
        }
    }
}

fn start_video(
    item: VideoItem,
    callback: VideoCallback,
    fps: u32,
    show_cursor: bool,
) -> Result<CaptureControl<VideoHandler, BoxError>, SubscriptionError> {
    let supported = |check: fn() -> Result<bool, _>| check().unwrap_or(false);

    let cursor = if !supported(GraphicsCaptureApi::is_cursor_settings_supported) {
        CursorCaptureSettings::Default
    } else if show_cursor {
        CursorCaptureSettings::WithCursor
    } else {
        CursorCaptureSettings::WithoutCursor
    };
    let border = if supported(GraphicsCaptureApi::is_border_settings_supported) {
        DrawBorderSettings::WithoutBorder
    } else {
        DrawBorderSettings::Default
    };
    let interval = if supported(GraphicsCaptureApi::is_minimum_update_interval_supported) {
        MinimumUpdateIntervalSettings::Custom(Duration::from_secs_f64(1.0 / f64::from(fps)))
    } else {
        MinimumUpdateIntervalSettings::Default
    };
    let flags = VideoFlags { callback };

    let control = match item {
        VideoItem::Monitor(monitor) => VideoHandler::start_free_threaded(Settings::new(
            monitor,
            cursor,
            border,
            SecondaryWindowSettings::Default,
            interval,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags,
        )),
        VideoItem::Window(window) => VideoHandler::start_free_threaded(Settings::new(
            window,
            cursor,
            border,
            SecondaryWindowSettings::Default,
            interval,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags,
        )),
    };
    control.map_err(SubscriptionError::platform)
}

struct VideoFlags {
    callback: VideoCallback,
}

struct VideoHandler {
    callback: VideoCallback,
}

impl GraphicsCaptureApiHandler for VideoHandler {
    type Flags = VideoFlags;
    type Error = BoxError;

    fn new(context: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            callback: context.flags.callback,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut WCFrame,
        _: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        // SystemRelativeTime, QPC based, in 100 ns units
        let display_time = u64::try_from(frame.timestamp()?.Duration).unwrap_or(0) * 100;
        let video = VideoFrame {
            display_time,
            width: frame.width(),
            height: frame.height(),
            format: PixelFormat::Bgra,
            native: NativeFrame::Direct3D11 {
                texture: frame.as_raw_texture(),
                device: frame.device(),
                context: frame.device_context(),
            },
        };
        (self.callback)(&video);
        Ok(())
    }
}

struct AudioThread {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<Result<(), String>>,
}

impl AudioThread {
    fn spawn(source: AudioSource, callback: AudioCallback) -> Result<Self, SubscriptionError> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);

        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut callback = callback;
            let result = capture_audio(&source, &mut callback, &thread_stop, &ready_tx)
                .map_err(|error| error.to_string());
            if let Err(error) = &result {
                // Only the first message is read; later sends are ignored
                let _ = ready_tx.try_send(Err(error.clone()));
            }
            result
        });

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop, handle }),
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(SubscriptionError::Platform(error))
            }
            Err(_) => Err(SubscriptionError::Platform(
                "Audio capture thread exited".into(),
            )),
        }
    }

    fn stop(self) -> Result<(), SubscriptionError> {
        self.stop.store(true, Ordering::Relaxed);
        match self.handle.join() {
            Ok(result) => result.map_err(SubscriptionError::Platform),
            Err(_) => Err(SubscriptionError::Platform(
                "Audio capture thread panicked".into(),
            )),
        }
    }
}

/// Event driven WASAPI loopback capture. Everything COM lives on this thread.
fn capture_audio(
    source: &AudioSource,
    callback: &mut AudioCallback,
    stop: &AtomicBool,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), WasapiError> {
    wasapi::initialize_mta().ok()?;

    // Process loopback can't report a mix format, so always ask for
    // 48 kHz stereo f32 and let the audio engine convert
    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        AUDIO_SAMPLE_RATE,
        AUDIO_CHANNELS,
        None,
    );
    let block_align = format.get_blockalign() as usize;

    let mut client = match source {
        AudioSource::System => DeviceEnumerator::new()?
            .get_default_device(&Direction::Render)?
            .get_iaudioclient()?,
        AudioSource::Process(pid) => AudioClient::new_application_loopback_client(*pid, true)?,
    };
    // Capturing from a render device (or a process) turns on loopback
    client.initialize_client(
        &format,
        &Direction::Capture,
        &StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: AUDIO_BUFFER_DURATION,
        },
    )?;
    let event = client.set_get_eventhandle()?;
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;
    let _ = ready.try_send(Ok(()));

    let mut buffer = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match event.wait_for_event(100) {
            Ok(()) => {}
            // Loopback streams are silent, not broken, while nothing plays
            Err(WasapiError::EventTimeout) => continue,
            Err(error) => return Err(error),
        }

        while let Some(frames) = capture.get_next_packet_size()?.filter(|frames| *frames > 0) {
            buffer.resize(frames as usize * block_align, 0);
            let (read, info) = capture.read_from_device(&mut buffer)?;
            let data = &mut buffer[..read as usize * block_align];
            if info.flags.silent {
                data.fill(0);
            }

            callback(&AudioFrame {
                // QPC position in 100 ns units, same clock as video frames
                display_time: info.timestamp.saturating_mul(100),
                sample_rate: AUDIO_SAMPLE_RATE as u32,
                channels: AUDIO_CHANNELS as u16,
                format: AudioSampleFormat::F32,
                frames: read as usize,
                data: AudioData::Interleaved(data),
            });
        }
    }

    client.stop_stream()
}
