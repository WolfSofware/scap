//! PipeWire video stream shared by [Capturer](crate::capturer::Capturer) and
//! [Subscription](crate::Subscription).
//!
//! When DMA-BUF is allowed the stream offers format modifiers, so the
//! compositor hands out GPU buffers and frames never touch the CPU. If the
//! compositor can't share DMA-BUFs it falls back to memfd buffers, which are
//! mapped into this process instead of copied.

use std::{
    cell::RefCell,
    mem::size_of,
    os::fd::BorrowedFd,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
};

use pipewire as pw;
use pw::{
    context::ContextRc,
    main_loop::MainLoopRc,
    properties::properties,
    spa::{
        self,
        param::{
            format::{FormatProperties, MediaSubtype, MediaType},
            video::{VideoFormat, VideoInfoRaw},
            ParamType,
        },
        pod::{
            serialize::PodSerializer, ChoiceValue, Object, Pod, Property, PropertyFlags, Value,
        },
        sys::{spa_buffer, spa_meta_header, SPA_META_Header},
        utils::{Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Id, Rectangle, SpaTypes},
    },
    stream::{Stream, StreamBox, StreamFlags, StreamState},
};

use super::error::LinCapError;
use crate::frame::{DmaBuf, DmaBufPlane, NativeFrame, PixelFormat, VideoFrame};

pub(crate) type FrameHandler = Box<dyn FnMut(&VideoFrame<'_>) + Send + 'static>;

const DRM_FORMAT_MOD_LINEAR: i64 = 0;
// DRM_FORMAT_MOD_INVALID: the driver's implicit layout
const DRM_FORMAT_MOD_INVALID: i64 = 0x00ff_ffff_ffff_ffff;

const FORMATS: [VideoFormat; 5] = [
    VideoFormat::BGRx,
    VideoFormat::BGRA,
    VideoFormat::RGBx,
    VideoFormat::RGBA,
    VideoFormat::xBGR,
];

pub(crate) struct VideoStreamOptions {
    /// PipeWire node to read from
    pub node_id: u32,
    pub fps: u32,
    /// Offer DMA-BUF modifiers; frames then arrive as [NativeFrame::DmaBuf]
    pub allow_dmabuf: bool,
    /// Deliver frames right away instead of waiting for [VideoStream::set_active]
    pub active: bool,
}

/// A PipeWire video stream running on its own thread
pub(crate) struct VideoStream {
    quit: pw::channel::Sender<()>,
    active: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<(), LinCapError>>>,
}

impl VideoStream {
    /// Connects to the node and returns once the stream is set up
    pub fn spawn(options: VideoStreamOptions, on_frame: FrameHandler) -> Result<Self, LinCapError> {
        let (quit, quit_receiver) = pw::channel::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let active = Arc::new(AtomicBool::new(options.active));

        let thread_active = Arc::clone(&active);
        let handle = std::thread::spawn(move || {
            let result = run(options, on_frame, thread_active, quit_receiver, &ready_sender);
            if let Err(error) = &result {
                let _ = ready_sender.try_send(Err(error.to_string()));
            }
            result
        });

        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                quit,
                active,
                handle: Some(handle),
            }),
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(LinCapError::new(error))
            }
            Err(_) => Err(LinCapError::new("PipeWire thread exited".into())),
        }
    }

    pub fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }

    pub fn stop(&mut self) -> Result<(), LinCapError> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        // Fails only when the loop already quit on its own
        let _ = self.quit.send(());
        handle
            .join()
            .map_err(|_| LinCapError::new("PipeWire thread panicked".into()))?
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct StreamData {
    format: VideoInfoRaw,
    dmabuf: bool,
    active: Arc<AtomicBool>,
    on_frame: FrameHandler,
    error: Rc<RefCell<Option<String>>>,
    mainloop: MainLoopRc,
}

