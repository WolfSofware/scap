//! PipeWire audio capture: the default output (everything the system plays)
//! or the playback streams of one process.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::mpsc,
    thread::JoinHandle,
};

use pipewire as pw;
use pw::{
    context::ContextRc,
    core::CoreRc,
    main_loop::MainLoopRc,
    node::{Node, NodeListener},
    properties::properties,
    spa::{
        param::{
            audio::{AudioFormat, AudioInfoRaw},
            format::{MediaSubtype, MediaType},
            ParamType,
        },
        pod::{serialize::PodSerializer, Object, Pod, Value},
        utils::{Direction, SpaTypes},
    },
    stream::{Stream, StreamBox, StreamFlags, StreamState},
    types::ObjectType,
};

use super::{error::LinCapError, video::monotonic_now};
use crate::frame::{AudioData, AudioFrame, AudioSampleFormat};

pub(crate) type AudioHandler = Box<dyn FnMut(&AudioFrame<'_>) + Send + 'static>;

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u32 = 2;
const PLAYBACK_STREAM_CLASS: &str = "Stream/Output/Audio";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioTarget {
    /// Monitor of the default output device
    System,
    /// Playback streams of this process id
    Process(u32),
}

/// An application with an open PipeWire playback stream
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaybackStream {
    pub node_id: u32,
    pub pid: u32,
    pub application: String,
    pub serial: String,
}

/// Lists the playback streams PipeWire knows about right now
pub(crate) fn playback_streams() -> Result<Vec<PlaybackStream>, LinCapError> {
    pw::init();
    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    collect_playback_streams(&mainloop, &core)
}

fn collect_playback_streams(
    mainloop: &MainLoopRc,
    core: &CoreRc,
) -> Result<Vec<PlaybackStream>, LinCapError> {
    let registry = core.get_registry_rc()?;
    let registry_weak = registry.downgrade();
    let streams = Rc::new(RefCell::new(Vec::new()));
    // Bound nodes and their listeners must outlive the roundtrips
    let nodes = Rc::new(RefCell::new(Vec::<(Node, NodeListener)>::new()));

    let found = Rc::clone(&streams);
    let bound = Rc::clone(&nodes);
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != ObjectType::Node
                || global
                    .props
                    .and_then(|props| props.get(*pw::keys::MEDIA_CLASS))
                    != Some(PLAYBACK_STREAM_CLASS)
            {
                return;
            }
            let Some(registry) = registry_weak.upgrade() else {
                return;
            };
            // Global properties omit the process id; the node's info has it
            let Ok(node) = registry.bind::<Node, _>(global) else {
                return;
            };
            let id = global.id;
            let found = Rc::clone(&found);
            let listener = node
                .add_listener_local()
                .info(move |info| {
                    let Some(props) = info.props() else {
                        return;
                    };
                    let Some(pid) = props
                        .get(*pw::keys::APP_PROCESS_ID)
                        .and_then(|pid| pid.parse().ok())
                    else {
                        return;
                    };
                    let mut found = found.borrow_mut();
                    if found.iter().any(|stream: &PlaybackStream| stream.node_id == id) {
                        return;
                    }
                    found.push(PlaybackStream {
                        node_id: id,
                        pid,
                        application: props
                            .get(*pw::keys::APP_NAME)
                            .or_else(|| props.get(*pw::keys::NODE_NAME))
                            .unwrap_or_default()
                            .to_string(),
                        serial: props
                            .get(*pw::keys::OBJECT_SERIAL)
                            .map_or_else(|| id.to_string(), str::to_string),
                    });
                })
                .register();
            bound.borrow_mut().push((node, listener));
        })
        .register();

    // The first roundtrip delivers the globals, the second the node infos
    roundtrip(mainloop, core)?;
    roundtrip(mainloop, core)?;

    let streams = streams.borrow().clone();
    Ok(streams)
}

fn roundtrip(mainloop: &MainLoopRc, core: &CoreRc) -> Result<(), LinCapError> {
    let done = Rc::new(Cell::new(false));
    let failure = Rc::new(RefCell::new(None));
    let pending = core.sync(0)?;
    let done_flag = Rc::clone(&done);
    let done_loop = mainloop.clone();
    let error_flag = Rc::clone(&done);
    let error_slot = Rc::clone(&failure);
    let error_loop = mainloop.clone();
    let _listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                done_flag.set(true);
                done_loop.quit();
            }
        })
        // A lost connection would otherwise never answer the sync
        .error(move |id, _, _, message| {
            if id == pw::core::PW_ID_CORE {
                *error_slot.borrow_mut() = Some(message.to_string());
                error_flag.set(true);
                error_loop.quit();
            }
        })
        .register();

    while !done.get() {
        mainloop.run();
    }
    let failure = failure.borrow_mut().take();
    match failure {
        Some(message) => Err(LinCapError::new(format!("PipeWire error: {message}"))),
        None => Ok(()),
    }
}

