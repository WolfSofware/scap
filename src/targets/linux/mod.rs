//! Linux targets come from the session's own display server: RandR monitors
//! and EWMH client windows on X11, `wl_output`s and
//! `ext-foreign-toplevel-list` toplevels on Wayland.

use super::{Display, Target, TargetError, Window};
use crate::utils::linux::display_server::{get_current_display_protocol, DisplayProtocol};

mod wayland;
pub(crate) mod x11;

/// Where a Linux display lives
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayHandle {
    /// A RandR monitor, in root window coordinates
    X11 {
        x: i16,
        y: i16,
        width: u16,
        height: u16,
    },
    /// A `wl_output`: position in compositor coordinates, size of the
    /// current mode in pixels
    Wayland {
        name: Option<String>,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
}

/// Which Linux window a [Window] is
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowHandle {
    /// X11 window id
    X11(u32),
    /// `ext_foreign_toplevel_handle_v1` identifier
    Wayland {
        identifier: String,
        app_id: Option<String>,
    },
}

pub(crate) fn current_protocol() -> Result<DisplayProtocol, TargetError> {
    get_current_display_protocol().ok_or_else(|| {
        TargetError::new("No graphical session: neither WAYLAND_DISPLAY nor DISPLAY is set")
    })
}

pub fn get_all_displays() -> Result<Vec<Display>, TargetError> {
    match current_protocol()? {
        DisplayProtocol::X11 => x11::get_all_displays(),
        DisplayProtocol::Wayland => wayland::get_all_displays(),
    }
}

pub fn get_all_windows() -> Result<Vec<Window>, TargetError> {
    match current_protocol()? {
        DisplayProtocol::X11 => x11::get_all_windows(),
        DisplayProtocol::Wayland => wayland::get_all_windows(),
    }
}

/// The primary monitor on X11, the first output on Wayland
pub fn get_main_display() -> Result<Display, TargetError> {
    get_all_displays()?
        .into_iter()
        .next()
        .ok_or_else(|| TargetError::new("No displays found"))
}

pub fn get_target_dimensions(target: &Target) -> Result<(u64, u64), TargetError> {
    match target {
        Target::Display(display) => Ok(match display.raw_handle {
            DisplayHandle::X11 { width, height, .. } => (width.into(), height.into()),
            DisplayHandle::Wayland { width, height, .. } => {
                (width.max(0) as u64, height.max(0) as u64)
            }
        }),
        Target::Window(window) => match window.raw_handle {
            WindowHandle::X11(id) => x11::window_size(id),
            WindowHandle::Wayland { .. } => Err(TargetError::new(
                "Wayland does not expose window sizes to other clients",
            )),
        },
    }
}

/// 32-bit FNV-1a, turns Wayland's string identifiers into stable ids
pub(crate) fn fnv1a(value: &str) -> u32 {
    value.bytes().fold(0x811c_9dc5, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

/// Process name from procfs, for windows without WM_CLASS
pub(crate) fn process_name(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::fnv1a;

    #[test]
    fn fnv1a_matches_reference_values() {
        assert_eq!(fnv1a(""), 0x811c_9dc5);
        assert_eq!(fnv1a("a"), 0xe40c_292c);
        assert_eq!(fnv1a("foobar"), 0xbf9c_f968);
    }
}
