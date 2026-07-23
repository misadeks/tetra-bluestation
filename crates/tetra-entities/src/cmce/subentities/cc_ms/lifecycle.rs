use super::*;

impl CcMsSubentity {
    pub fn new(telemetry: Option<TelemetrySink>) -> Self {
        Self {
            own_issi: None,
            dltime: TdmaTime::default(),
            calls: HashMap::new(),
            pending_originations: Vec::new(),
            telemetry,
        }
    }

    pub fn new_with_config(config: SharedConfig, telemetry: Option<TelemetrySink>) -> Self {
        let mut s = Self::new(telemetry);
        s.set_config(config);
        s
    }

    pub fn set_telemetry(&mut self, telemetry: Option<TelemetrySink>) {
        self.telemetry = telemetry;
    }

    pub(super) fn emit(&self, event: TelemetryEvent) {
        if let Some(sink) = &self.telemetry {
            sink.send(event);
        }
    }

    pub fn set_config(&mut self, config: SharedConfig) {
        self.own_issi = config.config().ms.as_ref().map(|ms| ms.issi);
    }

    pub fn call(&self, call_identifier: u16) -> Option<&MsCall> {
        self.calls.get(&call_identifier)
    }

    pub fn call_count(&self) -> usize {
        self.calls.len()
    }

    pub fn pending_origination_count(&self) -> usize {
        self.pending_originations.len()
    }

    pub fn handle_break(&mut self, queue: &mut MessageQueue) {
        // cl. 14.5.1.4.2 e / 14.5.2.2.4: BREAK switches U-plane off; a current
        // self grant is treated as ended as if U-TX CEASED had been sent.
        for id in self.calls.keys().copied().collect::<Vec<_>>() {
            let simplex_duplex = if let Some(call) = self.calls.get_mut(&id) {
                call.state = MsCcState::Restore;
                if call.tx_grant_state == MsTxGrantState::GrantedSelf {
                    call.tx_grant_state = MsTxGrantState::None;
                    call.pending_tx_request = false;
                }
                Some(call.simplex_duplex_selection)
            } else {
                None
            };
            if let Some(simplex_duplex) = simplex_duplex {
                self.configure_uplane(queue, id, false, false, simplex_duplex);
            }
        }
    }

    pub fn handle_reopen(&mut self, queue: &mut MessageQueue) {
        // cl. 17.3.3 MLE-REOPEN plus cl. 14.5.2.2.4: REOPEN indicates failed
        // restoration; clear the call cleanly.
        for id in self.calls.keys().copied().collect::<Vec<_>>() {
            let simplex_duplex = if let Some(call) = self.calls.get_mut(&id) {
                call.state = MsCcState::Release;
                call.disconnect_cause = Some(DisconnectCause::CallRestorationOfTheOtherUserFailed);
                Some(call.simplex_duplex_selection)
            } else {
                None
            };
            if let Some(simplex_duplex) = simplex_duplex {
                self.configure_uplane(queue, id, false, false, simplex_duplex);
            }
            self.calls.remove(&id);
        }
    }
}