fn run(
    options: VideoStreamOptions,
    on_frame: FrameHandler,
    active: Arc<AtomicBool>,
    quit: pw::channel::Receiver<()>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), LinCapError> {
    pw::init();

    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let stream = StreamBox::new(
        &core,
        "scap",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let error = Rc::new(RefCell::new(None));
    let data = StreamData {
        format: VideoInfoRaw::default(),
        dmabuf: false,
        active,
        on_frame,
        error: Rc::clone(&error),
        mainloop: mainloop.clone(),
    };

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(|_, data, _, new| {
            if let StreamState::Error(error) = new {
                *data.error.borrow_mut() = Some(format!("PipeWire stream error: {error}"));
                data.mainloop.quit();
            }
        })
        .param_changed(param_changed)
        .process(process)
        .register()?;

    let mut formats = Vec::new();
    if options.allow_dmabuf {
        formats.push(format_pod(
            &FORMATS,
            options.fps,
            Some(&[DRM_FORMAT_MOD_LINEAR, DRM_FORMAT_MOD_INVALID][..]),
        )?);
    }
    formats.push(format_pod(&FORMATS, options.fps, None)?);
    let mut params = formats
        .iter()
        .map(|bytes| pod_from_bytes(bytes))
        .collect::<Result<Vec<_>, _>>()?;

    stream.connect(
        Direction::Input,
        Some(options.node_id),
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    let quit_loop = mainloop.clone();
    let _quit = quit.attach(mainloop.loop_(), move |()| quit_loop.quit());

    let _ = ready.try_send(Ok(()));
    mainloop.run();

    let _ = stream.disconnect();
    let error = error.borrow_mut().take();
    match error {
        Some(error) => Err(LinCapError::new(error)),
        None => Ok(()),
    }
}

fn param_changed(stream: &Stream, data: &mut StreamData, id: u32, param: Option<&Pod>) {
    let Some(param) = param else {
        return;
    };
    if id != ParamType::Format.as_raw() {
        return;
    }
    match spa::param::format_utils::parse_format(param) {
        Ok((MediaType::Video, MediaSubtype::Raw)) => {}
        _ => return,
    }
    if let Err(error) = data.format.parse(param) {
        *data.error.borrow_mut() = Some(format!("Failed to parse PipeWire format: {error}"));
        data.mainloop.quit();
        return;
    }

    // The producer fixates the modifier (PipeWire's dma-buf negotiation), so
    // a modifier in the final format means the buffers will be DMA-BUFs
    data.dmabuf = param
        .as_object()
        .ok()
        .and_then(|object| object.find_prop(Id(FormatProperties::VideoModifier.as_raw())))
        .is_some();

    let result = buffers_pod(data.dmabuf).and_then(|buffers| {
        let meta = meta_pod()?;
        let mut params = [pod_from_bytes(&buffers)?, pod_from_bytes(&meta)?];
        Ok(stream.update_params(&mut params)?)
    });

    if let Err(error) = result {
        *data.error.borrow_mut() = Some(format!("PipeWire negotiation failed: {error}"));
        data.mainloop.quit();
    }
}

fn process(stream: &Stream, data: &mut StreamData) {
    // Skip to the newest buffer so a slow consumer doesn't fall behind
    let mut buffer: *mut pw::sys::pw_buffer = std::ptr::null_mut();
    loop {
        let next = unsafe { stream.dequeue_raw_buffer() };
        if next.is_null() {
            break;
        }
        if !buffer.is_null() {
            unsafe { stream.queue_raw_buffer(buffer) };
        }
        buffer = next;
    }
    if buffer.is_null() {
        return;
    }

    if data.active.load(Ordering::Relaxed) {
        let spa_buffer = unsafe { (*buffer).buffer };
        if !spa_buffer.is_null() {
            unsafe { deliver(spa_buffer, data) };
        }
    }

    unsafe { stream.queue_raw_buffer(buffer) };
}

/// # Safety
/// `buffer` must be a dequeued, not yet queued, PipeWire buffer
unsafe fn deliver(buffer: *mut spa_buffer, data: &mut StreamData) {
    let n_datas = (*buffer).n_datas as usize;
    if n_datas == 0 || (*buffer).datas.is_null() {
        return;
    }
    let datas = std::slice::from_raw_parts((*buffer).datas, n_datas);
    let first = &datas[0];
    if first.chunk.is_null() {
        return;
    }
    let chunk = &*first.chunk;
    if chunk.flags & pw::spa::sys::SPA_CHUNK_FLAG_CORRUPTED as i32 != 0 {
        return;
    }

    let size = data.format.size();
    let video_format = data.format.format();
    let display_time = match header_pts(buffer) {
        Some(pts) => pts,
        None => monotonic_now(),
    };

    let native = if first.type_ == pw::spa::sys::SPA_DATA_DmaBuf {
        let plane = |data: &pw::spa::sys::spa_data| {
            if data.chunk.is_null() || data.fd < 0 {
                return None;
            }
            Some(DmaBufPlane {
                fd: BorrowedFd::borrow_raw(data.fd as i32),
                offset: (*data.chunk).offset,
                stride: (*data.chunk).stride.max(0) as u32,
            })
        };
        let Some(first_plane) = plane(first) else {
            return;
        };
        // Unused slots repeat the first plane so every slot holds a valid fd
        let mut planes = [first_plane; 4];
        let mut plane_count = 1;
        for (slot, data) in planes.iter_mut().zip(datas).skip(1) {
            let Some(plane) = plane(data) else {
                break;
            };
            *slot = plane;
            plane_count += 1;
        }
        NativeFrame::DmaBuf(DmaBuf {
            fourcc: drm_fourcc(video_format),
            modifier: data.format.modifier(),
            planes,
            plane_count,
        })
    } else {
        if first.data.is_null() || chunk.size == 0 {
            return;
        }
        let offset = (chunk.offset % first.maxsize.max(1)) as usize;
        let len = (chunk.size as usize).min(first.maxsize as usize - offset);
        NativeFrame::Memory {
            data: std::slice::from_raw_parts((first.data as *const u8).add(offset), len),
            stride: chunk.stride.max(0) as u32,
        }
    };

    (data.on_frame)(&VideoFrame {
        display_time,
        width: size.width,
        height: size.height,
        format: pixel_format(video_format),
        native,
    });
}

/// # Safety
/// `buffer` must point to a valid `spa_buffer`
unsafe fn header_pts(buffer: *mut spa_buffer) -> Option<u64> {
    let metas = (*buffer).metas;
    if metas.is_null() {
        return None;
    }
    std::slice::from_raw_parts(metas, (*buffer).n_metas as usize)
        .iter()
        .find(|meta| meta.type_ == SPA_META_Header && !meta.data.is_null())
        .map(|meta| (*(meta.data as *const spa_meta_header)).pts)
        .and_then(|pts| u64::try_from(pts).ok())
        .filter(|pts| *pts > 0)
}

/// Same clock PipeWire uses for buffer timestamps
pub(crate) fn monotonic_now() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
    time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
}

fn format_pod(
    formats: &[VideoFormat],
    fps: u32,
    modifiers: Option<&[i64]>,
) -> Result<Vec<u8>, LinCapError> {
    let format = match formats {
        [format] => Value::Id(Id(format.as_raw())),
        formats => Value::Choice(ChoiceValue::Id(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Enum {
                default: Id(formats[0].as_raw()),
                alternatives: formats.iter().map(|format| Id(format.as_raw())).collect(),
            },
        ))),
    };

    let mut properties = vec![
        Property::new(
            FormatProperties::MediaType.as_raw(),
            Value::Id(Id(MediaType::Video.as_raw())),
        ),
        Property::new(
            FormatProperties::MediaSubtype.as_raw(),
            Value::Id(Id(MediaSubtype::Raw.as_raw())),
        ),
        Property::new(FormatProperties::VideoFormat.as_raw(), format),
    ];

    if let Some(modifiers) = modifiers {
        properties.push(Property {
            key: FormatProperties::VideoModifier.as_raw(),
            flags: PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE,
            value: Value::Choice(ChoiceValue::Long(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum {
                    default: modifiers[0],
                    alternatives: modifiers.to_vec(),
                },
            ))),
        });
    }

    properties.push(Property::new(
        FormatProperties::VideoSize.as_raw(),
        Value::Choice(ChoiceValue::Rectangle(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: Rectangle {
                    width: 1920,
                    height: 1080,
                },
                min: Rectangle {
                    width: 1,
                    height: 1,
                },
                max: Rectangle {
                    width: 8192,
                    height: 8192,
                },
            },
        ))),
    ));
    // Compositors offer a variable rate (0/1) capped by the max framerate
    properties.push(Property::new(
        FormatProperties::VideoMaxFramerate.as_raw(),
        Value::Choice(ChoiceValue::Fraction(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: Fraction { num: fps, denom: 1 },
                min: Fraction { num: 1, denom: 1 },
                max: Fraction { num: fps, denom: 1 },
            },
        ))),
    ));
    properties.push(Property::new(
        FormatProperties::VideoFramerate.as_raw(),
        Value::Choice(ChoiceValue::Fraction(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: Fraction { num: fps, denom: 1 },
                min: Fraction { num: 0, denom: 1 },
                max: Fraction { num: fps, denom: 1 },
            },
        ))),
    ));

    serialize(Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties,
    })
}

