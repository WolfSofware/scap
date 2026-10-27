use std::sync::{Arc, Mutex};

use core_graphics_helmer_fork::display::CGDisplay;
use screencapturekit::cm::SCFrameStatus;
use screencapturekit::prelude::*;

use super::{Config, SubscriptionError, TargetId};
use crate::capturer::engine::mac::pixelformat::get_pts_in_nanoseconds;
use crate::frame::{
    AudioData, AudioFrame, AudioSampleFormat, NativeFrame, PixelFormat as ScapPixelFormat,
    VideoFrame,
};

const AUDIO_SAMPLE_RATE: i32 = 48_000;
const AUDIO_CHANNELS: i32 = 2;
// kAudioFormatFlagIsNonInterleaved
const NON_INTERLEAVED_FLAG: u32 = 1 << 5;

pub struct Subscription {
    stream: SCStream,
    error: Arc<Mutex<Option<String>>>,
}

impl Subscription {
    pub fn stop(self) -> Result<(), SubscriptionError> {
        let stopped = self.stream.stop_capture();
        // A stream that died on its own can't be stopped; report why it died
        if let Some(error) = self.error.lock().ok().and_then(|mut error| error.take()) {
            return Err(SubscriptionError::Platform(error));
        }
        stopped.map_err(SubscriptionError::platform)
    }
}

pub fn start(config: Config) -> Result<Subscription, SubscriptionError> {
    let content = SCShareableContent::get().map_err(SubscriptionError::platform)?;
    let displays = content.displays();
    let not_found = || SubscriptionError::TargetNotFound(config.target);

    let (filter, width, height) = match config.target {
        TargetId::Display(id) => {
            let display = displays
                .iter()
                .find(|display| display.display_id() == id)
                .ok_or_else(not_found)?;
            let scale = display_scale(display);
            let filter = SCContentFilter::create()
                .with_display(display)
                .with_excluding_windows(&[])
                .build();
            (filter, display.width() as f64 * scale, display.height() as f64 * scale)
        }
        TargetId::Window(id) => {
            let window = content
                .windows()
                .into_iter()
                .find(|window| window.window_id() == id)
                .ok_or_else(not_found)?;
            let frame = window.frame();
            let scale = displays
                .iter()
                .find(|display| display.frame().contains_point(center(&frame)))
                .map_or(1.0, display_scale);
            let filter = SCContentFilter::create().with_window(&window).build();
            (filter, frame.size.width * scale, frame.size.height * scale)
        }
        TargetId::Application(pid) => {
            let application = content
                .applications()
                .into_iter()
                .find(|application| u32::try_from(application.process_id()) == Ok(pid))
                .ok_or_else(not_found)?;
            // Capture the display that shows most of the application's windows
            let windows = content
                .windows()
                .into_iter()
                .filter(|window| {
                    window.is_on_screen()
                        && window
                            .owning_application()
                            .is_some_and(|owner| owner.process_id() == application.process_id())
                })
                .collect::<Vec<_>>();
            let display = displays
                .iter()
                .max_by_key(|display| {
                    windows
                        .iter()
                        .filter(|window| display.frame().contains_point(center(&window.frame())))
                        .count()
                })
                .ok_or_else(|| SubscriptionError::Platform("No displays available".into()))?;
            let scale = display_scale(display);
            let filter = SCContentFilter::create()
                .with_display(display)
                .with_including_applications(&[&application], &[])
                .build();
            (filter, display.width() as f64 * scale, display.height() as f64 * scale)
        }
    };

    let capture_video = config.video.is_some();
    let capture_audio = config.audio.is_some();

    // ScreenCaptureKit always produces video; keep it tiny for audio-only use
    let (width, height, fps) = if capture_video {
        (even(width), even(height), config.fps)
    } else {
        (2, 2, 1)
    };

    let stream_config = SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_pixel_format(PixelFormat::BGRA)
        .with_shows_cursor(config.show_cursor)
        .with_minimum_frame_interval(&CMTime::new(1, fps as i32))
        .with_captures_audio(capture_audio)
        .with_sample_rate(AUDIO_SAMPLE_RATE)
        .with_channel_count(AUDIO_CHANNELS);

    let error = Arc::new(Mutex::new(None));
    let filter = filter.map_err(SubscriptionError::platform)?;
    let delegate_error = Arc::clone(&error);
    let mut stream = SCStream::new_with_delegate(
        &filter,
        &stream_config,
        ErrorHandler::new(move |error| {
            if let Ok(mut slot) = delegate_error.lock() {
                *slot = Some(error.to_string());
            }
        }),
    )
    .map_err(SubscriptionError::platform)?;

    if let Some(callback) = config.video {
        let callback = Mutex::new(callback);
        stream
            .add_output_handler(
                move |sample: CMSampleBuffer, _: SCStreamOutputType| {
                    let Some(frame) = video_frame(&sample) else {
                        return;
                    };
                    if let Ok(mut callback) = callback.lock() {
                        callback(&frame);
                    }
                },
                SCStreamOutputType::Screen,
            )
            .map_err(SubscriptionError::platform)?;
    }

    if let Some(callback) = config.audio {
        let callback = Mutex::new(callback);
        stream
            .add_output_handler(
                move |sample: CMSampleBuffer, _: SCStreamOutputType| {
                    let Ok(mut callback) = callback.lock() else {
                        return;
                    };
                    with_audio_frame(&sample, |frame| callback(frame));
                },
                SCStreamOutputType::Audio,
            )
            .map_err(SubscriptionError::platform)?;
    }

    stream.start_capture().map_err(SubscriptionError::platform)?;

    Ok(Subscription { stream, error })
}

