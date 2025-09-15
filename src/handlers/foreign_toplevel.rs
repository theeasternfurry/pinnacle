use smithay::reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface};

use crate::{
    delegate_foreign_toplevel,
    protocol::foreign_toplevel::{ForeignToplevelHandler, ForeignToplevelManagerState},
    state::{State, WithState},
};

impl ForeignToplevelHandler for State {
    fn foreign_toplevel_manager_state(&mut self) -> &mut ForeignToplevelManagerState {
        &mut self.pinnacle.foreign_toplevel_manager_state
    }

    fn activate(&mut self, wl_surface: WlSurface) {
        let _span = tracy_client::span!("ForeignToplevelHandler::activate");

        let Some(window) = self.pinnacle.window_for_surface(&wl_surface).cloned() else {
            return;
        };
        let Some(output) = window.output(&self.pinnacle) else {
            return;
        };

        self.pinnacle.keyboard_focus_stack.set_focus(window.clone());
        self.pinnacle.raise_window(window.clone());

        if !window.is_on_active_tag() {
            let new_active_tag = window.with_state(|state| {
                state
                    .tags
                    .iter()
                    .min_by_key(|tag| tag.id().to_inner())
                    .cloned()
            });

            if let Some(tag) = new_active_tag {
                crate::api::tag::switch_to(self, &tag);
            }
        } else {
            self.schedule_render(&output);
        }
    }

    fn close(&mut self, wl_surface: WlSurface) {
        let _span = tracy_client::span!("ForeignToplevelHandler::close");

        let Some(window) = self.pinnacle.window_for_surface(&wl_surface) else {
            return;
        };

        window.close();
    }

    fn set_fullscreen(&mut self, wl_surface: WlSurface, _wl_output: Option<WlOutput>) {
        let _span = tracy_client::span!("ForeignToplevelHandler::set_fullscreen");

        let Some(window) = self.pinnacle.window_for_surface(&wl_surface).cloned() else {
            return;
        };

        self.pinnacle
            .update_window_layout_mode(&window, |layout_mode| {
                layout_mode.set_client_fullscreen(true);
            });
    }

    fn unset_fullscreen(&mut self, wl_surface: WlSurface) {
        let _span = tracy_client::span!("ForeignToplevelHandler::unset_fullscreen");

        let Some(window) = self.pinnacle.window_for_surface(&wl_surface).cloned() else {
            return;
        };

        self.pinnacle
            .update_window_layout_mode(&window, |layout_mode| {
                layout_mode.set_client_fullscreen(false);
            });
    }

    fn set_maximized(&mut self, wl_surface: WlSurface) {
        let _span = tracy_client::span!("ForeignToplevelHandler::set_maximized");

        let Some(window) = self.pinnacle.window_for_surface(&wl_surface).cloned() else {
            return;
        };

        self.pinnacle
            .update_window_layout_mode(&window, |layout_mode| {
                layout_mode.set_client_maximized(true);
            });
    }

    fn unset_maximized(&mut self, wl_surface: WlSurface) {
        let _span = tracy_client::span!("ForeignToplevelHandler::unset_maximized");

        let Some(window) = self.pinnacle.window_for_surface(&wl_surface).cloned() else {
            return;
        };

        self.pinnacle
            .update_window_layout_mode(&window, |layout_mode| {
                layout_mode.set_client_maximized(false);
            });
    }

    // TODO:
    fn set_minimized(&mut self, wl_surface: WlSurface) {
        let _span = tracy_client::span!("ForeignToplevelHandler::set_minimized");

        let Some(window) = self.pinnacle.window_for_surface(&wl_surface) else {
            return;
        };

        window.with_state_mut(|state| state.minimized = true);

        let Some(output) = window.output(&self.pinnacle) else {
            return;
        };

        self.pinnacle.request_layout(&output);
        self.schedule_render(&output);
    }

    // TODO:
    fn unset_minimized(&mut self, wl_surface: WlSurface) {
        let _span = tracy_client::span!("ForeignToplevelHandler::unset_minimized");

        let Some(window) = self.pinnacle.window_for_surface(&wl_surface) else {
            return;
        };

        window.with_state_mut(|state| state.minimized = false);

        let Some(output) = window.output(&self.pinnacle) else {
            return;
        };

        self.pinnacle.request_layout(&output);
        self.schedule_render(&output);
    }
}
delegate_foreign_toplevel!(State);