fn buffers_pod(dmabuf: bool) -> Result<Vec<u8>, LinCapError> {
    let data_types = if dmabuf {
        1 << pw::spa::sys::SPA_DATA_DmaBuf
    } else {
        (1 << pw::spa::sys::SPA_DATA_MemFd) | (1 << pw::spa::sys::SPA_DATA_MemPtr)
    };
    serialize(Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![Property::new(
            pw::spa::sys::SPA_PARAM_BUFFERS_dataType,
            Value::Int(data_types),
        )],
    })
}

fn meta_pod() -> Result<Vec<u8>, LinCapError> {
    serialize(Object {
        type_: SpaTypes::ObjectParamMeta.as_raw(),
        id: ParamType::Meta.as_raw(),
        properties: vec![
            Property::new(
                pw::spa::sys::SPA_PARAM_META_type,
                Value::Id(Id(SPA_META_Header)),
            ),
            Property::new(
                pw::spa::sys::SPA_PARAM_META_size,
                Value::Int(size_of::<spa_meta_header>() as i32),
            ),
        ],
    })
}

fn serialize(object: Object) -> Result<Vec<u8>, LinCapError> {
    Ok(
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(object))?
            .0
            .into_inner(),
    )
}

fn pod_from_bytes(bytes: &[u8]) -> Result<&Pod, LinCapError> {
    Pod::from_bytes(bytes).ok_or_else(|| LinCapError::new("Failed to build PipeWire pod".into()))
}

