use std::collections::HashMap;

use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, EndpointId, Layer2Service, LinkId, MleHandle, Sap, SsiType, TdmaTime, TetraAddress, Todo};
use tetra_core::typed_pdu_fields::Type3FieldGeneric;
use tetra_pdus::cmce::{
    enums::{
        call_timeout::CallTimeout, call_timeout_setup_phase::CallTimeoutSetupPhase, cmce_pdu_type_dl::CmcePduTypeDl,
        disconnect_cause::DisconnectCause, party_type_identifier::PartyTypeIdentifier, transmission_grant::TransmissionGrant,
    },
    fields::{basic_service_information::BasicServiceInformation, dtmf, external_subscriber_number},
    pdus::{
        d_alert::DAlert, d_call_proceeding::DCallProceeding, d_call_restore::DCallRestore, d_connect::DConnect,
        d_connect_acknowledge::DConnectAcknowledge, d_disconnect::DDisconnect, d_info::DInfo, d_release::DRelease, d_setup::DSetup,
        d_tx_ceased::DTxCeased, d_tx_continue::DTxContinue, d_tx_granted::DTxGranted, d_tx_interrupt::DTxInterrupt, d_tx_wait::DTxWait,
        u_alert::UAlert, u_call_restore::UCallRestore, u_connect::UConnect, u_disconnect::UDisconnect, u_info::UInfo,
        u_release::URelease, u_setup::USetup, u_tx_ceased::UTxCeased, u_tx_demand::UTxDemand,
    },
};
use tetra_saps::{
    SapMsg, SapMsgInner,
    control::enums::{circuit_mode_type::CircuitModeType, communication_type::CommunicationType},
    lcmc::{LcmcMleConfigureReq, LcmcMleUnitdataReq},
    tmd::TmdCircuitDataReq,
    tncc,
};

use crate::{
    MessageQueue,
    net_telemetry::{TelemetryEvent, channel::TelemetrySink},
};

mod lifecycle;
mod pdu;
mod procedures;
mod routes;
mod state;
mod timers;
mod uplane;

// Re-export the CC-MS shared vocabulary so the `use super::*;` chain in every
// submodule (mirroring the cc_bs layout) resolves these names unqualified, and
// so `CcMsSubentity`'s public API keeps the same type paths as the monolith.
pub use state::{MsCall, MsCallKind, MsCallTimers, MsCcState, MsTxGrantState, MsUPlaneState};
use pdu::{
    default_speech_basic_service, kind_from_basic_service, pdu_basic_from_tncc, pdu_disconnect_cause_from_tncc, tncc_basic_from_pdu,
    tncc_call_status, tncc_call_status_raw, tncc_call_timeout, tncc_disconnect_cause, tncc_dtmf_indication_from_ie, tncc_setup_timeout,
    tncc_transmission_grant, tncc_transmission_status_from_grant,
};
use state::{CallRoute, PendingOrigination};
#[cfg(test)]
use timers::{call_timeout_to_timeslots, seconds_to_timeslots, setup_timeout_to_timeslots};

/// Clause 14 Call Control CMCE sub-entity, MS side.
pub struct CcMsSubentity {
    own_issi: Option<u32>,
    dltime: TdmaTime,
    calls: HashMap<u16, MsCall>,
    pending_originations: Vec<PendingOrigination>,
    telemetry: Option<TelemetrySink>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> CallRoute {
        CallRoute {
            main_address: TetraAddress::new(91, SsiType::Gssi),
            handle: 1,
            endpoint_id: 2,
            link_id: 3,
        }
    }

    #[test]
    fn t310_values_follow_clause_14_8_16() {
        assert_eq!(call_timeout_to_timeslots(CallTimeout::Infinite), None);
        assert_eq!(call_timeout_to_timeslots(CallTimeout::T30s), Some(seconds_to_timeslots(30)));
        assert_eq!(call_timeout_to_timeslots(CallTimeout::T5m), Some(seconds_to_timeslots(300)));
        assert_eq!(call_timeout_to_timeslots(CallTimeout::Reserved), None);
    }

    #[test]
    fn setup_predefined_is_not_invented() {
        assert_eq!(setup_timeout_to_timeslots(CallTimeoutSetupPhase::Predefined), None);
        assert_eq!(
            setup_timeout_to_timeslots(CallTimeoutSetupPhase::T1s),
            Some(seconds_to_timeslots(1))
        );
    }

    #[test]
    fn mo_group_setup_addresses_swmi_with_own_issi() {
        // Regression: an MO group-call U-SETUP must travel on the individual,
        // acknowledged basic link keyed on the MS's own ISSI. Addressing it with
        // the called GSSI forces the LLC onto BL-UDATA and the SwMI drops it.
        let mut cc = CcMsSubentity::new(None);
        cc.own_issi = Some(1234567);
        let mut q = MessageQueue::new();
        cc.originate_group_call(&mut q, 220, default_speech_basic_service(), true);
        let msg = q.pop_front().expect("U-SETUP should be queued");
        let SapMsgInner::LcmcMleUnitdataReq(prim) = msg.msg else {
            panic!("expected LcmcMleUnitdataReq");
        };
        assert_eq!(prim.main_address.ssi, 1234567);
        assert!(matches!(prim.main_address.ssi_type, SsiType::Issi));
    }

    #[test]
    fn mo_external_call_encloses_gateway_issi_and_dialled_digits() {
        // PABX/PSTN gateway call (cl. 14.5.6.2 / 14.8.20): an individual U-SETUP
        // addressed to the gateway ISSI (CPTI = SSI) that ALSO carries the dialled
        // digits in the External subscriber number IE. Decode the emitted PDU with
        // the same parser the BS uses to prove it is on-air valid.
        let mut cc = CcMsSubentity::new(None);
        cc.own_issi = Some(1234567);
        let mut q = MessageQueue::new();
        cc.originate_external_call(&mut q, 8000000, "0912345678", default_speech_basic_service(), true, true)
            .expect("valid dial string");
        let msg = q.pop_front().expect("U-SETUP should be queued");
        let SapMsgInner::LcmcMleUnitdataReq(mut prim) = msg.msg else {
            panic!("expected LcmcMleUnitdataReq");
        };
        // Layer-2 is still keyed on the MS's own ISSI (acknowledged basic link).
        assert_eq!(prim.main_address.ssi, 1234567);
        let pdu = USetup::from_bitbuf(&mut prim.sdu).expect("valid U-SETUP");
        assert_eq!(pdu.called_party_type_identifier, PartyTypeIdentifier::Ssi);
        assert_eq!(pdu.called_party_ssi, Some(8000000), "called party is the gateway ISSI");
        let esn = pdu.external_subscriber_number.expect("external subscriber number IE present");
        assert_eq!(external_subscriber_number::decode(&esn).as_deref(), Some("0912345678"));
    }

    #[test]
    fn mo_external_call_rejects_invalid_dial_string() {
        let mut cc = CcMsSubentity::new(None);
        cc.own_issi = Some(1234567);
        let mut q = MessageQueue::new();
        assert!(
            cc.originate_external_call(&mut q, 8000000, "12AB", default_speech_basic_service(), true, true)
                .is_err(),
            "an unencodable number must be refused"
        );
        assert!(q.pop_front().is_none(), "no U-SETUP is emitted for an invalid number");
    }

    fn active_individual_call(cc: &mut CcMsSubentity, cid: u16) {
        cc.calls.insert(
            cid,
            MsCall::new(
                cid,
                MsCcState::CallActive,
                MsCallKind::Individual,
                default_speech_basic_service(),
                true,
                route(),
                true,
            ),
        );
    }

