use std::{
    collections::{HashMap, hash_map::Entry},
    ops::Deref,
};

use smithay::{
    output::{Output, WeakOutput},
    reexports::{
        wayland_protocols_wlr::output_power_management::v1::server::{
            zwlr_output_power_manager_v1::{self, ZwlrOutputPowerManagerV1},
            zwlr_output_power_v1::{self, ZwlrOutputPowerV1},
        },
        wayland_server::{
            self, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, Resource, WEnum,
            backend::ClientId,
        },
    },
};
use tracing::warn;

use crate::state::WithState;

const VERSION: u32 = 1;

pub struct OutputPowerManagementState {
    clients: HashMap<WeakOutput, OutputPower>,
}

/// Newtype that fails the `ZwlrOutputPowerV1` on drop.
struct OutputPower {
    power: ZwlrOutputPowerV1,
    destroyed: bool,
}

impl Deref for OutputPower {
    type Target = ZwlrOutputPowerV1;

    fn deref(&self) -> &Self::Target {
        &self.power
    }
}

impl Drop for OutputPower {
    fn drop(&mut self) {
        if !self.destroyed {
            self.power.failed();
        }
    }
}

pub struct OutputPowerManagementGlobalData {
    filter: Box<dyn Fn(&Client) -> bool + Send + Sync + 'static>,
}

pub trait OutputPowerManagementHandler {
    fn output_power_management_state(&mut self) -> &mut OutputPowerManagementState;
    fn set_mode(&mut self, output: &Output, powered: bool);
}

impl OutputPowerManagementState {
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<ZwlrOutputPowerManagerV1, OutputPowerManagementGlobalData> + 'static,
        F: Fn(&Client) -> bool + Send + Sync + 'static,
    {
        let data = OutputPowerManagementGlobalData {
            filter: Box::new(filter),
        };

        display.create_global::<D, ZwlrOutputPowerManagerV1, _>(VERSION, data);

        Self {
            clients: HashMap::new(),
        }
    }

    pub fn output_removed(&mut self, output: &Output) {
        self.clients.remove(&output.downgrade());
    }

    pub fn mode_set(&self, output: &Output, powered: bool) {
        for client in self
            .clients
            .iter()
            .filter_map(|(op, power)| (op == output).then_some(power))
        {
            client.mode(match powered {
                true => zwlr_output_power_v1::Mode::On,
                false => zwlr_output_power_v1::Mode::Off,
            });
        }
    }
}

impl<D> GlobalDispatch<ZwlrOutputPowerManagerV1, OutputPowerManagementGlobalData, D>
    for OutputPowerManagementState
where
    D: Dispatch<ZwlrOutputPowerManagerV1, ()> + OutputPowerManagementHandler,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: wayland_server::New<ZwlrOutputPowerManagerV1>,
        _global_data: &OutputPowerManagementGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &OutputPowerManagementGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<ZwlrOutputPowerManagerV1, (), D> for OutputPowerManagementState
where
    D: Dispatch<ZwlrOutputPowerV1, ()> + OutputPowerManagementHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &ZwlrOutputPowerManagerV1,
        request: <ZwlrOutputPowerManagerV1 as wayland_server::Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwlr_output_power_manager_v1::Request::GetOutputPower { id, output } => {
                let power = data_init.init(id, ());

                let Some(output) = Output::from_resource(&output) else {
                    warn!("wlr-output-power-management: no output for wl_output {output:?}");
                    power.failed();
                    return;
                };

                match state
                    .output_power_management_state()
                    .clients
                    .entry(output.downgrade())
                {
                    Entry::Occupied(_) => {
                        warn!(
                            "wlr-output-power-management: {} already has an active power manager",
                            output.name()
                        );
                        power.failed();
                    }
                    Entry::Vacant(entry) => {
                        let is_powered = output.with_state(|state| state.powered);
                        power.mode(match is_powered {
                            true => zwlr_output_power_v1::Mode::On,
                            false => zwlr_output_power_v1::Mode::Off,
                        });

                        entry.insert(OutputPower {
                            power,
                            destroyed: false,
                        });
                    }
                }
            }
            zwlr_output_power_manager_v1::Request::Destroy => (),
            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<ZwlrOutputPowerV1, (), D> for OutputPowerManagementState
where
    D: Dispatch<ZwlrOutputPowerV1, ()> + OutputPowerManagementHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &ZwlrOutputPowerV1,
        request: <ZwlrOutputPowerV1 as wayland_server::Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwlr_output_power_v1::Request::SetMode { mode } => {
                let Some(output) = state
                    .output_power_management_state()
                    .clients
                    .iter()
                    .find_map(|(output, power)| {
                        (power.power == *resource).then_some(output.clone())
                    })
                else {
                    resource.failed();
                    return;
                };

                let Some(output) = output.upgrade() else {
                    state
                        .output_power_management_state()
                        .clients
                        .remove(&output);
                    return;
                };

                state.set_mode(
                    &output,
                    match mode {
                        WEnum::Value(zwlr_output_power_v1::Mode::On) => true,
                        WEnum::Value(zwlr_output_power_v1::Mode::Off) => false,
                        mode => {
                            resource.post_error(
                                zwlr_output_power_v1::Error::InvalidMode,
                                format!("invalid mode {mode:?}"),
                            );
                            return;
                        }
                    },
                );
            }
            zwlr_output_power_v1::Request::Destroy => {
                state
                    .output_power_management_state()
                    .clients
                    .retain(|_, power| {
                        let should_retain = power.power != *resource;
                        if !should_retain {
                            power.destroyed = true;
                        }
                        should_retain
                    });
            }
            _ => todo!(),
        }
    }

    fn destroyed(state: &mut D, _client: ClientId, resource: &ZwlrOutputPowerV1, _data: &()) {
        state
            .output_power_management_state()
            .clients
            .retain(|_, power| {
                let should_retain = power.power != *resource;
                if !should_retain {
                    power.destroyed = true;
                }
                should_retain
            });
    }
}

#[macro_export]
macro_rules! delegate_output_power_management {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1: $crate::protocol::output_power_management::OutputPowerManagementGlobalData
        ] => $crate::protocol::output_power_management::OutputPowerManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1: ()
        ] => $crate::protocol::output_power_management::OutputPowerManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::zwlr_output_power_v1::ZwlrOutputPowerV1: ()
        ] => $crate::protocol::output_power_management::OutputPowerManagementState);
    };
}
