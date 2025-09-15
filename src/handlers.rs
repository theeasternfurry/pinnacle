// SPDX-License-Identifier: GPL-3.0-or-later

pub mod decoration;
mod drm;
mod foreign_toplevel;
pub mod foreign_toplevel_list;
pub mod idle;
pub mod session_lock;
#[cfg(feature = "snowcap")]
pub mod snowcap_decoration;
pub mod xdg_activation;
mod xdg_shell;
pub mod xwayland;

use std::{collections::HashMap, os::fd::OwnedFd, sync::Arc};

use smithay::{
    backend::{
        input::TabletToolDescriptor,
        renderer::utils::{self, with_renderer_surface_state},
    },
    delegate_compositor, delegate_cursor_shape, delegate_data_control, delegate_data_device,
    delegate_ext_data_control, delegate_fractional_scale, delegate_keyboard_shortcuts_inhibit,
    delegate_layer_shell, delegate_output, delegate_pointer_constraints, delegate_pointer_gestures,
    delegate_presentation, delegate_primary_selection, delegate_relative_pointer, delegate_seat,
    delegate_security_context, delegate_shm, delegate_single_pixel_buffer, delegate_tablet_manager,
    delegate_viewporter, delegate_xwayland_keyboard_grab, delegate_xwayland_shell,
    desktop::{
        self, LayerSurface, PopupKind, PopupManager, WindowSurfaceType, find_popup_root_surface,
        get_popup_toplevel_coords, layer_map_for_output,
    },
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::LedState,
        pointer::{CursorImageStatus, CursorImageSurfaceData, PointerHandle},
    },
    output::{Mode, Output, Scale},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_positioner::ConstraintAdjustment,
        wayland_server::{
            Client, Resource,
            protocol::{
                wl_buffer::WlBuffer, wl_data_source::WlDataSource, wl_output::WlOutput,
                wl_surface::WlSurface,
            },
        },
    },
    utils::{Logical, Point, Rectangle},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            self, CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
        },
        fractional_scale::{self, FractionalScaleHandler},
        keyboard_shortcuts_inhibit::{
            KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState,
            KeyboardShortcutsInhibitor,
        },
        output::OutputHandler,
        pointer_constraints::{PointerConstraintsHandler, with_pointer_constraint},
        seat::WaylandFocus,
        security_context::{
            SecurityContext, SecurityContextHandler, SecurityContextListenerSource,
        },
        selection::{
            SelectionHandler, SelectionSource, SelectionTarget,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
                set_data_device_focus,
            },
            ext_data_control,
            primary_selection::{
                PrimarySelectionHandler, PrimarySelectionState, set_primary_focus,
            },
            wlr_data_control,
        },
        shell::{
            wlr_layer::{self, Layer, LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState},
            xdg::PopupSurface,
        },
        shm::{ShmHandler, ShmState},
        tablet_manager::TabletSeatHandler,
        xwayland_keyboard_grab::XWaylandKeyboardGrabHandler,
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::XWaylandClientData,
};
use tracing::{debug, error, trace, warn};