    #[test]
    fn dtmf_tone_start_emits_u_info_decodable_by_bs_parser() {
        // In-call DTMF (cl. 14.7.2.6 / 14.8.19): a tone-start U-INFO carrying the
        // dialled digits. Decode the emitted PDU with the BS-side parser to prove
        // it is on-air valid.
        let mut cc = CcMsSubentity::new(None);
        cc.own_issi = Some(1234567);
        let mut q = MessageQueue::new();
        active_individual_call(&mut cc, 7);
        let req = tncc::TnccDtmfRequest {
            access_priority: None,
            dtmf_tone_delimiter: tncc::DtmfToneDelimiter::Dtmf,
            number_of_dtmf_digits: Some(4),
            dtmf_digits: Some(vec![
                tncc::DtmfDigit::Digit1,
                tncc::DtmfDigit::Digit2,
                tncc::DtmfDigit::DigitStar,
                tncc::DtmfDigit::DigitHash,
            ]),
            traffic_stealing: None,
        };
        cc.handle_tncc_dtmf(&mut q, 7, &req).expect("DTMF accepted on an active individual call");
        let msg = q.pop_front().expect("U-INFO should be queued");
        let SapMsgInner::LcmcMleUnitdataReq(mut prim) = msg.msg else {
            panic!("expected LcmcMleUnitdataReq");
        };
        assert_eq!(prim.main_address.ssi, 1234567, "keyed on own ISSI");
        let pdu = UInfo::from_bitbuf(&mut prim.sdu).expect("valid U-INFO");
        assert_eq!(pdu.call_identifier, 7);
        let ie = pdu.dtmf.expect("DTMF IE present");
        let decoded = dtmf::decode(&ie).expect("DTMF IE decodes");
        assert_eq!(decoded.dtmf_type, dtmf::DTMF_TYPE_TONE_START);
        let digits: String = decoded.nibbles.iter().map(|n| dtmf::code_digit(*n).unwrap()).collect();
        assert_eq!(digits, "12*#");
    }

    #[test]
    fn dtmf_tone_end_emits_u_info_without_digits() {
        let mut cc = CcMsSubentity::new(None);
        cc.own_issi = Some(1234567);
        let mut q = MessageQueue::new();
        active_individual_call(&mut cc, 7);
        let req = tncc::TnccDtmfRequest {
            access_priority: None,
            dtmf_tone_delimiter: tncc::DtmfToneDelimiter::ToneEnd,
            number_of_dtmf_digits: None,
            dtmf_digits: None,
            traffic_stealing: None,
        };
        cc.handle_tncc_dtmf(&mut q, 7, &req).expect("tone-end accepted");
        let SapMsgInner::LcmcMleUnitdataReq(mut prim) = q.pop_front().expect("U-INFO queued").msg else {
            panic!("expected LcmcMleUnitdataReq");
        };
        let pdu = UInfo::from_bitbuf(&mut prim.sdu).expect("valid U-INFO");
        let decoded = dtmf::decode(&pdu.dtmf.expect("DTMF IE present")).expect("decodes");
        assert_eq!(decoded.dtmf_type, dtmf::DTMF_TYPE_TONE_END);
        assert!(decoded.nibbles.is_empty());
    }

    #[test]
    fn dtmf_rejected_for_group_call_and_unknown_call() {
        let mut cc = CcMsSubentity::new(None);
        cc.own_issi = Some(1234567);
        let mut q = MessageQueue::new();
        let tone_start = tncc::TnccDtmfRequest {
            access_priority: None,
            dtmf_tone_delimiter: tncc::DtmfToneDelimiter::Dtmf,
            number_of_dtmf_digits: Some(1),
            dtmf_digits: Some(vec![tncc::DtmfDigit::Digit1]),
            traffic_stealing: None,
        };
        // Unknown call.
        assert!(cc.handle_tncc_dtmf(&mut q, 99, &tone_start).is_err());
        // Group call.
        cc.calls.insert(
            8,
            MsCall::new(8, MsCcState::CallActive, MsCallKind::Group, default_speech_basic_service(), false, route(), true),
        );
        assert!(cc.handle_tncc_dtmf(&mut q, 8, &tone_start).is_err(), "DTMF not valid on a group call");
        assert!(q.pop_front().is_none(), "no U-INFO emitted on rejection");
    }

    /// Inbound DTMF (cl. 14.8.19 / Table 14.58): a downlink D-INFO carrying a
    /// DTMF type-3 element is surfaced to the TN as a TNCC-DTMF indication
    /// (Table 11.3). Tone-start digits, tone-end, and the "not supported"
    /// result are all forwarded; a reserved type is dropped.
    #[test]
    fn rx_d_info_dtmf_surfaces_tncc_dtmf_indication() {
        let (sink, source) = telemetry_channel();
        let mut cc = CcMsSubentity::new(Some(sink));
        cc.own_issi = Some(1234567);
        let mut q = MessageQueue::new();
        active_individual_call(&mut cc, 7);
        while source.try_recv().is_some() {}

        let d_info_with_dtmf = |dtmf: Option<Type3FieldGeneric>| DInfo {
            call_identifier: 7,
            reset_call_time_out_timer_t310_: false,
            poll_request: false,
            new_call_identifier: None,
            call_time_out: None,
            call_time_out_set_up_phase_t301_t302_: None,
            call_ownership: None,
            modify: None,
            call_status: None,
            temporary_address: None,
            notification_indicator: None,
            poll_response_percentage: None,
            poll_response_number: None,
            dtmf,
            facility: None,
            poll_response_addresses: None,
            proprietary: None,
        };
        let drain_dtmf = |source: &crate::net_telemetry::TelemetrySource| -> Option<tncc::TnccDtmfIndication> {
            let mut out = None;
            while let Some(ev) = source.try_recv() {
                if let TelemetryEvent::TnccDtmfIndication { call_identifier, indication } = ev {
                    assert_eq!(call_identifier, 7);
                    out = Some(indication);
                }
            }
            out
        };

        // Tone-start with digits "12*#".
        cc.rx_d_info(&mut q, d_info_with_dtmf(dtmf::encode_tone_start_digits("12*#")), route());
        let ind = drain_dtmf(&source).expect("tone-start DTMF surfaced");
        assert_eq!(ind.dtmf_tone_delimiter, Some(tncc::DtmfToneDelimiter::Dtmf));
        assert_eq!(ind.number_of_dtmf_digits, Some(4));
        assert_eq!(
            ind.dtmf_digits,
            Some(vec![tncc::DtmfDigit::Digit1, tncc::DtmfDigit::Digit2, tncc::DtmfDigit::DigitStar, tncc::DtmfDigit::DigitHash])
        );

        // Tone-end.
        cc.rx_d_info(&mut q, d_info_with_dtmf(Some(dtmf::encode_tone_end())), route());
        let ind = drain_dtmf(&source).expect("tone-end DTMF surfaced");
        assert_eq!(ind.dtmf_tone_delimiter, Some(tncc::DtmfToneDelimiter::ToneEnd));
        assert_eq!(ind.dtmf_digits, None);

        // "DTMF not supported" result (type 010).
        let not_supported = Type3FieldGeneric {
            field_id: tetra_pdus::cmce::enums::type3_elem_id::CmceType3ElemId::Dtmf.into_raw(),
            len: 3,
            data: vec![dtmf::DTMF_TYPE_NOT_SUPPORTED << 5],
        };
        cc.rx_d_info(&mut q, d_info_with_dtmf(Some(not_supported)), route());
        let ind = drain_dtmf(&source).expect("not-supported DTMF surfaced");
        assert_eq!(ind.dtmf_tone_delimiter, None);
        assert_eq!(ind.dtmf_result, Some(tncc::DtmfResult::DtmfNotSupported));

        // A D-INFO without any DTMF element raises no TNCC-DTMF indication.
        cc.rx_d_info(&mut q, d_info_with_dtmf(None), route());
        assert!(drain_dtmf(&source).is_none(), "no DTMF element => no TNCC-DTMF indication");
    }

