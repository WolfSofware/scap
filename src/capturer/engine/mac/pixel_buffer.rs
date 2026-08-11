use screencapturekit::cm::SCFrameStatus;
use screencapturekit::prelude::{CMSampleBuffer, CMSampleBufferExt, CMSampleBufferSCExt};
use std::{ops::Deref, sync::mpsc};

use crate::capturer::{engine::ChannelItem, RawCapturer};

pub struct PixelBuffer {
    display_time: u64,
    width: usize,
    height: usize,
    bytes_per_row: usize,
    buffer: CMSampleBuffer,
}

impl PixelBuffer {
    pub fn display_time(&self) -> u64 {
        self.display_time
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn buffer(&self) -> &CMSampleBuffer {
        &self.buffer
    }

    pub fn bytes_per_row(&self) -> usize {
        self.bytes_per_row
    }

    pub fn data(&self) -> PixelBufferData {
        let data = self
            .buffer
            .image_buffer()
            .and_then(|buffer| {
                buffer
                    .lock_read_only()
                    .ok()
                    .map(|guard| guard.as_slice().to_vec())
            })
            .unwrap_or_default();
        PixelBufferData(data)
    }

    pub fn planes(&self) -> Vec<Plane> {
        let Some(buffer) = self.buffer.image_buffer() else {
            return Vec::new();
        };
        let Ok(guard) = buffer.lock_read_only() else {
            return Vec::new();
        };
        (0..guard.plane_count())
            .filter_map(|index| {
                Some(Plane {
                    width: guard.width_of_plane(index),
                    height: guard.height_of_plane(index),
                    bytes_per_row: guard.bytes_per_row_of_plane(index),
                    data: guard.plane_data(index)?.to_vec(),
                })
            })
            .collect()
    }

    pub(crate) fn new(item: ChannelItem) -> Option<Self> {
        if !matches!(
            item.0.frame_status()?,
            SCFrameStatus::Complete | SCFrameStatus::Started | SCFrameStatus::Idle
        ) {
            return None;
        }
        let image = item.0.image_buffer()?;
        Some(Self {
            display_time: super::pixelformat::get_pts_in_nanoseconds(&item.0),
            width: image.width(),
            height: image.height(),
            bytes_per_row: image.bytes_per_row(),
            buffer: item.0,
        })
    }
}

impl From<PixelBuffer> for CMSampleBuffer {
    fn from(value: PixelBuffer) -> Self {
        value.buffer
    }
}

#[derive(Debug)]
pub struct Plane {
    width: usize,
    height: usize,
    bytes_per_row: usize,
    data: Vec<u8>,
}

impl Plane {
    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn bytes_per_row(&self) -> usize {
        self.bytes_per_row
    }

    pub fn data(&self) -> PixelBufferData {
        PixelBufferData(self.data.clone())
    }
}

pub struct PixelBufferData(Vec<u8>);

impl Deref for PixelBufferData {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl RawCapturer<'_> {
    #[cfg(target_os = "macos")]
    pub fn get_next_pixel_buffer(&self) -> Result<PixelBuffer, mpsc::RecvError> {
        use std::time::Duration;

        loop {
            if self
                .capturer
                .engine
                .error_flag
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(mpsc::RecvError);
            }

            let item = match self.capturer.rx.recv_timeout(Duration::from_millis(10)) {
                Ok(item) => item,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(mpsc::RecvError),
            };

            if let Some(frame) = PixelBuffer::new(item) {
                return Ok(frame);
            }
        }
    }
}
