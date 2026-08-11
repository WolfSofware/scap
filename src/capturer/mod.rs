pub mod engine;

use std::{error::Error, sync::mpsc};

use engine::ChannelItem;

use crate::{
    frame::{Frame, FrameType},
    has_permission, is_supported,
    targets::Target,
};

pub use engine::get_output_frame_size;

#[derive(Debug, Clone, Copy, Default)]
pub enum Resolution {
    _480p,
    _720p,
    _1080p,
    _1440p,
    _2160p,
    _4320p,

    #[default]
    Captured,
}

impl Resolution {
    fn value(&self, aspect_ratio: f32) -> Option<[u32; 2]> {
        let width = match *self {
            Resolution::_480p => 640,
            Resolution::_720p => 1280,
            Resolution::_1080p => 1920,
            Resolution::_1440p => 2560,
            Resolution::_2160p => 3840,
            Resolution::_4320p => 7680,
            Resolution::Captured => return None,
        };
        Some([width, (width as f32 / aspect_ratio).floor() as u32])
    }
}

#[derive(Debug, Default, Clone)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Default, Clone)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}
#[derive(Debug, Default, Clone)]
pub struct Area {
    pub origin: Point,
    pub size: Size,
}

/// Options passed to the screen capturer
#[derive(Debug, Default, Clone)]
pub struct Options {
    pub fps: u32,
    pub show_cursor: bool,
    pub show_highlight: bool,
    pub target: Option<Target>,
    pub crop_area: Option<Area>,
    pub output_type: FrameType,
    pub output_resolution: Resolution,
    // excluded targets will only work on macOS
    pub excluded_targets: Option<Vec<Target>>,
}

/// Screen capturer class
pub struct Capturer {
    engine: engine::Engine,
    rx: mpsc::Receiver<ChannelItem>,
    started: bool,
}

#[derive(Debug)]
pub enum CapturerBuildError {
    NotSupported,
    PermissionNotGranted,
    Initialization(String),
}

impl std::fmt::Display for CapturerBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapturerBuildError::NotSupported => write!(f, "Screen capturing is not supported"),
            CapturerBuildError::PermissionNotGranted => {
                write!(f, "Permission to capture the screen is not granted")
            }
            CapturerBuildError::Initialization(error) => f.write_str(error),
        }
    }
}

impl Error for CapturerBuildError {}

impl Capturer {
    /// Create a new capturer instance with the provided options
    #[deprecated(
        since = "0.0.6",
        note = "Use `build` instead of `new` to create a new capturer instance."
    )]
    pub fn new(options: Options) -> Result<Capturer, CapturerBuildError> {
        Self::build(options)
    }

    /// Build a new [Capturer] instance with the provided options
    pub fn build(options: Options) -> Result<Capturer, CapturerBuildError> {
        if !is_supported() {
            return Err(CapturerBuildError::NotSupported);
        }

        if !has_permission() {
            return Err(CapturerBuildError::PermissionNotGranted);
        }

        let (tx, rx) = mpsc::channel();
        let engine = engine::Engine::new(&options, tx)?;

        Ok(Capturer {
            engine,
            rx,
            started: false,
        })
    }

    // TODO
    // Prevent starting capture if already started
    /// Start capturing the frames
    pub fn start_capture(&mut self) -> Result<(), CapturerError> {
        if !self.started {
            self.engine.start()?;
            self.started = true;
        }
        Ok(())
    }

    /// Stop the capturer
    pub fn stop_capture(&mut self) -> Result<(), CapturerError> {
        if self.started {
            self.engine.stop()?;
            self.started = false;
        }
        Ok(())
    }

    /// Get the next captured frame
    pub fn get_next_frame(&self) -> Result<Frame, mpsc::RecvError> {
        loop {
            let res = self.rx.recv()?;

            if let Some(frame) = self.engine.process_channel_item(res) {
                return Ok(frame);
            }
        }
    }

    /// Get the dimensions the frames will be captured in
    pub fn get_output_frame_size(&mut self) -> Result<[u32; 2], crate::TargetError> {
        self.engine.get_output_frame_size()
    }

    pub fn raw(&self) -> RawCapturer {
        RawCapturer { capturer: self }
    }
}

impl Drop for Capturer {
    fn drop(&mut self) {
        if let Err(error) = self.stop_capture() {
            eprintln!("Failed to stop screen capture: {error}");
        }
    }
}

#[derive(Debug)]
pub struct CapturerError(String);

impl CapturerError {
    pub(crate) fn new(error: impl std::fmt::Display) -> Self {
        Self(error.to_string())
    }
}

impl std::fmt::Display for CapturerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for CapturerError {}

pub struct RawCapturer<'a> {
    capturer: &'a Capturer,
}

#[cfg(test)]
mod tests {
    use super::Resolution;

    #[test]
    fn captured_resolution_has_no_explicit_size() {
        assert_eq!(Resolution::Captured.value(16.0 / 9.0), None);
    }
}