use crate::{
    backend::Backend,
    delegate_gamma_control, delegate_output_management, delegate_output_power_management,
    delegate_screencopy,
    focus::{keyboard::KeyboardFocusTarget, pointer::PointerFocusTarget},
    hook::add_mapped_toplevel_pre_commit_hook,
    output::OutputMode,
    protocol::{
        gamma_control::{GammaControlHandler, GammaControlManagerState},
        output_management::{
            OutputConfiguration, OutputManagementHandler, OutputManagementManagerState,
        },
        output_power_management::{OutputPowerManagementHandler, OutputPowerManagementState},
        screencopy::{Screencopy, ScreencopyHandler},
    },
    state::{ClientState, Pinnacle, State, WithState},
    window::UnmappedState,
};

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.pinnacle.compositor_state
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        self.pinnacle.add_default_dmabuf_pre_commit_hook(surface);
    }

    fn commit(&mut self, surface: &WlSurface) {
        let _span = tracy_client::span!("CompositorHandler::commit");

        utils::on_commit_buffer_handler::<State>(surface);

        self.backend.early_import(surface);

        if compositor::is_sync_subsurface(surface) {
            return;
        }

        self.pinnacle.popup_manager.commit(surface);

        let mut root = surface.clone();
        while let Some(parent) = compositor::get_parent(&root) {
            root = parent;
        }

        self.pinnacle
            .root_surface_cache
            .insert(surface.clone(), root.clone());

        // Root surface commit
        if surface == &root {
            // Unmapped window commit
            if let Some(idx) = self
                .pinnacle
                .unmapped_windows
                .iter()
                .position(|win| win.window.wl_surface().as_deref() == Some(surface))
            {
                let Some(is_mapped) =
                    with_renderer_surface_state(surface, |state| state.buffer().is_some())
                else {
                    unreachable!("on_commit_buffer_handler was called previously");
                };

                // Unmapped window has become mapped
                if is_mapped {
                    let unmapped = self.pinnacle.unmapped_windows.remove(idx);

                    unmapped.window.on_commit();

                    if let Some(toplevel) = unmapped.window.toplevel() {
                        if let Some(hook) = self.pinnacle.dmabuf_hooks.remove(surface) {
                            compositor::remove_pre_commit_hook(surface, hook);
                        }

                        let hook_id = add_mapped_toplevel_pre_commit_hook(toplevel);

                        unmapped
                            .window
                            .with_state_mut(|state| state.mapped_hook_id = Some(hook_id));
                    }

                    self.map_new_window(unmapped);
                } else {
                    // Still unmapped

                    let mut unmapped = self.pinnacle.unmapped_windows.swap_remove(idx);
                    unmapped.window.on_commit();

                    if matches!(unmapped.state, UnmappedState::WaitingForTags { .. }) {
                        if unmapped.window.output(&self.pinnacle).is_some() {
                            self.pinnacle.request_window_rules(&mut unmapped);
                        } else if let Some(output) = self.pinnacle.focused_output().cloned()
                            && output.with_state(|state| !state.tags.is_empty())
                        {
                            // FIXME: If there are no tags and the window still commits a buffer,
                            // Pinnacle will crash at `map_new_window`.
                            unmapped.window.set_tags_to_output(&output);
                            self.pinnacle.request_window_rules(&mut unmapped);
                        }
                    }

                    if let Some(output) = unmapped.window.output(&self.pinnacle)
                        && let Some(toplevel) = unmapped.window.toplevel()
                    {
                        toplevel.with_pending_state(|state| {
                            state.bounds = self
                                .pinnacle
                                .space
                                .output_geometry(&output)
                                .map(|geo| geo.size);
                        });
                    }

                    self.pinnacle.unmapped_windows.push(unmapped);
                }

                return;
            }

            // Window surface commit
            if let Some(window) = self.pinnacle.window_for_surface(surface).cloned()
                && window.is_wayland()
            {
                let Some(is_mapped) =
                    with_renderer_surface_state(surface, |state| state.buffer().is_some())
                else {
                    unreachable!("on_commit_buffer_handler was called previously");
                };

                window.on_commit();

                // Toplevel has become unmapped,
                // see https://wayland.app/protocols/xdg-shell#xdg_toplevel
                if !is_mapped {
                    self.pinnacle.remove_window(&window, true);

                    let output = window.output(&self.pinnacle);

                    if let Some(output) = output {
                        self.pinnacle.request_layout(&output);
                    }
                }

                // Update reactive popups
                for (popup, _) in PopupManager::popups_for_surface(surface) {
                    if let PopupKind::Xdg(popup) = popup
                        && popup.with_pending_state(|state| state.positioner.reactive)
                    {
                        self.pinnacle.position_popup(&popup);
                        if let Err(err) = popup.send_pending_configure() {
                            warn!("Failed to configure reactive popup: {err}");
                        }
                    }
                }
            }
        }

        let outputs = if let Some(window) = self.pinnacle.window_for_surface(surface) {
            self.pinnacle.space.outputs_for_element(window) // surface is a window
        } else if let Some(window) = self.pinnacle.window_for_surface(&root) {
            self.pinnacle.space.outputs_for_element(window) // surface's root is a window
        } else if let Some(ref popup @ PopupKind::Xdg(ref surf)) =
            self.pinnacle.popup_manager.find_popup(surface)
        {
            if !surf.is_initial_configure_sent() {
                surf.send_configure().expect("initial configure sent twice");
                return;
            }

            let size = surf.with_pending_state(|state| state.geometry.size);
            let loc = find_popup_root_surface(popup)
                .ok()
                .and_then(|surf| self.pinnacle.window_for_surface(&surf))
                .and_then(|win| self.pinnacle.space.element_location(win));

            if let Some(loc) = loc {
                let geo = Rectangle::new(loc, size);
                let outputs = self
                    .pinnacle
                    .space
                    .outputs()
                    .filter_map(|output| {
                        let op_geo = self.pinnacle.space.output_geometry(output);
                        op_geo.and_then(|op_geo| op_geo.overlaps_or_touches(geo).then_some(output))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                outputs
            } else {
                let layer_output = find_popup_root_surface(popup)
                    .ok()
                    .and_then(|surf| {
                        self.pinnacle.space.outputs().find(|op| {
                            let map = layer_map_for_output(op);
                            map.layer_for_surface(&surf, WindowSurfaceType::TOPLEVEL)
                                .is_some()
                        })
                    })
                    .cloned();
                layer_output.map(|op| vec![op]).unwrap_or_default()
            }
        } else if let Some((output, layer)) = {
            // Holy borrow checker
            let mut outputs = self.pinnacle.space.outputs();
            outputs.find_map(|op| {
                let layer_map = layer_map_for_output(op);
                Some((
                    op.clone(),
                    layer_map
                        .layer_for_surface(surface, WindowSurfaceType::ALL)?
                        .clone(),
                ))
            })
        } {
            if !layer_surface_is_initial_configure_sent(&layer) {
                layer.layer_surface().send_configure();
                return;
            }

            let layer_changed = layer_map_for_output(&output).arrange();
            if layer_changed {
                self.pinnacle.request_layout(&output);
            }

            vec![output] // surface is a layer surface
        } else if matches!(self.pinnacle.cursor_state.cursor_image(), CursorImageStatus::Surface(s) if s == surface)
        {
            // This is a cursor surface

            // Update the hotspot if the buffer moved
            compositor::with_states(surface, |states| {
                let cursor_image_attributes = states.data_map.get::<CursorImageSurfaceData>();

                if let Some(mut cursor_image_attributes) =
                    cursor_image_attributes.map(|attrs| attrs.lock().unwrap())
                {
                    let buffer_delta = states
                        .cached_state
                        .get::<SurfaceAttributes>()
                        .current()
                        .buffer_delta
                        .take();
                    if let Some(buffer_delta) = buffer_delta {
                        cursor_image_attributes.hotspot -= buffer_delta;
                    }
                }
            });

            // TODO: granular
            self.pinnacle.space.outputs().cloned().collect()
        } else if self.pinnacle.dnd_icon.as_ref() == Some(surface) {
            // This is a dnd icon
            // TODO: granular
            self.pinnacle.space.outputs().cloned().collect()
        } else if let Some(output) = self
            .pinnacle
            .space
            .outputs()
            .find(|op| {
                op.with_state(|state| {
                    state
                        .lock_surface
                        .as_ref()
                        .is_some_and(|lock| lock.wl_surface() == surface)
                })
            })
            .cloned()
        {
            vec![output] // surface is a lock surface
        } else {
            #[cfg(feature = "snowcap")]
            if let Some((win, deco)) = self.pinnacle.windows.iter().find_map(|win| {
                let deco = win
                    .with_state(|state| {
                        state
                            .decoration_surfaces
                            .iter()
                            .find(|deco| deco.wl_surface() == surface || deco.wl_surface() == &root)
                            .cloned()
                    })
                    .map(|deco| (win.clone(), deco));
                deco
            }) {
                use std::sync::atomic::Ordering;

                if deco.with_state(|state| state.bounds_changed.fetch_and(false, Ordering::Relaxed))
                {
                    if win.with_state(|state| state.layout_mode.is_tiled())
                        && let Some(output) = win.output(&self.pinnacle)
                    {
                        self.pinnacle.request_layout(&output);
                    } else {
                        self.pinnacle.update_window_geometry(&win, false);
                    }
                }

                // FIXME: granular
                self.pinnacle.space.outputs().cloned().collect()
            } else {
                return;
            }

            #[cfg(not(feature = "snowcap"))]
            return;
        };

        for output in outputs {
            self.schedule_render(&output);
        }
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        let _span = tracy_client::span!("CompositorHandler::destroyed");

        let Some(root_surface) = self.pinnacle.root_surface_cache.get(surface) else {
            return;
        };
        let Some(window) = self.pinnacle.window_for_surface(root_surface) else {
            return;
        };
        let Some(output) = window.output(&self.pinnacle) else {
            return;
        };

        self.backend.with_renderer(|renderer| {
            window.capture_snapshot_and_store(
                renderer,
                output.current_scale().fractional_scale().into(),
                1.0,
            );
        });

        self.pinnacle
            .root_surface_cache
            .retain(|surf, root| surf != surface && root != surface);
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        if let Some(state) = client.get_data::<XWaylandClientData>() {
            return &state.compositor_state;
        }
        if let Some(state) = client.get_data::<ClientState>() {
            return &state.compositor_state;
        }
        panic!("Unknown client data type");
    }
}
delegate_compositor!(State);

fn layer_surface_is_initial_configure_sent(layer: &LayerSurface) -> bool {
    let _span = tracy_client::span!("layer_surface_is_initial_configure_sent");

    let initial_configure_sent = compositor::with_states(layer.wl_surface(), |states| {
        states
            .data_map
            .get::<LayerSurfaceData>()
            .unwrap()
            .lock()
            .unwrap()
            .initial_configure_sent
    });

    initial_configure_sent
}

impl ClientDndGrabHandler for State {
    fn started(
        &mut self,
        _source: Option<WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        self.pinnacle.dnd_icon = icon;
    }

    fn dropped(&mut self, _target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        self.pinnacle.dnd_icon = None;
    }
}

impl ServerDndGrabHandler for State {}

impl SelectionHandler for State {
    type SelectionUserData = ();

    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        debug!(?ty, ?source, "SelectionHandler::new_selection");

        if let Some(xwm) = self
            .pinnacle
            .xwayland_state
            .as_mut()
            .map(|xwayland| &mut xwayland.xwm)
            && let Err(err) = xwm.new_selection(ty, source.map(|source| source.mime_types()))
        {
            warn!(?err, ?ty, "Failed to set Xwayland selection");
        }
    }

    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        debug!(?ty, ?mime_type, ?fd, "SelectionHandler::send_selection");

        if let Some(xwm) = self
            .pinnacle
            .xwayland_state
            .as_mut()
            .map(|xwayland| &mut xwayland.xwm)
            && let Err(err) =
                xwm.send_selection(ty, mime_type, fd, self.pinnacle.loop_handle.clone())
        {
            warn!(?err, "Failed to send selection (X11 -> Wayland)");
        }
    }
}