    #[test]
    fn grant_self_switches_uplane_tx() {
        let mut cc = CcMsSubentity::new(None);
        let mut q = MessageQueue::new();
        cc.calls.insert(
            7,
            MsCall::new(
                7,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false,
                route(),
                true,
            ),
        );
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::Granted, None);
        let call = cc.call(7).unwrap();
        assert_eq!(call.tx_grant_state, MsTxGrantState::GrantedSelf);
        assert_eq!(
            call.last_uplane,
            Some(MsUPlaneState {
                switch_u_plane: true,
                tx_grant: true,
                simplex_duplex: false
            })
        );
        assert!(matches!(q.pop_front().unwrap().msg, SapMsgInner::LcmcMleConfigureReq(_)));
    }

    /// Regression (cl. 14.5.1.4 / 14.8.31): in a simplex group call the SwMI
    /// announces a *remote* talker with the "transmission granted" code (0) while
    /// naming that talker as the transmitting party. Taken literally this MS would
    /// enter `GrantedSelf` (switch its U-plane to transmit) and the simplex
    /// self-echo suppression would then drop the remote talker's downlink speech —
    /// the whole first talk spurt of the call would be silent. Resolving the grant
    /// to this MS's viewpoint must yield `GrantedOther` so the talker's speech is
    /// forwarded to the UI.
    #[test]
    fn remote_talker_granted_zero_resolves_to_other_and_keeps_audio() {
        let mut cc = CcMsSubentity::new(None);
        cc.own_issi = Some(1234567);
        let mut q = MessageQueue::new();
        cc.calls.insert(
            7,
            MsCall::new(
                7,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false, // simplex
                route(),
                true,
            ),
        );
        // A "granted" (0) naming another subscriber as the transmitting party.
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::Granted, Some(2200699));
        let call = cc.call(7).unwrap();
        assert_eq!(
            call.tx_grant_state,
            MsTxGrantState::GrantedOther,
            "a grant naming another subscriber must not put us in GrantedSelf"
        );
        assert_eq!(
            call.last_uplane,
            Some(MsUPlaneState { switch_u_plane: true, tx_grant: false, simplex_duplex: false }),
            "receiving the remote talker: U-plane on, transmit off"
        );
        // The remote talker's downlink speech must NOT be suppressed.
        cc.rx_downlink_traffic(1, false, Some(1), Some(91), &vec![0u8; 274]);
        assert_eq!(
            cc.call(7).unwrap().rx_speech_frames,
            1,
            "remote talker's speech must be forwarded, not suppressed as self-echo"
        );
    }

    /// The dual of the above: when WE are the transmitting party (grant 0 naming
    /// our own ISSI), the floor is genuinely ours (`GrantedSelf`) and the simplex
    /// talk-back must still be suppressed so the operator does not hear their own
    /// voice echoed back (cl. 14.5.1.4).
    #[test]
    fn self_talker_granted_zero_still_suppresses_downlink_echo() {
        let mut cc = CcMsSubentity::new(None);
        cc.own_issi = Some(1234567);
        let mut q = MessageQueue::new();
        cc.calls.insert(
            7,
            MsCall::new(
                7,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false, // simplex
                route(),
                true,
            ),
        );
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::Granted, Some(1234567));
        assert_eq!(cc.call(7).unwrap().tx_grant_state, MsTxGrantState::GrantedSelf);
        cc.rx_downlink_traffic(1, false, Some(1), Some(91), &vec![0u8; 274]);
        assert_eq!(
            cc.call(7).unwrap().rx_speech_frames,
            0,
            "our own simplex talk-back must stay suppressed"
        );
    }

    /// `resolve_floor_grant` only rewrites a "granted" that names a *different*
    /// subscriber; genuine self-grants (own ISSI or no named party) and explicit
    /// grants-to-other are passed through unchanged.
    #[test]
    fn resolve_floor_grant_only_rewrites_foreign_granted() {
        let mut cc = CcMsSubentity::new(None);
        cc.own_issi = Some(1234567);
        assert_eq!(
            cc.resolve_floor_grant(TransmissionGrant::Granted, Some(2200699)),
            TransmissionGrant::GrantedToOtherUser
        );
        assert_eq!(
            cc.resolve_floor_grant(TransmissionGrant::Granted, Some(1234567)),
            TransmissionGrant::Granted
        );
        assert_eq!(
            cc.resolve_floor_grant(TransmissionGrant::Granted, None),
            TransmissionGrant::Granted
        );
        assert_eq!(
            cc.resolve_floor_grant(TransmissionGrant::GrantedToOtherUser, Some(2200699)),
            TransmissionGrant::GrantedToOtherUser
        );
    }

    /// M4b (cl. 14.5.2 / 14.5.1.4): once the call is on a traffic channel (the
    /// U-plane has been switched on), floor-control PDUs are stolen from the
    /// assigned TCH half-slot (FACCH) and sent as acknowledged BL-DATA on the
    /// TCH-associated basic link (link_id 2), not on the setup control link.
    #[test]
    fn floor_signalling_steals_tch_once_on_traffic_channel() {
        let mut cc = CcMsSubentity::new(None);
        let mut q = MessageQueue::new();
        cc.calls.insert(
            7,
            MsCall::new(
                7,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false,
                route(), // control link_id 3
                true,
            ),
        );
        // Grant self → U-plane switched on: the call is now on the traffic channel.
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::Granted, None);
        while q.pop_front().is_some() {}

        // U-TX-CEASED while on the TCH must be stolen on the TCH-associated link.
        assert!(cc.cease_tx(&mut q, 7));
        let prim = loop {
            match q.pop_front().expect("cease should emit signalling").msg {
                SapMsgInner::LcmcMleUnitdataReq(p) => break p,
                _ => continue,
            }
        };
        assert!(prim.stealing_permission, "cease on TCH must steal the half-slot");
        assert!(prim.stealing_repeats_flag);
        assert_eq!(prim.link_id, 2, "must use the TCH-associated basic link");
    }

    /// U-TX-DEMAND raised while listening on the traffic channel is likewise
    /// stolen from the TCH (the MS is not the current talker but owns the U-plane
    /// receive path, cl. 14.5.1.4), on the TCH-associated basic link.
    #[test]
    fn tx_demand_steals_tch_while_listening() {
        let mut cc = CcMsSubentity::new(None);
        let mut q = MessageQueue::new();
        cc.calls.insert(
            7,
            MsCall::new(
                7,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false,
                route(),
                true,
            ),
        );
        // Grant to another user → U-plane switched on (receiving), MS is listening.
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::GrantedToOtherUser, None);
        while q.pop_front().is_some() {}

        assert!(cc.request_tx(&mut q, 7, 1));
        let prim = loop {
            match q.pop_front().expect("demand should emit signalling").msg {
                SapMsgInner::LcmcMleUnitdataReq(p) => break p,
                _ => continue,
            }
        };
        assert!(prim.stealing_permission, "demand on TCH must steal the half-slot");
        assert_eq!(prim.link_id, 2, "must use the TCH-associated basic link");
    }

    /// M4c ACK-key regression (cl. 22.3.2.3): for a GROUP call the stored call
    /// route addresses the group SSI, but the TCH-associated basic link is the
    /// individual, point-to-point MS↔SwMI acknowledged link — the SwMI acks the
    /// MS's own ISSI. The floor PDU must therefore be keyed on the own individual
    /// ISSI, otherwise the SwMI's ISSI-addressed BL-ACK never matches the
    /// group-keyed expected-ACK entry and the LLC retransmits to exhaustion.
    #[test]
    fn group_call_floor_signalling_keyed_on_own_issi() {
        const OWN_ISSI: u32 = 1234567;
        const GROUP_SSI: u32 = 220;
        let mut cc = CcMsSubentity::new(None);
        cc.own_issi = Some(OWN_ISSI);
        let mut q = MessageQueue::new();
        cc.calls.insert(
            7,
            MsCall::new(
                7,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false,
                // Group route as seen on air: the group number, Ssi-typed.
                CallRoute {
                    main_address: TetraAddress::new(GROUP_SSI, SsiType::Ssi),
                    handle: 1,
                    endpoint_id: 2,
                    link_id: 0,
                },
                true,
            ),
        );
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::Granted, None);
        while q.pop_front().is_some() {}

        assert!(cc.cease_tx(&mut q, 7));
        let prim = loop {
            match q.pop_front().expect("cease should emit signalling").msg {
                SapMsgInner::LcmcMleUnitdataReq(p) => break p,
                _ => continue,
            }
        };
        assert_eq!(prim.main_address.ssi, OWN_ISSI, "floor PDU must be keyed on own ISSI");
        assert!(matches!(prim.main_address.ssi_type, SsiType::Issi), "individual link");
        assert_ne!(prim.main_address.ssi, GROUP_SSI, "must NOT key on the group SSI");
        assert_eq!(prim.link_id, 2, "TCH-associated basic link");
        assert!(prim.stealing_permission);
    }

    /// ETSI TS 100 392-2 cl. 14.5.2.1.2: the SwMI periodically re-broadcasts the
    /// group D-SETUP for late entry. For the call originator these echoes carry
    /// its OWN calling party address and must be ignored — not recreate the
    /// call, not raise a duplicate TNCC-SETUP indication, and not apply the
    /// echoed transmission grant (which would knock the talking MS off its own
    /// granted floor).
    #[test]
    fn own_address_group_dsetup_rebroadcast_is_ignored() {
        const OWN_ISSI: u32 = 1234567;
        let (sink, source) = telemetry_channel();
        let mut cc = CcMsSubentity::new(Some(sink));
        cc.own_issi = Some(OWN_ISSI);
        let mut q = MessageQueue::new();
        cc.calls.insert(
            7,
            MsCall::new(
                7,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false,
                route(),
                true,
            ),
        );
        // MS holds the floor (GrantedSelf).
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::Granted, None);
        assert_eq!(cc.call(7).unwrap().tx_grant_state, MsTxGrantState::GrantedSelf);
        while source.try_recv().is_some() {} // discard prior telemetry
        while q.pop_front().is_some() {}

        // Own-address group D-SETUP re-broadcast granting the floor to "another
        // user" (the group echo of our own hold).
        let mut pdu = d_setup(7, false);
        pdu.basic_service_information = default_speech_basic_service();
        pdu.calling_party_address_ssi = Some(OWN_ISSI);
        pdu.transmission_grant = TransmissionGrant::GrantedToOtherUser;
        cc.rx_d_setup(&mut q, pdu, route());

        // Ignored: floor state preserved (still GrantedSelf), and NO TNCC event.
        assert_eq!(
            cc.call(7).unwrap().tx_grant_state,
            MsTxGrantState::GrantedSelf,
            "own-address D-SETUP echo must not knock the MS off its own floor"
        );
        assert!(
            source.try_recv().is_none(),
            "own-address D-SETUP re-broadcast must not raise any TNCC indication"
        );
    }

    /// ETSI TS 100 392-2 cl. 14.5.2.1.2: a group D-SETUP for a call the MS
    /// already tracks (late-entry re-broadcast from ANOTHER calling party)
    /// updates the floor in place — it does not raise a duplicate TNCC-SETUP
    /// indication — and surfaces the new talker as a TNCC-TX indication.
    #[test]
    fn known_group_dsetup_updates_in_place_and_surfaces_talker() {
        const OWN_ISSI: u32 = 1234567;
        const OTHER_TALKER: u32 = 555;
        let (sink, source) = telemetry_channel();
        let mut cc = CcMsSubentity::new(Some(sink));
        cc.own_issi = Some(OWN_ISSI);
        let mut q = MessageQueue::new();
        cc.calls.insert(
            7,
            MsCall::new(
                7,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false,
                route(),
                true,
            ),
        );
        while source.try_recv().is_some() {}

        let mut pdu = d_setup(7, false);
        pdu.basic_service_information = default_speech_basic_service();
        pdu.calling_party_address_ssi = Some(OTHER_TALKER);
        pdu.transmission_grant = TransmissionGrant::GrantedToOtherUser;
        cc.rx_d_setup(&mut q, pdu, route());

        let mut saw_setup = false;
        let mut tx_talker = None;
        while let Some(ev) = source.try_recv() {
            match ev {
                TelemetryEvent::TnccSetupIndication { .. } => saw_setup = true,
                TelemetryEvent::TnccTxIndication { indication, .. } => {
                    tx_talker = indication.transmitting_party_ssi;
                }
                _ => {}
            }
        }
        assert!(!saw_setup, "known-call re-broadcast must NOT raise a duplicate TNCC-SETUP indication");
        assert_eq!(tx_talker, Some(OTHER_TALKER), "the new talker must be surfaced via TNCC-TX indication");
        assert_eq!(cc.call(7).unwrap().current_speaker_ssi, Some(OTHER_TALKER));
    }

    /// ETSI TS 100 392-2 cl. 14.8.31 / 14.5.1.4: a FRESH incoming group D-SETUP
    /// (first contact / late entry — the call is not yet tracked) whose
    /// transmission-grant element already reads "granted to another user" means
    /// this MS joins as a listener that is immediately receiving that party's
    /// speech. Besides the one-shot TNCC-SETUP indication, the live floor/talker
    /// state must be surfaced via a TNCC-TX indication (the UI derives the talker
    /// and "receiving" state from TNCC-TX, not from TNCC-SETUP); otherwise the
    /// call shows the floor free with no talker until the next D-TX-GRANTED, which
    /// the SwMI need not send while the same party keeps talking. The U-plane must
    /// also be switched on so downlink speech plays out.
    #[test]
    fn fresh_group_dsetup_surfaces_talker_and_switches_uplane_on() {
        const OTHER_TALKER: u32 = 555;
        let (sink, source) = telemetry_channel();
        let mut cc = CcMsSubentity::new(Some(sink));
        cc.own_issi = Some(1234567);
        let mut q = MessageQueue::new();
        while source.try_recv().is_some() {}

        let mut pdu = d_setup(7, false);
        pdu.basic_service_information = default_speech_basic_service();
        pdu.calling_party_address_ssi = Some(OTHER_TALKER);
        pdu.transmission_grant = TransmissionGrant::GrantedToOtherUser;
        cc.rx_d_setup(&mut q, pdu, route());

        let mut saw_setup = false;
        let mut tx_talker = None;
        let mut tx_status = None;
        while let Some(ev) = source.try_recv() {
            match ev {
                TelemetryEvent::TnccSetupIndication { .. } => saw_setup = true,
                TelemetryEvent::TnccTxIndication { indication, .. } => {
                    tx_talker = indication.transmitting_party_ssi;
                    tx_status = Some(indication.transmission_status);
                }
                _ => {}
            }
        }
        assert!(saw_setup, "a fresh incoming group call must raise a TNCC-SETUP indication");
        assert_eq!(tx_talker, Some(OTHER_TALKER), "the current talker must be surfaced via TNCC-TX indication");
        assert_eq!(
            tx_status,
            Some(tncc::TransmissionStatus::TransmissionGrantedToAnotherUser),
            "floor state must read granted-to-another-user"
        );
        assert_eq!(cc.call(7).unwrap().current_speaker_ssi, Some(OTHER_TALKER));
        assert_eq!(
            cc.call(7).unwrap().last_uplane.map(|u| u.switch_u_plane),
            Some(true),
            "the listener's U-plane must be switched on so downlink speech plays out"
        );
    }

    /// A fresh incoming group D-SETUP announcing the call with NO active talker
    /// (transmission grant "not granted") must NOT fabricate a talker: only the
    /// TNCC-SETUP indication is raised, and no spurious TNCC-TX indication.
    #[test]
    fn fresh_group_dsetup_without_talker_raises_no_tx_indication() {
        let (sink, source) = telemetry_channel();
        let mut cc = CcMsSubentity::new(Some(sink));
        cc.own_issi = Some(1234567);
        let mut q = MessageQueue::new();
        while source.try_recv().is_some() {}

        let mut pdu = d_setup(8, false);
        pdu.basic_service_information = default_speech_basic_service();
        pdu.calling_party_address_ssi = Some(555);
        pdu.transmission_grant = TransmissionGrant::NotGranted;
        cc.rx_d_setup(&mut q, pdu, route());

        let mut saw_setup = false;
        let mut saw_tx = false;
        while let Some(ev) = source.try_recv() {
            match ev {
                TelemetryEvent::TnccSetupIndication { .. } => saw_setup = true,
                TelemetryEvent::TnccTxIndication { .. } => saw_tx = true,
                _ => {}
            }
        }
        assert!(saw_setup, "a fresh incoming group call must raise a TNCC-SETUP indication");
        assert!(!saw_tx, "no talker (grant not-granted) must not raise a TNCC-TX indication");
    }

    /// ETSI TS 100 392-2 cl. 14.5.2.1.2 / 14.5.1.1: while THIS MS holds the floor
    /// (GrantedSelf), a periodic group D-SETUP late-entry re-broadcast carrying a
    /// PREVIOUS talker's calling-party address and a stale GrantedToOtherUser
    /// grant must NOT revoke our own active transmission. Real-time floor control
    /// is governed only by D-TX-GRANTED / D-TX-CEASED / D-TX-INTERRUPT. (Observed
    /// on-air: the re-broadcast cut the MS off its own floor mid-talkspurt.)
    #[test]
    fn foreign_address_group_dsetup_rebroadcast_does_not_revoke_own_floor() {
        const OWN_ISSI: u32 = 1234567;
        const PREV_TALKER: u32 = 2200699;
        let (sink, source) = telemetry_channel();
        let mut cc = CcMsSubentity::new(Some(sink));
        cc.own_issi = Some(OWN_ISSI);
        let mut q = MessageQueue::new();
        cc.calls.insert(
            7,
            MsCall::new(
                7,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false,
                route(),
                true,
            ),
        );
        // MS holds the floor (GrantedSelf) — it is actively transmitting.
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::Granted, None);
        assert_eq!(cc.call(7).unwrap().tx_grant_state, MsTxGrantState::GrantedSelf);
        while source.try_recv().is_some() {} // discard prior telemetry
        while q.pop_front().is_some() {}

        // Stale late-entry re-broadcast: calling party = the PREVIOUS talker,
        // grant = GrantedToOtherUser.
        let mut pdu = d_setup(7, false);
        pdu.basic_service_information = default_speech_basic_service();
        pdu.calling_party_address_ssi = Some(PREV_TALKER);
        pdu.transmission_grant = TransmissionGrant::GrantedToOtherUser;
        cc.rx_d_setup(&mut q, pdu, route());

        // Our floor is preserved and no U-plane reconfigure (tx_grant off) is
        // emitted, so the MS keeps transmitting.
        assert_eq!(
            cc.call(7).unwrap().tx_grant_state,
            MsTxGrantState::GrantedSelf,
            "a foreign-address D-SETUP re-broadcast must not knock the MS off its own floor"
        );
        assert!(
            source.try_recv().is_none(),
            "re-broadcast while holding the floor must not raise a TNCC-TX indication"
        );
        assert!(
            q.pop_front().is_none(),
            "re-broadcast while holding the floor must not emit an LCMC-CONFIGURE (tx_grant off)"
        );
    }

    // --- MT individual-call answer path (cl. 14.5.1.1.1) ----------------------

    use crate::net_telemetry::channel::telemetry_channel;
    use tetra_pdus::cmce::enums::cmce_pdu_type_ul::CmcePduTypeUl;

    fn individual_speech() -> BasicServiceInformation {
        BasicServiceInformation {
            circuit_mode_type: CircuitModeType::TchS,
            encryption_flag: false,
            communication_type: CommunicationType::P2p,
            slots_per_frame: None,
            speech_service: Some(0),
        }
    }

    /// Minimal D-SETUP for an MT individual call with the given Hook method
    /// selection IE (cl. 14.8.23).
    fn d_setup(call_id: u16, hook_on_off: bool) -> DSetup {
        DSetup {
            call_identifier: call_id,
            call_time_out: CallTimeout::Infinite,
            hook_method_selection: hook_on_off,
            simplex_duplex_selection: false,
            basic_service_information: individual_speech(),
            transmission_grant: TransmissionGrant::Granted,
            transmission_request_permission: true,
            call_priority: 0,
            notification_indicator: None,
            temporary_address: None,
            calling_party_address_ssi: Some(101),
            calling_party_extension: None,
            external_subscriber_number: None,
            facility: None,
            dm_ms_address: None,
            proprietary: None,
        }
    }

    fn d_connect_ack(call_id: u16) -> DConnectAcknowledge {
        DConnectAcknowledge {
            call_identifier: call_id,
            call_time_out: 1, // T30s (cl. 14.8.16) so T310 actually arms
            transmission_grant: 0, // Granted
            transmission_request_permission: true,
            notification_indicator: None,
            facility: None,
            proprietary: None,
        }
    }

    fn setup_response() -> tncc::TnccSetupResponse {
        tncc::TnccSetupResponse {
            access_priority: None,
            basic_service_information: None,
            clir_control: None,
            hook_method_selection: tncc::HookMethodSelection::NoHookSignallingDirectThroughConnect,
            simplex_duplex_selection: tncc::SimplexDuplexSelection::SimplexOperation,
            traffic_stealing: None,
        }
    }

    fn complete_request() -> tncc::TnccCompleteRequest {
        tncc::TnccCompleteRequest {
            access_priority: None,
            basic_service_information_offered: None,
            hook_method: tncc::HookMethodSelection::HookOnHookOffSignallingOrCallAcceptanceSignalling,
            simplex_duplex: tncc::SimplexDuplexSelection::SimplexOperation,
            traffic_stealing: None,
        }
    }

    /// Drain the queue and return the CMCE uplink PDU type of every
    /// LCMC-MLE-UNITDATA request emitted, in order.
    fn drain_ul_pdu_types(q: &mut MessageQueue) -> Vec<CmcePduTypeUl> {
        let mut out = Vec::new();
        while let Some(m) = q.pop_front() {
            if let SapMsgInner::LcmcMleUnitdataReq(mut req) = m.msg {
                req.sdu.seek(0);
                if let Ok(raw) = req.sdu.read_field(5, "pdu_type") {
                    if let Ok(t) = CmcePduTypeUl::try_from(raw) {
                        out.push(t);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn on_off_hook_setup_response_sends_only_u_alert() {
        // cl. 14.5.1.1.1: on/off-hook TNCC-SETUP response → U-ALERT only, stay MT-CALL-SETUP.
        let mut cc = CcMsSubentity::new(None);
        let mut q = MessageQueue::new();
        cc.rx_d_setup(&mut q, d_setup(7, true), route());
        assert!(cc.call(7).unwrap().hook_on_off, "D-SETUP hook method must be recorded");

        let ok = cc.handle_tncc_setup_response(&mut q, 7, &setup_response());
        assert!(ok);
        assert_eq!(drain_ul_pdu_types(&mut q), vec![CmcePduTypeUl::UAlert]);
        assert_eq!(cc.call(7).unwrap().state, MsCcState::MtCallSetup);
    }

    #[test]
    fn on_off_hook_complete_sends_u_connect_and_arms_t301() {
        // cl. 14.5.1.1.1: on/off-hook TNCC-COMPLETE → U-CONNECT + start T301, stay MT-CALL-SETUP.
        let mut cc = CcMsSubentity::new(None);
        let mut q = MessageQueue::new();
        cc.rx_d_setup(&mut q, d_setup(7, true), route());
        // Simulate a provisioned/D-INFO Call time-out set-up phase value (cl. 14.8.17)
        // so T301 arms a real deadline; without one, predefined is not invented.
        cc.calls.get_mut(&7).unwrap().timers.setup_timeout = Some(CallTimeoutSetupPhase::T5s);
        let _ = cc.handle_tncc_setup_response(&mut q, 7, &setup_response());
        assert_eq!(drain_ul_pdu_types(&mut q), vec![CmcePduTypeUl::UAlert]);

        let ok = cc.handle_tncc_complete(&mut q, 7, &complete_request());
        assert!(ok);
        assert_eq!(drain_ul_pdu_types(&mut q), vec![CmcePduTypeUl::UConnect]);
        let call = cc.call(7).unwrap();
        assert_eq!(call.state, MsCcState::MtCallSetup);
        assert_eq!(call.timers.setup_timeout, Some(CallTimeoutSetupPhase::T5s));
        assert!(call.timers.setup_phase_deadline.is_some(), "T301 must be armed");
    }

    #[test]
    fn direct_setup_response_sends_u_connect() {
        // cl. 14.5.1.1.1: direct set-up TNCC-SETUP response → U-CONNECT immediately + start T301.
        let mut cc = CcMsSubentity::new(None);
        let mut q = MessageQueue::new();
        cc.rx_d_setup(&mut q, d_setup(7, false), route());
        assert!(!cc.call(7).unwrap().hook_on_off);

        let ok = cc.handle_tncc_setup_response(&mut q, 7, &setup_response());
        assert!(ok);
        assert_eq!(drain_ul_pdu_types(&mut q), vec![CmcePduTypeUl::UConnect]);
        let call = cc.call(7).unwrap();
        assert_eq!(call.state, MsCcState::MtCallSetup);
        // T301 arm attempted; D-SETUP carried no set-up-phase value so predefined stays uninvented.
        assert_eq!(call.timers.setup_timeout, Some(CallTimeoutSetupPhase::Predefined));
    }

    #[test]
    fn d_connect_ack_activates_and_swaps_timers() {
        // cl. 14.5.1.1.1: D-CONNECT ACK → CALL-ACTIVE, stop T301, start T310, TNCC-COMPLETE confirm.
        let (sink, source) = telemetry_channel();
        let mut cc = CcMsSubentity::new(Some(sink));
        let mut q = MessageQueue::new();
        cc.rx_d_setup(&mut q, d_setup(7, true), route());
        cc.calls.get_mut(&7).unwrap().timers.setup_timeout = Some(CallTimeoutSetupPhase::T5s);
        let _ = cc.handle_tncc_setup_response(&mut q, 7, &setup_response());
        let _ = cc.handle_tncc_complete(&mut q, 7, &complete_request());
        let _ = drain_ul_pdu_types(&mut q);
        assert!(cc.call(7).unwrap().timers.setup_phase_deadline.is_some(), "T301 running");
        while source.try_recv().is_some() {} // discard setup-phase telemetry

        cc.rx_d_connect_ack(&mut q, d_connect_ack(7), route());
        let call = cc.call(7).unwrap();
        assert_eq!(call.state, MsCcState::CallActive);
        assert!(call.timers.setup_phase_deadline.is_none(), "T301 stopped");
        assert!(call.timers.call_deadline.is_some(), "T310 started");

        let mut saw_confirm = false;
        while let Some(ev) = source.try_recv() {
            if matches!(ev, TelemetryEvent::TnccCompleteConfirm { .. }) {
                saw_confirm = true;
            }
        }
        assert!(saw_confirm, "TNCC-COMPLETE confirm must be emitted");
    }

    #[test]
    fn downlink_speech_gated_on_uplane_switch() {
        let mut cc = CcMsSubentity::new(None);
        let mut q = MessageQueue::new();
        cc.calls.insert(
            7,
            MsCall::new(
                7,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false,
                route(),
                true,
            ),
        );

        // U-plane still switched off: received speech is discarded (cl. 14.5.1.4).
        cc.rx_downlink_traffic(3, false, None, None, &[1u8; 274]);
        assert_eq!(cc.call(7).unwrap().rx_speech_frames, 0, "no frames accepted while U-plane off");

        // A grant to another user switches the U-plane on for receive; subsequent
        // speech frames are accepted (we are a listener, not the talker).
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::GrantedToOtherUser, Some(2200699));
        assert_eq!(cc.call(7).unwrap().last_uplane.map(|u| u.switch_u_plane), Some(true));

        cc.rx_downlink_traffic(3, false, None, None, &[1u8; 274]);
        cc.rx_downlink_traffic(3, false, None, None, &[1u8; 274]);
        assert_eq!(cc.call(7).unwrap().rx_speech_frames, 2, "frames accepted while U-plane on");
    }

    #[test]
    fn simplex_talkback_suppressed_while_holding_floor() {
        // ETSI TS 100 392-2 cl. 14.5.1.4: while this MS holds the floor on a
        // simplex call, the serving cell repeats the talker's own speech on the
        // downlink. Forwarding it to the UI would echo the operator's own voice,
        // so it must be suppressed. Regression for the on-air self-echo bug.
        let (sink, source) = telemetry_channel();
        let mut cc = CcMsSubentity::new(Some(sink));
        let mut q = MessageQueue::new();
        cc.calls.insert(
            11,
            MsCall::new(
                11,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false, // simplex
                route(),
                true,
            ),
        );
        // Grant to self: the MS holds the floor and is transmitting.
        cc.apply_transmission_grant(&mut q, 11, TransmissionGrant::Granted, None);
        assert_eq!(cc.call(11).unwrap().tx_grant_state, MsTxGrantState::GrantedSelf);
        while source.try_recv().is_some() {} // discard grant-phase telemetry

        cc.rx_downlink_traffic(3, false, None, None, &[1u8; 274]);
        cc.rx_downlink_traffic(3, false, None, None, &[1u8; 274]);

        assert_eq!(cc.call(11).unwrap().rx_speech_frames, 0, "own talk-back not counted");
        assert!(source.try_recv().is_none(), "no MsSpeechFrame echoed to the UI");
    }

    #[test]
    fn duplex_still_receives_while_transmitting() {
        // A duplex call is full-duplex: while this MS transmits it must still
        // receive and forward the far end (cl. 14.5.1.4). The simplex talk-back
        // suppression must NOT apply here.
        let (sink, source) = telemetry_channel();
        let mut cc = CcMsSubentity::new(Some(sink));
        let mut q = MessageQueue::new();
        cc.calls.insert(
            12,
            MsCall::new(
                12,
                MsCcState::CallActive,
                MsCallKind::Individual,
                default_speech_basic_service(),
                true, // duplex
                route(),
                true,
            ),
        );
        cc.apply_transmission_grant(&mut q, 12, TransmissionGrant::Granted, None);
        assert_eq!(cc.call(12).unwrap().tx_grant_state, MsTxGrantState::GrantedSelf);
        while source.try_recv().is_some() {} // discard grant-phase telemetry

        cc.rx_downlink_traffic(3, false, None, None, &[1u8; 274]);

        assert_eq!(cc.call(12).unwrap().rx_speech_frames, 1, "far-end speech still received on duplex");
        assert!(
            matches!(source.try_recv(), Some(TelemetryEvent::MsSpeechFrame { .. })),
            "duplex far-end frame forwarded to the UI"
        );
    }

    #[test]
    fn downlink_speech_emits_telemetry_frame() {
        // cl. 14.5.1.4: with the U-plane switched on, each decoded TCH/S block is
        // forwarded to the UI as an MsSpeechFrame carrying the type-1 bits, the
        // per-call sequence, the BFI, and the current talker.
        let (sink, source) = telemetry_channel();
        let mut cc = CcMsSubentity::new(Some(sink));
        let mut q = MessageQueue::new();
        cc.calls.insert(
            9,
            MsCall::new(
                9,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false,
                route(),
                true,
            ),
        );
        // GrantedToOtherUser switches the U-plane on for receive and records the
        // remote talker as the current speaker.
        cc.apply_transmission_grant(&mut q, 9, TransmissionGrant::GrantedToOtherUser, Some(2200699));
        while source.try_recv().is_some() {} // discard grant-phase telemetry

        // First frame is good, second is a bad frame (BFI set).
        let good = vec![1u8; 274];
        cc.rx_downlink_traffic(2, false, None, None, &good);
        cc.rx_downlink_traffic(2, true, None, None, &vec![0u8; 274]);

        let frames: Vec<_> = std::iter::from_fn(|| source.try_recv())
            .filter_map(|ev| match ev {
                TelemetryEvent::MsSpeechFrame {
                    call_identifier,
                    timeslot,
                    sequence,
                    transmitting_party_ssi,
                    frame_bits,
                    bad_frame,
                    data,
                } => Some((call_identifier, timeslot, sequence, transmitting_party_ssi, frame_bits, bad_frame, data)),
                _ => None,
            })
            .collect();

        assert_eq!(frames.len(), 2, "one MsSpeechFrame per decoded block");
        assert_eq!(frames[0], (9, 2, 1, Some(2200699), 274, false, good.clone()));
        assert_eq!(frames[1].2, 2, "per-call sequence increments");
        assert!(frames[1].5, "BFI propagated on the bad frame");
    }

    /// cl. 23.5.5: with several concurrent group calls all U-plane-on, a decoded
    /// speech frame tagged (by the MAC) with the owning SSI is attributed to the
    /// call addressed to that party — not an arbitrary U-plane-on call.
    #[test]
    fn downlink_speech_demuxed_by_owner_ssi() {
        let mut cc = CcMsSubentity::new(None);
        let mut q = MessageQueue::new();

        let mk_route = |ssi: u32| CallRoute {
            main_address: TetraAddress::new(ssi, SsiType::Gssi),
            handle: 1,
            endpoint_id: 2,
            link_id: 3,
        };
        for (call_id, gssi) in [(10u16, 220u32), (11u16, 2208u32)] {
            cc.calls.insert(
                call_id,
                MsCall::new(
                    call_id,
                    MsCcState::CallActive,
                    MsCallKind::Group,
                    default_speech_basic_service(),
                    false,
                    mk_route(gssi),
                    true,
                ),
            );
            // Both calls have the U-plane switched on (remote talker).
            cc.apply_transmission_grant(&mut q, call_id, TransmissionGrant::GrantedToOtherUser, None);
        }

        // A frame owned by GSSI 2208 must land only on call 11.
        cc.rx_downlink_traffic(2, false, Some(19), Some(2208), &[1u8; 274]);
        assert_eq!(cc.call(10).unwrap().rx_speech_frames, 0, "frame not misattributed to the other group");
        assert_eq!(cc.call(11).unwrap().rx_speech_frames, 1, "frame attributed to its owning call");

        // A frame owned by GSSI 220 lands only on call 10.
        cc.rx_downlink_traffic(3, false, Some(17), Some(220), &[1u8; 274]);
        assert_eq!(cc.call(10).unwrap().rx_speech_frames, 1, "second owner routed independently");
        assert_eq!(cc.call(11).unwrap().rx_speech_frames, 1, "unchanged");

        // A frame owned by a group we have no call for is dropped.
        cc.rx_downlink_traffic(4, false, Some(30), Some(9999), &[1u8; 274]);
        assert_eq!(cc.call(10).unwrap().rx_speech_frames, 1, "unknown-owner frame dropped");
        assert_eq!(cc.call(11).unwrap().rx_speech_frames, 1, "unknown-owner frame dropped");
    }

    /// ETSI TS 100 392-2 cl. 14.2.4.1: a single-transceiver MS has one U-plane /
    /// traffic-channel resource. While an individual (point-to-point) call is
    /// engaged and holds that resource, a concurrently-notified group call must
    /// NOT seize it — the group's U-plane switch-on (and the LCMC-MLE CONFIGURE
    /// that would reconfigure the lower layers off the private call) is withheld,
    /// so the active private call is never disrupted.
    #[test]
    fn group_call_uplane_withheld_while_individual_call_engaged() {
        let mut cc = CcMsSubentity::new(None);
        let mut q = MessageQueue::new();
        let grp_route = CallRoute {
            main_address: TetraAddress::new(220, SsiType::Gssi),
            handle: 1,
            endpoint_id: 4,
            link_id: 3,
        };

        // Engaged duplex individual call: its U-plane is switched on (GrantedSelf).
        cc.calls.insert(
            5,
            MsCall::new(5, MsCcState::CallActive, MsCallKind::Individual, default_speech_basic_service(), true, route(), true),
        );
        cc.apply_transmission_grant(&mut q, 5, TransmissionGrant::Granted, None);
        assert_eq!(cc.call(5).unwrap().last_uplane.map(|u| u.switch_u_plane), Some(true));
        while q.pop_front().is_some() {} // discard the individual call's CONFIGURE

        // A group call is notified (remote talker) while the private call runs.
        cc.calls.insert(
            9,
            MsCall::new(9, MsCcState::CallActive, MsCallKind::Group, default_speech_basic_service(), false, grp_route, true),
        );
        cc.apply_transmission_grant(&mut q, 9, TransmissionGrant::GrantedToOtherUser, Some(2200699));

        // No U-plane CONFIGURE was emitted for the group call, and its U-plane
        // stayed off; the individual call keeps the resource.
        assert!(
            q.iter().all(|m| !matches!(m.msg, SapMsgInner::LcmcMleConfigureReq(_))),
            "group call must not reconfigure the U-plane while an individual call is engaged"
        );
        assert_ne!(
            cc.call(9).unwrap().last_uplane.map(|u| u.switch_u_plane),
            Some(true),
            "group U-plane must stay off while the individual call is engaged"
        );
        assert_eq!(cc.call(5).unwrap().last_uplane.map(|u| u.switch_u_plane), Some(true));

        // Group downlink traffic is therefore not played out (U-plane off).
        cc.rx_downlink_traffic(2, false, Some(19), Some(220), &[1u8; 274]);
        assert_eq!(cc.call(9).unwrap().rx_speech_frames, 0, "no group audio while the private call is engaged");
    }

    /// ETSI TS 100 392-2 cl. 14.2.4.1 / 14.5.1.1: once the engaged individual
    /// call releases the sole U-plane resource, a withheld group call's periodic
    /// D-SETUP late-entry re-broadcast switches its U-plane on (late entry).
    #[test]
    fn withheld_group_call_activates_when_individual_call_releases() {
        let mut cc = CcMsSubentity::new(None);
        let mut q = MessageQueue::new();
        let grp_route = CallRoute {
            main_address: TetraAddress::new(220, SsiType::Gssi),
            handle: 1,
            endpoint_id: 4,
            link_id: 3,
        };

        cc.calls.insert(
            5,
            MsCall::new(5, MsCcState::CallActive, MsCallKind::Individual, default_speech_basic_service(), true, route(), true),
        );
        cc.apply_transmission_grant(&mut q, 5, TransmissionGrant::Granted, None);
        cc.calls.insert(
            9,
            MsCall::new(9, MsCcState::CallActive, MsCallKind::Group, default_speech_basic_service(), false, grp_route, true),
        );
        cc.apply_transmission_grant(&mut q, 9, TransmissionGrant::GrantedToOtherUser, Some(2200699));
        assert_ne!(cc.call(9).unwrap().last_uplane.map(|u| u.switch_u_plane), Some(true), "withheld while engaged");

        // Private call ends, freeing the U-plane resource.
        cc.calls.remove(&5);
        while q.pop_front().is_some() {}

        // Periodic group re-broadcast now switches the U-plane on (late entry).
        cc.apply_transmission_grant(&mut q, 9, TransmissionGrant::GrantedToOtherUser, Some(2200699));
        assert_eq!(
            cc.call(9).unwrap().last_uplane.map(|u| u.switch_u_plane),
            Some(true),
            "group U-plane activates once the individual call frees the resource"
        );
        assert!(
            q.iter().any(|m| matches!(m.msg, SapMsgInner::LcmcMleConfigureReq(_))),
            "CONFIGURE emitted for the group call after the private call releases"
        );
    }

    /// cl. 23.5.5 / 14.5.1.4: in a duplex or MS-originated group call the serving
    /// cell also addresses this MS individually (floor grants / MAC-RESOURCE) on
    /// the call's shared usage marker, so the MAC transiently binds the marker to
    /// our own ISSI. Downlink traffic that arrives tagged with that own address
    /// (not any call's main address) must still be delivered to the sole
    /// U-plane-on call — never dropped — while remaining unambiguous when only one
    /// call is active.
    #[test]
    fn downlink_speech_recovered_when_marker_bound_to_own_issi() {
        const OWN_ISSI: u32 = 1234567;
        let mut cc = CcMsSubentity::new(None);
        cc.own_issi = Some(OWN_ISSI);
        let mut q = MessageQueue::new();

        cc.calls.insert(
            22,
            MsCall::new(
                22,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false,
                CallRoute {
                    main_address: TetraAddress::new(2200699, SsiType::Gssi),
                    handle: 1,
                    endpoint_id: 2,
                    link_id: 3,
                },
                true,
            ),
        );
        // Remote talker: the sole call's U-plane is switched on.
        cc.apply_transmission_grant(&mut q, 22, TransmissionGrant::GrantedToOtherUser, None);

        // Frame tagged with the group address is delivered (primary attribution).
        cc.rx_downlink_traffic(4, false, Some(22), Some(2200699), &[1u8; 274]);
        assert_eq!(cc.call(22).unwrap().rx_speech_frames, 1, "group-owned frame delivered");

        // Frame tagged with our OWN ISSI (marker contaminated by an individual
        // MAC-RESOURCE addressed to us) is recovered onto the sole U-plane-on
        // call, not dropped.
        cc.rx_downlink_traffic(4, false, Some(22), Some(OWN_ISSI), &[1u8; 274]);
        assert_eq!(
            cc.call(22).unwrap().rx_speech_frames,
            2,
            "own-ISSI-tagged frame recovered onto the sole U-plane-on call"
        );
    }

    #[test]
    fn uplink_speech_forwarded_only_while_talker() {
        let mut cc = CcMsSubentity::new(None);
        let mut q = MessageQueue::new();
        cc.calls.insert(
            7,
            MsCall::new(
                7,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false,
                route(),
                true,
            ),
        );

        // UI-supplied 274-bit type-1 block, one-bit-per-byte (alternating), the
        // same layout as the downlink MsSpeechFrame.
        let ui_frame: Vec<u8> = (0..274u16).map(|i| (i % 2) as u8).collect();

        // Before any grant: a pushed frame is dropped (the MS must not transmit
        // traffic it holds no floor for) and driving the source emits nothing.
        cc.push_uplink_speech(7, 274, &ui_frame);
        cc.drive_uplink_source(&mut q);
        assert_eq!(cc.call(7).unwrap().tx_speech_frames, 0, "no source frames without the floor");
        assert!(
            q.iter().all(|m| !matches!(m.msg, SapMsgInner::TmdCircuitDataReq(_))),
            "no TMD source pushed to the MAC while not talking"
        );

        // Grant to self switches the U-plane on and makes us the talker
        // (cl. 14.5.1.4). With no queued UI frame yet, the source emits nothing —
        // the MAC fills comfort silence on underrun (cl. 23), CC-MS does not.
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::Granted, None);
        let mut q_idle = MessageQueue::new();
        cc.drive_uplink_source(&mut q_idle);
        assert_eq!(cc.call(7).unwrap().tx_speech_frames, 0, "no synthesised source without UI audio");
        assert!(
            q_idle.iter().all(|m| !matches!(m.msg, SapMsgInner::TmdCircuitDataReq(_))),
            "CC-MS pushes nothing on underrun; the MAC owns silence"
        );

        // Now the UI supplies a frame while we hold the floor: it is packed and
        // clocked down to the MAC.
        cc.push_uplink_speech(7, 274, &ui_frame);
        let mut q2 = MessageQueue::new();
        cc.drive_uplink_source(&mut q2);
        assert_eq!(cc.call(7).unwrap().tx_speech_frames, 1, "one source frame forwarded while talking");
        let pushed: Vec<_> = q2
            .iter()
            .filter(|m| m.dest == TetraEntity::Umac && matches!(m.msg, SapMsgInner::TmdCircuitDataReq(_)))
            .collect();
        assert_eq!(pushed.len(), 1, "exactly one TMD source frame pushed to the MAC");
        let SapMsgInner::TmdCircuitDataReq(req) = &pushed[0].msg else {
            unreachable!()
        };
        assert_eq!(req.data.len(), 35, "274-bit type-1 block packed to 35 bytes");
        // Verify the packing: bit i (one-bit-per-byte) → bit (7 - i%8) of byte i/8.
        let mut expected = vec![0u8; 35];
        for i in 0..274usize {
            if ui_frame[i] & 1 != 0 {
                expected[i / 8] |= 1 << (7 - (i % 8));
            }
        }
        assert_eq!(req.data, expected, "type-1 bits MSB-first packed correctly");
    }

    #[test]
    fn uplink_speech_dropped_when_floor_held_by_other() {
        // cl. 14.5.1.4 / 23: while another party holds the floor (GrantedToOther,
        // U-plane on for receive) the MS is not the talker and must not source
        // uplink traffic. A pushed frame is discarded.
        let mut cc = CcMsSubentity::new(None);
        let mut q = MessageQueue::new();
        cc.calls.insert(
            7,
            MsCall::new(
                7,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false,
                route(),
                true,
            ),
        );
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::GrantedToOtherUser, Some(2200699));

        let ui_frame: Vec<u8> = vec![1u8; 274];
        cc.push_uplink_speech(7, 274, &ui_frame);
        let mut q2 = MessageQueue::new();
        cc.drive_uplink_source(&mut q2);
        assert_eq!(cc.call(7).unwrap().tx_speech_frames, 0, "no uplink source while another party talks");
        assert!(
            q2.iter().all(|m| !matches!(m.msg, SapMsgInner::TmdCircuitDataReq(_))),
            "no TMD source pushed while not the talker"
        );
    }

    #[test]
    fn uplink_source_fifo_bounded_drop_oldest() {
        // Overflow protection (not a jitter buffer): the per-call source FIFO is
        // bounded, dropping the oldest frame so transmit latency stays capped.
        let mut cc = CcMsSubentity::new(None);
        let mut q = MessageQueue::new();
        cc.calls.insert(
            7,
            MsCall::new(
                7,
                MsCcState::CallActive,
                MsCallKind::Group,
                default_speech_basic_service(),
                false,
                route(),
                true,
            ),
        );
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::Granted, None);

        // Push more frames than the bound in one burst; the FIFO must not grow
        // past the bound.
        for _ in 0..(super::uplane::UPLINK_SOURCE_MAX_FRAMES + 3) {
            cc.push_uplink_speech(7, 274, &vec![1u8; 274]);
        }
        assert_eq!(
            cc.call(7).unwrap().uplink_source_frames.len(),
            super::uplane::UPLINK_SOURCE_MAX_FRAMES,
            "FIFO bounded to the drop-oldest limit"
        );
    }
}