/// A PipeWire audio capture stream running on its own thread
pub(crate) struct AudioStream {
    quit: pw::channel::Sender<()>,
    handle: Option<JoinHandle<Result<(), LinCapError>>>,
}

impl AudioStream {
    pub fn spawn(target: AudioTarget, on_audio: AudioHandler) -> Result<Self, LinCapError> {
        let (quit, quit_receiver) = pw::channel::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);

        let handle = std::thread::spawn(move || {
            let result = run(target, on_audio, quit_receiver, &ready_sender);
            if let Err(error) = &result {
                let _ = ready_sender.try_send(Err(error.to_string()));
            }
            result
        });

        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                quit,
                handle: Some(handle),
            }),
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(LinCapError::new(error))
            }
            Err(_) => Err(LinCapError::new("PipeWire audio thread exited".into())),
        }
    }

    pub fn stop(&mut self) -> Result<(), LinCapError> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        let _ = self.quit.send(());
        handle
            .join()
            .map_err(|_| LinCapError::new("PipeWire audio thread panicked".into()))?
    }
}

impl Drop for AudioStream {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct StreamData {
    format: AudioInfoRaw,
    on_audio: AudioHandler,
    error: Rc<RefCell<Option<String>>>,
    mainloop: MainLoopRc,
}

fn run(
    target: AudioTarget,
    on_audio: AudioHandler,
    quit: pw::channel::Receiver<()>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), LinCapError> {
    pw::init();

    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Screen",
    };
    let mut flags = StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS;
    match target {
        AudioTarget::System => props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true"),
        AudioTarget::Process(pid) => {
            // Captures the first playback stream the process has open
            // Link straight to the application's playback stream
            let stream = collect_playback_streams(&mainloop, &core)?
                .into_iter()
                .find(|stream| stream.pid == pid)
                .ok_or_else(|| {
                    LinCapError::new(format!("Process {pid} has no PipeWire playback stream"))
                })?;
            props.insert(*pw::keys::TARGET_OBJECT, stream.serial);
            // When the app closes that stream, end the capture instead of
            // letting the session manager fall back to the microphone
            props.insert("node.dont-fallback", "true");
            flags |= StreamFlags::DONT_RECONNECT;
        }
    }

    let stream = StreamBox::new(&core, "scap-audio", props)?;

    let error = Rc::new(RefCell::new(None));
    let data = StreamData {
        format: AudioInfoRaw::default(),
        on_audio,
        error: Rc::clone(&error),
        mainloop: mainloop.clone(),
    };

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(|_, data, _, new| {
            if let StreamState::Error(error) = new {
                *data.error.borrow_mut() = Some(format!("PipeWire audio stream error: {error}"));
                data.mainloop.quit();
            }
        })
        .param_changed(|_, data, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != ParamType::Format.as_raw() {
                return;
            }
            match pw::spa::param::format_utils::parse_format(param) {
                Ok((MediaType::Audio, MediaSubtype::Raw)) => {}
                _ => return,
            }
            if let Err(error) = data.format.parse(param) {
                *data.error.borrow_mut() = Some(format!("Failed to parse audio format: {error}"));
                data.mainloop.quit();
            }
        })
        .process(process)
        .register()?;

    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(CHANNELS);
    let format = PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(Object {
            type_: SpaTypes::ObjectParamFormat.as_raw(),
            id: ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        }),
    )?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&format)
        .ok_or_else(|| LinCapError::new("Failed to build PipeWire audio format".into()))?];

    stream.connect(
        Direction::Input,
        None,
        flags,
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

fn process(stream: &Stream, data: &mut StreamData) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let channels = data.format.channels();
    let sample_rate = data.format.rate();
    let Some(sample_format) = sample_format(data.format.format()) else {
        return;
    };
    let frame_size = sample_format.bytes_per_sample() * channels as usize;
    if frame_size == 0 {
        return;
    }

    let datas = buffer.datas_mut();
    let Some(first) = datas.first_mut() else {
        return;
    };
    let offset = first.chunk().offset() as usize;
    let size = first.chunk().size() as usize;
    let Some(bytes) = first.data() else {
        return;
    };
    let Some(samples) = bytes.get(offset..offset + size) else {
        return;
    };

    (data.on_audio)(&AudioFrame {
        display_time: monotonic_now(),
        sample_rate,
        channels: channels as u16,
        format: sample_format,
        frames: samples.len() / frame_size,
        data: AudioData::Interleaved(samples),
    });
}

fn sample_format(format: AudioFormat) -> Option<AudioSampleFormat> {
    match format {
        AudioFormat::F32LE => Some(AudioSampleFormat::F32),
        AudioFormat::S16LE => Some(AudioSampleFormat::I16),
        AudioFormat::S32LE => Some(AudioSampleFormat::I32),
        _ => None,
    }
}