impl DataDeviceHandler for State {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.pinnacle.data_device_state
    }
}
delegate_data_device!(State);

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.pinnacle.primary_selection_state
    }
}
delegate_primary_selection!(State);

impl wlr_data_control::DataControlHandler for State {
    fn data_control_state(&mut self) -> &mut wlr_data_control::DataControlState {
        &mut self.pinnacle.wlr_data_control_state
    }
}
delegate_data_control!(State);

impl ext_data_control::DataControlHandler for State {
    fn data_control_state(&mut self) -> &mut ext_data_control::DataControlState {
        &mut self.pinnacle.ext_data_control_state
    }
}
delegate_ext_data_control!(State);

impl SeatHandler for State {
    type KeyboardFocus = KeyboardFocusTarget;
    type PointerFocus = PointerFocusTarget;
    type TouchFocus = PointerFocusTarget;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.pinnacle.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.pinnacle.cursor_state.set_cursor_image(image);
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&Self::KeyboardFocus>) {
        let _span = tracy_client::span!("SeatHandler::focus_changed");

        let focus_client = focused.and_then(|foc_target| {
            self.pinnacle
                .display_handle
                .get_client(foc_target.wl_surface()?.id())
                .ok()
        });
        set_data_device_focus(&self.pinnacle.display_handle, seat, focus_client.clone());
        set_primary_focus(&self.pinnacle.display_handle, seat, focus_client);
    }

    fn led_state_changed(&mut self, _seat: &Seat<Self>, led_state: LedState) {
        for device in self.pinnacle.input_state.libinput_state.devices.keys() {
            device.clone().led_update(led_state.into());
        }
    }
}
delegate_seat!(State);

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.pinnacle.shm_state
    }
}
delegate_shm!(State);

