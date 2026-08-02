use super::*;

/// TCH-associated basic link identity (cl. 21.4.3.3 basic link addressing;
/// cl. 23.4.2 TCH-associated signalling). Floor-control PDUs stolen from a
/// traffic half-slot (FACCH) travel on this basic link (link_id 2), not on the
/// assigned control channel's basic link (link_id 0 = MCCH/SACCH).
const TCH_ASSOCIATED_LINK_ID: LinkId = 2;

/// Decide how a floor-control PDU (U-TX-DEMAND / U-TX-CEASED) must be carried
/// (cl. 14.5.2). Once the call is on a traffic channel — the U-plane has been
/// switched on for it (cl. 14.5.1.4) — the PDU is stolen from the assigned TCH
/// half-slot (FACCH) and sent as acknowledged BL-DATA on the TCH-associated
/// basic link (link_id 2), which is how the SwMI actually receives it. Before a
/// traffic channel is assigned it falls back to the assigned control channel
/// (MCCH, link_id 0) as plain acknowledged BL-DATA — stealing pre-TCH would
/// force the LLC onto unacknowledged BL-UDATA, which the SwMI MLE discards.
/// Returns `(route, stealing)`.
///
/// ACK-KEY (cl. 22.3.2.3): the TCH-associated (and MCCH) basic link is the
/// individual, point-to-point MS↔SwMI acknowledged link. The LLC keys its
/// send-sequence space, expected-ACK entry and incoming-ACK match on the
/// layer-2 `main_address`, and the SwMI acknowledges the MS's OWN individual
/// ISSI (the on-air MAC source). For a group call `call.route.main_address` is
/// the group number, so keying the acknowledged BL-DATA on it makes the SwMI's
/// ISSI-addressed BL-ACK unmatchable → the entry retransmits to exhaustion.
/// Key the floor route on the MS's own individual ISSI instead; the SwMI
/// correlates the call via the Call identifier IE *inside* the CMCE PDU
/// (cl. 14.5.2 / 14.8.4), not via the layer-2 address. This mirrors the MM
/// registration / U-SETUP uplink path, which already addresses own ISSI. For an
/// individual call `call.route.main_address` is already own ISSI, so this is a
/// no-op there. It is also on-air neutral: the stolen MAC-DATA source address is
/// UMAC's config ISSI regardless of this field.
fn floor_route(call: &MsCall, own_issi: Option<u32>) -> (CallRoute, bool) {
    let mut route = call.route;
    if let Some(issi) = own_issi {
        route.main_address = TetraAddress::new(issi, SsiType::Issi);
    }
    let on_traffic_channel = call.last_uplane.map(|u| u.switch_u_plane).unwrap_or(false);
    if on_traffic_channel {
        route.link_id = TCH_ASSOCIATED_LINK_ID;
        (route, true)
    } else {
        (route, false)
    }
}

impl CcMsSubentity {
    /// U-TX DEMAND (cl. 14.5.2.2.1 a; PDU cl. 14.7.2.12).
    pub fn request_tx(&mut self, queue: &mut MessageQueue, call_identifier: u16, tx_demand_priority: u8) -> bool {
        if tx_demand_priority > 3 {
            tracing::warn!(call_identifier, tx_demand_priority, "CMCE-MS: invalid two-bit TX demand priority");
            return false;
        }
        let own_issi = self.own_issi;
        let Some(call) = self.calls.get_mut(&call_identifier) else {
            return false;
        };
        if !call.transmission_request_allowed {
            tracing::warn!(call_identifier, "CMCE-MS: SwMI disallows TX DEMAND");
            return false;
        }
        let pdu = UTxDemand {
            call_identifier,
            tx_demand_priority,
            encryption_control: call.basic_service.encryption_flag,
            reserved: false,
            facility: None,
            dm_ms_address: None,
            proprietary: None,
        };
        // Steal the assigned TCH half-slot once on a traffic channel; fall back
        // to the control channel as acknowledged BL-DATA pre-TCH (cl. 14.5.2).
        // Keyed on own ISSI so the SwMI's ACK matches (cl. 22.3.2.3).
        let (route, stealing) = floor_route(call, own_issi);
        call.pending_tx_request = true;
        self.send_pdu(queue, &pdu, route, stealing, stealing);
        true
    }

    /// U-TX CEASED (cl. 14.5.2.2.1 e; PDU cl. 14.7.2.11).
    pub fn cease_tx(&mut self, queue: &mut MessageQueue, call_identifier: u16) -> bool {
        let own_issi = self.own_issi;
        let Some(call) = self.calls.get_mut(&call_identifier) else {
            return false;
        };
        // Determine the floor route BEFORE tearing the U-plane down below, so the
        // cease is still stolen from the traffic channel it is leaving.
        let (route, stealing) = floor_route(call, own_issi);
        let simplex_duplex = call.simplex_duplex_selection;
        call.tx_grant_state = MsTxGrantState::None;
        call.pending_tx_request = false;
        self.send_pdu(
            queue,
            &UTxCeased {
                call_identifier,
                facility: None,
                dm_ms_address: None,
                proprietary: None,
            },
            route,
            stealing,
            stealing,
        );
        self.configure_uplane(queue, call_identifier, false, false, simplex_duplex);
        true
    }

