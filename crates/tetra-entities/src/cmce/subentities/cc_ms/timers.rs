use super::*;

const TIMESLOT_DURATION_MS: f64 = 170.0 / 12.0;

impl CcMsSubentity {
    pub fn tick_start(&mut self, queue: &mut MessageQueue, ts: TdmaTime) {
        self.dltime = ts;
        let expired: Vec<(u16, bool)> = self
            .calls
            .iter()
            .filter_map(|(id, call)| {
                if call.timers.setup_phase_deadline.map_or(false, |deadline| deadline.age(ts) >= 0) {
                    Some((*id, true))
                } else if call.timers.call_deadline.map_or(false, |deadline| deadline.age(ts) >= 0) {
                    Some((*id, false))
                } else {
                    None
                }
            })
            .collect();
        for (id, setup_phase) in expired {
            tracing::warn!(call_identifier = id, setup_phase, "CMCE-MS: call timer expired");
            let _ = self.disconnect_call(queue, id, DisconnectCause::ExpiryOfTimer);
        }
    }
}

impl MsCall {
    pub(super) fn start_setup_timer(&mut self, now: TdmaTime, timeout: CallTimeoutSetupPhase) {
        self.timers.setup_timeout = Some(timeout);
        self.timers.setup_phase_deadline = setup_timeout_to_timeslots(timeout).map(|slots| now.add_timeslots(slots));
        if self.timers.setup_phase_deadline.is_none() {
            tracing::warn!(
                call_identifier = self.call_identifier,
                "CMCE-MS: predefined setup timer has no codeplug value; not armed"
            );
        }
    }

    pub(super) fn start_call_timer(&mut self, now: TdmaTime, timeout: CallTimeout) {
        self.timers.call_timeout = timeout;
        self.timers.call_deadline = call_timeout_to_timeslots(timeout).map(|slots| now.add_timeslots(slots));
        if timeout == CallTimeout::Reserved {
            tracing::warn!(call_identifier = self.call_identifier, "CMCE-MS: reserved T310 value; not armed");
        }
    }
}

#[inline]
pub(super) fn seconds_to_timeslots(seconds: i32) -> i32 {
    (f64::from(seconds) * 1_000.0 / TIMESLOT_DURATION_MS) as i32
}

/// cl. 14.8.17; predefined is not invented without a codeplug value.
pub(super) fn setup_timeout_to_timeslots(timeout: CallTimeoutSetupPhase) -> Option<i32> {
    match timeout {
        CallTimeoutSetupPhase::Predefined => None,
        CallTimeoutSetupPhase::T1s => Some(seconds_to_timeslots(1)),
        CallTimeoutSetupPhase::T2s => Some(seconds_to_timeslots(2)),
        CallTimeoutSetupPhase::T5s => Some(seconds_to_timeslots(5)),
        CallTimeoutSetupPhase::T10s => Some(seconds_to_timeslots(10)),
        CallTimeoutSetupPhase::T20s => Some(seconds_to_timeslots(20)),
        CallTimeoutSetupPhase::T30s => Some(seconds_to_timeslots(30)),
        CallTimeoutSetupPhase::T60s => Some(seconds_to_timeslots(60)),
    }
}

/// cl. 14.8.16 T310 values.
pub(super) fn call_timeout_to_timeslots(timeout: CallTimeout) -> Option<i32> {
    match timeout {
        CallTimeout::Infinite | CallTimeout::Reserved => None,
        CallTimeout::T30s => Some(seconds_to_timeslots(30)),
        CallTimeout::T45s => Some(seconds_to_timeslots(45)),
        CallTimeout::T60s => Some(seconds_to_timeslots(60)),
        CallTimeout::T2m => Some(seconds_to_timeslots(120)),
        CallTimeout::T3m => Some(seconds_to_timeslots(180)),
        CallTimeout::T4m => Some(seconds_to_timeslots(240)),
        CallTimeout::T5m => Some(seconds_to_timeslots(300)),
        CallTimeout::T6m => Some(seconds_to_timeslots(360)),
        CallTimeout::T8m => Some(seconds_to_timeslots(480)),
        CallTimeout::T10m => Some(seconds_to_timeslots(600)),
        CallTimeout::T12m => Some(seconds_to_timeslots(720)),
        CallTimeout::T15m => Some(seconds_to_timeslots(900)),
        CallTimeout::T20m => Some(seconds_to_timeslots(1200)),
        CallTimeout::T30m => Some(seconds_to_timeslots(1800)),
    }
}
