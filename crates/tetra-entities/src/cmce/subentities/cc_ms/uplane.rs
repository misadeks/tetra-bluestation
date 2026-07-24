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
    /// a U-plane-on call is present the frame is forwarded to the external UI
    /// over the telemetry SAP as a [`TelemetryEvent::MsSpeechFrame`].
    ///
    /// The payload is the channel-decoded TCH/S type-1 bit block (ETSI TS 100
    /// 392-2 cl. 19.4): 274 bits carried one-bit-per-byte, i.e. two 137-bit ACELP
    /// speech frames per EN 300 395-2. The stack performs no vocoding — the UI
    /// runs the ACELP decoder (type-1 bits → PCM). `bfi` (bad-frame indicator)
    /// carries the channel-decode CRC result so the UI can apply substitution and
    /// muting on corrupted frames instead of decoding them as valid speech.
    pub fn rx_downlink_traffic(&mut self, ts: u8, bfi: bool, data: &[u8]) {
        // Find the active call currently receiving on the U-plane. M1 assumes a
        // single simultaneous call on the serving carrier; the assigned-timeslot
        // demux (matching `ts` to a specific call) arrives with the
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
        let call_identifier = call.call_identifier;
        let sequence = call.rx_speech_frames;
        let transmitting_party_ssi = call.current_speaker_ssi;
        tracing::info!(
            call = call_identifier,
            frames = sequence,
            bits = data.len(),
            bfi,
            "CC-MS: received downlink speech frame (U-plane)"
        );

        self.emit(TelemetryEvent::MsSpeechFrame {
            call_identifier,
            timeslot: ts,
            sequence,
            transmitting_party_ssi,
            frame_bits: data.len() as u16,
            bad_frame: bfi,
            data: data.to_vec(),
        });
    }

    /// Supply an uplink TCH/S U-plane source frame while this MS holds the floor.
    ///
    /// Symmetric to `rx_downlink_traffic`: CC-MS owns the U-plane in both
    /// directions (ETSI TS 100 392-2 cl. 14.5.1.4). While a call has the
    /// transmission grant to self (`GrantedSelf`) with the U-plane switched on,
    /// CC-MS is the source of the uplink speech stream and pushes frames down to
    /// the MAC (TMD-SAP) transmit path. The MAC (UMAC) is the transmit *timing*
    /// authority (cl. 23): it buffers these frames and clocks exactly one out per
    /// granted uplink traffic slot, so this source rate is deliberately not the
    /// emission gate.
    ///
    /// For now the source is a labelled deterministic silence / comfort-noise
    /// 274-bit TCH/S type-1 frame (all zeros, packed bytes). A real ACELP vocoder
    /// egress (microphone → speech codec) is a follow-up; no vocoder is invented
    /// here.
    pub fn drive_uplink_source(&mut self, queue: &mut MessageQueue) {
        // Single simultaneous call on the serving carrier (as in the M1 receive
        // path). Find the call currently transmitting on the U-plane.
        let Some(call) = self.calls.values_mut().find(|c| {
            c.tx_grant_state == MsTxGrantState::GrantedSelf
                && c.last_uplane.map(|u| u.switch_u_plane).unwrap_or(false)
        }) else {
            return;
        };

        // Labelled deterministic silence frame: 274 zero type-1 bits carried as
        // packed bytes (ceil(274/8) = 35). UMAC clamps to the TCH/S type-1 size.
        // TODO: replace with real ACELP vocoder egress (follow-up).
        const TCH_S_TYPE1_BYTES: usize = 35;
        let data = vec![0u8; TCH_S_TYPE1_BYTES];

        call.tx_speech_frames = call.tx_speech_frames.saturating_add(1);
        let call_identifier = call.call_identifier;

        queue.push_back(SapMsg {
            sap: Sap::TmdSap,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Umac,
            msg: SapMsgInner::TmdCircuitDataReq(TmdCircuitDataReq {
                // Slot authority is UMAC's assigned-timeslot record (cl. 21.5.2),
                // not this field; it is left at 0 as a "don't care".
                ts: 0,
                data,
            }),
        });
        tracing::trace!(
            call = call_identifier,
            "CC-MS: supplied uplink speech source frame (U-plane, silence)"
        );
    }
}