    /// TNCC-DTMF request (Table 11.3, cl. 11.3.3.3): send in-call DTMF as a
    /// U-INFO PDU carrying the DTMF IE (PDU cl. 14.7.2.6; IE cl. 14.8.19).
    ///
    /// DTMF is meaningful only on a through-connected point-to-point (individual
    /// / duplex) call — typically a PABX/PSTN gateway call where the digits post-
    /// dial an extension or navigate an IVR. It is refused for a group call and
    /// for an unknown call identifier. The U-INFO is carried like the floor PDUs
    /// (cl. 14.5.2): stolen from the assigned TCH half-slot (FACCH) once the call
    /// is on a traffic channel, otherwise on the control channel — keyed on the
    /// MS's own ISSI so the SwMI's acknowledgement matches (cl. 22.3.2.3), with
    /// the call correlated by the Call identifier IE inside the PDU (cl. 14.8.4).
    ///
    /// `dtmf_tone_delimiter = Dtmf` sends a "tone start" element carrying the
    /// digits; `= ToneEnd` sends a "tone end" element (no digits). Returns an
    /// error describing why the request was rejected (no U-INFO is emitted then).
    pub fn handle_tncc_dtmf(
        &mut self,
        queue: &mut MessageQueue,
        call_identifier: u16,
        request: &tncc::TnccDtmfRequest,
    ) -> Result<(), String> {
        let own_issi = self.own_issi;
        let Some(call) = self.calls.get(&call_identifier) else {
            return Err(format!("TNCC-DTMF for unknown call identifier {call_identifier}"));
        };
        if call.kind != MsCallKind::Individual {
            return Err("TNCC-DTMF is only valid on an individual/duplex (point-to-point) call".to_string());
        }
        let dtmf_ie = match request.dtmf_tone_delimiter {
            tncc::DtmfToneDelimiter::Dtmf => {
                let digits = request.dtmf_digits.as_deref().unwrap_or(&[]);
                if digits.is_empty() {
                    return Err("TNCC-DTMF tone-start requires at least one digit".to_string());
                }
                if let Some(n) = request.number_of_dtmf_digits {
                    if n as usize != digits.len() {
                        return Err(format!(
                            "TNCC-DTMF number_of_dtmf_digits ({n}) disagrees with dtmf_digits ({})",
                            digits.len()
                        ));
                    }
                }
                let nibbles: Vec<u8> = digits.iter().map(|d| dtmf_digit_nibble(*d)).collect();
                dtmf::encode_tone_start(&nibbles)
                    .ok_or_else(|| format!("TNCC-DTMF has too many digits ({}, max 254)", nibbles.len()))?
            }
            tncc::DtmfToneDelimiter::ToneEnd => dtmf::encode_tone_end(),
        };
        let pdu = UInfo {
            call_identifier,
            poll_response: false,
            modify: None,
            dtmf: Some(dtmf_ie),
            facility: None,
            proprietary: None,
        };
        let (route, stealing) = floor_route(call, own_issi);
        self.send_pdu(queue, &pdu, route, stealing, stealing);
        Ok(())
    }
}

/// Map a TNCC DTMF digit to its 4-bit DTMF-digit code (ETSI TS 100 392-2
/// cl. 14.8.19a / Table 14.57).
fn dtmf_digit_nibble(d: tncc::DtmfDigit) -> u8 {
    match d {
        tncc::DtmfDigit::Digit0 => 0x0,
        tncc::DtmfDigit::Digit1 => 0x1,
        tncc::DtmfDigit::Digit2 => 0x2,
        tncc::DtmfDigit::Digit3 => 0x3,
        tncc::DtmfDigit::Digit4 => 0x4,
        tncc::DtmfDigit::Digit5 => 0x5,
        tncc::DtmfDigit::Digit6 => 0x6,
        tncc::DtmfDigit::Digit7 => 0x7,
        tncc::DtmfDigit::Digit8 => 0x8,
        tncc::DtmfDigit::Digit9 => 0x9,
        tncc::DtmfDigit::DigitStar => 0xA,
        tncc::DtmfDigit::DigitHash => 0xB,
        tncc::DtmfDigit::DigitA => 0xC,
        tncc::DtmfDigit::DigitB => 0xD,
        tncc::DtmfDigit::DigitC => 0xE,
        tncc::DtmfDigit::DigitD => 0xF,
    }
}
