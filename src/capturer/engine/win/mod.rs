use crate::{
    capturer::{Area, Options, Point, Size},
    frame::{BGRAFrame, Frame, FrameType},
    targets::{self, Target, TargetError},
};
use std::cmp;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};
use windows_capture::capture::Context;
use windows_capture::{
    capture::{CaptureControl, GraphicsCaptureApiHandler},
    frame::Frame as WCFrame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor as WCMonitor,
    settings::{ColorFormat, CursorCaptureSettings, DrawBorderSettings, Settings as WCSettings},
    window::Window as WCWindow,
};

#[derive(Debug)]
struct Capturer {
    pub tx: mpsc::Sender<Frame>,
    pub crop: Option<Area>,
}

#[derive(Clone)]
enum Settings {
    Window(WCSettings<FlagStruct, WCWindow>),
    Display(WCSettings<FlagStruct, WCMonitor>),
}

pub struct WCStream {
    settings: Settings,
    capture_control: Option<CaptureControl<Capturer, Box<dyn std::error::Error + Send + Sync>>>,
}

impl GraphicsCaptureApiHandler for Capturer {
    type Flags = FlagStruct;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(context: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            tx: context.flags.tx,
            crop: context.flags.crop,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut WCFrame,
        _: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        match &self.crop {
            Some(cropped_area) => {
                // get the cropped area
                let start_x = cropped_area.origin.x as u32;
                let start_y = cropped_area.origin.y as u32;
                let end_x = (cropped_area.origin.x + cropped_area.size.width) as u32;
                let end_y = (cropped_area.origin.y + cropped_area.size.height) as u32;

                // crop the frame
                let mut cropped_buffer = frame.buffer_crop(start_x, start_y, end_x, end_y)?;

                // get raw frame buffer
                let raw_frame_buffer = match cropped_buffer.as_nopadding_buffer() {
                    Ok(buffer) => buffer,
                    Err(_) => return Err(("Failed to get raw buffer").into()),
                };

                let current_time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64;

                let bgr_frame = BGRAFrame {
                    display_time: current_time,
                    width: cropped_area.size.width as i32,
                    height: cropped_area.size.height as i32,
                    data: raw_frame_buffer.to_vec(),
                };

                if self.tx.send(Frame::BGRA(bgr_frame)).is_err() {
                    return Ok(());
                }
            }
            None => {
                // get raw frame buffer
                let mut frame_buffer = frame.buffer()?;
                let raw_frame_buffer = frame_buffer.as_raw_buffer();
                let frame_data = raw_frame_buffer.to_vec();
                let current_time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64;
                let bgr_frame = BGRAFrame {
                    display_time: current_time,
                    width: frame.width() as i32,
                    height: frame.height() as i32,
                    data: frame_data,
                };

                if self.tx.send(Frame::BGRA(bgr_frame)).is_err() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        println!("Closed");
        Ok(())
    }
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

#[derive(Clone, Debug)]
struct FlagStruct {
    pub tx: mpsc::Sender<Frame>,
    pub crop: Option<Area>,
}

pub fn create_capturer(options: &Options, tx: mpsc::Sender<Frame>) -> Result<WCStream, String> {
    let target = options
        .target
        .clone()
        .map_or_else(|| targets::get_main_display().map(Target::Display), Ok)
        .map_err(|error| error.to_string())?;

    let color_format = match options.output_type {
        FrameType::BGRAFrame => ColorFormat::Bgra8,
        _ => ColorFormat::Rgba8,
    };

    let show_cursor = match options.show_cursor {
        true => CursorCaptureSettings::WithCursor,
        false => CursorCaptureSettings::WithoutCursor,
    };

    let settings = match target {
        Target::Display(display) => Settings::Display(WCSettings::new(
            WCMonitor::from_raw_hmonitor(display.raw_handle.0),
            show_cursor,
            DrawBorderSettings::Default,
            color_format,
            FlagStruct {
                tx,
                crop: Some(get_crop_area(options).map_err(|error| error.to_string())?),
            },
        )),
        Target::Window(window) => Settings::Window(WCSettings::new(
            WCWindow::from_raw_hwnd(window.raw_handle.0),
            show_cursor,
            DrawBorderSettings::Default,
            color_format,
            FlagStruct {
                tx,
                crop: Some(get_crop_area(options).map_err(|error| error.to_string())?),
            },
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

fn get_absolute_value(value: f64, scale_factor: f64) -> f64 {
    let value = (value * scale_factor).floor();
    value + value % 2.0
}

pub fn get_crop_area(options: &Options) -> Result<Area, TargetError> {
    let target = options
        .target
        .clone()
        .map_or_else(|| targets::get_main_display().map(Target::Display), Ok)?;

    let (width, height) = targets::get_target_dimensions(&target)?;

    let scale_factor = targets::get_scale_factor(&target)?;
    Ok(options
        .crop_area
        .as_ref()
        .map(|val| {
            // WINDOWS: limit values [input-width, input-height] = [146, 50]
            Area {
                origin: Point {
                    x: get_absolute_value(val.origin.x, scale_factor),
                    y: get_absolute_value(val.origin.y, scale_factor),
                },
                size: Size {
                    width: get_absolute_value(val.size.width, scale_factor),
                    height: get_absolute_value(val.size.height, scale_factor),
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
