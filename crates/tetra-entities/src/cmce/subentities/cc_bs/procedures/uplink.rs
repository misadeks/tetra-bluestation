use super::super::dtmf::{DtmfKind, decode_dtmf, pack_type3_bits_to_bytes};
use super::*;

impl CcBsSubentity {
    /// Handle parsed U-SETUP and dispatch into group/individual FSM paths.
    pub(in crate::cmce::subentities::cc_bs) fn fsm_on_u_setup(
        &mut self,
        queue: &mut MessageQueue,
        message: &SapMsg,
        pdu: &USetup,
        calling_party: TetraAddress,
    ) {
        // Check if we can satisfy this request
        if !Self::feature_check_u_setup(pdu) {
            tracing::info!(
                "CMCE: rejecting U-SETUP from ISSI {} due to unsupported critical feature(s)",
                calling_party.ssi
            );
            let SapMsgInner::LcmcMleUnitdataInd(prim) = &message.msg else {
                panic!()
            };
            let reject_call_id = self.circuits.get_next_call_id();
            let sdu = Self::build_d_release(reject_call_id, DisconnectCause::IncompatibleTrafficCase);
            let msg = Self::build_sapmsg_direct(sdu, self.dltime, calling_party, prim.handle, prim.link_id, prim.endpoint_id);
            queue.push_back(msg);
            return;
        }

        // Handle P2P (individual) call setup separately
        if pdu.basic_service_information.communication_type == CommunicationType::P2p {
            self.fsm_on_u_setup_p2p(queue, message, pdu, calling_party);
            return;
        }
        self.fsm_on_u_setup_group(queue, message, pdu, calling_party);
    }

    /// Handle parsed U-TX CEASED.
    pub(in crate::cmce::subentities::cc_bs) fn fsm_on_u_tx_ceased(
        &mut self,
        queue: &mut MessageQueue,
        sender: TetraAddress,
        pdu: UTxCeased,
    ) {
        let call_id = pdu.call_identifier;

        if self.individual_calls.contains_key(&call_id) {
            tracing::debug!("U-TX CEASED for individual call_id={}, ignoring", call_id);
            return;
        }

        if let Err(err) = self.fsm_group_on_tx_ceased(queue, call_id, sender) {
            match err {
                GroupTransitionError::UnknownCall(_) => {
                    tracing::warn!("U-TX CEASED for unknown call_id={}", call_id);
                }
                GroupTransitionError::InvalidTransition { state, .. } => {
                    tracing::debug!(
                        "U-TX CEASED ignored for call_id={} due to invalid transition in state {:?}",
                        call_id,
                        state
                    );
                }
                GroupTransitionError::NotCurrentSpeaker {
                    sender_issi,
                    current_speaker_issi,
                    ..
                } => {
                    tracing::warn!(
                        "U-TX CEASED from non-current speaker ISSI {} on call_id={} (current speaker={}), ignoring",
                        sender_issi,
                        call_id,
                        current_speaker_issi
                    );
                }
                GroupTransitionError::MissingCachedSetup(_) => {
                    tracing::error!("U-TX CEASED call_id={} missing cached D-SETUP", call_id);
                }
            }
        }
    }

    /// Handle parsed U-TX DEMAND.
    pub(in crate::cmce::subentities::cc_bs) fn fsm_on_u_tx_demand(
        &mut self,
        queue: &mut MessageQueue,
        requesting_party: TetraAddress,
        pdu: UTxDemand,
    ) {
        let call_id = pdu.call_identifier;

        if self.individual_calls.contains_key(&call_id) {
            tracing::debug!("U-TX DEMAND for individual call_id={}, ignoring", call_id);
            return;
        }

        tracing::info!("U-TX DEMAND: ISSI {} requests floor on call_id={}", requesting_party.ssi, call_id);
        if let Err(err) = self.fsm_group_on_tx_demand(queue, call_id, requesting_party) {
            match err {
                GroupTransitionError::UnknownCall(_) => {
                    tracing::warn!("U-TX DEMAND for unknown call_id={}", call_id);
                }
                GroupTransitionError::InvalidTransition { state, .. } => {
                    tracing::debug!(
                        "U-TX DEMAND ignored for call_id={} due to invalid transition in state {:?}",
                        call_id,
                        state
                    );
                }
                GroupTransitionError::MissingCachedSetup(_) => {
                    tracing::error!("U-TX DEMAND call_id={} missing cached D-SETUP", call_id);
                }
                GroupTransitionError::NotCurrentSpeaker { .. } => {
                    tracing::debug!("U-TX DEMAND hit unexpected NotCurrentSpeaker transition error call_id={}", call_id);
                }
            }
        }
    }

