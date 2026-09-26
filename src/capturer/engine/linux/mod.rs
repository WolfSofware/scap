use std::sync::mpsc;

use crate::{
    capturer::{Area, Options},
    frame::{
        convert::{to_frame, PackedImage, Rect},
        Frame, FrameType, NativeFrame, VideoFrame,
    },
};

use self::{
    error::LinCapError,
    portal::{ScreenCastPortal, SourceType},
    video::{VideoStream, VideoStreamOptions},
};

pub(crate) mod audio;
mod error;
pub(crate) mod portal;
pub(crate) mod video;

/// `Options::fps` defaults to 0, which PipeWire can't negotiate
const DEFAULT_FPS: u32 = 60;

pub struct LinuxCapturer {
    stream: VideoStream,
    // The PipeWire node is removed when the portal session's connection
    // closes, so keep it open for as long as the stream runs
    _connection: dbus::blocking::Connection,
}

impl LinuxCapturer {
    pub fn new(options: &Options, tx: mpsc::Sender<Frame>) -> Result<Self, LinCapError> {
        let connection = dbus::blocking::Connection::new_session()?;
        let node_id = ScreenCastPortal::new(&connection)
            .source_types(SourceType::ANY)
            .show_cursor(options.show_cursor)?
            .create_stream()?
            .pw_node_id();

        let output_type = options.output_type;
        let crop = options.crop_area.as_ref().and_then(crop_rect);

        // Frames are copied into owned buffers, so ask for mappable memory.
        // The sender lives in the stream; when the stream ends it is dropped
        // and `get_next_frame` returns instead of blocking forever.
        let stream = VideoStream::spawn(
            VideoStreamOptions {
                node_id,
                fps: if options.fps == 0 { DEFAULT_FPS } else { options.fps },
                allow_dmabuf: false,
                active: false,
            },
            Box::new(move |frame| {
                if let Some(frame) = copy_frame(frame, crop, output_type) {
                    let _ = tx.send(frame);
                }
            }),
        )?;

        Ok(Self {
            stream,
            _connection: connection,
        })
    }

    pub fn start_capture(&self) -> Result<(), LinCapError> {
        self.stream.set_active(true);
        Ok(())
    }

    pub fn stop_capture(&mut self) -> Result<(), LinCapError> {
        self.stream.set_active(false);
        self.stream.stop()
    }
}

/// Copies a mapped PipeWire frame into the requested [Frame] type,
/// cropping and dropping row padding in the same pass
fn copy_frame(frame: &VideoFrame<'_>, crop: Option<Rect>, output_type: FrameType) -> Option<Frame> {
    let NativeFrame::Memory { data, stride } = frame.native else {
        return None;
    };
    let width = frame.width as usize;
    let image = PackedImage {
        data,
        stride: if stride == 0 { width * 4 } else { stride as usize },
        width,
        height: frame.height as usize,
        format: frame.format,
    };
    let image = match crop {
        Some(rect) => image.cropped(rect)?,
        None => image,
    };
    to_frame(image, output_type, frame.display_time)
}

/// The crop area in whole pixels; negative origins are clamped to 0
fn crop_rect(area: &Area) -> Option<Rect> {
    let rect = Rect {
        x: area.origin.x.max(0.0) as usize,
        y: area.origin.y.max(0.0) as usize,
        width: area.size.width.max(0.0) as usize,
        height: area.size.height.max(0.0) as usize,
    };
    (rect.width > 0 && rect.height > 0).then_some(rect)
}

pub fn create_capturer(
    options: &Options,
    tx: mpsc::Sender<Frame>,
) -> Result<LinuxCapturer, LinCapError> {
    LinuxCapturer::new(options, tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capturer::{Point, Size},
        frame::PixelFormat,
    };

    fn frame(data: &[u8], stride: u32) -> VideoFrame<'_> {
        VideoFrame {
            display_time: 42,
            width: 2,
            height: 2,
            format: PixelFormat::Rgbx,
            native: NativeFrame::Memory { data, stride },
        }
    }

    #[test]
    fn frames_follow_the_requested_output_type() {
        // 2x2 RGBx with 4 bytes of row padding
        let data = [
            1, 2, 3, 0, 4, 5, 6, 0, 9, 9, 9, 9, //
            7, 8, 9, 0, 10, 11, 12, 0, 9, 9, 9, 9,
        ];
        let Some(Frame::BGRA(bgra)) = copy_frame(&frame(&data, 12), None, FrameType::BGRAFrame)
        else {
            panic!("expected BGRA");
        };
        assert_eq!(bgra.display_time, 42);
        assert_eq!(&bgra.data[..8], &[3, 2, 1, 255, 6, 5, 4, 255]);

        let Some(Frame::RGB(rgb)) = copy_frame(&frame(&data, 12), None, FrameType::RGB) else {
            panic!("expected RGB");
        };
        assert_eq!(rgb.data, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);

        let Some(Frame::YUVFrame(yuv)) = copy_frame(&frame(&data, 12), None, FrameType::YUVFrame)
        else {
            panic!("expected NV12");
        };
        assert_eq!((yuv.width, yuv.height), (2, 2));
    }

    #[test]
    fn crop_area_is_applied() {
        let data = [
            1, 2, 3, 0, 4, 5, 6, 0, //
            7, 8, 9, 0, 10, 11, 12, 0,
        ];
        let crop = crop_rect(&Area {
            origin: Point { x: 1.0, y: 1.0 },
            size: Size {
                width: 5.0,
                height: 5.0,
            },
        });
        let Some(Frame::RGB(rgb)) = copy_frame(&frame(&data, 0), crop, FrameType::RGB) else {
            panic!("expected RGB");
        };
        assert_eq!((rgb.width, rgb.height), (1, 1));
        assert_eq!(rgb.data, vec![10, 11, 12]);
    }

    #[test]
    fn empty_crop_areas_are_ignored() {
        let area = Area {
            origin: Point { x: -4.0, y: 0.0 },
            size: Size {
                width: 0.0,
                height: 10.0,
            },
        };
        assert_eq!(crop_rect(&area), None);
    }
}