fn video_frame(sample: &CMSampleBuffer) -> Option<VideoFrame<'_>> {
    if !matches!(
        sample.frame_status()?,
        SCFrameStatus::Complete | SCFrameStatus::Started
    ) {
        return None;
    }
    // Retains the IOSurface-backed pixel buffer; no pixels are touched
    let pixel_buffer = sample.pixel_buffer()?;
    Some(VideoFrame {
        display_time: get_pts_in_nanoseconds(sample),
        width: pixel_buffer.width() as u32,
        height: pixel_buffer.height() as u32,
        format: pixel_format(pixel_buffer.pixel_format()),
        native: NativeFrame::CoreVideo(sample),
    })
}

fn with_audio_frame(sample: &CMSampleBuffer, callback: impl FnOnce(&AudioFrame<'_>)) {
    let Some(description) = sample.format_description() else {
        return;
    };
    let Ok(buffers) = sample.audio_buffer_list() else {
        return;
    };

    let format = match (
        description.audio_is_float(),
        description.audio_bits_per_channel(),
    ) {
        (true, Some(32)) => AudioSampleFormat::F32,
        (false, Some(16)) => AudioSampleFormat::I16,
        (false, Some(32)) => AudioSampleFormat::I32,
        _ => return,
    };
    let sample_rate = description.audio_sample_rate().unwrap_or(0.0) as u32;
    let channels = description.audio_channel_count().unwrap_or(0) as u16;
    let frames = usize::try_from(sample.num_samples()).unwrap_or(0);
    let non_interleaved = description
        .audio_format_flags()
        .is_some_and(|flags| flags & NON_INTERLEAVED_FLAG != 0);

    let planes = buffers.iter().map(|buffer| buffer.data()).collect::<Vec<_>>();
    let data = match planes.as_slice() {
        [interleaved] if !non_interleaved || channels == 1 => AudioData::Interleaved(interleaved),
        planes => AudioData::Planar(planes),
    };

    callback(&AudioFrame {
        display_time: get_pts_in_nanoseconds(sample),
        sample_rate,
        channels,
        format,
        frames,
        data,
    });
}

fn pixel_format(os_type: u32) -> ScapPixelFormat {
    match &os_type.to_be_bytes() {
        b"BGRA" => ScapPixelFormat::Bgra,
        b"420v" | b"420f" => ScapPixelFormat::Nv12,
        _ => ScapPixelFormat::Other(os_type),
    }
}

fn display_scale(display: &SCDisplay) -> f64 {
    CGDisplay::new(display.display_id())
        .display_mode()
        .filter(|mode| mode.width() > 0)
        .map_or(1.0, |mode| mode.pixel_width() as f64 / mode.width() as f64)
}

fn center(rect: &CGRect) -> CGPoint {
    CGPoint::new(
        rect.origin.x + rect.size.width / 2.0,
        rect.origin.y + rect.size.height / 2.0,
    )
}

fn even(value: f64) -> u32 {
    let value = value.max(2.0) as u32;
    value - value % 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_core_video_pixel_formats() {
        assert_eq!(pixel_format(u32::from_be_bytes(*b"BGRA")), ScapPixelFormat::Bgra);
        assert_eq!(pixel_format(u32::from_be_bytes(*b"420v")), ScapPixelFormat::Nv12);
        assert_eq!(pixel_format(7), ScapPixelFormat::Other(7));
    }

    #[test]
    fn output_size_is_even_and_non_zero() {
        assert_eq!(even(1921.0), 1920);
        assert_eq!(even(0.0), 2);
    }
}
