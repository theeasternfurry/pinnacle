use std::time::Duration;

use smithay::{
    delegate_xdg_activation,
    input::Seat,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::xdg_activation::{
        XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
    },
};
use tracing::debug;

use crate::state::State;

pub const XDG_ACTIVATION_TOKEN_TIMEOUT: Duration = Duration::from_secs(10);

pub enum ActivationContext {
    FocusIfPossible,
    UrgentOnly,
}

impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.pinnacle.xdg_activation_state
    }

    fn token_created(&mut self, token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        let _span = tracy_client::span!("XdgActivationHandler::token_created");

        let Some((serial, seat)) = data.serial else {
            data.user_data
                .insert_if_missing(|| ActivationContext::UrgentOnly);
            debug!(
                ?token,
                "xdg-activation: created urgent-only token for missing seat/serial"
            );
            return true;
        };

        let Some(seat) = Seat::<State>::from_resource(&seat) else {
            data.user_data
                .insert_if_missing(|| ActivationContext::UrgentOnly);
            debug!(
                ?token,
                "xdg-activation: created urgent-only token for unknown seat"
            );
            return true;
        };

        let keyboard = seat.get_keyboard().unwrap();

        let valid = keyboard
            .last_enter()
            .is_some_and(|last_enter| serial.is_no_older_than(&last_enter));

        if valid {
            data.user_data
                .insert_if_missing(|| ActivationContext::FocusIfPossible);
            debug!(?token, "xdg-activation: created focus-if-possible token");
        } else {
            debug!(?token, "xdg-activation: invalid token");
        }

        valid
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        let _span = tracy_client::span!("XdgActivationHandler::request_activation");

        if token_data.timestamp.elapsed() >= XDG_ACTIVATION_TOKEN_TIMEOUT {
            debug!("xdg-activation: token {} timed out", token.as_str());
            self.pinnacle.xdg_activation_state.remove_token(&token);
            return;
        }

        let Some(context) = token_data.user_data.get::<ActivationContext>() else {
            debug!("xdg-activation: request without context");
            self.pinnacle.xdg_activation_state.remove_token(&token);
            return;
        };

        if let Some(window) = self.pinnacle.window_for_surface(&surface).cloned() {
            match context {
                ActivationContext::FocusIfPossible => {
                    if window.is_on_active_tag() {
                        let Some(output) = window.output(&self.pinnacle) else {
                            debug!(
                                "xdg-activation: focus-if-possible request on window but it had no tags"
                            );
                            self.pinnacle.xdg_activation_state.remove_token(&token);
                            return;
                        };

                        self.pinnacle.raise_window(window.clone());

                        self.pinnacle.keyboard_focus_stack.set_focus(window);

                        self.schedule_render(&output);
                    }
                }
                ActivationContext::UrgentOnly => {
                    // TODO: add urgent state to windows, use in a focus border/taskbar flash
                }
            }
        } else if let Some(unmapped) = self.pinnacle.unmapped_window_for_surface_mut(&surface) {
            unmapped.activation_token_data = Some(token_data);
        } else {
            debug!("xdg-activation: no window for request");
        }

        self.pinnacle.xdg_activation_state.remove_token(&token);
    }
}
delegate_xdg_activation!(State);
