use super::*;

impl CcMsSubentity {
    /// U-CALL RESTORE (cl. 14.5.2.2.4; PDU cl. 14.7.2.2). Phase 1 wires this
    /// seam but only receives MLE-REOPEN, the unsuccessful restoration indication.
    pub fn request_call_restore(&self, queue: &mut MessageQueue, call_identifier: u16) -> bool {
        let Some(call) = self.calls.get(&call_identifier) else {
            return false;
        };
        let pdu = UCallRestore {
            call_identifier,
            request_to_transmit_send_data: call.pending_tx_request || call.tx_grant_state == MsTxGrantState::GrantedSelf,
            other_party_type_identifier: PartyTypeIdentifier::Ssi.into_raw() as u8,
            other_party_short_number_address: None,
            other_party_ssi: Some(call.route.main_address.ssi as u64),
            other_party_extension: None,
            basic_service_information: Some(call.basic_service.clone()),
            facility: None,
            dm_ms_address: None,
            proprietary: None,
        };
        self.send_pdu(queue, &pdu, call.route, false, false);
        true
    }
}
