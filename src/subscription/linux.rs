//! On X11 targets are captured by id straight from the X server through a
//! MIT-SHM segment the server writes into. Wayland doesn't let clients read
//! other clients' surfaces, so there the ScreenCast portal asks the user to
//! confirm (the portal decides what is shared; the id only picks whether a
//! monitor or a window is offered) and frames arrive as PipeWire DMA-BUFs.
//! Audio always comes from PipeWire.

use std::{
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use x11rb::{
    connection::Connection,
    errors::ReplyError,
    protocol::{
        shm::ConnectionExt as _,
        xproto::{ConnectionExt as _, ImageFormat},
    },
    rust_connection::RustConnection,
};

use super::{Config, SubscriptionError, TargetId, VideoCallback};
use crate::{
    capturer::engine::linux::{
        audio::{AudioStream, AudioTarget},
        portal::{ScreenCastPortal, SourceType},
        video::{monotonic_now, VideoStream, VideoStreamOptions},
    },
    frame::{NativeFrame, PixelFormat, VideoFrame},
    targets::{
        linux::{current_protocol, x11},
        DisplayHandle,
    },
    utils::linux::display_server::DisplayProtocol,
};

pub struct Subscription {
    video: Option<Video>,
    audio: Option<AudioStream>,
}

enum Video {
    X11(X11Capture),
    Portal {
        stream: VideoStream,
        // Closing the portal session removes the PipeWire node
        _connection: dbus::blocking::Connection,
    },
}

impl Subscription {
    pub fn stop(mut self) -> Result<(), SubscriptionError> {
        let video = match self.video.take() {
            Some(Video::X11(capture)) => capture.stop(),
            Some(Video::Portal { mut stream, .. }) => {
                stream.stop().map_err(SubscriptionError::platform)
            }
            None => Ok(()),
        };
        let audio = match self.audio.as_mut() {
            Some(audio) => audio.stop().map_err(SubscriptionError::platform),
            None => Ok(()),
        };
        video.and(audio)
    }
}

enum VideoSource {
    X11(X11Source),
    Portal(SourceType),
}

#[derive(Clone, Copy)]
enum X11Source {
    Region {
        x: i16,
        y: i16,
        width: u16,
        height: u16,
    },
    Window(u32),
}

pub fn start(config: Config) -> Result<Subscription, SubscriptionError> {
    let target = config.target;
    let (source, audio_target) = resolve(target, config.video.is_some(), config.audio.is_some())?;

    let video = match (source, config.video) {
        (Some(VideoSource::X11(source)), Some(callback)) => {
            Some(Video::X11(X11Capture::spawn(source, config.fps, callback)?))
        }
        (Some(VideoSource::Portal(source_types)), Some(callback)) => {
            let connection =
                dbus::blocking::Connection::new_session().map_err(SubscriptionError::platform)?;
            let node_id = ScreenCastPortal::new(&connection)
                .source_types(source_types)
                .show_cursor(config.show_cursor)
                .and_then(|portal| portal.create_stream())
                .map_err(SubscriptionError::platform)?
                .pw_node_id();
            let stream = VideoStream::spawn(
                VideoStreamOptions {
                    node_id,
                    fps: config.fps,
                    allow_dmabuf: true,
                    active: true,
                },
                callback,
            )
            .map_err(SubscriptionError::platform)?;
            Some(Video::Portal {
                stream,
                _connection: connection,
            })
        }
        _ => None,
    };

    let audio = match (audio_target, config.audio) {
        (Some(audio_target), Some(callback)) => match AudioStream::spawn(audio_target, callback) {
            Ok(audio) => Some(audio),
            Err(error) => {
                if let Some(video) = video {
                    let _ = (Subscription { video: Some(video), audio: None }).stop();
                }
                return Err(SubscriptionError::platform(error));
            }
        },
        _ => None,
    };

    Ok(Subscription { video, audio })
}

fn resolve(
    target: TargetId,
    needs_video: bool,
    needs_audio: bool,
) -> Result<(Option<VideoSource>, Option<AudioTarget>), SubscriptionError> {
    let not_found = || SubscriptionError::TargetNotFound(target);
    let protocol = current_protocol().map_err(SubscriptionError::platform)?;

    match (protocol, target) {
        (DisplayProtocol::X11, TargetId::Display(id)) => {
            let display = x11::get_all_displays()
                .map_err(SubscriptionError::platform)?
                .into_iter()
                .find(|display| display.id == id)
                .ok_or_else(not_found)?;
            let DisplayHandle::X11 {
                x,
                y,
                width,
                height,
            } = display.raw_handle
            else {
                return Err(not_found());
            };
            Ok((
                Some(VideoSource::X11(X11Source::Region {
                    x,
                    y,
                    width,
                    height,
                })),
                Some(AudioTarget::System),
            ))
        }
        (DisplayProtocol::X11, TargetId::Window(id)) => {
            let window = x11::get_all_windows()
                .map_err(SubscriptionError::platform)?
                .into_iter()
                .find(|window| window.id == id)
                .ok_or_else(not_found)?;
            let audio = match window.process_id {
                Some(pid) => Some(AudioTarget::Process(pid)),
                None if needs_audio => {
                    return Err(SubscriptionError::Unsupported(format!(
                        "Window {id} doesn't report its process (_NET_WM_PID), so its audio can't be found"
                    )))
                }
                None => None,
            };
            Ok((Some(VideoSource::X11(X11Source::Window(id))), audio))
        }
        (DisplayProtocol::X11, TargetId::Application(pid)) => {
            // X11 captures windows, so video follows the app's largest window
            let window = x11::get_all_windows()
                .map_err(SubscriptionError::platform)?
                .into_iter()
                .filter(|window| window.process_id == Some(pid))
                .max_by_key(|window| {
                    x11::window_size(window.id).map_or(0, |(width, height)| width * height)
                });
            if window.is_none() && needs_video {
                return Err(not_found());
            }
            Ok((
                window.map(|window| VideoSource::X11(X11Source::Window(window.id))),
                Some(AudioTarget::Process(pid)),
            ))
        }
        (DisplayProtocol::Wayland, TargetId::Display(id)) => {
            let displays = crate::targets::get_all_displays().map_err(SubscriptionError::platform)?;
            if !displays.iter().any(|display| display.id == id) {
                return Err(not_found());
            }
            Ok((
                Some(VideoSource::Portal(SourceType::MONITOR)),
                Some(AudioTarget::System),
            ))
        }
        (DisplayProtocol::Wayland, TargetId::Window(id)) => {
            let windows = crate::targets::get_all_windows().map_err(SubscriptionError::platform)?;
            if !windows.iter().any(|window| window.id == id) {
                return Err(not_found());
            }
            if needs_audio {
                return Err(SubscriptionError::Unsupported(
                    "Wayland doesn't reveal which process owns a window; subscribe to TargetId::Application for its audio".into(),
                ));
            }
            Ok((Some(VideoSource::Portal(SourceType::WINDOW)), None))
        }
        (DisplayProtocol::Wayland, TargetId::Application(pid)) => {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                return Err(not_found());
            }
            Ok((
                Some(VideoSource::Portal(SourceType::WINDOW)),
                Some(AudioTarget::Process(pid)),
            ))
        }
    }
}

