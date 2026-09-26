use crate::{
    capturer::{Area, Options, Point, Size},
    frame::{
        convert::{to_frame, PackedImage},
        Frame, FrameType, PixelFormat,
    },
    targets::{self, Target, TargetError},
};
use std::cmp;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows_capture::capture::Context;
use windows_capture::{
    capture::{CaptureControl, GraphicsCaptureApiHandler},
    frame::Frame as WCFrame,
    graphics_capture_api::{GraphicsCaptureApi, InternalCaptureControl},
    monitor::Monitor as WCMonitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings as WCSettings,
    },
    window::Window as WCWindow,
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The only sender of the capturer's channel. Taking it out when the capture
/// ends disconnects the channel, so `get_next_frame` returns instead of
/// waiting forever for a window that was closed.
type SharedSender = Arc<Mutex<Option<mpsc::Sender<Frame>>>>;

struct Capturer {
    tx: SharedSender,
    crop: Option<Area>,
    output_type: FrameType,
    first_frame_timestamp: Option<i64>,
    start_unix_nanos: u64,
}

#[derive(Clone)]
enum Settings {
    Window(WCSettings<FlagStruct, WCWindow>),
    Display(WCSettings<FlagStruct, WCMonitor>),
}

pub struct WCStream {
    settings: Settings,
    capture_control: Option<CaptureControl<Capturer, BoxError>>,
}

impl GraphicsCaptureApiHandler for Capturer {
    type Flags = FlagStruct;
    type Error = BoxError;

    fn new(context: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            tx: context.flags.tx,
            crop: context.flags.crop,
            output_type: context.flags.output_type,
            first_frame_timestamp: None,
            start_unix_nanos: u64::try_from(
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
            )
            .unwrap_or(u64::MAX),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut WCFrame,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        match self.deliver(frame) {
            Ok(true) => Ok(()),
            // Nobody is receiving anymore; stop capturing
            Ok(false) => {
                capture_control.stop();
                Ok(())
            }
            Err(error) => {
                self.disconnect();
                Err(error)
            }
        }
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        // The captured window or monitor went away
        self.disconnect();
        Ok(())
    }
}

impl Capturer {
    /// Converts and sends one frame; `Ok(false)` once the receiver is gone
    fn deliver(&mut self, frame: &mut WCFrame) -> Result<bool, BoxError> {
        let frame_timestamp = frame.timestamp()?.Duration;
        let first_frame_timestamp = *self.first_frame_timestamp.get_or_insert(frame_timestamp);
        let display_time = timestamp_from_timespan(
            self.start_unix_nanos,
            first_frame_timestamp,
            frame_timestamp,
        );

        // Crop on the GPU so only the requested region is read back
        let mut buffer = match &self.crop {
            Some(area) => match crop_bounds(area, frame.width(), frame.height()) {
                Some([start_x, start_y, end_x, end_y]) => {
                    frame.buffer_crop(start_x, start_y, end_x, end_y)?
                }
                // The window shrank past the crop area; wait for it to grow
                None => return Ok(true),
            },
            None => frame.buffer()?,
        };

        let (width, height, stride) = (
            buffer.width() as usize,
            buffer.height() as usize,
            buffer.row_pitch() as usize,
        );
        // Read the mapped texture in place: one copy into the output frame
        let image = PackedImage {
            data: buffer.as_raw_buffer(),
            stride,
            width,
            height,
            format: PixelFormat::Bgra,
        };
        let Some(output) = to_frame(image, self.output_type, display_time) else {
            return Ok(true);
        };

        let sender = self.tx.lock().map_err(|_| "Frame channel lock poisoned")?;
        Ok(sender
            .as_ref()
            .is_some_and(|sender| sender.send(output).is_ok()))
    }

    fn disconnect(&self) {
        if let Ok(mut sender) = self.tx.lock() {
            sender.take();
        }
    }
}

/// Crop area clamped to the frame as `[start_x, start_y, end_x, end_y]`
/// with exclusive ends; `None` when nothing of it is inside the frame
fn crop_bounds(area: &Area, frame_width: u32, frame_height: u32) -> Option<[u32; 4]> {
    let clamp = |value: f64, max: u32| value.max(0.0).min(f64::from(max)) as u32;
    let start_x = clamp(area.origin.x, frame_width);
    let start_y = clamp(area.origin.y, frame_height);
    let end_x = clamp(area.origin.x + area.size.width, frame_width);
    let end_y = clamp(area.origin.y + area.size.height, frame_height);
    (start_x < end_x && start_y < end_y).then_some([start_x, start_y, end_x, end_y])
}

fn timestamp_from_timespan(start_ns: u64, first: i64, current: i64) -> u64 {
    let elapsed_100ns = current.saturating_sub(first).max(0) as u64;
    start_ns.saturating_add(elapsed_100ns.saturating_mul(100))
}

impl WCStream {
    pub fn start_capture(&mut self) -> Result<(), String> {
        let cc = match &self.settings {
            Settings::Display(st) => Capturer::start_free_threaded(st.to_owned()),
            Settings::Window(st) => Capturer::start_free_threaded(st.to_owned()),
        };

        self.capture_control = Some(cc.map_err(|error| error.to_string())?);
        Ok(())
    }