    /// Handle parsed U-INFO.
    pub(in crate::cmce::subentities::cc_bs) fn fsm_on_u_info(
        &mut self,
        queue: &mut MessageQueue,
        sender: TetraAddress,
        handle: u32,
        link_id: u32,
        endpoint_id: u32,
        pdu: UInfo,
    ) {
        let call_id = pdu.call_identifier;
        let Some(call) = self.individual_calls.get(&call_id).cloned() else {
            tracing::warn!("U-INFO for unknown/non-individual call_id={}, rejecting", call_id);
            let sdu = Self::build_d_release(call_id, DisconnectCause::InvalidCallIdentifier);
            queue.push_back(Self::build_sapmsg_direct(sdu, self.dltime, sender, handle, link_id, endpoint_id));
            return;
        };

        if !call.is_active() {
            tracing::debug!(
                "U-INFO for non-active individual call_id={} state={:?}, ignoring",
                call_id,
                call.state
            );
            return;
        }

        if let Some(call_mut) = self.individual_calls.get_mut(&call_id) {
            call_mut.active_timer_started = Some(self.dltime);
        }

        if pdu.facility.is_some() || pdu.proprietary.is_some() {
            unimplemented_log!(
                "U-INFO facility/proprietary not supported call_id={} facility={} proprietary={}",
                call_id,
                pdu.facility.is_some(),
                pdu.proprietary.is_some()
            );
        }

        if let Some(modify) = pdu.modify {
            self.fsm_on_u_info_modify(queue, sender, &call, call_id, modify);
        }

        let Some(dtmf) = pdu.dtmf.as_ref() else {
            tracing::trace!(
                "U-INFO call_id={} has no DTMF element (modify={:?} facility={} proprietary={})",
                call_id,
                pdu.modify,
                pdu.facility.is_some(),
                pdu.proprietary.is_some()
            );
            return;
        };

        if !call.called_over_brew && !call.calling_over_brew {
            tracing::trace!("U-INFO call_id={} is local individual call; DTMF is not forwarded", call_id);
            return;
        }

        let Some(brew_uuid) = call.brew_uuid else {
            tracing::warn!("U-INFO call_id={} marked Brew-routed but missing brew_uuid", call_id);
            return;
        };

        let decoded = decode_dtmf(dtmf);
        if decoded.full_len_bits > decoded.parsed_bits {
            tracing::warn!(
                "U-INFO call_id={} DTMF payload is {} bits, parser retained only first {} bits",
                call_id,
                decoded.full_len_bits,
                decoded.parsed_bits
            );
        }
        if decoded.malformed {
            tracing::warn!(
                "U-INFO call_id={} has malformed DTMF payload (len={} bits, parsed={} bits, data={:?}, kind={:?})",
                call_id,
                decoded.full_len_bits,
                decoded.parsed_bits,
                dtmf.data,
                decoded.kind
            );
        }

        match decoded.kind {
            DtmfKind::ToneStart | DtmfKind::LegacyDigits => {}
            DtmfKind::ToneEnd => {
                tracing::trace!("U-INFO call_id={} DTMF tone end", call_id);
                return;
            }
            DtmfKind::NotSupported => {
                tracing::info!("U-INFO call_id={} DTMF not supported indication", call_id);
                return;
            }
            DtmfKind::NotSubscribed => {
                tracing::info!("U-INFO call_id={} DTMF not subscribed indication", call_id);
                return;
            }
            DtmfKind::Reserved(v) => {
                tracing::trace!("U-INFO call_id={} DTMF reserved type value {}", call_id, v);
                return;
            }
            DtmfKind::Invalid => {
                tracing::trace!("U-INFO call_id={} invalid/empty DTMF payload, ignoring", call_id);
                return;
            }
        }
        if decoded.digits.is_empty() {
            tracing::trace!("U-INFO call_id={} DTMF has no decoded digits, ignoring", call_id);
            return;
        }

        let (length_bits, data) = pack_type3_bits_to_bytes(dtmf);
        if length_bits == 0 || data.is_empty() {
            tracing::debug!("U-INFO call_id={} has empty DTMF payload, ignoring", call_id);
            return;
        }

        tracing::info!(
            "U-INFO (individual Brew) call_id={} uuid={} dtmf_kind={:?} digits='{}' dtmf_bits={} dtmf_bytes={}",
            call_id,
            brew_uuid,
            decoded.kind,
            decoded.digits,
            length_bits,
            data.len()
        );

        for ch in decoded.digits.chars() {
            let digit = ch as u8;

            queue.push_back(SapMsg {
                sap: Sap::Control,
                src: TetraEntity::Cmce,
                dest: TetraEntity::Brew,
                msg: SapMsgInner::CmceCallControl(CallControl::NetworkCircuitDtmf {
                    brew_uuid,
                    length_bits: 8,
                    data: vec![digit],
                }),
            });
        }
    }