struct X11Capture {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<Result<(), String>>,
}

impl X11Capture {
    fn spawn(source: X11Source, fps: u32, callback: VideoCallback) -> Result<Self, SubscriptionError> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);

        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut callback = callback;
            let result = capture_x11(source, fps, &mut callback, &thread_stop, &ready_sender);
            if let Err(error) = &result {
                let _ = ready_sender.try_send(Err(error.clone()));
            }
            result
        });

        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self { stop, handle }),
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(SubscriptionError::Platform(error))
            }
            Err(_) => Err(SubscriptionError::Platform("X11 capture thread exited".into())),
        }
    }

    fn stop(self) -> Result<(), SubscriptionError> {
        self.stop.store(true, Ordering::Relaxed);
        match self.handle.join() {
            Ok(result) => result.map_err(SubscriptionError::Platform),
            Err(_) => Err(SubscriptionError::Platform("X11 capture thread panicked".into())),
        }
    }
}

fn capture_x11(
    source: X11Source,
    fps: u32,
    callback: &mut VideoCallback,
    stop: &AtomicBool,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let (connection, screen_number) = x11rb::connect(None).map_err(|error| error.to_string())?;
    let root = connection.setup().roots[screen_number].root;
    // Without MIT-SHM (a remote display, another IPC namespace) the image
    // comes over the socket instead
    let mut use_shm = connection
        .shm_query_version()
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some();

    let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
    let mut segment: Option<ShmSegment> = None;
    let mut next_frame = Instant::now();
    let _ = ready.try_send(Ok(()));

    let result = (|| {
        while !stop.load(Ordering::Relaxed) {
            let (drawable, x, y, width, height) = match source {
                X11Source::Region {
                    x,
                    y,
                    width,
                    height,
                } => (root, x, y, width, height),
                X11Source::Window(id) => {
                    let geometry = connection
                        .get_geometry(id)
                        .map_err(|error| error.to_string())?
                        .reply()
                        .map_err(|_| format!("Window {id} was closed"))?;
                    (id, 0, 0, geometry.width, geometry.height)
                }
            };

            if width > 0 && height > 0 {
                let size = usize::from(width) * usize::from(height) * 4;

                if use_shm && segment.as_ref().is_none_or(|segment| segment.size < size) {
                    if let Some(old) = segment.take() {
                        old.detach(&connection);
                    }
                    // The extension can be advertised yet unusable
                    segment = ShmSegment::new(&connection, size).ok();
                    use_shm = segment.is_some();
                }

                let grabbed = match &segment {
                    Some(segment) if use_shm => connection
                        .shm_get_image(
                            drawable,
                            x,
                            y,
                            width,
                            height,
                            !0,
                            ImageFormat::Z_PIXMAP.into(),
                            segment.id,
                            0,
                        )
                        .map_err(|error| error.to_string())?
                        .reply()
                        .map(|reply| {
                            check_depth(reply.depth)?;
                            // The server wrote straight into our mapping
                            callback(&bgrx_frame(segment.as_slice(size), width, height));
                            Ok::<(), String>(())
                        }),
                    _ => connection
                        .get_image(ImageFormat::Z_PIXMAP, drawable, x, y, width, height, !0)
                        .map_err(|error| error.to_string())?
                        .reply()
                        .map(|reply| {
                            check_depth(reply.depth)?;
                            callback(&bgrx_frame(&reply.data, width, height));
                            Ok::<(), String>(())
                        }),
                };
                match grabbed {
                    Ok(delivered) => delivered?,
                    // BadMatch while a window is unmapped or partly off
                    // screen; skip the frame and try again
                    Err(ReplyError::X11Error(_)) => {}
                    Err(error) => return Err(error.to_string()),
                }
            }

            next_frame += interval;
            let now = Instant::now();
            if next_frame > now {
                std::thread::sleep(next_frame - now);
            } else {
                next_frame = now;
            }
        }
        Ok(())
    })();

    if let Some(segment) = segment {
        segment.detach(&connection);
    }
    result
}

