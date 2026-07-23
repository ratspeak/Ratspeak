//! Privacy-reviewed Activity adapter shared by interface command modules.

use ratspeak_runtime::activity::producer::{
    InterfaceActivity, InterfaceClass, InterfaceTransition, TcpEndpoint, interface_activity,
};

use crate::state::{ActivityRequestFence, AppState};

pub(crate) fn record_interface_event(
    state: &AppState,
    fence: ActivityRequestFence,
    class: InterfaceClass,
    transition: InterfaceTransition,
    endpoint: Option<TcpEndpoint>,
) {
    let _ = state.activity.record_event_fenced(
        || state.is_current_activity_origin_fence(fence),
        move || {
            Ok(interface_activity(InterfaceActivity {
                class,
                transition,
                endpoint,
            }))
        },
    );
}
