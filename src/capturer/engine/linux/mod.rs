use std::sync::mpsc;

use crate::{
    capturer::Options,
    frame::{
        BGRAFrame, BGRxFrame, Frame, NativeFrame, PixelFormat, RGBxFrame, VideoFrame, XBGRFrame,
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

        // Frames are copied into owned buffers, so ask for mappable memory
        let stream = VideoStream::spawn(
            VideoStreamOptions {
                node_id,
                fps: options.fps,
                allow_dmabuf: false,
                active: false,
            },
            Box::new(move |frame| {
                if let Some(frame) = copy_frame(frame) {
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

/// Copies a mapped PipeWire frame into an owned [Frame], dropping row padding
fn copy_frame(frame: &VideoFrame<'_>) -> Option<Frame> {
    let NativeFrame::Memory { data, stride } = frame.native else {
        return None;
    };
    let data = tight_rows(data, stride as usize, frame.width as usize * 4, frame.height as usize)?;
    let (display_time, width, height) = (frame.display_time, frame.width as i32, frame.height as i32);

    Some(match frame.format {
        PixelFormat::Bgrx => Frame::BGRx(BGRxFrame {
            display_time,
            width,
            height,
            data,
        }),
        PixelFormat::Bgra => Frame::BGRA(BGRAFrame {
            display_time,
            width,
            height,
            data,
        }),
        // RGBA shares RGBx's layout; the alpha byte is the padding byte
        PixelFormat::Rgbx | PixelFormat::Rgba => Frame::RGBx(RGBxFrame {
            display_time,
            width,
            height,
            data,
        }),
        PixelFormat::Xbgr => Frame::XBGR(XBGRFrame {
            display_time,
            width,
            height,
            data,
        }),
        _ => return None,
    })
}

fn tight_rows(data: &[u8], stride: usize, row: usize, height: usize) -> Option<Vec<u8>> {
    let stride = if stride == 0 { row } else { stride };
    if stride < row || data.len() < stride * height.saturating_sub(1) + row {
        return None;
    }
    if stride == row {
        return Some(data[..row * height].to_vec());
    }
    let mut tight = Vec::with_capacity(row * height);
    for line in data.chunks(stride).take(height) {
        tight.extend_from_slice(&line[..row]);
    }
    Some(tight)
}

pub fn create_capturer(
    options: &Options,
    tx: mpsc::Sender<Frame>,
) -> Result<LinuxCapturer, LinCapError> {
    LinuxCapturer::new(options, tx)
}

#[cfg(test)]
mod tests {
    use super::tight_rows;

    #[test]
    fn strips_row_padding() {
        let data = [1, 2, 0, 3, 4, 0];
        assert_eq!(tight_rows(&data, 3, 2, 2), Some(vec![1, 2, 3, 4]));
        assert_eq!(tight_rows(&data[..5], 3, 2, 2), Some(vec![1, 2, 3, 4]));
        assert_eq!(tight_rows(&data, 0, 3, 2), Some(data.to_vec()));
        assert_eq!(tight_rows(&data, 3, 2, 3), None);
    }
}
