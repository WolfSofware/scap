use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Mutex};
use std::{cmp, sync::Arc};

use pixelformat::get_pts_in_nanoseconds;
use screencapturekit::cm::SCFrameStatus;
use screencapturekit::prelude::*;

use crate::frame::{Frame, FrameType};
use crate::targets::{Target, TargetError};
use crate::{
    capturer::{Area, Options, Point, Size},
    frame::BGRAFrame,
    targets,
};

use super::ChannelItem;

mod pixel_buffer;
mod pixelformat;

pub use pixel_buffer::PixelBuffer;

pub struct Capturer {
    pub tx: mpsc::Sender<ChannelItem>,
}

impl Capturer {
    pub fn new(tx: mpsc::Sender<ChannelItem>) -> Self {
        Capturer { tx }
    }
}

impl SCStreamOutputTrait for Capturer {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        let _ = self.tx.send((sample, of_type));
    }
}

pub fn create_capturer(
    options: &Options,
    tx: mpsc::Sender<ChannelItem>,
    error_flag: Arc<AtomicBool>,
    error_text: Arc<Mutex<Option<String>>>,
) -> Result<SCStream, String> {
    // If no target is specified, capture the main display
    let target = options
        .target
        .clone()
        .map_or_else(|| targets::get_main_display().map(Target::Display), Ok)
        .map_err(|error| error.to_string())?;

    let content = SCShareableContent::get().map_err(|error| error.to_string())?;

    let filter = match target {
        Target::Window(window) => {
            let sc_window = content
                .windows()
                .into_iter()
                .find(|sc_win| sc_win.window_id() == window.id)
                .ok_or_else(|| format!("Window {} is no longer available", window.id))?;
            SCContentFilter::create().with_window(&sc_window).build()
        }
        Target::Display(display) => {
            let sc_display = content
                .displays()
                .into_iter()
                .find(|sc_dis| sc_dis.display_id() == display.id)
                .ok_or_else(|| format!("Display {} is no longer available", display.id))?;
            let excluded_windows = content.windows().into_iter().filter(|window| {
                options.excluded_targets.as_ref().is_some_and(|targets| {
                    targets.iter().any(|target| {
                        matches!(target, Target::Window(excluded) if excluded.id == window.window_id())
                    })
                })
            }).collect::<Vec<_>>();
            let excluded_refs = excluded_windows.iter().collect::<Vec<_>>();
            SCContentFilter::create()
                .with_display(&sc_display)
                .with_excluding_windows(&excluded_refs)
                .build()
        }
    };

    let crop_area = get_crop_area(options).map_err(|error| error.to_string())?;

    let source_rect = CGRect::new(
        crop_area.origin.x,
        crop_area.origin.y,
        crop_area.size.width,
        crop_area.size.height,
    );

    let pixel_format = match options.output_type {
        FrameType::YUVFrame => PixelFormat::YCbCr_420v,
        FrameType::BGR0 | FrameType::RGB | FrameType::BGRAFrame => PixelFormat::BGRA,
    };

    let [width, height] = get_output_frame_size(options).map_err(|error| error.to_string())?;

    let stream_config = SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_source_rect(source_rect)
        .with_pixel_format(pixel_format)
        .with_shows_cursor(options.show_cursor)
        .with_minimum_frame_interval(&CMTime::new(1, options.fps as i32));

    let delegate_flag = error_flag.clone();
    let mut stream = SCStream::new_with_delegate(
        &filter,
        &stream_config,
        ErrorHandler::new(move |error| {
            // Раньше здесь стоял `eprintln!`. У приложения, запущенного не из
            // терминала, stderr не ведёт никуда, а флаг ниже читал только
            // сырой путь (`get_next_pixel_buffer`). Поэтому отказ потока
            // выглядел как вечная тишина: ни кадров, ни ошибки, ни следа.
            // Текст сохраняем — за ним придёт `get_next_frame`.
            if let Ok(mut slot) = error_text.lock() {
                *slot = Some(error.to_string());
            }
            delegate_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }),
    );
    stream.add_output_handler(Capturer::new(tx), SCStreamOutputType::Screen);

    Ok(stream)
}

pub fn get_output_frame_size(options: &Options) -> Result<[u32; 2], TargetError> {
    let target = options
        .target
        .clone()
        .map_or_else(|| targets::get_main_display().map(Target::Display), Ok)?;

    let scale_factor = targets::get_scale_factor(&target)?;
    let source_rect = get_crop_area(options)?;

    // Calculate the output height & width based on the required resolution
    // Output width and height need to be multiplied by scale (or dpi)
    let mut output_width = (source_rect.size.width as u32) * (scale_factor as u32);
    let mut output_height = (source_rect.size.height as u32) * (scale_factor as u32);
    // 1200x800
    if let Some([resolved_width, resolved_height]) = options
        .output_resolution
        .value((source_rect.size.width as f32) / (source_rect.size.height as f32))
    {
        // 1280 x 853
        output_width = cmp::min(output_width, resolved_width);
        output_height = cmp::min(output_height, resolved_height);
    }

    output_width -= output_width % 2;
    output_height -= output_height % 2;

    Ok([output_width, output_height])
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
            let input_width = val.size.width + (val.size.width % 2.0);
            let input_height = val.size.height + (val.size.height % 2.0);

            Area {
                origin: Point {
                    x: val.origin.x,
                    y: val.origin.y,
                },
                size: Size {
                    width: input_width,
                    height: input_height,
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

pub fn process_sample_buffer(
    sample: CMSampleBuffer,
    of_type: SCStreamOutputType,
    output_type: FrameType,
) -> Option<Frame> {
    if let SCStreamOutputType::Screen = of_type {
        let frame_status = sample.frame_status()?;

        match frame_status {
            SCFrameStatus::Complete | SCFrameStatus::Started => {
                return match output_type {
                    FrameType::YUVFrame => {
                        pixelformat::create_yuv_frame(sample).map(Frame::YUVFrame)
                    }
                    FrameType::RGB => pixelformat::create_rgb_frame(sample).map(Frame::RGB),
                    FrameType::BGR0 => pixelformat::create_bgr_frame(sample).map(Frame::BGR0),
                    FrameType::BGRAFrame => pixelformat::create_bgra_frame(sample).map(Frame::BGRA),
                };
            }
            SCFrameStatus::Idle => {
                // Quick hack - just send an empty frame, and the caller can figure out how to handle it
                if let FrameType::BGRAFrame = output_type {
                    return Some(Frame::BGRA(BGRAFrame {
                        display_time: get_pts_in_nanoseconds(&sample),
                        width: 0,
                        height: 0,
                        data: vec![],
                    }));
                }
            }
            _ => {}
        }
    }

    None
}
