//! Debugging utilities.
//!
//! WARNING: This module is not governed by the API stability guarantees.

use pinnacle_api_defs::pinnacle::{
    debug::v1::{
        SetCursorPlaneScanoutRequest, SetDamageVisualizationRequest,
        SetOpaqueRegionVisualizationRequest, SetProcessPipingRequest,
    },
    util::v1::SetOrToggle,
};

use crate::{BlockOnTokio, client::Client};

/// Sets damage visualization.
///
/// When on, parts of the screen that are damaged after rendering will have
/// red rectangles drawn where the damage is.
pub fn set_damage_visualization(set: bool) {
    Client::debug()
        .set_damage_visualization(SetDamageVisualizationRequest {
            set_or_toggle: match set {
                true => SetOrToggle::Set,
                false => SetOrToggle::Unset,
            }
            .into(),
        })
        .block_on_tokio()
        .unwrap();
}

/// Toggles damage visualization.
///
/// When on, parts of the screen that are damaged after rendering will have
/// red rectangles drawn where the damage is.
pub fn toggle_damage_visualization() {
    Client::debug()
        .set_damage_visualization(SetDamageVisualizationRequest {
            set_or_toggle: SetOrToggle::Toggle.into(),
        })
        .block_on_tokio()
        .unwrap();
}

/// Sets opaque region visualization.
///
/// When on, parts of the screen that are opaque will have a transparent blue rectangle
/// drawn over it, while parts that are not opaque will have a transparent red rectangle
/// drawn.
pub fn set_opaque_region_visualization(set: bool) {
    Client::debug()
        .set_opaque_region_visualization(SetOpaqueRegionVisualizationRequest {
            set_or_toggle: match set {
                true => SetOrToggle::Set,
                false => SetOrToggle::Unset,
            }
            .into(),
        })
        .block_on_tokio()
        .unwrap();
}

/// Toggles opaque region visualization.
///
/// When on, parts of the screen that are opaque will have a transparent blue rectangle
/// drawn over it, while parts that are not opaque will have a transparent red rectangle
/// drawn.
pub fn toggle_opaque_region_visualization() {
    Client::debug()
        .set_opaque_region_visualization(SetOpaqueRegionVisualizationRequest {
            set_or_toggle: SetOrToggle::Toggle.into(),
        })
        .block_on_tokio()
        .unwrap();
}

/// Enables or disables cursor plane scanout.
pub fn set_cursor_plane_scanout(set: bool) {
    Client::debug()
        .set_cursor_plane_scanout(SetCursorPlaneScanoutRequest {
            set_or_toggle: match set {
                true => SetOrToggle::Set,
                false => SetOrToggle::Unset,
            }
            .into(),
        })
        .block_on_tokio()
        .unwrap();
}

/// Toggles cursor plane scanout.
pub fn toggle_cursor_plane_scanout() {
    Client::debug()
        .set_cursor_plane_scanout(SetCursorPlaneScanoutRequest {
            set_or_toggle: SetOrToggle::Toggle.into(),
        })
        .block_on_tokio()
        .unwrap();
}

/// Enables or disables process spawning setting up pipes to expose fds to the config.
pub fn set_process_piping(set: bool) {
    Client::debug()
        .set_process_piping(SetProcessPipingRequest {
            set_or_toggle: match set {
                true => SetOrToggle::Set,
                false => SetOrToggle::Unset,
            }
            .into(),
        })
        .block_on_tokio()
        .unwrap();
}

/// Toggles process spawning setting up pipes to expose fds to the config.
pub fn toggle_process_piping() {
    Client::debug()
        .set_process_piping(SetProcessPipingRequest {
            set_or_toggle: SetOrToggle::Toggle.into(),
        })
        .block_on_tokio()
        .unwrap();
}
