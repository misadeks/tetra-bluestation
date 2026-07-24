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
fn floor_route(call: &MsCall) -> (CallRoute, bool) {
    let on_traffic_channel = call.last_uplane.map(|u| u.switch_u_plane).unwrap_or(false);
    if on_traffic_channel {
        (
            CallRoute {
                link_id: TCH_ASSOCIATED_LINK_ID,
                ..call.route
            },
            true,
        )
    } else {
        (call.route, false)
    }
}

impl CcMsSubentity {
    /// U-TX DEMAND (cl. 14.5.2.2.1 a; PDU cl. 14.7.2.12).
    pub fn request_tx(&mut self, queue: &mut MessageQueue, call_identifier: u16, tx_demand_priority: u8) -> bool {
        if tx_demand_priority > 3 {
            tracing::warn!(call_identifier, tx_demand_priority, "CMCE-MS: invalid two-bit TX demand priority");
            return false;
        }
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
        let (route, stealing) = floor_route(call);
        call.pending_tx_request = true;
        self.send_pdu(queue, &pdu, route, stealing, stealing);
        true
    }

    /// U-TX CEASED (cl. 14.5.2.2.1 e; PDU cl. 14.7.2.11).
    pub fn cease_tx(&mut self, queue: &mut MessageQueue, call_identifier: u16) -> bool {
        let Some(call) = self.calls.get_mut(&call_identifier) else {
            return false;
        };
        // Determine the floor route BEFORE tearing the U-plane down below, so the
        // cease is still stolen from the traffic channel it is leaving.
        let (route, stealing) = floor_route(call);
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
}
