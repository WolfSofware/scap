use std::sync::mpsc;

use super::{CapturerBuildError, CapturerError, Options};
use crate::frame::Frame;
use crate::targets::TargetError;

#[cfg(target_os = "macos")]
pub mod mac;

#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
pub type ChannelItem = (
    screencapturekit::cm::CMSampleBuffer,
    screencapturekit::prelude::SCStreamOutputType,
);
#[cfg(not(target_os = "macos"))]
pub type ChannelItem = Frame;

pub fn get_output_frame_size(options: &Options) -> Result<[u32; 2], TargetError> {
    #[cfg(target_os = "macos")]
    {
        mac::get_output_frame_size(options)
    }

    #[cfg(target_os = "windows")]
    {
        win::get_output_frame_size(options)
    }

    #[cfg(target_os = "linux")]
    {
        // TODO: How to calculate this on Linux?
        return Ok([0, 0]);
    }
}

pub struct Engine {
    options: Options,

    #[cfg(target_os = "macos")]
    mac: screencapturekit::stream::SCStream,
    #[cfg(target_os = "macos")]
    error_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,

    #[cfg(target_os = "windows")]
    win: win::WCStream,

    #[cfg(target_os = "linux")]
    linux: linux::LinuxCapturer,
}

impl Engine {
    pub fn new(
        options: &Options,
        tx: mpsc::Sender<ChannelItem>,
    ) -> Result<Engine, CapturerBuildError> {
        #[cfg(target_os = "macos")]
        {
            let error_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mac = mac::create_capturer(options, tx, error_flag.clone())
                .map_err(CapturerBuildError::Initialization)?;

            Ok(Engine {
                mac,
                error_flag,
                options: (*options).clone(),
            })
        }

        #[cfg(target_os = "windows")]
        {
            let win =
                win::create_capturer(options, tx).map_err(CapturerBuildError::Initialization)?;
            return Ok(Engine {
                win,
                options: (*options).clone(),
            });
        }

        #[cfg(target_os = "linux")]
        {
            let linux = linux::create_capturer(options, tx)
                .map_err(|error| CapturerBuildError::Initialization(error.to_string()))?;
            return Ok(Engine {
                linux,
                options: (*options).clone(),
            });
        }
    }

    pub fn start(&mut self) -> Result<(), CapturerError> {
        #[cfg(target_os = "macos")]
        {
            // self.mac.add_output(Capturer::new(tx));
            self.mac.start_capture().map_err(CapturerError::new)?;
        }

        #[cfg(target_os = "windows")]
        {
            self.win.start_capture().map_err(CapturerError::new)?;
        }

        #[cfg(target_os = "linux")]
        {
            self.linux.start_capture().map_err(CapturerError::new)?;
        }
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), CapturerError> {
        #[cfg(target_os = "macos")]
        {
            self.mac.stop_capture().map_err(CapturerError::new)?;
        }

        #[cfg(target_os = "windows")]
        {
            self.win.stop_capture().map_err(CapturerError::new)?;
        }

        #[cfg(target_os = "linux")]
        {
            self.linux.stop_capture().map_err(CapturerError::new)?;
        }
        Ok(())
    }

    pub fn get_output_frame_size(&mut self) -> Result<[u32; 2], TargetError> {
        get_output_frame_size(&self.options)
    }

    pub fn process_channel_item(&self, data: ChannelItem) -> Option<Frame> {
        #[cfg(target_os = "macos")]
        {
            mac::process_sample_buffer(data.0, data.1, self.options.output_type)
        }
        #[cfg(not(target_os = "macos"))]
        return Some(data);
    }
}
