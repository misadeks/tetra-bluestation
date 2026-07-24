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
    pub fn rx_downlink_traffic(&mut self, ts: u8, bfi: bool, usage_marker: Option<u8>, owner_ssi: Option<u32>, data: &[u8]) {
        // Demultiplex the frame to the call it belongs to. The MAC tags each
        // frame with the destination SSI the serving cell bound the traffic's
        // usage marker to (cl. 23.5.5); attribute it to the call addressed to
        // that party whose U-plane is switched on (cl. 14.5.1.4). When the MAC
        // could not resolve an owner (`owner_ssi` is `None` — e.g. an individual
        // call before the usage-marker binding is observed), fall back to the
        // sole U-plane-on call, preserving the single-call receive behaviour.
        let call = match owner_ssi {
            Some(ssi) => self.calls.values_mut().find(|c| {
                c.route.main_address.ssi == ssi && c.last_uplane.map(|u| u.switch_u_plane).unwrap_or(false)
            }),
            None => self
                .calls
                .values_mut()
                .find(|c| c.last_uplane.map(|u| u.switch_u_plane).unwrap_or(false)),
        };
        let Some(call) = call else {
            tracing::trace!(
                "rx_downlink_traffic: no U-plane-on call for owner {:?} (marker {:?}), dropping speech frame",
                owner_ssi,
                usage_marker
            );
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
            marker = usage_marker,
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

    /// Accept one uplink TCH/S U-plane speech block from the external UI while
    /// this MS holds the floor for the call.
    ///
    /// Symmetric intake to `rx_downlink_traffic`'s egress: CC-MS owns the
    /// U-plane in both directions (ETSI TS 100 392-2 cl. 14.5.1.4). The UI runs
    /// the ACELP vocoder (microphone → speech codec, EN 300 395-2) and delivers
    /// each 274-bit type-1 block (cl. 19.4) here, `frame_bits` bits carried
    /// one-bit-per-byte, matching the downlink `MsSpeechFrame` layout. The frame
    /// is packed to the MAC transmit byte layout and queued; `drive_uplink_source`
    /// clocks it down to the MAC, which is the transmit-timing authority (cl. 23).
    ///
    /// A frame is accepted only for a call whose transmission grant is to self
    /// (`GrantedSelf`) with the U-plane switched on — i.e. the MS actually holds
    /// the floor. Frames arriving otherwise (no grant, floor held by another
    /// party, unknown call) are discarded: the MS must not transmit traffic it
    /// has no grant for. No vocoder is synthesised in the stack.
    pub fn push_uplink_speech(&mut self, call_identifier: u16, frame_bits: u16, data: &[u8]) {
        let Some(call) = self.calls.get_mut(&call_identifier) else {
            tracing::trace!(call = call_identifier, "push_uplink_speech: unknown call, dropping frame");
            return;
        };
        let holds_floor = call.tx_grant_state == MsTxGrantState::GrantedSelf
            && call.last_uplane.map(|u| u.switch_u_plane).unwrap_or(false);
        if !holds_floor {
            tracing::trace!(
                call = call_identifier,
                grant = ?call.tx_grant_state,
                "push_uplink_speech: MS does not hold the floor, dropping uplink speech frame"
            );
            return;
        }

        let Some(packed) = pack_tch_s_type1(frame_bits, data) else {
            tracing::warn!(
                call = call_identifier,
                frame_bits,
                got = data.len(),
                "push_uplink_speech: malformed TCH/S type-1 block, dropping"
            );
            return;
        };

        // Bounded drop-oldest: cap transmit latency if the UI supplies frames
        // faster than the MAC clocks them out. This is overflow protection, not a
        // jitter buffer (the UI↔stack link is local, cl. 23 timing owns pacing).
        if call.uplink_source_frames.len() >= UPLINK_SOURCE_MAX_FRAMES {
            call.uplink_source_frames.pop_front();
        }
        call.uplink_source_frames.push_back(packed);
    }

    /// Clock one queued uplink U-plane speech frame down to the MAC per tick.
    ///
    /// While a call holds the floor (`GrantedSelf`, U-plane on), forward the next
    /// UI-supplied TCH/S block (queued by `push_uplink_speech`) to the MAC over
    /// the TMD-SAP. The MAC is the transmit-timing authority (cl. 23): it buffers
    /// the frame and emits exactly one per granted uplink traffic slot, filling
    /// silence on underrun — so when the UI has supplied nothing this tick, CC-MS
    /// pushes nothing and the MAC transmits comfort silence on the granted slot.
    pub fn drive_uplink_source(&mut self, queue: &mut MessageQueue) {
        let Some(call) = self.calls.values_mut().find(|c| {
            c.tx_grant_state == MsTxGrantState::GrantedSelf
                && c.last_uplane.map(|u| u.switch_u_plane).unwrap_or(false)
        }) else {
            return;
        };

        let Some(data) = call.uplink_source_frames.pop_front() else {
            return;
        };

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
            "CC-MS: supplied uplink speech source frame (U-plane) to the MAC"
        );
    }
}

/// Bound on the per-call uplink U-plane source FIFO (`uplink_source_frames`).
/// Overflow protection only; the MAC owns transmit timing (cl. 23).
pub(super) const UPLINK_SOURCE_MAX_FRAMES: usize = 4;

/// Number of channel-coded TCH/S type-1 bits per speech block (ETSI TS 100
/// 392-2 cl. 19.4): two 137-bit ACELP frames (EN 300 395-2).
const TCH_S_TYPE1_BITS: usize = 274;

/// Pack a UI-supplied TCH/S type-1 speech block into the MAC transmit byte
/// layout (`ceil(274/8) = 35` bytes, MSB-first, last byte's low 2 bits padding).
///
/// The UI delivers `frame_bits` bits one-bit-per-byte (mirroring the downlink
/// `MsSpeechFrame`); this accepts that unpacked form (>= 274 bytes) and, for
/// robustness, an already-packed 35-byte block. Anything else is rejected.
fn pack_tch_s_type1(frame_bits: u16, data: &[u8]) -> Option<Vec<u8>> {
    const PACKED_BYTES: usize = (TCH_S_TYPE1_BITS + 7) / 8;

    // Already packed — pass through.
    if data.len() == PACKED_BYTES {
        return Some(data.to_vec());
    }
    // Unpacked one-bit-per-byte: need at least a full type-1 block.
    if (frame_bits as usize) < TCH_S_TYPE1_BITS || data.len() < TCH_S_TYPE1_BITS {
        return None;
    }
    let mut out = vec![0u8; PACKED_BYTES];
    for bit_idx in 0..TCH_S_TYPE1_BITS {
        if data[bit_idx] & 1 != 0 {
            out[bit_idx / 8] |= 1 << (7 - (bit_idx % 8));
        }
    }
    Some(out)
}