impl OutputHandler for State {
    fn output_bound(&mut self, output: Output, wl_output: WlOutput) {
        let _span = tracy_client::span!("OutputHandler::output_bound");

        crate::protocol::foreign_toplevel::on_output_bound(self, &output, &wl_output);
    }
}
delegate_output!(State);

delegate_viewporter!(State);

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let _span = tracy_client::span!("FractionalScaleHandler::new_fractional_scale");

        // comment yanked from anvil
        // Here we can set the initial fractional scale
        //
        // First we look if the surface already has a primary scan-out output, if not
        // we test if the surface is a subsurface and try to use the primary scan-out output
        // of the root surface. If the root also has no primary scan-out output we just try
        // to use the first output of the toplevel.
        // If the surface is the root we also try to use the first output of the toplevel.
        //
        // If all the above tests do not lead to a output we just use the first output
        // of the space (which in case of anvil will also be the output a toplevel will
        // initially be placed on)
        let mut root = surface.clone();
        while let Some(parent) = compositor::get_parent(&root) {
            root = parent;
        }

        compositor::with_states(&surface, |states| {
            let primary_scanout_output =
                desktop::utils::surface_primary_scanout_output(&surface, states)
                    .or_else(|| {
                        if root != surface {
                            compositor::with_states(&root, |states| {
                                desktop::utils::surface_primary_scanout_output(&root, states)
                                    .or_else(|| {
                                        self.pinnacle.window_for_surface(&root).and_then(|window| {
                                            self.pinnacle
                                                .space
                                                .outputs_for_element(window)
                                                .first()
                                                .cloned()
                                        })
                                    })
                            })
                        } else {
                            self.pinnacle.window_for_surface(&root).and_then(|window| {
                                self.pinnacle
                                    .space
                                    .outputs_for_element(window)
                                    .first()
                                    .cloned()
                            })
                        }
                    })
                    .or_else(|| self.pinnacle.space.outputs().next().cloned());
            if let Some(output) = primary_scanout_output {
                fractional_scale::with_fractional_scale(states, |fractional_scale| {
                    fractional_scale.set_preferred_scale(output.current_scale().fractional_scale());
                });
            }
        });
    }
}

