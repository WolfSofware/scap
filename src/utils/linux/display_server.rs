//! Discovery of the display servers installed on a Linux system.
//!
//! X servers and Xwayland are detected by their executables, Wayland and X11
//! desktop sessions by the `.desktop` files display managers read from
//! `wayland-sessions/` and `xsessions/` in every `XDG_DATA_DIRS` entry.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

/// Windowing protocol a display server speaks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayProtocol {
    X11,
    Wayland,
}

/// How a display server was discovered
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayServerSource {
    /// A display server executable (Xorg, Xwayland, ...)
    Executable(PathBuf),
    /// A desktop session registered for display managers
    Session(PathBuf),
}

/// A display server or desktop session installed on the system
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayServer {
    pub protocol: DisplayProtocol,
    /// Human readable name, e.g. `Xorg`, `Xwayland`, `GNOME`, `Sway`
    pub name: String,
    /// Command a display manager runs to start the session, if known
    pub exec: Option<String>,
    pub source: DisplayServerSource,
}

const X_SERVER_BINARIES: [(&str, &str); 4] = [
    ("Xorg", "usr/bin/Xorg"),
    ("Xorg", "usr/lib/xorg/Xorg"),
    ("Xorg", "usr/libexec/Xorg"),
    ("Xwayland", "usr/bin/Xwayland"),
];

/// Returns the display servers and desktop sessions installed on this system
pub fn get_installed_display_servers() -> Vec<DisplayServer> {
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    let data_dirs = data_dirs
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| dir.trim_start_matches('/'))
        .collect::<Vec<_>>();
    detect_in(Path::new("/"), &data_dirs)
}

/// Returns the protocol of the session this process runs in
pub fn get_current_display_protocol() -> Option<DisplayProtocol> {
    current_protocol_from(|name| std::env::var(name).ok())
}

fn current_protocol_from(env: impl Fn(&str) -> Option<String>) -> Option<DisplayProtocol> {
    match env("XDG_SESSION_TYPE").as_deref() {
        Some("wayland") => return Some(DisplayProtocol::Wayland),
        Some("x11") => return Some(DisplayProtocol::X11),
        _ => {}
    }
    if env("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()) {
        return Some(DisplayProtocol::Wayland);
    }
    if env("DISPLAY").is_some_and(|value| !value.is_empty()) {
        return Some(DisplayProtocol::X11);
    }
    None
}

fn detect_in(root: &Path, data_dirs: &[&str]) -> Vec<DisplayServer> {
    let mut servers = Vec::new();
    let mut seen_binaries = HashSet::new();

    for (name, relative) in X_SERVER_BINARIES {
        let path = root.join(relative);
        if !path.is_file() || !seen_binaries.insert(name) {
            continue;
        }
        let protocol = if name == "Xwayland" {
            DisplayProtocol::Wayland
        } else {
            DisplayProtocol::X11
        };
        servers.push(DisplayServer {
            protocol,
            name: name.to_string(),
            exec: None,
            source: DisplayServerSource::Executable(path),
        });
    }

    // Display managers let sessions in earlier data dirs shadow later ones
    let mut seen_sessions = HashSet::new();
    for (directory, protocol) in [
        ("wayland-sessions", DisplayProtocol::Wayland),
        ("xsessions", DisplayProtocol::X11),
    ] {
        for data_dir in data_dirs {
            let Ok(entries) = fs::read_dir(root.join(data_dir).join(directory)) else {
                continue;
            };
            let mut paths = entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "desktop"))
                .collect::<Vec<_>>();
            paths.sort();

            for path in paths {
                let Some(file_name) = path.file_name().map(|name| name.to_owned()) else {
                    continue;
                };
                if !seen_sessions.insert((protocol, file_name)) {
                    continue;
                }
                let Ok(contents) = fs::read_to_string(&path) else {
                    continue;
                };
                let Some(entry) = parse_session_entry(&contents) else {
                    continue;
                };
                servers.push(DisplayServer {
                    protocol,
                    name: entry.name,
                    exec: entry.exec,
                    source: DisplayServerSource::Session(path),
                });
            }
        }
    }

    servers
}

