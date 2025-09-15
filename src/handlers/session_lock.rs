use smithay::{
    delegate_session_lock,
    output::Output,
    reexports::wayland_server::protocol::wl_output::WlOutput,
    wayland::session_lock::{
        LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
    },
};
use tracing::{debug, warn};

use crate::{
    output::BlankingState,
    state::{State, WithState},
};

/// State of a session lock.
#[derive(Default, Debug)]
pub enum LockState {
    /// There is no session lock.
    #[default]
    Unlocked,
    /// A session lock request came in and we are in the process of blanking outputs.
    Locking(SessionLocker),
    /// The session is locked.
    Locked,
}

impl LockState {
    /// Returns `true` if the lock state is [`Locking`].
    ///
    /// [`Locking`]: LockState::Locking
    #[must_use]
    pub fn is_locking(&self) -> bool {
        matches!(self, Self::Locking(..))
    }

    /// Returns `true` if the lock state is [`Unlocked`].
    ///
    /// [`Unlocked`]: LockState::Unlocked
    #[must_use]
    pub fn is_unlocked(&self) -> bool {
        matches!(self, Self::Unlocked)
    }

    /// Returns `true` if the lock state is [`Locked`].
    ///
    /// [`Locked`]: LockState::Locked
    #[must_use]
    pub fn is_locked(&self) -> bool {
        matches!(self, Self::Locked)
    }
}

impl SessionLockHandler for State {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.pinnacle.session_lock_manager_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        debug!("Received session lock request");

        if self.pinnacle.lock_state.is_locking() || self.pinnacle.lock_state.is_locked() {
            debug!("Denying lock request; another client has already locked the session");
            return;
        }

        self.pinnacle.lock_state = LockState::Locking(confirmation);
        self.pinnacle.schedule(
            |state| {
                let all_outputs_blanked = state.pinnacle.space.outputs().all(|op| {
                    op.with_state(|st| matches!(st.blanking_state, BlankingState::Blanked))
                });
                !state.pinnacle.lock_state.is_locking() || all_outputs_blanked
            },
            |state| match std::mem::take(&mut state.pinnacle.lock_state) {
                LockState::Unlocked => (),
                LockState::Locking(locker) => {
                    debug!("Locking session");
                    locker.lock();
                    state.pinnacle.lock_state = LockState::Locked;
                    for output in state.pinnacle.space.outputs().cloned().collect::<Vec<_>>() {
                        state.schedule_render(&output);
                    }
                }
                LockState::Locked => state.pinnacle.lock_state = LockState::Locked,
            },
        )
    }

    fn unlock(&mut self) {
        debug!("Session lock unlocked");

        for output in self.pinnacle.space.outputs() {
            output.with_state_mut(|state| {
                state.lock_surface.take();
                state.blanking_state = BlankingState::NotBlanked;
            });
        }
        self.pinnacle.lock_state = LockState::Unlocked;

        self.pinnacle.lock_surface_focus.take();
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        let Some(output) = Output::from_resource(&output) else {
            warn!(
                "Session lock surface received but output doesn't exist for wl_output {output:?}"
            );
            return;
        };

        debug!(output = output.name(), "Session lock surface received");

        if self.pinnacle.lock_state.is_unlocked() {
            debug!(
                output = output.name(),
                "Lock surface received but session is unlocked"
            );
            return;
        }

        if output.with_state(|state| state.lock_surface.is_some()) {
            debug!(output = output.name(), "Output already has a lock surface");
            return;
        }

        let Some(geo) = self.pinnacle.space.output_geometry(&output) else {
            return;
        };

        surface.with_pending_state(|state| {
            state.size = Some((geo.size.w as u32, geo.size.h as u32).into())
        });
        surface.send_configure();

        if self.pinnacle.lock_surface_focus.is_none() {
            self.pinnacle.lock_surface_focus = Some(surface.clone());
        }

        output.with_state_mut(|state| state.lock_surface.replace(surface));

        self.schedule_render(&output);
    }
}
delegate_session_lock!(State);
