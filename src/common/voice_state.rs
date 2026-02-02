use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// Runtime-configurable "voice MVP" knobs.
///
/// This is intentionally minimal: it does NOT implement voice call control yet.
/// It only lets the scheduler advertise a traffic slot and transmit placeholder
/// STCH blocks (so we can iterate toward speech later without breaking SDS).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceState {
    /// Enable/disable the placeholder traffic slot behaviour.
    pub enabled: bool,
    /// Which timeslot to treat as the placeholder traffic channel (2..4 typical).
    pub tch_ts: u8,
    /// Usage marker / traffic channel number reported in AACH (4..).
    pub tchan: u8,
}

impl Default for VoiceState {
    fn default() -> Self {
        Self { enabled: false, tch_ts: 2, tchan: 4 }
    }
}

static VOICE_STATE: OnceLock<Mutex<VoiceState>> = OnceLock::new();

fn state() -> &'static Mutex<VoiceState> {
    VOICE_STATE.get_or_init(|| Mutex::new(VoiceState::default()))
}

pub fn get() -> VoiceState {
    state().lock().unwrap().clone()
}

pub fn set(new_state: VoiceState) {
    *state().lock().unwrap() = new_state;
}

pub fn set_enabled(enabled: bool) {
    let mut g = state().lock().unwrap();
    g.enabled = enabled;
}
