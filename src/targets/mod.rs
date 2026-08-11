#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "linux")]
mod linux;

#[derive(Debug, Clone)]
pub struct Window {
    pub id: u32,
    pub title: String,

    #[cfg(target_os = "windows")]
    pub raw_handle: windows::Win32::Foundation::HWND,

    #[cfg(target_os = "macos")]
    pub raw_handle: core_graphics_helmer_fork::window::CGWindowID,
}

#[derive(Debug, Clone)]
pub struct Display {
    pub id: u32,
    pub title: String,

    #[cfg(target_os = "windows")]
    pub raw_handle: windows::Win32::Graphics::Gdi::HMONITOR,

    #[cfg(target_os = "macos")]
    pub raw_handle: core_graphics_helmer_fork::display::CGDisplay,
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
    #[cfg(target_os = "macos")]
    return mac::get_all_targets();

    #[cfg(target_os = "windows")]
    return win::get_all_targets();

    #[cfg(target_os = "linux")]
    Ok(linux::get_all_targets())
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
    Err(TargetError::new(
        "Linux capture target is selected by the desktop portal",
    ))
}

pub fn get_target_dimensions(target: &Target) -> Result<(u64, u64), TargetError> {
    #[cfg(target_os = "macos")]
    return mac::get_target_dimensions(target);

    #[cfg(target_os = "windows")]
    return win::get_target_dimensions(target);

    #[cfg(target_os = "linux")]
    Err(TargetError::new(
        "Linux target dimensions are reported by PipeWire",
    ))
}