delegate_fractional_scale!(State);

delegate_relative_pointer!(State);

delegate_presentation!(State);

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.pinnacle.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: wlr_layer::LayerSurface,
        output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let _span = tracy_client::span!("WlrLayerShellHandler::new_layer_surface");

        let output = output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.pinnacle.focused_output().cloned());

        let Some(output) = output else {
            error!("New layer surface, but there was no output to map it on");
            return;
        };

        if let Err(err) =
            layer_map_for_output(&output).map_layer(&desktop::LayerSurface::new(surface, namespace))
        {
            error!("Failed to map layer surface: {err}");
        };
    }

    fn layer_destroyed(&mut self, surface: wlr_layer::LayerSurface) {
        let _span = tracy_client::span!("WlrLayerShellHandler::layer_destroyed");

        self.pinnacle
            .on_demand_layer_focus
            .take_if(|layer| layer.layer_surface() == &surface);

        let mut output: Option<Output> = None;
        if let Some((mut map, layer, op)) = self.pinnacle.space.outputs().find_map(|o| {
            let map = layer_map_for_output(o);
            let layer = map
                .layers()
                .find(|&layer| layer.layer_surface() == &surface)
                .cloned();
            layer.map(|layer| (map, layer, o))
        }) {
            map.unmap_layer(&layer);
            output = Some(op.clone());
        }

        if let Some(output) = output {
            self.pinnacle.request_layout(&output);
        }
    }

    fn new_popup(&mut self, _parent: wlr_layer::LayerSurface, popup: PopupSurface) {
        trace!("WlrLayerShellHandler::new_popup");
        self.pinnacle.position_popup(&popup);
    }
}
delegate_layer_shell!(State);

