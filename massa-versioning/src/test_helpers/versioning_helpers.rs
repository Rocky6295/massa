use crate::versioning::{Advance, ComponentState, LockedIn, MipInfo, MipState};

use massa_models::config::VERSIONING_THRESHOLD_TRANSITION_ACCEPTED;
use massa_time::MassaTime;

// TODO: rename versioning_info
pub fn advance_state_until(at_state: ComponentState, versioning_info: &MipInfo) -> MipState {
    // A helper function to advance a state
    // Assume enough time between versioning info start & timeout
    // TODO: allow to give a threshold as arg?

    let start = versioning_info.start;
    let timeout = versioning_info.timeout;

    if matches!(at_state, ComponentState::Error) {
        return MipState {
            state: ComponentState::error(),
            history: Default::default(),
        };
    }

    let mut state = MipState::new(start.saturating_sub(MassaTime::from_millis(1)));

    if matches!(at_state, ComponentState::Defined(_)) {
        return state;
    }

    let mut advance_msg = Advance {
        start_timestamp: start,
        timeout,
        threshold: Default::default(),
        now: start.saturating_add(MassaTime::from_millis(1)),
        activation_delay: versioning_info.activation_delay,
    };
    state.on_advance(&advance_msg);

    if matches!(at_state, ComponentState::Started(_)) {
        return state;
    }

    if matches!(at_state, ComponentState::Failed(_)) {
        advance_msg.now = timeout.saturating_add(MassaTime::from_millis(1));
        state.on_advance(&advance_msg);
        return state;
    }

    if let ComponentState::LockedIn(LockedIn { at: locked_in_time }) = at_state {
        advance_msg.now = locked_in_time;
        advance_msg.threshold = VERSIONING_THRESHOLD_TRANSITION_ACCEPTED;
        state.on_advance(&advance_msg);
        return state;
    } else {
        advance_msg.now = start.saturating_add(MassaTime::from_millis(2));
        advance_msg.threshold = VERSIONING_THRESHOLD_TRANSITION_ACCEPTED;
        state.on_advance(&advance_msg);
    }

    advance_msg.now = advance_msg
        .now
        .saturating_add(versioning_info.activation_delay)
        .saturating_add(MassaTime::from_millis(1));

    state.on_advance(&advance_msg);
    // Active
    state
}
