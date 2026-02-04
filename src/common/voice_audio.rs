use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::common::tdma_time::TdmaTime;
use crate::common::voice_state;

const DEFAULT_TCH_TS: u8 = 2;
const DEFAULT_TCHAN: u8 = 4;

const ALLOC_RETRY_SLOTS: i32 = 72; // 4 frames
const MAX_ALLOC_ATTEMPTS: u8 = 10;
// One hyperframe = 4 * 18 * 60 timeslots (~61s). Use that as the default timeout budget.
const CALL_SETUP_TIMEOUT_SLOTS: i32 = 4 * 18 * 60; // ~1 minute
const CALL_IDLE_TIMEOUT_SLOTS: i32 = 4 * 18 * 60; // ~1 minute
const RELEASE_TIMEOUT_SLOTS: i32 = 18 * 10; // ~10 frames

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallDuplexMode {
    Simplex,
    Duplex,
}

impl CallDuplexMode {
    pub fn from_simplex_duplex_flag(flag: bool) -> Self {
        if flag { CallDuplexMode::Duplex } else { CallDuplexMode::Simplex }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    Idle,
    Setup,
    Assigned,
    Active,
    Releasing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BearerState {
    None,
    Requested,
    Granted,
    Confirmed,
    Active,
}

#[derive(Debug, Clone)]
pub struct CallLeg {
    pub issi: u32,
    pub bearer_state: BearerState,
    pub last_change: i32,
}

#[derive(Debug, Clone)]
pub struct VoiceCall {
    pub call_id: u16,
    pub caller_issi: u32,
    pub called_gssi: Option<u32>,
    /// For individual calls, the called subscriber (SSI). For group calls this is None.
    pub called_issi: Option<u32>,
    pub mode: CallDuplexMode,
    pub tch_ts: u8,
    pub tchan: u8,
    pub tx_owner_issi: Option<u32>,
    pub state: CallState,
    pub last_state_change: i32,
    pub last_activity: i32,
    pub last_alloc_sent: i32,
    pub next_alloc_at: i32,
    pub alloc_attempts: u8,
    pub legs: HashMap<u32, CallLeg>,
}

#[derive(Debug, Default)]
struct CallRegistry {
    calls: HashMap<u16, VoiceCall>,
}

#[derive(Debug, Default)]
struct TrafficChannelManager {
    ts_to_call: HashMap<u8, u16>,
}

#[derive(Debug, Default)]
struct VoiceAudioState {
    registry: CallRegistry,
    tcm: TrafficChannelManager,
}

static VOICE_AUDIO_STATE: OnceLock<Mutex<VoiceAudioState>> = OnceLock::new();

fn state() -> &'static Mutex<VoiceAudioState> {
    VOICE_AUDIO_STATE.get_or_init(|| Mutex::new(VoiceAudioState::default()))
}

fn now_int(ts: TdmaTime) -> i32 {
    ts.to_int()
}

fn set_state(call: &mut VoiceCall, new_state: CallState, now_i: i32) {
    if call.state != new_state {
        tracing::info!(
            "VOICE state: call_id={} {:?} -> {:?}",
            call.call_id,
            call.state,
            new_state
        );
        call.state = new_state;
        call.last_state_change = now_i;
    }
}

impl TrafficChannelManager {
    fn reserve_ts(&mut self, call_id: u16, preferred_ts: u8) -> u8 {
        if let Some(existing) = self.ts_to_call.get(&preferred_ts).copied() {
            if existing == call_id {
                tracing::info!("VOICE tchan: reuse ts {} for call_id {}", preferred_ts, call_id);
                return preferred_ts;
            }
            tracing::warn!(
                "VOICE tchan: ts {} already assigned to call_id {}, attempting reassignment for call_id {}",
                preferred_ts,
                existing,
                call_id
            );
        } else {
            self.ts_to_call.insert(preferred_ts, call_id);
            tracing::info!("VOICE tchan: assign ts {} to call_id {}", preferred_ts, call_id);
            return preferred_ts;
        }

        for ts in 2..=4 {
            if !self.ts_to_call.contains_key(&ts) {
                self.ts_to_call.insert(ts, call_id);
                tracing::info!("VOICE tchan: assign ts {} to call_id {}", ts, call_id);
                return ts;
            }
        }

        // No free slot; override preferred slot as last resort.
        self.ts_to_call.insert(preferred_ts, call_id);
        tracing::warn!("VOICE tchan: force-assign ts {} to call_id {}", preferred_ts, call_id);
        preferred_ts
    }

