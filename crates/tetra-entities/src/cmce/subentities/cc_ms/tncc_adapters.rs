use super::*;

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

    /// TNCC-SETUP response / TNCC-COMPLETE request adapter (Tables 11.8/11.2).
    pub fn handle_tncc_answer(&mut self, queue: &mut MessageQueue, call_identifier: u16) -> bool {
        self.answer_call(queue, call_identifier, false)
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
