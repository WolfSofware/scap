//! Cross Platform, Performant and High Quality screen recordings

pub mod capturer;
pub mod frame;
pub mod subscription;
pub mod targets;
mod utils;

// Helper Methods
pub use subscription::{Subscription, SubscriptionBuilder, SubscriptionError, TargetId};
pub use targets::get_all_applications;
pub use targets::get_all_displays;
pub use targets::get_all_targets;
pub use targets::get_all_windows;
pub use targets::Application;
pub use targets::Target;
pub use targets::TargetError;
pub use utils::has_permission;
pub use utils::is_supported;
pub use utils::request_permission;

#[cfg(target_os = "linux")]
pub use utils::linux::display_server::{
    get_current_display_protocol, get_installed_display_servers, DisplayProtocol, DisplayServer,
    DisplayServerSource,
};

#[cfg(target_os = "macos")]
pub mod engine {
    pub use crate::capturer::engine::mac;
}