    fn fsm_on_u_info_modify(&mut self, queue: &mut MessageQueue, sender: TetraAddress, call: &IndividualCall, call_id: u16, modify: u64) {
        if call.called_over_brew || call.calling_over_brew {
            unimplemented_log!("U-INFO modify over Brew not supported call_id={} modify=0x{:03x}", call_id, modify);
            return;
        }

        if let Some(call_mut) = self.individual_calls.get_mut(&call_id)
            && call_mut.apply_modify().is_err()
        {
            tracing::debug!("U-INFO modify rejected by formal FSM call_id={}", call_id);
            return;
        }

        let (target_addr, target_ts, target_usage) = if sender.ssi == call.calling_addr.ssi {
            (call.called_addr, call.called_ts, call.called_usage)
        } else if sender.ssi == call.called_addr.ssi {
            (call.calling_addr, call.calling_ts, call.calling_usage)
        } else {
            tracing::warn!(
                "U-INFO modify call_id={} from unexpected ISSI {} (calling {}, called {})",
                call_id,
                sender.ssi,
                call.calling_addr.ssi,
                call.called_addr.ssi
            );
            return;
        };

        tracing::info!(
            "CMCE: forwarding U-INFO modify call_id={} modify=0x{:03x} from ISSI {} to ISSI {}",
            call_id,
            modify,
            sender.ssi,
            target_addr.ssi
        );
        let sdu = Self::build_d_info(call_id, Some(modify), Some(CallStatus::Callcontinue), true);
        let msg = Self::build_sapmsg_stealing(sdu, self.dltime, target_addr, target_ts, Some(target_usage));
        queue.push_back(msg);
    }

