use x11rb::{
    connection::Connection,
    protocol::{
        randr::ConnectionExt as _,
        xproto::{AtomEnum, ConnectionExt as _},
    },
    rust_connection::RustConnection,
};

use super::{process_name, DisplayHandle, WindowHandle};
use crate::targets::{Display, TargetError, Window};

x11rb::atom_manager! {
    pub Atoms: AtomsCookie {
        _NET_CLIENT_LIST,
        _NET_WM_NAME,
        _NET_WM_PID,
        UTF8_STRING,
    }
}

pub(crate) fn connect() -> Result<(RustConnection, usize), TargetError> {
    x11rb::connect(None).map_err(TargetError::new)
}

/// RandR monitors, primary first; the whole screen when RandR is missing.
/// A monitor's id is its name atom, which stays the same while the X server runs.
pub fn get_all_displays() -> Result<Vec<Display>, TargetError> {
    let (connection, screen_number) = connect()?;
    let screen = &connection.setup().roots[screen_number];

    let monitors = connection
        .randr_get_monitors(screen.root, true)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| reply.monitors)
        .unwrap_or_default();

    if monitors.is_empty() {
        return Ok(vec![Display {
            id: 0,
            title: format!("Screen {screen_number}"),
            raw_handle: DisplayHandle::X11 {
                x: 0,
                y: 0,
                width: screen.width_in_pixels,
                height: screen.height_in_pixels,
            },
        }]);
    }

    let mut displays = monitors
        .into_iter()
        .map(|monitor| {
            let title = connection
                .get_atom_name(monitor.name)
                .map_err(TargetError::new)?
                .reply()
                .map_err(TargetError::new)?
                .name;
            Ok((
                monitor.primary,
                Display {
                    id: monitor.name,
                    title: String::from_utf8_lossy(&title).into_owned(),
                    raw_handle: DisplayHandle::X11 {
                        x: monitor.x,
                        y: monitor.y,
                        width: monitor.width,
                        height: monitor.height,
                    },
                },
            ))
        })
        .collect::<Result<Vec<_>, TargetError>>()?;
    displays.sort_by_key(|(primary, _)| !primary);
    Ok(displays.into_iter().map(|(_, display)| display).collect())
}

/// Client windows the window manager lists in `_NET_CLIENT_LIST`
pub fn get_all_windows() -> Result<Vec<Window>, TargetError> {
    let (connection, screen_number) = connect()?;
    let root = connection.setup().roots[screen_number].root;
    let atoms = Atoms::new(&connection)
        .map_err(TargetError::new)?
        .reply()
        .map_err(TargetError::new)?;

    let clients = connection
        .get_property(false, root, atoms._NET_CLIENT_LIST, AtomEnum::WINDOW, 0, u32::MAX)
        .map_err(TargetError::new)?
        .reply()
        .map_err(TargetError::new)?;
    let Some(clients) = clients.value32() else {
        return Ok(Vec::new());
    };

    // Windows can close while we query them; those are skipped
    Ok(clients
        .filter_map(|id| window_info(&connection, &atoms, id))
        .collect())
}

fn window_info(connection: &RustConnection, atoms: &Atoms, id: u32) -> Option<Window> {
    let property = |property: u32, type_: AtomEnum| {
        connection
            .get_property(false, id, property, type_, 0, u32::MAX)
            .ok()?
            .reply()
            .ok()
    };

    let title = property(atoms._NET_WM_NAME, AtomEnum::ANY)
        .filter(|reply| !reply.value.is_empty())
        .or_else(|| property(AtomEnum::WM_NAME.into(), AtomEnum::ANY))
        .map(|reply| String::from_utf8_lossy(&reply.value).into_owned())
        .filter(|title| !title.is_empty())?;
    let process_id = property(atoms._NET_WM_PID, AtomEnum::CARDINAL)
        .and_then(|reply| reply.value32()?.next());
    // WM_CLASS is "instance\0class\0"; the class names the application
    let app_name = property(AtomEnum::WM_CLASS.into(), AtomEnum::STRING)
        .and_then(|reply| wm_class_name(&reply.value))
        .or_else(|| process_id.and_then(process_name));

    Some(Window {
        id,
        title,
        process_id,
        app_name,
        raw_handle: WindowHandle::X11(id),
    })
}

fn wm_class_name(value: &[u8]) -> Option<String> {
    let mut parts = value
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned());
    let instance = parts.next();
    parts.next().or(instance)
}

pub fn window_size(id: u32) -> Result<(u64, u64), TargetError> {
    let (connection, _) = connect()?;
    let geometry = connection
        .get_geometry(id)
        .map_err(TargetError::new)?
        .reply()
        .map_err(TargetError::new)?;
    Ok((geometry.width.into(), geometry.height.into()))
}

#[cfg(test)]
mod tests {
    use super::wm_class_name;

    #[test]
    fn wm_class_prefers_class_over_instance() {
        assert_eq!(wm_class_name(b"navigator\0Firefox\0").as_deref(), Some("Firefox"));
        assert_eq!(wm_class_name(b"xterm\0").as_deref(), Some("xterm"));
        assert_eq!(wm_class_name(b""), None);
    }
}