/// ZPixmap images of 24 and 32 bit visuals are B, G, R, padding on little endian
fn bgrx_frame(data: &[u8], width: u16, height: u16) -> VideoFrame<'_> {
    let stride = u32::from(width) * 4;
    let size = (stride as usize * usize::from(height)).min(data.len());
    VideoFrame {
        display_time: monotonic_now(),
        width: width.into(),
        height: height.into(),
        format: PixelFormat::Bgrx,
        native: NativeFrame::Memory {
            data: &data[..size],
            stride,
        },
    }
}

fn check_depth(depth: u8) -> Result<(), String> {
    match depth {
        24 | 32 => Ok(()),
        depth => Err(format!("Unsupported X11 visual depth {depth}; only 24 and 32 bit are supported")),
    }
}

/// A System V shared memory segment attached to both us and the X server
struct ShmSegment {
    id: u32,
    address: *mut u8,
    size: usize,
}

impl ShmSegment {
    fn new(connection: &RustConnection, size: usize) -> Result<Self, String> {
        let os_error = || std::io::Error::last_os_error().to_string();

        let shmid = unsafe { libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o600) };
        if shmid < 0 {
            return Err(os_error());
        }
        let address = unsafe { libc::shmat(shmid, ptr::null(), 0) };
        if address as isize == -1 {
            let error = os_error();
            unsafe { libc::shmctl(shmid, libc::IPC_RMID, ptr::null_mut()) };
            return Err(error);
        }

        let attached = connection
            .generate_id()
            .map_err(|error| error.to_string())
            .and_then(|id| {
                connection
                    .shm_attach(id, shmid as u32, false)
                    .map_err(|error| error.to_string())?
                    .check()
                    .map_err(|error| error.to_string())?;
                Ok(id)
            });
        // Freed by the kernel once both sides detach
        unsafe { libc::shmctl(shmid, libc::IPC_RMID, ptr::null_mut()) };

        match attached {
            Ok(id) => Ok(Self {
                id,
                address: address.cast(),
                size,
            }),
            Err(error) => {
                unsafe { libc::shmdt(address) };
                Err(error)
            }
        }
    }

    fn as_slice(&self, len: usize) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.address, len.min(self.size)) }
    }

    fn detach(self, connection: &RustConnection) {
        let _ = connection.shm_detach(self.id);
        let _ = connection.flush();
        unsafe { libc::shmdt(self.address.cast()) };
    }
}
