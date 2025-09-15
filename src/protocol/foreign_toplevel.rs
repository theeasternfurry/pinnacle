// SPDX-License-Identifier: GPL-3.0-or-later

// Hands down plagiarized from Niri

use std::collections::{HashMap, hash_map::Entry};

use smithay::{
    desktop::WindowSurface,
    output::{Output, WeakOutput},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_protocols_wlr::foreign_toplevel::v1::server::{
            zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
            zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
        },
        wayland_server::{
            self, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, Resource,
            backend::ClientId,
            protocol::{wl_output::WlOutput, wl_surface::WlSurface},
        },
    },
    wayland::{compositor, seat::WaylandFocus, shell::xdg::XdgToplevelSurfaceData},
};
use tracing::error;

use crate::{
    state::{Pinnacle, State, WithState},
    window::WindowElement,
};

const VERSION: u32 = 3;

pub struct ForeignToplevelManagerState {
    display: DisplayHandle,
    instances: Vec<ZwlrForeignToplevelManagerV1>,
    toplevels: HashMap<WlSurface, ToplevelData>,
}

#[derive(Default)]
struct ToplevelData {
    title: Option<String>,
    app_id: Option<String>,
    states: Vec<zwlr_foreign_toplevel_handle_v1::State>,
    output: Option<WeakOutput>,
    instances: HashMap<ZwlrForeignToplevelHandleV1, Vec<WlOutput>>,
    // TODO:
    // parent: Option<ZwlrForeignToplevelHandleV1>,
}

pub trait ForeignToplevelHandler {
    fn foreign_toplevel_manager_state(&mut self) -> &mut ForeignToplevelManagerState;
    fn activate(&mut self, wl_surface: WlSurface);
    fn close(&mut self, wl_surface: WlSurface);
    fn set_fullscreen(&mut self, wl_surface: WlSurface, wl_output: Option<WlOutput>);
    fn unset_fullscreen(&mut self, wl_surface: WlSurface);
    fn set_maximized(&mut self, wl_surface: WlSurface);
    fn unset_maximized(&mut self, wl_surface: WlSurface);
    fn set_minimized(&mut self, wl_surface: WlSurface);
    fn unset_minimized(&mut self, wl_surface: WlSurface);
}

pub struct ForeignToplevelGlobalData {
    filter: Box<dyn Fn(&Client) -> bool + Send + Sync>,
}

impl ForeignToplevelManagerState {
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<ZwlrForeignToplevelManagerV1, ForeignToplevelGlobalData>
            + Dispatch<ZwlrForeignToplevelManagerV1, ()>
            + 'static,
        F: Fn(&Client) -> bool + Send + Sync + 'static,
    {
        let global_data = ForeignToplevelGlobalData {
            filter: Box::new(filter),
        };

        display.create_global::<D, ZwlrForeignToplevelManagerV1, _>(VERSION, global_data);

        Self {
            display: display.clone(),
            instances: Vec::new(),
            toplevels: HashMap::new(),
        }
    }
}

pub fn refresh(state: &mut State) {
    let _span = tracy_client::span!("foreign_toplevel::refresh");

    state
        .pinnacle
        .foreign_toplevel_manager_state
        .toplevels
        .retain(|surface, data| {
            if state
                .pinnacle
                .windows
                .iter()
                .any(|win| win.wl_surface().is_some_and(|surf| &*surf == surface))
            {
                return true;
            }

            for instance in data.instances.keys() {
                instance.closed();
            }

            false
        });

    let mut focused = None;

    let focused_win = state.pinnacle.keyboard_focus_stack.current_focus().cloned();

    // PERF: We sacrifice performance to the borrow checker with this clone
    for window in state
        .pinnacle
        .windows
        .clone()
        .iter()
        .filter(|win| !win.is_x11_override_redirect())
    {
        if let Some(win) = focused_win.as_ref()
            && win == window
        {
            focused = Some(window.clone());
            continue;
        }

        if let Some(pending_data) = pending_toplevel_data_for(&state.pinnacle, window) {
            let Some(surface) = window.wl_surface() else {
                continue;
            };
            refresh_toplevel(
                &mut state.pinnacle.foreign_toplevel_manager_state,
                &surface,
                pending_data,
            );
        }
    }

    // Refresh the focused window last so it gets the focused event after the previoous focused
    // window gets the unfocused event.
    if let Some(window) = focused
        && let Some(pending_data) = pending_toplevel_data_for(&state.pinnacle, &window)
    {
        let Some(surface) = window.wl_surface() else {
            return;
        };
        refresh_toplevel(
            &mut state.pinnacle.foreign_toplevel_manager_state,
            &surface,
            pending_data,
        );
    }
}

