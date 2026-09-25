use cocoa::appkit::{NSApp, NSScreen};
use cocoa::base::{id, nil};
use cocoa::foundation::{NSRect, NSString, NSUInteger};
use core_graphics_helmer_fork::display::{CGDirectDisplayID, CGDisplay, CGMainDisplayID};
use core_graphics_helmer_fork::window::CGWindowID;
use objc::{msg_send, sel, sel_impl};
use screencapturekit::shareable_content::SCShareableContent;

use super::{Application, Display, Target, TargetError, Window};

fn get_display_name(display_id: CGDirectDisplayID) -> String {
    unsafe {
        // Get all screens
        let screens: id = NSScreen::screens(nil);
        let count: u64 = msg_send![screens, count];

        for i in 0..count {
            let screen: id = msg_send![screens, objectAtIndex: i];
            let device_description: id = msg_send![screen, deviceDescription];
            let display_id_number: id = msg_send![device_description, objectForKey: NSString::alloc(nil).init_str("NSScreenNumber")];
            let display_id_number: u32 = msg_send![display_id_number, unsignedIntValue];

            if display_id_number == display_id {
                let localized_name: id = msg_send![screen, localizedName];
                let name: *const i8 = msg_send![localized_name, UTF8String];
                if !name.is_null() {
                    return std::ffi::CStr::from_ptr(name)
                        .to_string_lossy()
                        .into_owned();
                }
            }
        }

        format!("Unknown Display {}", display_id)
    }
}

pub fn get_all_displays() -> Result<Vec<Display>, TargetError> {
    let content = SCShareableContent::get().map_err(|error| TargetError::new(error.to_string()))?;

    Ok(content
        .displays()
        .into_iter()
        .map(|display| {
            let id: CGDirectDisplayID = display.display_id();
            Display {
                id,
                title: get_display_name(id),
                raw_handle: CGDisplay::new(id),
            }
        })
        .collect())
}

pub fn get_all_windows() -> Result<Vec<Window>, TargetError> {
    let content = SCShareableContent::get().map_err(|error| TargetError::new(error.to_string()))?;

    Ok(content
        .windows()
        .into_iter()
        .filter_map(|window| {
            let title = window.title()?;
            let id = window.window_id();
            let application = window.owning_application();
            let raw_handle: CGWindowID = id;

            Some(Window {
                id,
                title,
                process_id: application
                    .as_ref()
                    .and_then(|application| u32::try_from(application.process_id()).ok()),
                app_name: application.map(|application| application.application_name()),
                raw_handle,
            })
        })
        .collect())
}

pub fn get_all_applications() -> Result<Vec<Application>, TargetError> {
    let content = SCShareableContent::get().map_err(|error| TargetError::new(error.to_string()))?;

    Ok(content
        .applications()
        .into_iter()
        .filter_map(|application| {
            let name = application.application_name();
            if name.is_empty() {
                return None;
            }
            Some(Application {
                pid: u32::try_from(application.process_id()).ok()?,
                name,
            })
        })
        .collect())
}

pub fn get_main_display() -> Result<Display, TargetError> {
    let id = unsafe { CGMainDisplayID() };
    let title = get_display_name(id);

    Ok(Display {
        id,
        title,
        raw_handle: CGDisplay::new(id),
    })
}

pub fn get_scale_factor(target: &Target) -> Result<f64, TargetError> {
    Ok(match target {
        Target::Window(window) => unsafe {
            let cg_win_id = window.raw_handle;
            let ns_app: id = NSApp();
            let ns_window: id = msg_send![ns_app, windowWithWindowNumber: cg_win_id as NSUInteger];
            if ns_window == nil {
                return Err(TargetError::new("Window is no longer available"));
            }
            msg_send![ns_window, backingScaleFactor]
        },
        Target::Display(display) => {
            let mode = display
                .raw_handle
                .display_mode()
                .ok_or_else(|| TargetError::new("Display mode is unavailable"))?;
            (mode.pixel_width() / mode.width()) as f64
        }
    })
}

pub fn get_target_dimensions(target: &Target) -> Result<(u64, u64), TargetError> {
    Ok(match target {
        Target::Window(window) => unsafe {
            let cg_win_id = window.raw_handle;
            let ns_app: id = NSApp();
            let ns_window: id = msg_send![ns_app, windowWithWindowNumber: cg_win_id as NSUInteger];
            if ns_window == nil {
                return Err(TargetError::new("Window is no longer available"));
            }
            let frame: NSRect = msg_send![ns_window, frame];
            (frame.size.width as u64, frame.size.height as u64)
        },
        Target::Display(display) => {
            let mode = display
                .raw_handle
                .display_mode()
                .ok_or_else(|| TargetError::new("Display mode is unavailable"))?;
            (mode.width(), mode.height())
        }
    })
}
