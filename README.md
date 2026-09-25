![Github banner](./.github/banner.gif)

![GitHub Repo stars](https://img.shields.io/github/stars/WolfSofware/scap?style=for-the-badge&logo=github&label=Github%20Stars&labelColor=black)
![Release](https://img.shields.io/github/v/tag/WolfSofware/scap?style=for-the-badge&logo=github&label=release&labelColor=black)
![MSRV](https://img.shields.io/badge/MSRV-1.82-orange?style=for-the-badge&logo=rust&logoColor=white&labelColor=black)
![License](https://img.shields.io/badge/license-MIT-blue?style=for-the-badge&labelColor=black)

A Rust library for high-quality screen capture that leverages native OS APIs for optimal performance!

1. macOS: [ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit) (video and audio)
2. Windows: [Windows.Graphics.Capture](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture?view=winrt-22621) (video), [WASAPI loopback](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording) (audio)
3. Linux: [PipeWire](https://pipewire.org) through the ScreenCast portal on Wayland, MIT-SHM on X11 (video), PipeWire (audio)

> This is the [WolfSofware](https://github.com/WolfSofware/scap) fork of [CapSoftware/scap](https://github.com/CapSoftware/scap). The `scap` name on crates.io belongs to upstream, so depend on this fork through git (see [installation](#installation)).

---

## features

1. Cross-platform across Windows, Mac and Linux!
2. Checks for support and recording permissions.
3. Query list of captureable targets (displays, windows and applications) with ids and names.
4. Exclude certain targets from being captured.
5. Subscribe to one display, window or application by id and receive its video frames and audio in callbacks.
6. Zero-copy frames: callbacks get the native GPU buffer (`IOSurface`, `ID3D11Texture2D`, DMA-BUF).
7. List the display servers installed on Linux.

## installation

```toml
[dependencies]
scap = { git = "https://github.com/WolfSofware/scap", tag = "v0.1.0" }
```

Build requirements:

| Platform | Requirements |
| --- | --- |
| macOS | macOS 12.3+ to run (13.0+ for audio). Building needs Xcode 26 or Command Line Tools with the macOS 26 SDK, because `screencapturekit` 8 compiles a Swift bridge against the Metal 4 APIs. |
| Windows | Windows 10 1903+ (process audio needs Windows 10 2004+). |
| Linux | `libpipewire-0.3-dev`, `libdbus-1-dev`, `clang`/`libclang-dev` and `pkg-config` to build; PipeWire and `xdg-desktop-portal` at runtime on Wayland. |

## contributing

We found most of Rust's tooling around screen capture either very outdated, non-performant or platform-specific. This project is our attempt to change that. Contributions, PRs and Issues are most welcome!

If you want to contribute code, here's a quick primer:

1. Clone the repo and run it with `cargo run`.
2. Explore the API and library code in [lib.rs](./src/lib.rs).
3. Platform-specific code lives in the `win`, `mac` and `linux` modules.
4. The [main.rs](./src/main.rs) is a small program that "consumes" the library, for easy testing.

## usage

```rust
use scap::{
    capturer::{Point, Area, Size, Capturer, Options},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Check if the platform is supported
    if !scap::is_supported() {
        println!("❌ Platform not supported");
        return Ok(());
    }

    // Check if we have permission to capture screen
    // If we don't, request it.
    if !scap::has_permission() {
        println!("❌ Permission not granted. Requesting permission...");
        if !scap::request_permission() {
            println!("❌ Permission denied");
            return Ok(());
        }
    }

    // Get recording targets
    let targets = scap::get_all_targets()?;
    println!("Targets: {:?}", targets);

    // All your displays and windows are targets
    // You can filter this and capture the one you need.

    // Create Options
    let options = Options {
        fps: 60,
        target: None, // None captures the primary display
        show_cursor: true,
        show_highlight: true,
        excluded_targets: None,
        output_type: scap::frame::FrameType::BGRAFrame,
        output_resolution: scap::capturer::Resolution::_720p,
        crop_area: Some(Area {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: 2000.0,
                height: 1000.0,
            },
        }),
        ..Default::default()
    };

    // Create Capturer
    let mut capturer = Capturer::build(options)?;

    // Start Capture
    capturer.start_capture()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    // Stop Capture
    capturer.stop_capture()?;
    Ok(())
}
```

### subscribing to a target

```rust
use scap::{Subscription, TargetId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let display = scap::get_all_displays()?.remove(0);

    let subscription = Subscription::builder(TargetId::Display(display.id))
        .fps(60)
        // `frame.native` borrows the GPU buffer and is valid during the call
        .on_video(|frame| println!("{}x{} {:?}", frame.width, frame.height, frame.format))
        // Optional: everything the display plays, or one app's audio for
        // TargetId::Window / TargetId::Application
        .on_audio(|audio| println!("{} samples", audio.frames))
        .start()?;

    std::thread::sleep(std::time::Duration::from_secs(5));
    subscription.stop()?;
    Ok(())
}
```

Platform notes:

- Wayland doesn't let one client read another's windows, so the ScreenCast portal asks the user what to share. The target id picks whether a monitor or a window is offered; it can't preselect it. Wayland also hides window owners, so window audio needs `TargetId::Application`.
- Window lists on Wayland need `ext-foreign-toplevel-list-v1` (wlroots compositors, KDE, COSMIC, niri); GNOME doesn't expose one.
- On X11, frames come from a MIT-SHM segment the X server writes into. They are CPU memory, not DMA-BUF, and don't include the cursor.
- Application audio on Linux follows the app's PipeWire playback stream, which has to exist when the subscription starts.

Run the capture tests on a machine with a display (they are ignored by default):

```sh
cargo test --test capture -- --ignored --test-threads=1
```

## using with tauri

scap works inside [Tauri](https://tauri.app) 2 apps (checked against `tauri` 2.11.5, `tauri-build` 2.6.3 and `tauri-plugin-opener` 2.5.4). `Subscription` and the target types are `Send`, so they fit in managed state, and callbacks can forward data through an IPC channel:

```rust
use std::sync::Mutex;

use scap::{Subscription, TargetId};
use tauri::{ipc::Channel, State};

#[derive(Default)]
struct Capture(Mutex<Option<Subscription>>);

#[tauri::command]
fn start_capture(display_id: u32, frames: Channel<u64>, state: State<'_, Capture>) -> Result<(), String> {
    let subscription = Subscription::builder(TargetId::Display(display_id))
        .on_video(move |frame| {
            // Runs on the capture thread; never block it on the UI
            let _ = frames.send(frame.display_time);
        })
        .start()
        .map_err(|error| error.to_string())?;
    *state.0.lock().unwrap() = Some(subscription);
    Ok(())
}
```

Register it with `.manage(Capture::default())` and `tauri::generate_handler![start_capture]`. Things to plan for:

- **Frames don't cross IPC for free.** Frames are GPU buffers valid only during the callback. Encode them (for example with a hardware encoder) or render them natively instead of sending raw pixels to the webview.
- **macOS.** Set `bundle.macOS.minimumSystemVersion` to `"12.3"` (`"13.0"` with audio). The screen recording permission belongs to the app bundle; under `tauri dev` it is granted to your terminal instead.
- **Windows.** Tauri uses `windows` 0.61 and scap uses 0.62; both compile side by side. Use `windows` 0.62 types when you work with `NativeFrame::Direct3D11`.
- **Linux.** Don't bundle `libpipewire` into an AppImage; it has to match the PipeWire daemon on the host.

## license

The code in this repository is open-sourced under the [MIT license](./LICENSE), though it may be relying on dependencies that are licensed differently. Please consult their documentation for exact terms.

## contributors

<!-- ALL-CONTRIBUTORS-LIST:START - Do not remove or modify this section -->
<!-- prettier-ignore-start -->
<!-- markdownlint-disable -->
<table>
  <tbody>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/Pranav2612000"><img src="https://avatars.githubusercontent.com/u/20909078?v=4?s=100" width="100px;" alt="Pranav Joglekar"/><br /><sub><b>Pranav Joglekar</b></sub></a><br /><a href="#code-Pranav2612000" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://www.sid.me"><img src="https://avatars.githubusercontent.com/u/30227512?v=4?s=100" width="100px;" alt="Siddharth"/><br /><sub><b>Siddharth</b></sub></a><br /><a href="#code-clearlysid" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="http://dev-rohan.in"><img src="https://avatars.githubusercontent.com/u/48467821?v=4?s=100" width="100px;" alt="Rohan Punjani"/><br /><sub><b>Rohan Punjani</b></sub></a><br /><a href="#code-RohanPunjani" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/NiiightmareXD"><img src="https://avatars.githubusercontent.com/u/90005793?v=4?s=100" width="100px;" alt="NiiightmareXD"/><br /><sub><b>NiiightmareXD</b></sub></a><br /><a href="#code-NiiightmareXD" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="http://bringeber.dev"><img src="https://avatars.githubusercontent.com/u/83474682?v=4?s=100" width="100px;" alt="MAlba124"/><br /><sub><b>MAlba124</b></sub></a><br /><a href="#code-MAlba124" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://peerlist.io/anubhavitis"><img src="https://avatars.githubusercontent.com/u/26124625?v=4?s=100" width="100px;" alt="Anubhav Singhal"/><br /><sub><b>Anubhav Singhal</b></sub></a><br /><a href="#code-anubhavitis" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="http://linkedin.com/in/vasusharma7"><img src="https://avatars.githubusercontent.com/u/40715071?v=4?s=100" width="100px;" alt="Vasu Sharma"/><br /><sub><b>Vasu Sharma</b></sub></a><br /><a href="#code-vasusharma7" title="Code">💻</a></td>
    </tr>
  </tbody>
</table>

<!-- markdownlint-restore -->
<!-- prettier-ignore-end -->

<!-- ALL-CONTRIBUTORS-LIST:END -->

## credits

This project builds on top of the fabulous work done by:

-   [Cap Software](https://github.com/CapSoftware/scap) and [Helmer](https://github.com/helmerapp/scap), the original authors and maintainers of scap
-   [@MAlba124](https://github.com/MAlba124) for Linux support via Pipewire
-   [@svtlabs](https://github.com/svtlabs) for [screencapturekit-rs](https://github.com/svtlabs/screencapturekit-rs)
-   [@NiiightmareXD](https://github.com/NiiightmareXD) for [windows-capture](https://github.com/NiiightmareXD/windows-capture)
