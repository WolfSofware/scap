//! Callback based capture of a single display, window or application.
//!
//! ```no_run
//! use scap::{Subscription, TargetId};
//!
//! let display = scap::get_all_displays()?.remove(0);
//! let subscription = Subscription::builder(TargetId::Display(display.id))
//!     .fps(60)
//!     .on_video(|frame| {
//!         // `frame.native` borrows the GPU buffer; nothing was copied
//!         println!("{}x{} @ {}", frame.width, frame.height, frame.display_time);
//!     })
//!     .on_audio(|audio| println!("{} audio frames", audio.frames))
//!     .start()?;
//! std::thread::sleep(std::time::Duration::from_secs(5));
//! subscription.stop()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Callbacks run on a thread owned by the capture API (or by scap). Return
//! quickly; a slow callback delays the next frame instead of queueing it.

use std::fmt;

use crate::frame::{AudioFrame, VideoFrame};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use mac as platform;
#[cfg(target_os = "windows")]
use win as platform;

/// What a [Subscription] captures, by the `id` reported by
/// [get_all_displays](crate::get_all_displays),
/// [get_all_windows](crate::get_all_windows) or
/// [get_all_applications](crate::get_all_applications)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetId {
    Display(u32),
    Window(u32),
    /// Process id of the application
    Application(u32),
}

pub(crate) type VideoCallback = Box<dyn FnMut(&VideoFrame<'_>) + Send + 'static>;
pub(crate) type AudioCallback = Box<dyn FnMut(&AudioFrame<'_>) + Send + 'static>;

pub(crate) struct Config {
    pub target: TargetId,
    pub fps: u32,
    pub show_cursor: bool,
    pub video: Option<VideoCallback>,
    pub audio: Option<AudioCallback>,
}

/// Configures and starts a [Subscription]
pub struct SubscriptionBuilder {
    config: Config,
}

impl SubscriptionBuilder {
    /// Upper bound for the video frame rate (default 60)
    pub fn fps(mut self, fps: u32) -> Self {
        self.config.fps = fps;
        self
    }

    /// Whether the cursor is drawn into video frames (default `true`)
    pub fn show_cursor(mut self, show_cursor: bool) -> Self {
        self.config.show_cursor = show_cursor;
        self
    }

    /// Receive video frames. Without this callback no video is captured.
    pub fn on_video(mut self, callback: impl FnMut(&VideoFrame<'_>) + Send + 'static) -> Self {
        self.config.video = Some(Box::new(callback));
        self
    }

    /// Receive the audio the target plays: everything on the system for a
    /// display, the owning process for a window or application. Without this
    /// callback no audio is captured.
    pub fn on_audio(mut self, callback: impl FnMut(&AudioFrame<'_>) + Send + 'static) -> Self {
        self.config.audio = Some(Box::new(callback));
        self
    }

    pub fn start(self) -> Result<Subscription, SubscriptionError> {
        if self.config.video.is_none() && self.config.audio.is_none() {
            return Err(SubscriptionError::NothingToCapture);
        }
        if self.config.fps == 0 {
            return Err(SubscriptionError::Platform("fps must be greater than 0".into()));
        }
        if !crate::is_supported() {
            return Err(SubscriptionError::NotSupported);
        }
        if !crate::has_permission() {
            return Err(SubscriptionError::PermissionNotGranted);
        }
        Ok(Subscription {
            inner: Some(platform::start(self.config)?),
        })
    }
}

/// A running capture of one target. Dropping it stops the capture.
pub struct Subscription {
    inner: Option<platform::Subscription>,
}

impl Subscription {
    pub fn builder(target: TargetId) -> SubscriptionBuilder {
        SubscriptionBuilder {
            config: Config {
                target,
                fps: 60,
                show_cursor: true,
                video: None,
                audio: None,
            },
        }
    }

    /// Stops capturing and reports any error the capture ended with
    pub fn stop(mut self) -> Result<(), SubscriptionError> {
        match self.inner.take() {
            Some(inner) => inner.stop(),
            None => Ok(()),
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            if let Err(error) = inner.stop() {
                eprintln!("Failed to stop subscription: {error}");
            }
        }
    }
}

#[derive(Debug)]
pub enum SubscriptionError {
    NotSupported,
    PermissionNotGranted,
    /// Neither `on_video` nor `on_audio` was set
    NothingToCapture,
    TargetNotFound(TargetId),
    /// The target exists but this platform can't capture it the requested way
    Unsupported(String),
    Platform(String),
}

impl SubscriptionError {
    pub(crate) fn platform(error: impl fmt::Display) -> Self {
        Self::Platform(error.to_string())
    }
}

impl fmt::Display for SubscriptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubscriptionError::NotSupported => f.write_str("Screen capturing is not supported"),
            SubscriptionError::PermissionNotGranted => {
                f.write_str("Permission to capture the screen is not granted")
            }
            SubscriptionError::NothingToCapture => {
                f.write_str("Set on_video and/or on_audio before starting a subscription")
            }
            SubscriptionError::TargetNotFound(target) => write!(f, "{target:?} was not found"),
            SubscriptionError::Unsupported(error) | SubscriptionError::Platform(error) => {
                f.write_str(error)
            }
        }
    }
}

impl std::error::Error for SubscriptionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_without_callbacks_is_rejected() {
        let result = Subscription::builder(TargetId::Display(0)).start();
        assert!(matches!(result, Err(SubscriptionError::NothingToCapture)));
    }

    // Apps keep subscriptions in shared state (e.g. `tauri::State<Mutex<_>>`)
    #[test]
    fn subscription_can_move_between_threads() {
        fn assert_send<T: Send>() {}
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send::<Subscription>();
        assert_send_sync::<SubscriptionError>();
        assert_send_sync::<crate::Target>();
        assert_send_sync::<crate::Application>();
    }

    #[test]
    fn zero_fps_is_rejected() {
        let result = Subscription::builder(TargetId::Display(0))
            .fps(0)
            .on_video(|_| {})
            .start();
        assert!(matches!(result, Err(SubscriptionError::Platform(_))));
    }
}
