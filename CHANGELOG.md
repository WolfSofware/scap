# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/WolfSofware/scap/compare/v0.1.0...v0.2.0) - 2026-10-05

### Fixed

- Windows: `Options::fps` limits the frame rate (it was ignored).
- Windows: `RGB`, `BGR0` and `YUVFrame` outputs return those frame types; they used to arrive as `BGRA` frames holding RGBA bytes. `YUVFrame` is converted to BT.709 NV12.
- Windows: a crop area that no longer fits the window (after a resize) is clamped instead of ending the capture, and frames are cropped only when `crop_area` is set.
- Windows and Linux: `get_next_frame` returns an error when the captured window closes or the stream ends, instead of blocking forever.
- Linux: `Capturer` honours `output_type` and `crop_area`, and `fps: 0` (the default) no longer breaks format negotiation.

### Changed

- Windows and Linux: `Capturer` frames are copied once, straight from the mapped capture buffer into the output frame.
- **Breaking (Linux):** `Capturer` returns the requested `output_type` instead of PipeWire's native `BGRx`/`RGBx`/`XBGR` frames.

## [0.1.0](https://github.com/WolfSofware/scap/compare/v0.0.8...v0.1.0) - 2026-09-25

### Added

- `get_all_displays`, `get_all_windows` and `get_all_applications` list capture targets with ids and names on every platform; `Window` now reports its `process_id` and `app_name`.
- `Subscription` captures one display, window or application by id and delivers frames to a callback without copying them: `IOSurface`-backed `CMSampleBuffer` on macOS, `ID3D11Texture2D` on Windows, DMA-BUF (or mapped shared memory) on Linux.
- Optional audio for subscriptions: ScreenCaptureKit on macOS, WASAPI system or per-process loopback on Windows, PipeWire on Linux.
- Linux: `get_installed_display_servers` and `get_current_display_protocol`.
- Linux: displays and windows are listed through RandR/EWMH on X11 and `wl_output`/`ext-foreign-toplevel-list-v1` on Wayland.
- Integration tests in `tests/capture.rs` (run with `--ignored`) and an `examples/subscribe.rs` example.

### Changed

- **Breaking:** `windows-capture` 2.0.1 and `windows` 0.62 on Windows; `pipewire` 0.10 and `dbus` 0.9.12 on Linux. The minimum Rust version is now 1.82 (required by `windows` 0.62).
- **Breaking:** `Window` has new `process_id` and `app_name` fields; on Linux `Display` and `Window` gained a `raw_handle`.
- The `targets` module is public.
- `sysinfo` is only a dependency on macOS.
- Windows: `Display` and `Window` are `Send + Sync`, so targets can be shared across threads (e.g. in Tauri state).
- The license is the standard MIT text, matching upstream; the earlier copy had a non-standard grant condition.
- Windows: display titles are the monitor's friendly name (falling back to the GDI device name), and untitled windows are no longer listed.
- Linux: `get_all_targets` lists real displays and windows, and returns an error outside a graphical session instead of an empty list.

### Fixed

- Windows: uncropped frames no longer include the texture's row padding.
- Linux: frames are sized by the PipeWire chunk instead of the whole buffer, and several capturers can run at once (the capture state is no longer global).

## [0.0.8](https://github.com/CapSoftware/scap/compare/v0.0.7...v0.0.8) - 2024-12-10

### Other

- rewind version
- Update mod.rs
- Bump version (backwards compatible bug fix)
- Change as_raw_nopadding_buffer to as_nopadding_buffer
- Make GraphicsCaptureApiHandler's fn `new` implementation for Capturer have the correct signature
- handle macos stream errors with error flag
- expose CMSampleBuffer

## [0.0.7](https://github.com/CapSoftware/scap/compare/v0.0.6...v0.0.7) - 2024-11-07

### Features

- Adds `RawCapturer::get_next_pixel_buffer`, a macOS-specific method to get the next frame as its raw pixel buffer, allowing allocations from pixel format conversion to be avoided.

### Fixed

- `windows` crate versions updated (#120)

## [0.0.6](https://github.com/CapSoftware/scap/compare/v0.0.5...v0.0.6) - 2024-11-05

### Added

- adds correct crop_area
- get_crop_area for specific targets
- adds scale_factor support for windows and displays on mac
- get_main_display func improved
- add unique identifier to unknown displays on mac
- adds correct name of displays on macos
- make scale_factor f64
- exclude windows without title
- adds windows as targets on mac
- restructure util functions and add display name windows

### Fixed

- Revert to DrawBorderSettings::Default on Windows
- modified get_crop_area to include scale_factor for windows
- minor change for scale factor
- use cg types from sckit_sys
- output frame size target
- windows tweaks
- macos imports after restructure

### Other

- backwards compatability + enum error
- vendor apple-sys bindings
- Merge pull request [#95](https://github.com/CapSoftware/scap/pull/95) from MAlba124/main
- Make STREAM_STATE_CHANGED_TO_ERROR reset on stop_capture
- Fix restart on pipewire capturer
- Merge pull request [#89](https://github.com/CapSoftware/scap/pull/89) from MAlba124/main
- update .all-contributorsrc
- update README.md
- cleanup deps and remove cgtype in favor of area
- update readme and add todo for windows
- Merge branch 'feat/solo-target' into feat/use-targets-mac
- Merge branch 'feat/solo-target' into feat/mac-targets-scale-factor
- Merge pull request [#81](https://github.com/CapSoftware/scap/pull/81) from helmerapp/feat/windows-improvements
- Merge branch 'main' into feat/windows-targets
- Merge branch 'feat/windows-targets' of https://github.com/helmerapp/scap into feat/windows-targets
- extract pixelformat conversions to different file
- source rect simplifier
- shorten width, height
- windows engine
- tweak example app
- updates readme

## [0.0.5](https://github.com/helmerapp/scap/compare/v0.0.4...v0.0.5) - 2024-05-25

### Other
- don't build before releasing
- remove CHANGELOG
