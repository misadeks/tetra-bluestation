use super::*;

impl CcMsSubentity {
    /// U-DISCONNECT initiated clearing (cl. 14.5.2.3.1; PDU cl. 14.7.2.4).
    pub fn disconnect_call(&mut self, queue: &mut MessageQueue, call_identifier: u16, cause: DisconnectCause) -> bool {
        let Some(call) = self.calls.get_mut(&call_identifier) else {
            return false;
        };
        let route = call.route;
        let simplex_duplex = call.simplex_duplex_selection;
        call.state = MsCcState::Disconnect;
        call.disconnect_cause = Some(cause);
        self.send_pdu(
            queue,
            &UDisconnect {
                call_identifier,
                disconnect_cause: cause,
                facility: None,
                proprietary: None,
            },
            route,
            false,
            false,
        );
        self.configure_uplane(queue, call_identifier, false, false, simplex_duplex);
        true
    }

    /// U-RELEASE response to D-DISCONNECT (D-DISCONNECT cl. 14.7.1.6; U-RELEASE cl. 14.7.2.8).
    pub fn release_call(&mut self, queue: &mut MessageQueue, call_identifier: u16, cause: DisconnectCause) -> bool {
        let Some(call) = self.calls.get_mut(&call_identifier) else {
            return false;
        };
        let route = call.route;
        let simplex_duplex = call.simplex_duplex_selection;
        call.state = MsCcState::Release;
        call.disconnect_cause = Some(cause);
        self.send_pdu(
            queue,
            &URelease {
                call_identifier,
                disconnect_cause: cause,
                facility: None,
                proprietary: None,
            },
            route,
            false,
            false,
        );
        self.configure_uplane(queue, call_identifier, false, false, simplex_duplex);
        true
    }
}