pub(crate) fn pixel_format(format: VideoFormat) -> PixelFormat {
    match format {
        VideoFormat::BGRx => PixelFormat::Bgrx,
        VideoFormat::BGRA => PixelFormat::Bgra,
        VideoFormat::RGBx => PixelFormat::Rgbx,
        VideoFormat::RGBA => PixelFormat::Rgba,
        VideoFormat::xBGR => PixelFormat::Xbgr,
        VideoFormat::NV12 => PixelFormat::Nv12,
        format => PixelFormat::Other(drm_fourcc(format)),
    }
}

/// SPA names formats by byte order, DRM by little-endian word order
pub(crate) fn drm_fourcc(format: VideoFormat) -> u32 {
    let code = match format {
        VideoFormat::BGRx => b"XR24",
        VideoFormat::BGRA => b"AR24",
        VideoFormat::RGBx => b"XB24",
        VideoFormat::RGBA => b"AB24",
        VideoFormat::xRGB => b"BX24",
        VideoFormat::xBGR => b"RX24",
        VideoFormat::NV12 => b"NV12",
        _ => return 0,
    };
    u32::from_le_bytes(*code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pw::spa::pod::PodPropFlags;

    #[test]
    fn maps_spa_formats_to_drm_fourcc() {
        // DRM_FORMAT_XRGB8888 and DRM_FORMAT_ABGR8888
        assert_eq!(drm_fourcc(VideoFormat::BGRx), 0x3432_5258);
        assert_eq!(drm_fourcc(VideoFormat::RGBA), 0x3432_4241);
        assert_eq!(drm_fourcc(VideoFormat::I420), 0);
        assert_eq!(pixel_format(VideoFormat::BGRx), PixelFormat::Bgrx);
    }

    #[test]
    fn format_pods_round_trip() {
        for modifiers in [
            None,
            Some(&[DRM_FORMAT_MOD_LINEAR, DRM_FORMAT_MOD_INVALID][..]),
        ] {
            let bytes = format_pod(&FORMATS, 60, modifiers).unwrap();
            let pod = Pod::from_bytes(&bytes).unwrap();
            assert!(pod.as_object().is_ok());
        }
    }

    #[test]
    fn modifiers_are_offered_unfixated() {
        let bytes = format_pod(
            &FORMATS,
            30,
            Some(&[DRM_FORMAT_MOD_LINEAR, DRM_FORMAT_MOD_INVALID][..]),
        )
        .unwrap();
        let pod = Pod::from_bytes(&bytes).unwrap();
        let prop = pod
            .as_object()
            .unwrap()
            .find_prop(Id(FormatProperties::VideoModifier.as_raw()))
            .unwrap();
        assert!(prop
            .flags()
            .contains(PodPropFlags::MANDATORY | PodPropFlags::DONT_FIXATE));
        assert!(prop.value().is_choice());
    }
}