    fn release_ts(&mut self, call_id: u16, ts: u8) {
        if let Some(existing) = self.ts_to_call.get(&ts).copied() {
            if existing == call_id {
                self.ts_to_call.remove(&ts);
                tracing::info!("VOICE tchan: release ts {} from call_id {}", ts, call_id);
            }
        }
    }
}

/// Start a new voice call in the shared voice-audio registry.
///
/// The MAC scheduler uses the legs stored here to send MAC-RESOURCE channel allocation
/// to *each* MS that participates in the call (at minimum: caller + called for individual calls).
pub fn start_call(
    call_id: u16,
    caller_issi: u32,
    called_gssi: Option<u32>,
    called_issi: Option<u32>,
    now: TdmaTime,
    mode: CallDuplexMode,
) -> VoiceCall {
    let mut g = state().lock().unwrap();

    let now_i = now_int(now);

    let v = voice_state::get();
    let preferred_ts = if (2..=4).contains(&v.tch_ts) { v.tch_ts } else { DEFAULT_TCH_TS };
    let tchan = if v.tchan >= 4 { v.tchan } else { DEFAULT_TCHAN };
    let tch_ts = g.tcm.reserve_ts(call_id, preferred_ts);

    let mut legs = HashMap::new();
    legs.insert(
        caller_issi,
        CallLeg {
            issi: caller_issi,
            bearer_state: BearerState::Requested,
            last_change: 0,
        },
    );

    // Individual call: ensure the called MS is also tracked so it can receive channel allocation.
    if let Some(issi) = called_issi {
        if issi != caller_issi {
            legs.insert(
                issi,
                CallLeg {
                    issi,
                    bearer_state: BearerState::Requested,
                    last_change: 0,
                },
            );
        }
    }

    let call = VoiceCall {
        call_id,
        caller_issi,
        called_gssi,
        called_issi,
        mode,
        tch_ts,
        tchan,
        tx_owner_issi: None,
        state: CallState::Setup,
        last_state_change: now_i,
        // last_activity is compared against now_i; initialize to now so it cannot instantly time out.
        last_activity: now_i,
        last_alloc_sent: i32::MIN,
        next_alloc_at: now_i,
        alloc_attempts: 0,
        legs,
    };

    tracing::info!(
        "VOICE call: start call_id={} caller={} called={:?} gssi={:?} mode={:?} tch_ts={} tchan={}",
        call_id,
        caller_issi,
        called_issi,
        called_gssi,
        mode,
        tch_ts,
        tchan
    );

    g.registry.calls.insert(call_id, call.clone());
    call
}

pub fn end_call(call_id: u16, reason: &str) {
    let mut g = state().lock().unwrap();
    if let Some(call) = g.registry.calls.remove(&call_id) {
        g.tcm.release_ts(call_id, call.tch_ts);
        tracing::info!("VOICE call: end call_id={} reason={}", call_id, reason);
    }
}

pub fn set_talker(call_id: u16, tx_owner_issi: Option<u32>) {
    let mut g = state().lock().unwrap();
    if let Some(call) = g.registry.calls.get_mut(&call_id) {
        let prev = call.tx_owner_issi;
        call.tx_owner_issi = tx_owner_issi;
        if prev != tx_owner_issi {
            tracing::info!(
                "VOICE talker: call_id={} {:?} -> {:?}",
                call_id,
                prev,
                tx_owner_issi
            );
        }
        if let Some(issi) = tx_owner_issi {
            call.legs.entry(issi).or_insert(CallLeg { issi, bearer_state: BearerState::Active, last_change: 0 });
        }
    }
}

pub fn mark_activity(call_id: u16) {
    let mut g = state().lock().unwrap();
    if let Some(call) = g.registry.calls.get_mut(&call_id) {
        // Legacy helper kept for callers that don't have a timestamp.
        // Treat as “activity happened now-ish” by resetting to 0 only if it was never set.
        // Prefer mark_activity_now().
        if call.last_activity == 0 {
            call.last_activity = 1;
        }
    }
}

/// Record activity using the current TDMA time.
pub fn mark_activity_now(call_id: u16, now: TdmaTime) {
    let mut g = state().lock().unwrap();
    let now_i = now_int(now);
    if let Some(call) = g.registry.calls.get_mut(&call_id) {
        call.last_activity = now_i;
    }
}

pub fn on_ul_tch(ts: u8, now: TdmaTime) {
    let mut g = state().lock().unwrap();
    let now_i = now_int(now);
    let Some(call_id) = g.tcm.ts_to_call.get(&ts).copied() else { return; };
    if let Some(call) = g.registry.calls.get_mut(&call_id) {
        set_state(call, CallState::Active, now_i);
        call.last_activity = now_i;
        if let Some(issi) = call.tx_owner_issi {
            let leg = call.legs.entry(issi).or_insert(CallLeg { issi, bearer_state: BearerState::Active, last_change: now_i });
            leg.bearer_state = BearerState::Active;
            leg.last_change = now_i;
        }
    }
}

pub fn tick(now: TdmaTime) {
    let now_i = now_int(now);
    let mut to_release = Vec::new();

    {
        let mut g = state().lock().unwrap();
        for call in g.registry.calls.values_mut() {
            if call.last_activity == 0 { call.last_activity = now_i; }
            if call.last_state_change == 0 { call.last_state_change = now_i; }

            match call.state {
                CallState::Setup => {
                    if now_i - call.last_state_change > CALL_SETUP_TIMEOUT_SLOTS {
                        set_state(call, CallState::Releasing, now_i);
                    }
                }
                CallState::Assigned => {
                    if call.alloc_attempts > MAX_ALLOC_ATTEMPTS && now_i - call.last_alloc_sent > CALL_SETUP_TIMEOUT_SLOTS {
                        set_state(call, CallState::Releasing, now_i);
                    }
                }
                CallState::Active => {
                    // In simplex, lack of floor-owner for too long likely means the call is abandoned.
                    // In duplex, we may legitimately have no “current talker” while the call is up.
                    let idle = now_i - call.last_activity;
                    if matches!(call.mode, CallDuplexMode::Simplex)
                        && call.tx_owner_issi.is_none()
                        && idle > CALL_IDLE_TIMEOUT_SLOTS
                    {
                        set_state(call, CallState::Releasing, now_i);
                    }
                }
                CallState::Releasing => {
                    if now_i - call.last_state_change > RELEASE_TIMEOUT_SLOTS {
                        to_release.push(call.call_id);
                    }
                }
                CallState::Idle => {}
            }
        }
    }

    for call_id in to_release {
        end_call(call_id, "timeout");
    }
}

/// Return the next (call, target_issi) pair that needs a MAC-RESOURCE channel allocation.
///
/// We send chanalloc to each call leg during setup so both parties can retune to the traffic channel.
pub fn next_chanalloc_due(now: TdmaTime) -> Option<(VoiceCall, u32)> {
    let now_i = now_int(now);
    let mut g = state().lock().unwrap();

    for call in g.registry.calls.values_mut() {
        if matches!(call.state, CallState::Setup | CallState::Assigned) {
            if now_i >= call.next_alloc_at {
                call.alloc_attempts = call.alloc_attempts.saturating_add(1);
                call.last_alloc_sent = now_i;
                call.next_alloc_at = now_i + ALLOC_RETRY_SLOTS;
                set_state(call, CallState::Assigned, now_i);
                tracing::info!(
                    "VOICE chanalloc schedule: call_id={} attempt={} next_at={}",
                    call.call_id,
                    call.alloc_attempts,
                    call.next_alloc_at
                );
                // Pick the next leg that still needs bearer allocation.
                let mut target: Option<u32> = None;
                for (issi, leg) in call.legs.iter_mut() {
                    if matches!(leg.bearer_state, BearerState::Requested) {
                        leg.bearer_state = BearerState::Granted;
                        leg.last_change = now_i;
                        target = Some(*issi);
                        break;
                    }
                }

                // If all legs are already granted, default to the caller.
                let target = target.unwrap_or(call.caller_issi);
                if call.alloc_attempts > MAX_ALLOC_ATTEMPTS {
                    set_state(call, CallState::Releasing, now_i);
                    continue;
                }
                return Some((call.clone(), target));
            }
        }
    }
    None
}

pub fn get_call(call_id: u16) -> Option<VoiceCall> {
    let g = state().lock().unwrap();
    g.registry.calls.get(&call_id).cloned()
}

pub fn get_call_for_ts(ts: u8) -> Option<VoiceCall> {
    let g = state().lock().unwrap();
    let call_id = g.tcm.ts_to_call.get(&ts).copied()?;
    let call = g.registry.calls.get(&call_id)?;
    if matches!(call.state, CallState::Assigned | CallState::Active) {
        Some(call.clone())
    } else {
        None
    }
}

pub fn get_any_call() -> Option<VoiceCall> {
    let g = state().lock().unwrap();
    g.registry.calls.values().next().cloned()
}

pub fn traffic_tchan_for_ts(ts: u8) -> Option<u8> {
    get_call_for_ts(ts).map(|c| c.tchan)
}

pub fn is_traffic_ts(ts: u8) -> bool {
    traffic_tchan_for_ts(ts).is_some()
}