impl ScreencopyHandler for State {
    fn frame(&mut self, frame: Screencopy) {
        let _span = tracy_client::span!("ScreencopyHandler::frame");

        let output = frame.output().clone();
        if !frame.with_damage() {
            self.schedule_render(&output);
        }
        output.with_state_mut(|state| state.screencopies.push(frame));
    }
}
delegate_screencopy!(State);

impl GammaControlHandler for State {
    fn gamma_control_manager_state(&mut self) -> &mut GammaControlManagerState {
        &mut self.pinnacle.gamma_control_manager_state
    }

    fn get_gamma_size(&mut self, output: &Output) -> Option<u32> {
        let _span = tracy_client::span!("GammaControlHandler::get_gamma_size");

        let Backend::Udev(udev) = &self.backend else {
            return None;
        };

        match udev.gamma_size(output) {
            Ok(0) => None, // Setting gamma is not supported
            Ok(size) => Some(size),
            Err(err) => {
                warn!(
                    "Failed to get gamma size for output {}: {err}",
                    output.name()
                );
                None
            }
        }
    }

    fn set_gamma(&mut self, output: &Output, gammas: [&[u16]; 3]) -> bool {
        let _span = tracy_client::span!("GammaControlHandler::set_gamma");

        let Backend::Udev(udev) = &mut self.backend else {
            warn!("Setting gamma is not supported on the winit backend");
            return false;
        };

        match udev.set_gamma(output, Some(gammas)) {
            Ok(_) => true,
            Err(err) => {
                warn!("Failed to set gamma for output {}: {err}", output.name());
                false
            }
        }
    }

    fn gamma_control_destroyed(&mut self, output: &Output) {
        let _span = tracy_client::span!("GammaControlHandler::gamma_control_destroyed");

        let Backend::Udev(udev) = &mut self.backend else {
            warn!("Resetting gamma is not supported on the winit backend");
            return;
        };

        if let Err(err) = udev.set_gamma(output, None) {
            warn!("Failed to set gamma for output {}: {err}", output.name());
        }
    }
}
delegate_gamma_control!(State);

impl SecurityContextHandler for State {
    fn context_created(&mut self, source: SecurityContextListenerSource, context: SecurityContext) {
        let _span = tracy_client::span!("SecurityContextHandler::context_created");

        self.pinnacle
            .loop_handle
            .insert_source(source, move |client, _, state| {
                let client_state = Arc::new(ClientState {
                    is_restricted: true,
                    ..Default::default()
                });

                if let Err(err) = state
                    .pinnacle
                    .display_handle
                    .insert_client(client, client_state)
                {
                    warn!("Failed to insert a restricted client: {err}");
                } else {
                    trace!("Inserted a restricted client, context={context:?}");
                }
            })
            .expect("Failed to insert security context listener source into event loop");
    }
}
delegate_security_context!(State);

