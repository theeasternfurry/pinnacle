use std::{num::NonZeroU64, time::Duration};

use smithay::utils::{Clock, Monotonic};
use tracing::error;

pub struct FrameClock {
    last_presentation_time: Option<Duration>,
    refresh_interval_ns: Option<NonZeroU64>,
    vrr: bool,
}

impl FrameClock {
    pub fn new(refresh_interval: Option<Duration>) -> Self {
        let refresh_interval_ns = refresh_interval.map(|interval| {
            assert_eq!(interval.as_secs(), 0);
            NonZeroU64::new(interval.subsec_nanos().into()).unwrap()
        });

        Self {
            last_presentation_time: None,
            refresh_interval_ns,
            // This always starts false, setting vrr to true is a runtime operation
            vrr: false,
        }
    }

    pub fn refresh_interval(&self) -> Option<Duration> {
        self.refresh_interval_ns
            .map(|ns| Duration::from_nanos(ns.get()))
    }

    pub fn set_vrr(&mut self, vrr: bool) {
        if self.vrr == vrr {
            return;
        }

        self.vrr = vrr;
        self.last_presentation_time = None;
    }

    pub fn vrr(&self) -> bool {
        self.vrr
    }

    pub fn presented(&mut self, presentation_time: Duration) {
        if presentation_time.is_zero() {
            // Not interested in these
            return;
        }

        self.last_presentation_time = Some(presentation_time);
    }

    /// Returns the amount of time from now to the time of the next estimated presentation.
    pub fn time_to_next_presentation(&self, clock: &Clock<Monotonic>) -> Duration {
        let mut now: Duration = clock.now().into();

        let Some(refresh_interval_ns) = self.refresh_interval_ns else {
            return Duration::ZERO;
        };

        let Some(last_presentation_time) = self.last_presentation_time else {
            return Duration::ZERO;
        };

        let refresh_interval_ns = refresh_interval_ns.get();

        if now <= last_presentation_time {
            // Got an early vblank
            let orig_now = now;
            now += Duration::from_nanos(refresh_interval_ns);

            if now < last_presentation_time {
                // Not sure when this can happen
                error!(
                    now = ?orig_now,
                    ?last_presentation_time,
                    "Got a 2+ early vblank, {:?} until presentation",
                    last_presentation_time - now,
                );
                now = last_presentation_time + Duration::from_nanos(refresh_interval_ns);
            }
        }

        let duration_since_last = now - last_presentation_time;
        let ns_since_last = duration_since_last.as_nanos() as u64;
        let ns_to_next = (ns_since_last / refresh_interval_ns + 1) * refresh_interval_ns;

        // If VRR is enabled and more than one frame passed since last presentation, assume that we
        // can present immediately.
        if self.vrr && ns_to_next > refresh_interval_ns {
            Duration::ZERO
        } else {
            last_presentation_time + Duration::from_nanos(ns_to_next) - now
        }
    }

    pub fn time_since_last_presentation(&self, clock: &Clock<Monotonic>) -> Option<Duration> {
        self.last_presentation_time
            .and_then(|past| Duration::from(clock.now()).checked_sub(past))
    }
}
