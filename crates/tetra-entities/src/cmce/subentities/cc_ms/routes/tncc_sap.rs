use super::*;

// TNCC-SAP (upper boundary) ingress route for CC-MS.
//
// The user application drives MS call control across the TNCC-SAP
// (TS 100 392-2 cl. 11.3.3): TNCC-SETUP / -TX / -RELEASE requests and the
// TNCC-SETUP response / TNCC-COMPLETE answer. These adapters translate each
// TNCC primitive into the Phase-1 CC engine's origination/answer/floor/release
// procedures; no call-control behaviour is duplicated here. This mirrors the
// cc_bs `routes/ra.rs` application-ingress route (the BS's network-side call
// control), keeping every CC ingress SAP under `routes/`.
impl CcMsSubentity {
    /// TNCC-SETUP request (Table 11.8, cl. 11.3.3.8) adapter: build U-SETUP
    /// through the Phase-1 CC engine; no call-control behaviour is duplicated.
    pub fn handle_tncc_setup_request(&mut self, queue: &mut MessageQueue, request: &tncc::TnccSetupRequest) -> Result<(), String> {
        let Some(called_party_ssi) = request.called_party_ssi else {
            return Err("TNCC-SETUP request without called party SSI is not supported by this MS CC engine".to_string());
        };
        let basic = pdu_basic_from_tncc(&request.basic_service_information)?;
        match request.called_party_type_identifier {
            tncc::CalledPartyTypeIdentifier::Ssi => {
                if request.basic_service_information.communication_type == tncc::CommunicationType::PointToPoint {
                    self.originate_individual_call(
                        queue,
                        called_party_ssi,
                        basic,
                        request.simplex_duplex_selection.as_bool(),
                        request.request_to_transmit_send_data.as_bool(),
                    );
                } else {
                    self.originate_group_call(queue, called_party_ssi, basic, request.request_to_transmit_send_data.as_bool());
                }
                Ok(())
            }
            tncc::CalledPartyTypeIdentifier::Sna | tncc::CalledPartyTypeIdentifier::Tsi => {
                Err("TNCC-SETUP SNA/TSI called-party addressing is not implemented by the Phase-1 engine".to_string())
            }
        }
    }

    /// TNCC-SETUP response adapter (Table 11.8, cl. 11.3.3.8) for the MT
    /// answer, per cl. 14.5.1.1.1. The signalling mode is dictated by the
    /// D-SETUP Hook method selection IE (cl. 14.8.23) stored on the call:
    /// on/off-hook → send U-ALERT and remain in MT-CALL-SETUP; direct set-up →
    /// send U-CONNECT immediately and start T301.
    pub fn handle_tncc_setup_response(
        &mut self,
        queue: &mut MessageQueue,
        call_identifier: u16,
        response: &tncc::TnccSetupResponse,
    ) -> bool {
        let Some(call) = self.calls.get(&call_identifier) else {
            return false;
        };
        if call.hook_on_off {
            self.send_u_alert(queue, call_identifier, response)
        } else {
            let basic_service = response
                .basic_service_information
                .as_ref()
                .and_then(|b| pdu_basic_from_tncc(b).ok())
                .unwrap_or_else(|| call.basic_service.clone());
            self.connect_call(queue, call_identifier, response.simplex_duplex_selection.as_bool(), basic_service)
        }
    }

    /// TNCC-COMPLETE request adapter (Table 11.2, cl. 11.3.3.2): the on/off-hook
    /// called user has answered, so CC sends U-CONNECT and starts T301
    /// (cl. 14.5.1.1.1), remaining in MT-CALL-SETUP.
    pub fn handle_tncc_complete(
        &mut self,
        queue: &mut MessageQueue,
        call_identifier: u16,
        request: &tncc::TnccCompleteRequest,
    ) -> bool {
        let Some(call) = self.calls.get(&call_identifier) else {
            return false;
        };
        let basic_service = request
            .basic_service_information_offered
            .as_ref()
            .and_then(|b| pdu_basic_from_tncc(b).ok())
            .unwrap_or_else(|| call.basic_service.clone());
        self.connect_call(queue, call_identifier, request.simplex_duplex.as_bool(), basic_service)
    }

    /// TNCC-TX request adapter (Table 11.9).
    pub fn handle_tncc_tx_request(&mut self, queue: &mut MessageQueue, call_identifier: u16, request: tncc::TnccTxRequest) -> bool {
        match request.transmission_condition {
            tncc::TransmissionCondition::RequestToTransmit => {
                self.request_tx(queue, call_identifier, request.tx_demand_priority.into_raw())
            }
            tncc::TransmissionCondition::TransmissionCeased => self.cease_tx(queue, call_identifier),
        }
    }

    /// TNCC-RELEASE request adapter (Table 11.7).
    pub fn handle_tncc_release_request(
        &mut self,
        queue: &mut MessageQueue,
        call_identifier: u16,
        request: tncc::TnccReleaseRequest,
    ) -> Result<(), String> {
        let cause = pdu_disconnect_cause_from_tncc(request.disconnect_cause)?;
        let acted = match request.disconnect_type {
            tncc::DisconnectType::DisconnectCall => self.disconnect_call(queue, call_identifier, cause),
            tncc::DisconnectType::LeaveCallWithoutDisconnection | tncc::DisconnectType::LeaveCallTemporarily => {
                self.release_call(queue, call_identifier, cause)
            }
        };
        if acted {
            Ok(())
        } else {
            Err("unknown call identifier".to_string())
        }
    }
}