    /// Handle parsed U-RELEASE.
    pub(in crate::cmce::subentities::cc_bs) fn fsm_on_u_release(
        &mut self,
        queue: &mut MessageQueue,
        sender: TetraAddress,
        handle: u32,
        link_id: u32,
        endpoint_id: u32,
        pdu: URelease,
    ) {
        let call_id = pdu.call_identifier;
        let disconnect_cause = pdu.disconnect_cause;

        tracing::info!("U-RELEASE: call_id={} cause={}", call_id, disconnect_cause);
        if self.individual_calls.contains_key(&call_id) {
            tracing::info!("U-RELEASE (individual) call_id={} cause={}", call_id, disconnect_cause);
            self.release_individual_call(queue, call_id, disconnect_cause);
        } else if self.active_calls.contains_key(&call_id) || self.cached_setups.contains_key(&call_id) {
            self.release_group_call(queue, call_id, disconnect_cause);
        } else {
            tracing::debug!(
                "U-RELEASE for unknown call_id={} (likely duplicate), completing idempotently",
                call_id
            );
            let sdu = Self::build_d_release(call_id, disconnect_cause);
            queue.push_back(Self::build_sapmsg_direct(sdu, self.dltime, sender, handle, link_id, endpoint_id));
        }
    }

    /// Handle parsed U-DISCONNECT.
    pub(in crate::cmce::subentities::cc_bs) fn fsm_on_u_disconnect(
        &mut self,
        queue: &mut MessageQueue,
        sender: TetraAddress,
        ul_handle: u32,
        ul_link_id: u32,
        ul_endpoint_id: u32,
        pdu: UDisconnect,
    ) {
        let call_id = pdu.call_identifier;
        let disconnect_cause = pdu.disconnect_cause;

        if self.individual_calls.contains_key(&call_id) {
            tracing::info!("U-DISCONNECT (individual) call_id={} cause={}", call_id, disconnect_cause);
            if let Some(call) = self.individual_calls.get_mut(&call_id) {
                call.begin_disconnect();
            }
            self.release_individual_call_from_u_disconnect(queue, call_id, disconnect_cause, sender.ssi);
            return;
        }

        let Some(call) = self.active_calls.get(&call_id) else {
            tracing::debug!(
                "U-DISCONNECT for unknown call_id={} (likely duplicate), completing idempotently",
                call_id
            );
            let sdu = Self::build_d_release(call_id, disconnect_cause);
            queue.push_back(Self::build_sapmsg_direct(
                sdu,
                self.dltime,
                sender,
                ul_handle,
                ul_link_id,
                ul_endpoint_id,
            ));
            return;
        };

        let is_call_owner = matches!(&call.origin, CallOrigin::Local { caller_addr } if caller_addr.ssi == sender.ssi);

        if is_call_owner {
            tracing::info!("U-DISCONNECT: call owner ISSI {} disconnecting call_id={}", sender.ssi, call_id);
            if let Some(call) = self.active_calls.get_mut(&call_id) {
                call.begin_disconnect();
            }
            self.release_group_call(queue, call_id, DisconnectCause::UserRequestedDisconnection);
            return;
        }

        tracing::info!(
            "U-DISCONNECT: non-call-owner ISSI {} rejected for call_id={} cause={}",
            sender.ssi,
            call_id,
            disconnect_cause
        );

        let d_release = DRelease {
            call_identifier: call_id,
            disconnect_cause: DisconnectCause::RequestedServiceNotAvailable,
            notification_indicator: None,
            facility: None,
            proprietary: None,
        };
        tracing::info!("-> {:?} (to ISSI {})", d_release, sender.ssi);

        let mut sdu = BitBuffer::new_autoexpand(32);
        d_release.to_bitbuf(&mut sdu).expect("Failed to serialize DRelease");
        sdu.seek(0);

        let sender_addr = TetraAddress::new(sender.ssi, SsiType::Issi);
        let msg = SapMsg {
            sap: Sap::LcmcSap,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LcmcMleUnitdataReq(LcmcMleUnitdataReq {
                sdu,
                handle: ul_handle,
                endpoint_id: ul_endpoint_id,
                link_id: ul_link_id,
                layer2service: Layer2Service::Todo,
                pdu_prio: 0,
                layer2_qos: 0,
                stealing_permission: false,
                stealing_repeats_flag: false,
                chan_alloc: None,
                main_address: sender_addr,
                tx_reporter: None,
            }),
        };
        queue.push_back(msg);
    }
}