fn pending_toplevel_data_for(
    pinnacle: &Pinnacle,
    win: &WindowElement,
) -> Option<PendingToplevelData> {
    let focused_win = pinnacle.keyboard_focus_stack.current_focus();

    let surface = win.wl_surface()?;

    let output = win.output(pinnacle);

    let focused = focused_win == Some(win);

    compositor::with_states(&surface, |states| match win.underlying_surface() {
        WindowSurface::Wayland(_toplevel) => {
            let role = states
                .data_map
                .get::<XdgToplevelSurfaceData>()?
                .lock()
                .ok()?;

            Some(PendingToplevelData {
                title: role.title.clone(),
                app_id: role.app_id.clone(),
                maximized: role.current.states.contains(xdg_toplevel::State::Maximized),
                minimized: win.with_state(|state| state.minimized),
                fullscreen: role
                    .current
                    .states
                    .contains(xdg_toplevel::State::Fullscreen),
                _activated: role.current.states.contains(xdg_toplevel::State::Activated),
                focused,
                output,
            })
        }
        WindowSurface::X11(x11_surface) => Some(PendingToplevelData {
            title: Some(x11_surface.title()),
            app_id: Some(x11_surface.class()),
            maximized: x11_surface.is_maximized(),
            minimized: x11_surface.is_minimized(),
            fullscreen: x11_surface.is_fullscreen(),
            _activated: x11_surface.is_activated(),
            focused,
            output,
        }),
    })
}

pub fn on_output_bound(state: &mut State, output: &Output, wl_output: &WlOutput) {
    let Some(client) = wl_output.client() else {
        return;
    };

    let protocol_state = &mut state.pinnacle.foreign_toplevel_manager_state;
    for data in protocol_state.toplevels.values_mut() {
        if data.output.as_ref() != Some(&output.downgrade()) {
            continue;
        }

        for (instance, outputs) in data.instances.iter_mut() {
            if instance.client().as_ref() != Some(&client) {
                continue;
            }

            instance.output_enter(wl_output);
            instance.done();
            outputs.push(wl_output.clone());
        }
    }
}

struct PendingToplevelData {
    title: Option<String>,
    app_id: Option<String>,
    maximized: bool,
    minimized: bool,
    fullscreen: bool,
    _activated: bool,
    focused: bool,
    output: Option<Output>,
}