struct SessionEntry {
    name: String,
    exec: Option<String>,
}

/// Parses the `[Desktop Entry]` group of a session file. Hidden and
/// `NoDisplay` sessions are skipped the same way display managers skip them.
fn parse_session_entry(contents: &str) -> Option<SessionEntry> {
    let mut in_entry = false;
    let mut name = None;
    let mut exec = None;

    for line in contents.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match (key.trim(), value.trim()) {
            ("Name", value) => name = Some(value.to_string()),
            ("Exec", value) => exec = Some(value.to_string()),
            ("Hidden" | "NoDisplay", "true") => return None,
            _ => {}
        }
    }

    Some(SessionEntry { name: name?, exec })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "scap-display-servers-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn detects_binaries_and_sessions() {
        let root = temp_root("detect");
        write(&root, "usr/lib/xorg/Xorg", "");
        write(&root, "usr/bin/Xwayland", "");
        write(
            &root,
            "usr/share/wayland-sessions/gnome.desktop",
            "[Desktop Entry]\nName=GNOME\nExec=gnome-session\n",
        );
        write(
            &root,
            "usr/share/wayland-sessions/sway.desktop",
            "# comment\n[Desktop Entry]\nName=Sway\nExec=sway\n[Desktop Action x]\nName=Other\n",
        );
        write(
            &root,
            "usr/share/xsessions/plasmax11.desktop",
            "[Desktop Entry]\nName=Plasma (X11)\nExec=startplasma-x11\n",
        );
        write(
            &root,
            "usr/share/xsessions/hidden.desktop",
            "[Desktop Entry]\nName=Hidden\nHidden=true\n",
        );
        write(&root, "usr/share/xsessions/readme.txt", "not a session");

        let servers = detect_in(&root, &["usr/share"]);
        let summary = servers
            .iter()
            .map(|server| (server.protocol, server.name.as_str(), server.exec.as_deref()))
            .collect::<Vec<_>>();

        assert_eq!(
            summary,
            vec![
                (DisplayProtocol::X11, "Xorg", None),
                (DisplayProtocol::Wayland, "Xwayland", None),
                (DisplayProtocol::Wayland, "GNOME", Some("gnome-session")),
                (DisplayProtocol::Wayland, "Sway", Some("sway")),
                (DisplayProtocol::X11, "Plasma (X11)", Some("startplasma-x11")),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn earlier_data_dirs_shadow_later_ones() {
        let root = temp_root("shadow");
        write(
            &root,
            "usr/local/share/wayland-sessions/sway.desktop",
            "[Desktop Entry]\nName=Sway (local)\nExec=/usr/local/bin/sway\n",
        );
        write(
            &root,
            "usr/share/wayland-sessions/sway.desktop",
            "[Desktop Entry]\nName=Sway\nExec=sway\n",
        );

        let servers = detect_in(&root, &["usr/local/share", "usr/share"]);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "Sway (local)");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_system_has_no_display_servers() {
        let root = temp_root("empty");
        assert!(detect_in(&root, &["usr/share"]).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn current_protocol_prefers_session_type() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            }
        };

        assert_eq!(
            current_protocol_from(env(&[("XDG_SESSION_TYPE", "x11"), ("WAYLAND_DISPLAY", "wayland-0")])),
            Some(DisplayProtocol::X11)
        );
        assert_eq!(
            current_protocol_from(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")])),
            Some(DisplayProtocol::Wayland)
        );
        assert_eq!(
            current_protocol_from(env(&[("XDG_SESSION_TYPE", "tty"), ("DISPLAY", ":0")])),
            Some(DisplayProtocol::X11)
        );
        assert_eq!(current_protocol_from(env(&[])), None);
    }
}
