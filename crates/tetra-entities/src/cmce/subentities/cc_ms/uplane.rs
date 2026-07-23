use super::*;

impl CcMsSubentity {
    pub(super) fn apply_transmission_grant(
        &mut self,
        queue: &mut MessageQueue,
        call_identifier: u16,
        grant: TransmissionGrant,
        speaker: Option<u32>,
    ) {
        let Some(call) = self.calls.get_mut(&call_identifier) else { return };
        call.current_speaker_ssi = speaker.or(call.current_speaker_ssi);
        let mut configure = None;
        match grant {
            TransmissionGrant::Granted => {
                call.tx_grant_state = MsTxGrantState::GrantedSelf;
                call.pending_tx_request = false;
                configure = Some((true, true, call.simplex_duplex_selection));
            }
            TransmissionGrant::NotGranted => {
                call.tx_grant_state = MsTxGrantState::None;
                call.pending_tx_request = false;
            }
            TransmissionGrant::RequestQueued => {
                call.tx_grant_state = MsTxGrantState::RequestQueued;
                call.pending_tx_request = true;
            }
            TransmissionGrant::GrantedToOtherUser => {
                call.tx_grant_state = MsTxGrantState::GrantedOther;
                configure = Some((true, false, call.simplex_duplex_selection));
            }
        }
        let _ = call;
        if let Some((switch_u_plane, tx_grant, simplex_duplex)) = configure {
            self.configure_uplane(queue, call_identifier, switch_u_plane, tx_grant, simplex_duplex);
        }
    }

    pub(super) fn configure_uplane(
        &mut self,
        queue: &mut MessageQueue,
        call_identifier: u16,
        switch_u_plane: bool,
        tx_grant: bool,
        simplex_duplex: bool,
    ) {
        let Some(call) = self.calls.get_mut(&call_identifier) else { return };
        let state = MsUPlaneState {
            switch_u_plane,
            tx_grant,
            simplex_duplex,
        };
        call.last_uplane = Some(state);
        queue.push_back(SapMsg {
            sap: Sap::LcmcSap,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LcmcMleConfigureReq(LcmcMleConfigureReq {
                endpoint_id: call.route.endpoint_id,
                chan_change_accepted: None,
                chan_change_handle: 0 as Todo,
                call_release: if switch_u_plane { None } else { Some(0 as Todo) },
                encryption_flag: call.basic_service.encryption_flag,
                circuit_mode_type: call.basic_service.circuit_mode_type,
                add_temp_gssi: None,
                del_temp_gssi: None,
                simplex_duplex,
                tx_grant,
                switch_u_plane,
            }),
        });
    }

    /// Deliver a decoded downlink TCH/S speech frame into the U-plane.
    ///
    /// The lower MAC (LMAC → UMAC) forwards decoded traffic frames tagged with
    /// the timeslot they arrived on. Per ETSI TS 100 392-2 cl. 14.5.1.4, received
    /// U-plane traffic is only meaningful while the CC has switched the U-plane
    /// on for an active call (call present / receiving speech), so a frame that
    /// arrives while every call has the U-plane switched off is discarded. When
    /// a U-plane-on call is present the frame is accepted into that call's
    /// receive path — the minimal audio egress until a vocoder/audio sink is
    /// wired.
    pub fn rx_downlink_traffic(&mut self, _ts: u8, data: &[u8]) {
        // Find the active call currently receiving on the U-plane. M1 assumes a
        // single simultaneous call on the serving carrier; the assigned-timeslot
        // demux (matching `_ts` to a specific call) arrives with the
        // channel-allocation handling in a later milestone.
        let Some(call) = self
            .calls
            .values_mut()
            .find(|c| c.last_uplane.map(|u| u.switch_u_plane).unwrap_or(false))
        else {
            tracing::trace!("rx_downlink_traffic: no call with U-plane switched on, dropping speech frame");
            return;
        };

        call.rx_speech_frames = call.rx_speech_frames.saturating_add(1);
        tracing::info!(
            call = call.call_identifier,
            frames = call.rx_speech_frames,
            bits = data.len(),
            "CC-MS: received downlink speech frame (U-plane)"
        );
    }
}