/// Refresh foreign toplevel handle state.
fn refresh_toplevel(
    protocol_state: &mut ForeignToplevelManagerState,
    wl_surface: &WlSurface,
    pending_data: PendingToplevelData,
) {
    let states = to_state_vec(
        pending_data.maximized,
        pending_data.minimized,
        pending_data.fullscreen,
        pending_data.focused,
    );

    match protocol_state.toplevels.entry(wl_surface.clone()) {
        Entry::Occupied(entry) => {
            let data = entry.into_mut();

            let mut new_title = None;
            if data.title != pending_data.title {
                data.title.clone_from(&pending_data.title);
                new_title = pending_data.title.as_deref();

                if new_title.is_none() {
                    error!("toplevel title changed to None");
                }
            }

            let mut new_app_id = None;
            if data.app_id != pending_data.app_id {
                data.app_id.clone_from(&pending_data.app_id);
                new_app_id = pending_data.app_id.as_deref();

                if new_app_id.is_none() {
                    error!("toplevel app_id changed to None");
                }
            }

            let mut states_changed = false;
            if data.states != states {
                data.states = states;
                states_changed = true;
            }

            let mut output_changed = false;
            let pending_output = pending_data.output.as_ref().map(|op| op.downgrade());
            if data.output != pending_output {
                data.output.clone_from(&pending_output);
                output_changed = true;
            }

            // TODO:
            // let mut parent_changed = false;
            // while let Some(parent) = compositor::get_parent(wl_surface) {}

            let something_changed =
                new_title.is_some() || new_app_id.is_some() || states_changed || output_changed;

            if something_changed {
                for (instance, outputs) in data.instances.iter_mut() {
                    if let Some(new_title) = new_title {
                        instance.title(new_title.to_owned());
                    }
                    if let Some(new_app_id) = new_app_id {
                        instance.app_id(new_app_id.to_owned());
                    }
                    if states_changed {
                        instance.state(
                            data.states
                                .iter()
                                .flat_map(|state| (*state as u32).to_ne_bytes())
                                .collect(),
                        );
                    }
                    if output_changed {
                        for wl_output in outputs.drain(..) {
                            instance.output_leave(&wl_output);
                        }
                        if let Some(output) = data.output.as_ref().and_then(|op| op.upgrade())
                            && let Some(client) = instance.client()
                        {
                            for wl_output in output.client_outputs(&client) {
                                instance.output_enter(&wl_output);
                                outputs.push(wl_output);
                            }
                        }
                    }
                    instance.done();
                }
            }

            for outputs in data.instances.values_mut() {
                // Clean up dead wl_outputs.
                outputs.retain(|x| x.is_alive());
            }
        }
        Entry::Vacant(entry) => {
            let mut data = ToplevelData {
                title: pending_data.title.clone(),
                app_id: pending_data.app_id.clone(),
                states,
                output: pending_data.output.map(|op| op.downgrade()),
                instances: HashMap::new(),
                // parent: TODO:
            };

            for manager in protocol_state.instances.iter() {
                if let Some(client) = manager.client() {
                    data.add_instance::<State>(&protocol_state.display, &client, manager);
                }
            }

            entry.insert(data);
        }
    }
}

impl ToplevelData {
    fn add_instance<D>(
        &mut self,
        display: &DisplayHandle,
        client: &Client,
        manager: &ZwlrForeignToplevelManagerV1,
    ) where
        D: Dispatch<ZwlrForeignToplevelHandleV1, ()> + 'static,
    {
        let toplevel = client
            .create_resource::<ZwlrForeignToplevelHandleV1, _, D>(display, manager.version(), ())
            .unwrap();
        manager.toplevel(&toplevel);

        if let Some(title) = self.title.clone() {
            toplevel.title(title);
        }

        if let Some(app_id) = self.app_id.clone() {
            toplevel.app_id(app_id);
        }

        // TODO:
        // toplevel.parent(self.parent.as_ref());

        toplevel.state(
            self.states
                .iter()
                .flat_map(|state| (*state as u32).to_ne_bytes())
                .collect(),
        );

        let mut outputs = Vec::new();
        if let Some(output) = self.output.as_ref().and_then(|op| op.upgrade()) {
            for wl_output in output.client_outputs(client) {
                toplevel.output_enter(&wl_output);
                outputs.push(wl_output);
            }
        }

        toplevel.done();

        self.instances.insert(toplevel, outputs);
    }
}

impl<D> GlobalDispatch<ZwlrForeignToplevelManagerV1, ForeignToplevelGlobalData, D>
    for ForeignToplevelManagerState