    pub fn stop_capture(&mut self) -> Result<(), String> {
        if let Some(capture_control) = self.capture_control.take() {
            capture_control.stop().map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

#[derive(Clone)]
struct FlagStruct {
    tx: SharedSender,
    crop: Option<Area>,
    output_type: FrameType,
}

pub fn create_capturer(options: &Options, tx: mpsc::Sender<Frame>) -> Result<WCStream, String> {
    let target = options
        .target
        .clone()
        .map_or_else(|| targets::get_main_display().map(Target::Display), Ok)
        .map_err(|error| error.to_string())?;

    let supported = |check: fn() -> Result<bool, _>| check().unwrap_or(false);

    let show_cursor = if !supported(GraphicsCaptureApi::is_cursor_settings_supported) {
        CursorCaptureSettings::Default
    } else if options.show_cursor {
        CursorCaptureSettings::WithCursor
    } else {
        CursorCaptureSettings::WithoutCursor
    };

    let draw_border = if !supported(GraphicsCaptureApi::is_border_settings_supported) {
        DrawBorderSettings::Default
    } else if options.show_highlight {
        DrawBorderSettings::WithBorder
    } else {
        DrawBorderSettings::WithoutBorder
    };

    // Windows otherwise delivers a frame for every screen update
    let update_interval = if options.fps > 0
        && supported(GraphicsCaptureApi::is_minimum_update_interval_supported)
    {
        MinimumUpdateIntervalSettings::Custom(Duration::from_secs_f64(1.0 / f64::from(options.fps)))
    } else {
        MinimumUpdateIntervalSettings::Default
    };

    // Only crop when asked to; a full frame needs no GPU copy region
    let crop = match options.crop_area {
        Some(_) => Some(get_crop_area(options).map_err(|error| error.to_string())?),
        None => None,
    };
    let flags = FlagStruct {
        tx: Arc::new(Mutex::new(Some(tx))),
        crop,
        output_type: options.output_type,
    };

    // Every output type is converted from BGRA on the CPU
    let settings = match target {
        Target::Display(display) => Settings::Display(WCSettings::new(
            WCMonitor::from_raw_hmonitor(display.raw_handle.0),
            show_cursor,
            draw_border,
            SecondaryWindowSettings::Default,
            update_interval,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags,
        )),
        Target::Window(window) => Settings::Window(WCSettings::new(
            WCWindow::from_raw_hwnd(window.raw_handle.0),
            show_cursor,
            draw_border,
            SecondaryWindowSettings::Default,
            update_interval,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags,
        )),
    };

    Ok(WCStream {
        settings,
        capture_control: None,
    })
}

pub fn get_output_frame_size(options: &Options) -> Result<[u32; 2], TargetError> {
    let crop_area = get_crop_area(options)?;

    let mut output_width = (crop_area.size.width) as u32;
    let mut output_height = (crop_area.size.height) as u32;

    if let Some([resolved_width, resolved_height]) = options
        .output_resolution
        .value((crop_area.size.width as f32) / (crop_area.size.height as f32))
    {
        output_width = cmp::min(output_width, resolved_width);
        output_height = cmp::min(output_height, resolved_height);
    }

    output_width -= output_width % 2;
    output_height -= output_height % 2;

    Ok([output_width, output_height])
}

fn get_absolute_value(value: f64) -> f64 {
    let value = value.floor();
    value + value % 2.0
}

pub fn get_crop_area(options: &Options) -> Result<Area, TargetError> {
    let target = options
        .target
        .clone()
        .map_or_else(|| targets::get_main_display().map(Target::Display), Ok)?;

    let (width, height) = targets::get_target_dimensions(&target)?;

    Ok(options
        .crop_area
        .as_ref()
        .map(|val| {
            // WINDOWS: limit values [input-width, input-height] = [146, 50]
            Area {
                origin: Point {
                    x: get_absolute_value(val.origin.x),
                    y: get_absolute_value(val.origin.y),
                },
                size: Size {
                    width: get_absolute_value(val.size.width),
                    height: get_absolute_value(val.size.height),
                },
            }
        })
        .unwrap_or_else(|| Area {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: width as f64,
                height: height as f64,
            },
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_timestamp_is_monotonic() {
        assert_eq!(timestamp_from_timespan(1_000, 10, 60), 6_000);
        assert_eq!(timestamp_from_timespan(1_000, 10, 5), 1_000);
    }

    #[test]
    fn crop_bounds_are_clamped_to_the_frame() {
        let area = |x, y, width, height| Area {
            origin: Point { x, y },
            size: Size { width, height },
        };
        assert_eq!(crop_bounds(&area(10.0, 20.0, 100.0, 50.0), 1920, 1080), Some([10, 20, 110, 70]));
        assert_eq!(crop_bounds(&area(1900.0, 0.0, 100.0, 50.0), 1920, 1080), Some([1900, 0, 1920, 50]));
        assert_eq!(crop_bounds(&area(-5.0, -5.0, 10.0, 10.0), 1920, 1080), Some([0, 0, 5, 5]));
        assert_eq!(crop_bounds(&area(2000.0, 0.0, 100.0, 50.0), 1920, 1080), None);
    }
}
