use super::*;

impl CcBsSubentity {
    pub(in crate::cmce::subentities::cc_bs) fn fsm_on_u_call_restore(
        &mut self,
        queue: &mut MessageQueue,
        sender: TetraAddress,
        handle: u32,
        link_id: u32,
        endpoint_id: u32,
        pdu: UCallRestore,
    ) {
        let call_id = pdu.call_identifier;

        if let Some(call) = self.individual_calls.get_mut(&call_id) {
            if !call.is_active() || (sender.ssi != call.calling_addr.ssi && sender.ssi != call.called_addr.ssi) {
                self.reject_call_restore(queue, sender, handle, link_id, endpoint_id, call_id);
                return;
            }

            if call.begin_restore().is_err() {
                self.reject_call_restore(queue, sender, handle, link_id, endpoint_id, call_id);
                return;
            }
            call.active_timer_started = Some(self.dltime);
            let grant = if pdu.request_to_transmit_send_data {
                TransmissionGrant::Granted
            } else {
                TransmissionGrant::NotGranted
            };
            call.complete_restore();
            self.send_d_call_restore(queue, sender, handle, link_id, endpoint_id, call_id, grant);
            return;
        }

        if let Some(call) = self.active_calls.get_mut(&call_id) {
            if call.begin_restore().is_err() {
                self.reject_call_restore(queue, sender, handle, link_id, endpoint_id, call_id);
                return;
            }
            let grant = if !pdu.request_to_transmit_send_data {
                TransmissionGrant::NotGranted
            } else if !call.tx_active || call.source_issi == sender.ssi {
                call.grant_floor(sender.ssi, Some(sender));
                TransmissionGrant::Granted
            } else {
                TransmissionGrant::GrantedToOtherUser
            };

            call.complete_restore();
            self.send_d_call_restore(queue, sender, handle, link_id, endpoint_id, call_id, grant);
            return;
        }

        self.reject_call_restore(queue, sender, handle, link_id, endpoint_id, call_id);
    }

    fn send_d_call_restore(
        &self,
        queue: &mut MessageQueue,
        sender: TetraAddress,
        handle: u32,
        link_id: u32,
        endpoint_id: u32,
        call_id: u16,
        grant: TransmissionGrant,
    ) {
        let sdu = Self::build_d_call_restore(call_id, grant, Some(CallStatus::Callcontinue));
        let msg = Self::build_sapmsg_direct(sdu, self.dltime, sender, handle, link_id, endpoint_id);
        queue.push_back(msg);
    }

    fn reject_call_restore(
        &self,
        queue: &mut MessageQueue,
        sender: TetraAddress,
        handle: u32,
        link_id: u32,
        endpoint_id: u32,
        call_id: u16,
    ) {
        tracing::info!("CMCE: rejecting U-CALL RESTORE for unknown or inactive call_id={}", call_id);
        let sdu = Self::build_d_release(call_id, DisconnectCause::CallRestorationOfTheOtherUserFailed);
        let msg = Self::build_sapmsg_direct(sdu, self.dltime, sender, handle, link_id, endpoint_id);
        queue.push_back(msg);
    }
}