impl PointerConstraintsHandler for State {
    fn new_constraint(&mut self, _surface: &WlSurface, pointer: &PointerHandle<Self>) {
        let _span = tracy_client::span!("PointerConstraintsHandler::new_constraint");

        self.pinnacle
            .maybe_activate_pointer_constraint(pointer.current_location());
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        let _span = tracy_client::span!("PointerConstraintsHandler::cursor_position_hint");

        if with_pointer_constraint(surface, pointer, |constraint| {
            constraint.is_some_and(|c| c.is_active())
        }) {
            let Some((current_focus, current_focus_loc)) =
                self.pinnacle.pointer_contents.focus_under.as_ref()
            else {
                return;
            };

            if current_focus.wl_surface().as_deref() != Some(surface) {
                return;
            }

            pointer.set_location(*current_focus_loc + location);
        }
    }
}
delegate_pointer_constraints!(State);

impl XWaylandShellHandler for State {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.pinnacle.xwayland_shell_state
    }
}
delegate_xwayland_shell!(State);

impl OutputManagementHandler for State {
    fn output_management_manager_state(&mut self) -> &mut OutputManagementManagerState {
        &mut self.pinnacle.output_management_manager_state
    }

    fn apply_configuration(&mut self, config: HashMap<Output, OutputConfiguration>) -> bool {
        let _span = tracy_client::span!("OutputManagementHandler::apply_configuration");

        for (output, config) in config {
            match config {
                OutputConfiguration::Disabled => {
                    self.pinnacle.set_output_enabled(&output, false);
                    self.set_output_powered(&output, false);
                }
                OutputConfiguration::Enabled {
                    mode,
                    position,
                    transform,
                    scale,
                    adaptive_sync,
                } => {
                    self.pinnacle.set_output_enabled(&output, true);
                    self.set_output_powered(&output, true);

                    let mode = mode.map(|(size, refresh)| {
                        if let Some(refresh) = refresh {
                            Mode {
                                size,
                                refresh: refresh.get() as i32,
                            }
                        } else {
                            output
                                .with_state(|state| {
                                    state
                                        .modes
                                        .iter()
                                        .filter(|mode| mode.size == size)
                                        .max_by_key(|mode| mode.refresh)
                                        .copied()
                                })
                                .unwrap_or(Mode {
                                    size,
                                    refresh: 60_000,
                                })
                        }
                    });

                    self.pinnacle.change_output_state(
                        &mut self.backend,
                        &output,
                        mode.map(OutputMode::Smithay),
                        transform,
                        scale.map(Scale::Fractional),
                        position,
                    );

                    if let Some(adaptive_sync) = adaptive_sync {
                        self.backend.set_output_vrr(&output, adaptive_sync);
                        output.with_state_mut(|state| {
                            state.is_vrr_on_demand = false;
                        });
                    }

                    self.pinnacle.request_layout(&output);

                    self.schedule_render(&output);
                }
            }
        }
        self.pinnacle
            .output_management_manager_state
            .update::<State>();
        true
    }

    fn test_configuration(&mut self, config: HashMap<Output, OutputConfiguration>) -> bool {
        debug!(?config);
        true
    }
}
delegate_output_management!(State);

impl OutputPowerManagementHandler for State {
    fn output_power_management_state(&mut self) -> &mut OutputPowerManagementState {
        &mut self.pinnacle.output_power_management_state
    }

    fn set_mode(&mut self, output: &Output, powered: bool) {
        let _span = tracy_client::span!("OutputPowerManagementHandler::set_mode");

        self.set_output_powered(output, powered);

        if powered {
            self.schedule_render(output);
        }
    }
}
delegate_output_power_management!(State);

