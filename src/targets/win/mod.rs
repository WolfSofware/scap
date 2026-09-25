use super::{Display, Target, TargetError, Window};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::HMONITOR,
};
use windows_capture::{monitor::Monitor, window::Window as WCWindow};

pub fn get_all_displays() -> Result<Vec<Display>, TargetError> {
    Monitor::enumerate()
        .map_err(TargetError::new)?
        .into_iter()
        .map(display_from_monitor)
        .collect()
}

pub fn get_all_windows() -> Result<Vec<Window>, TargetError> {
    Ok(WCWindow::enumerate()
        .map_err(TargetError::new)?
        .into_iter()
        .filter_map(|window| {
            // Windows without a title are tool windows the user can't pick
            let title = window.title().ok().filter(|title| !title.is_empty())?;
            let process_id = window.process_id().ok();
            // Fails for elevated processes; the window stays capturable
            let app_name = window
                .process_name()
                .ok()
                .map(|name| name.trim_end_matches(".exe").to_string());

            Some(Window {
                id: window.as_raw_hwnd() as usize as u32,
                title,
                process_id,
                app_name,
                raw_handle: HWND(window.as_raw_hwnd()),
            })
        })
        .collect())
}

fn display_from_monitor(monitor: Monitor) -> Result<Display, TargetError> {
    // `name` is the friendly monitor name; fall back to the GDI device name
    let title = monitor
        .name()
        .ok()
        .filter(|name| !name.is_empty())
        .map_or_else(|| monitor.device_name(), Ok)
        .map_err(TargetError::new)?;

    Ok(Display {
        id: monitor.as_raw_hmonitor() as usize as u32,
        title,
        raw_handle: HMONITOR(monitor.as_raw_hmonitor()),
    })
}

pub fn get_main_display() -> Result<Display, TargetError> {
    display_from_monitor(Monitor::primary().map_err(TargetError::new)?)
}

// Referred to: https://github.com/tauri-apps/tao/blob/ab792dbd6c5f0a708c818b20eaff1d9a7534c7c1/src/platform_impl/windows/dpi.rs#L50
pub fn get_scale_factor(target: &Target) -> Result<f64, TargetError> {
    const BASE_DPI: u32 = 96;

    let mut dpi_x = 0;
    let mut dpi_y = 0;

    let dpi = match target {
        Target::Window(window) => unsafe { GetDpiForWindow(window.raw_handle) },
        Target::Display(display) => unsafe {
            if GetDpiForMonitor(
                display.raw_handle,
                MDT_EFFECTIVE_DPI,
                &mut dpi_x,
                &mut dpi_y,
            )
            .is_ok()
            {
                dpi_x.into()
            } else {
                BASE_DPI
            }
        },
    };

    let scale_factor = dpi as f64 / BASE_DPI as f64;
    Ok(scale_factor)
}

pub fn get_target_dimensions(target: &Target) -> Result<(u64, u64), TargetError> {
    Ok(match target {
        Target::Window(window) => unsafe {
            let hwnd = window.raw_handle;

            // get width and height of the window
            let mut rect = RECT::default();
            windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect)
                .map_err(TargetError::new)?;
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;

            (width as u64, height as u64)
        },
        Target::Display(display) => {
            let monitor = Monitor::from_raw_hmonitor(display.raw_handle.0);

            (
                monitor.width().map_err(TargetError::new)? as u64,
                monitor.height().map_err(TargetError::new)? as u64,
            )
        }
    })
}
