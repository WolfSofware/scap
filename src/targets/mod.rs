#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "linux")]
pub(crate) mod linux;

#[cfg(target_os = "linux")]
pub use linux::{DisplayHandle, WindowHandle};

#[derive(Debug, Clone)]
pub struct Window {
    pub id: u32,
    pub title: String,
    /// Process that owns the window, when the platform reports it
    pub process_id: Option<u32>,
    /// Name of the owning application, when the platform reports it
    pub app_name: Option<String>,

    #[cfg(target_os = "windows")]
    pub raw_handle: windows::Win32::Foundation::HWND,

    #[cfg(target_os = "macos")]
    pub raw_handle: core_graphics_helmer_fork::window::CGWindowID,

    #[cfg(target_os = "linux")]
    pub raw_handle: WindowHandle,
}

#[derive(Debug, Clone)]
pub struct Display {
    pub id: u32,
    pub title: String,

    #[cfg(target_os = "windows")]
    pub raw_handle: windows::Win32::Graphics::Gdi::HMONITOR,

    #[cfg(target_os = "macos")]
    pub raw_handle: core_graphics_helmer_fork::display::CGDisplay,

    #[cfg(target_os = "linux")]
    pub raw_handle: DisplayHandle,
}

// HWND and HMONITOR wrap raw pointers, but they are process-wide handle
// values, not memory; any thread may pass them to the Win32 APIs scap calls
#[cfg(target_os = "windows")]
unsafe impl Send for Window {}
#[cfg(target_os = "windows")]
unsafe impl Sync for Window {}
#[cfg(target_os = "windows")]
unsafe impl Send for Display {}
#[cfg(target_os = "windows")]
unsafe impl Sync for Display {}

/// A running application that owns at least one capturable window
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Application {
    /// Process id; use it with [TargetId::Application](crate::TargetId::Application)
    pub pid: u32,
    pub name: String,
}

#[derive(Debug, Clone)]
pub enum Target {
    Window(Window),
    Display(Display),
}

#[derive(Debug)]
pub struct TargetError(String);

impl TargetError {
    pub(crate) fn new(error: impl std::fmt::Display) -> Self {
        Self(error.to_string())
    }
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TargetError {}

/// Returns a list of targets that can be captured
pub fn get_all_targets() -> Result<Vec<Target>, TargetError> {
    let mut targets = get_all_displays()?
        .into_iter()
        .map(Target::Display)
        .collect::<Vec<_>>();
    targets.extend(get_all_windows()?.into_iter().map(Target::Window));
    Ok(targets)
}

/// Returns the displays (monitors) that can be captured
pub fn get_all_displays() -> Result<Vec<Display>, TargetError> {
    #[cfg(target_os = "macos")]
    return mac::get_all_displays();

    #[cfg(target_os = "windows")]
    return win::get_all_displays();

    #[cfg(target_os = "linux")]
    return linux::get_all_displays();
}

/// Returns the windows that can be captured
pub fn get_all_windows() -> Result<Vec<Window>, TargetError> {
    #[cfg(target_os = "macos")]
    return mac::get_all_windows();

    #[cfg(target_os = "windows")]
    return win::get_all_windows();

    #[cfg(target_os = "linux")]
    return linux::get_all_windows();
}

/// Returns the applications that own capturable windows, sorted by name
pub fn get_all_applications() -> Result<Vec<Application>, TargetError> {
    #[cfg(target_os = "macos")]
    let mut applications = mac::get_all_applications()?;

    #[cfg(not(target_os = "macos"))]
    let mut applications = applications_from_windows(&get_all_windows()?);

    // Wayland hides window owners; PipeWire still knows which apps play audio
    #[cfg(target_os = "linux")]
    if let Ok(streams) = crate::capturer::engine::linux::audio::playback_streams() {
        for stream in streams {
            if !applications.iter().any(|application| application.pid == stream.pid) {
                let name = linux::process_name(stream.pid).unwrap_or(stream.application);
                applications.push(Application {
                    pid: stream.pid,
                    name,
                });
            }
        }
    }

    applications.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.pid.cmp(&b.pid)));
    Ok(applications)
}

/// Groups windows by owning process; windows without a process id are skipped
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn applications_from_windows(windows: &[Window]) -> Vec<Application> {
    let mut applications: Vec<Application> = Vec::new();
    for window in windows {
        let Some(pid) = window.process_id else {
            continue;
        };
        if applications.iter().any(|application| application.pid == pid) {
            continue;
        }
        let name = window
            .app_name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| window.title.clone());
        applications.push(Application { pid, name });
    }
    applications
}

/// Looks up a display by the id reported in [get_all_displays]
pub fn find_display(id: u32) -> Result<Option<Display>, TargetError> {
    Ok(get_all_displays()?.into_iter().find(|display| display.id == id))
}

/// Looks up a window by the id reported in [get_all_windows]
pub fn find_window(id: u32) -> Result<Option<Window>, TargetError> {
    Ok(get_all_windows()?.into_iter().find(|window| window.id == id))
}

pub fn get_scale_factor(target: &Target) -> Result<f64, TargetError> {
    #[cfg(target_os = "macos")]
    return mac::get_scale_factor(target);

    #[cfg(target_os = "windows")]
    return win::get_scale_factor(target);

    #[cfg(target_os = "linux")]
    Ok(1.0)
}

pub fn get_main_display() -> Result<Display, TargetError> {
    #[cfg(target_os = "macos")]
    return mac::get_main_display();

    #[cfg(target_os = "windows")]
    return win::get_main_display();

    #[cfg(target_os = "linux")]
    return linux::get_main_display();
}

pub fn get_target_dimensions(target: &Target) -> Result<(u64, u64), TargetError> {
    #[cfg(target_os = "macos")]
    return mac::get_target_dimensions(target);

    #[cfg(target_os = "windows")]
    return win::get_target_dimensions(target);

    #[cfg(target_os = "linux")]
    return linux::get_target_dimensions(target);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: u32, process_id: Option<u32>, app_name: Option<&str>) -> Window {
        Window {
            id,
            title: format!("Window {id}"),
            process_id,
            app_name: app_name.map(str::to_string),
            #[cfg(target_os = "macos")]
            raw_handle: id,
            #[cfg(target_os = "windows")]
            raw_handle: windows::Win32::Foundation::HWND(std::ptr::null_mut()),
            #[cfg(target_os = "linux")]
            raw_handle: WindowHandle::X11(id),
        }
    }

    #[test]
    fn applications_are_grouped_by_process() {
        let windows = [
            window(1, Some(10), Some("Editor")),
            window(2, Some(10), Some("Editor")),
            window(3, Some(20), None),
            window(4, None, Some("Orphan")),
            window(5, Some(30), Some("")),
        ];

        assert_eq!(
            applications_from_windows(&windows),
            vec![
                Application {
                    pid: 10,
                    name: "Editor".into()
                },
                Application {
                    pid: 20,
                    name: "Window 3".into()
                },
                Application {
                    pid: 30,
                    name: "Window 5".into()
                },
            ]
        );
    }
}