where
    D: GlobalDispatch<ZwlrForeignToplevelManagerV1, ForeignToplevelGlobalData>
        + Dispatch<ZwlrForeignToplevelManagerV1, ()>
        + Dispatch<ZwlrForeignToplevelHandleV1, ()>
        + ForeignToplevelHandler,
{
    fn bind(
        state: &mut D,
        handle: &DisplayHandle,
        client: &Client,
        resource: wayland_server::New<ZwlrForeignToplevelManagerV1>,
        _global_data: &ForeignToplevelGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(resource, ());

        let state = state.foreign_toplevel_manager_state();

        for data in state.toplevels.values_mut() {
            data.add_instance::<D>(handle, client, &manager);
        }

        state.instances.push(manager);
    }

    fn can_view(client: Client, global_data: &ForeignToplevelGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<ZwlrForeignToplevelManagerV1, (), D> for ForeignToplevelManagerState
where
    D: Dispatch<ZwlrForeignToplevelManagerV1, ()> + ForeignToplevelHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &ZwlrForeignToplevelManagerV1,
        request: <ZwlrForeignToplevelManagerV1 as Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwlr_foreign_toplevel_manager_v1::Request::Stop => {
                resource.finished();

                state
                    .foreign_toplevel_manager_state()
                    .instances
                    .retain(|instance| instance != resource);
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut D,
        _client: ClientId,
        resource: &ZwlrForeignToplevelManagerV1,
        _data: &(),
    ) {
        state
            .foreign_toplevel_manager_state()
            .instances
            .retain(|instance| instance != resource);
    }
}

impl<D> Dispatch<ZwlrForeignToplevelHandleV1, (), D> for ForeignToplevelManagerState
where
    D: Dispatch<ZwlrForeignToplevelHandleV1, ()> + ForeignToplevelHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &ZwlrForeignToplevelHandleV1,
        request: <ZwlrForeignToplevelHandleV1 as Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let Some((surface, _)) = state
            .foreign_toplevel_manager_state()
            .toplevels
            .iter()
            .find(|(_, data)| data.instances.contains_key(resource))
        else {
            return;
        };
        let surface = surface.clone();

        match request {
            zwlr_foreign_toplevel_handle_v1::Request::SetMaximized => state.set_maximized(surface),
            zwlr_foreign_toplevel_handle_v1::Request::UnsetMaximized => {
                state.unset_maximized(surface);
            }
            zwlr_foreign_toplevel_handle_v1::Request::SetMinimized => state.set_minimized(surface),
            zwlr_foreign_toplevel_handle_v1::Request::UnsetMinimized => {
                state.unset_minimized(surface);
            }
            zwlr_foreign_toplevel_handle_v1::Request::Activate { seat: _ } => {
                state.activate(surface);
            }
            zwlr_foreign_toplevel_handle_v1::Request::Close => state.close(surface),
            zwlr_foreign_toplevel_handle_v1::Request::SetRectangle { .. } => (),
            zwlr_foreign_toplevel_handle_v1::Request::Destroy => (),
            zwlr_foreign_toplevel_handle_v1::Request::SetFullscreen { output } => {
                state.set_fullscreen(surface, output);
            }
            zwlr_foreign_toplevel_handle_v1::Request::UnsetFullscreen => {
                state.unset_fullscreen(surface);
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut D,
        _client: ClientId,
        resource: &ZwlrForeignToplevelHandleV1,
        _data: &(),
    ) {
        for data in state
            .foreign_toplevel_manager_state()
            .toplevels
            .values_mut()
        {
            data.instances.retain(|instance, _| instance != resource);
        }
    }
}

fn to_state_vec(
    maximized: bool,
    minimized: bool,
    fullscreen: bool,

    // activated
    has_focus: bool,
) -> Vec<zwlr_foreign_toplevel_handle_v1::State> {
    let mut state_vec = Vec::new();
    if maximized {
        state_vec.push(zwlr_foreign_toplevel_handle_v1::State::Maximized);
    }
    if fullscreen {
        state_vec.push(zwlr_foreign_toplevel_handle_v1::State::Fullscreen);
    }
    if minimized {
        state_vec.push(zwlr_foreign_toplevel_handle_v1::State::Minimized);
    }

    // HACK: wlr-foreign-toplevel-management states:
    //
    // These have the same meaning as the states with the same names defined in xdg-toplevel
    //
    // However, clients such as sfwbar and fcitx seem to treat the activated state as keyboard
    // focus, i.e. they don't expect multiple windows to have it set at once. Even Waybar which
    // handles multiple activated windows correctly uses it in its design in such a way that
    // keyboard focus would make more sense. Let's do what the clients expect.
    if has_focus {
        state_vec.push(zwlr_foreign_toplevel_handle_v1::State::Activated);
    }

    state_vec
}

#[macro_export]
macro_rules! delegate_foreign_toplevel {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1: $crate::protocol::foreign_toplevel::ForeignToplevelGlobalData
        ] => $crate::protocol::foreign_toplevel::ForeignToplevelManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1: ()
        ] => $crate::protocol::foreign_toplevel::ForeignToplevelManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1: ()
        ] => $crate::protocol::foreign_toplevel::ForeignToplevelManagerState);
    };
}