impl TabletSeatHandler for State {
    fn tablet_tool_image(&mut self, tool: &TabletToolDescriptor, image: CursorImageStatus) {
        // TODO:
        let _ = tool;
        let _ = image;
    }
}
delegate_tablet_manager!(State);

delegate_cursor_shape!(State);

impl KeyboardShortcutsInhibitHandler for State {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.pinnacle.keyboard_shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        // TODO: Some way to not unconditionally activate the inhibitor
        inhibitor.activate();
    }
}
delegate_keyboard_shortcuts_inhibit!(State);

impl XWaylandKeyboardGrabHandler for State {
    fn keyboard_focus_for_xsurface(&self, surface: &WlSurface) -> Option<Self::KeyboardFocus> {
        self.pinnacle
            .window_for_surface(surface)
            .cloned()
            .map(KeyboardFocusTarget::from)
    }
}
delegate_xwayland_keyboard_grab!(State);

delegate_pointer_gestures!(State);

delegate_single_pixel_buffer!(State);

impl Pinnacle {
    fn position_popup(&self, popup: &PopupSurface) {
        let _span = tracy_client::span!("Pinnacle::position_popup");

        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };

        let mut positioner = popup.with_pending_state(|state| state.positioner);

        let popup_geo = (|| -> Option<Rectangle<i32, Logical>> {
            let parent = popup.get_parent_surface()?;

            if parent == root {
                // Slide toplevel popup x's instead of flipping; this mimics Awesome
                positioner
                    .constraint_adjustment
                    .remove(ConstraintAdjustment::FlipX);

                #[cfg(feature = "snowcap")]
                {
                    // Offset by the decoration offset

                    let offset = if let Some(win) = self.window_for_surface(&root) {
                        win.with_state(|state| {
                            let bounds = state.max_decoration_bounds();
                            Point::new(bounds.left as i32, bounds.top as i32)
                        })
                    } else {
                        Default::default()
                    };

                    positioner.offset += offset;
                }
            }

            let (root_global_loc, output) = if let Some(win) = self.window_for_surface(&root) {
                let win_geo = self.space.element_geometry(win)?;
                (win_geo.loc, self.focused_output()?.clone())
            } else {
                self.space.outputs().find_map(|op| {
                    let layer_map = layer_map_for_output(op);
                    let layer = layer_map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)?;
                    let output_loc = self.space.output_geometry(op)?.loc;
                    Some((
                        layer_map.layer_geometry(layer)?.loc + output_loc,
                        op.clone(),
                    ))
                })?
            };

            let parent_global_loc = if root == parent {
                root_global_loc
            } else {
                root_global_loc + get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()))
            };

            let mut output_geo = self.space.output_geometry(&output)?;

            // Make local to parent
            output_geo.loc -= parent_global_loc;
            Some(positioner.get_unconstrained_geometry(output_geo))
        })()
        .unwrap_or_else(|| positioner.get_geometry());

        popup.with_pending_state(|state| {
            state.geometry = popup_geo;
        });
    }

    // From Niri
    /// Attempt to activate any pointer constraint on the pointer focus at `new_pos`.
    pub fn maybe_activate_pointer_constraint(&self, new_pos: Point<f64, Logical>) {
        let _span = tracy_client::span!("Pinnacle::maybe_activate_pointer_constraint");

        let Some((surface, surface_loc)) = self.pointer_contents_under(new_pos).focus_under else {
            return;
        };
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let Some(surface) = surface.wl_surface() else {
            return;
        };
        with_pointer_constraint(&surface, &pointer, |constraint| {
            let Some(constraint) = constraint else { return };

            if constraint.is_active() {
                return;
            }

            // Constraint does not apply if not within region.
            if let Some(region) = constraint.region() {
                let new_pos_surface_local = new_pos - surface_loc;
                if !region.contains(new_pos_surface_local.to_i32_round()) {
                    return;
                }
            }

            constraint.activate();
        });
    }
}
