use wayland_client::{
    backend::ObjectId,
    event_created_child,
    protocol::{wl_output, wl_registry},
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};

use super::{fnv1a, DisplayHandle, WindowHandle};
use crate::targets::{Display, TargetError, Window};

#[derive(Default)]
struct Output {
    global: u32,
    name: Option<String>,
    description: Option<String>,
    make_model: Option<String>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Default)]
struct Toplevel {
    identifier: Option<String>,
    title: Option<String>,
    app_id: Option<String>,
    closed: bool,
}

#[derive(Default)]
struct State {
    outputs: Vec<Output>,
    toplevel_list: Option<ExtForeignToplevelListV1>,
    toplevels: Vec<(ObjectId, Toplevel)>,
}

/// Binds the globals and waits until the compositor has described them
fn query() -> Result<State, TargetError> {
    let connection = Connection::connect_to_env().map_err(TargetError::new)?;
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    connection.display().get_registry(&handle, ());

    let mut state = State::default();
    // globals, then output and toplevel events, then toplevel details
    for _ in 0..3 {
        queue.roundtrip(&mut state).map_err(TargetError::new)?;
    }
    if let Some(list) = state.toplevel_list.take() {
        list.stop();
    }
    Ok(state)
}

/// `wl_output`s; a display's id is the output's registry name
pub fn get_all_displays() -> Result<Vec<Display>, TargetError> {
    Ok(query()?
        .outputs
        .into_iter()
        .map(|output| {
            let title = output
                .description
                .clone()
                .or_else(|| output.name.clone())
                .or_else(|| output.make_model.clone())
                .unwrap_or_else(|| format!("Output {}", output.global));
            Display {
                id: output.global,
                title,
                raw_handle: DisplayHandle::Wayland {
                    name: output.name,
                    x: output.x,
                    y: output.y,
                    width: output.width,
                    height: output.height,
                },
            }
        })
        .collect())
}

/// Toplevels from `ext-foreign-toplevel-list-v1`. Compositors without it
/// (GNOME among them) report no windows. Wayland doesn't reveal process ids.
pub fn get_all_windows() -> Result<Vec<Window>, TargetError> {
    Ok(query()?
        .toplevels
        .into_iter()
        .filter_map(|(_, toplevel)| {
            if toplevel.closed {
                return None;
            }
            let identifier = toplevel.identifier?;
            Some(Window {
                id: fnv1a(&identifier),
                title: toplevel.title.unwrap_or_default(),
                process_id: None,
                app_name: toplevel.app_id.clone(),
                raw_handle: WindowHandle::Wayland {
                    identifier,
                    app_id: toplevel.app_id,
                },
            })
        })
        .collect())
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        if interface == wl_output::WlOutput::interface().name {
            registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), handle, name);
            state.outputs.push(Output {
                global: name,
                ..Default::default()
            });
        } else if interface == ExtForeignToplevelListV1::interface().name {
            state.toplevel_list =
                Some(registry.bind::<ExtForeignToplevelListV1, _, _>(name, 1, handle, ()));
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(output) = state
            .outputs
            .iter_mut()
            .find(|output| output.global == *global)
        else {
            return;
        };
        match event {
            wl_output::Event::Geometry {
                x, y, make, model, ..
            } => {
                output.x = x;
                output.y = y;
                output.make_model = Some(format!("{make} {model}").trim().to_string());
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                let current = matches!(flags, WEnum::Value(flags) if flags.contains(wl_output::Mode::Current));
                if current {
                    output.width = width;
                    output.height = height;
                }
            }
            wl_output::Event::Name { name } => output.name = Some(name),
            wl_output::Event::Description { description } => {
                output.description = Some(description)
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            state.toplevels.push((toplevel.id(), Toplevel::default()));
        }
    }

    event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id();
        let Some((_, toplevel)) = state.toplevels.iter_mut().find(|(key, _)| *key == id) else {
            return;
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => {
                toplevel.identifier = Some(identifier)
            }
            ext_foreign_toplevel_handle_v1::Event::Title { title } => toplevel.title = Some(title),
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                toplevel.app_id = Some(app_id)
            }
            ext_foreign_toplevel_handle_v1::Event::Closed => toplevel.closed = true,
            _ => {}
        }
    }
}
