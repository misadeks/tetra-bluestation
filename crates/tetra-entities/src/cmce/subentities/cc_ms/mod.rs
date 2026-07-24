use std::collections::HashMap;

use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, EndpointId, Layer2Service, LinkId, MleHandle, Sap, SsiType, TdmaTime, TetraAddress, Todo};
use tetra_pdus::cmce::{
    enums::{
        call_timeout::CallTimeout, call_timeout_setup_phase::CallTimeoutSetupPhase, cmce_pdu_type_dl::CmcePduTypeDl,
        disconnect_cause::DisconnectCause, party_type_identifier::PartyTypeIdentifier, transmission_grant::TransmissionGrant,
    },
    fields::basic_service_information::BasicServiceInformation,
    pdus::{
        d_alert::DAlert, d_call_proceeding::DCallProceeding, d_call_restore::DCallRestore, d_connect::DConnect,
        d_connect_acknowledge::DConnectAcknowledge, d_disconnect::DDisconnect, d_info::DInfo, d_release::DRelease, d_setup::DSetup,
        d_tx_ceased::DTxCeased, d_tx_continue::DTxContinue, d_tx_granted::DTxGranted, d_tx_interrupt::DTxInterrupt, d_tx_wait::DTxWait,
        u_alert::UAlert, u_call_restore::UCallRestore, u_connect::UConnect, u_disconnect::UDisconnect, u_release::URelease,
        u_setup::USetup, u_tx_ceased::UTxCeased, u_tx_demand::UTxDemand,
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
    tncc_call_status, tncc_call_status_raw, tncc_call_timeout, tncc_disconnect_cause, tncc_setup_timeout, tncc_transmission_grant,
    tncc_transmission_status_from_grant,
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
        cc.rx_downlink_traffic(3, &[1u8; 274]);
        assert_eq!(cc.call(7).unwrap().rx_speech_frames, 0, "no frames accepted while U-plane off");

        // Grant switches the U-plane on; subsequent speech frames are accepted.
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::Granted, None);
        assert_eq!(cc.call(7).unwrap().last_uplane.map(|u| u.switch_u_plane), Some(true));

        cc.rx_downlink_traffic(3, &[1u8; 274]);
        cc.rx_downlink_traffic(3, &[1u8; 274]);
        assert_eq!(cc.call(7).unwrap().rx_speech_frames, 2, "frames accepted while U-plane on");
    }

    #[test]
    fn uplink_source_supplied_only_while_talker() {
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

        // No transmission grant yet: CC-MS supplies no uplink U-plane source.
        cc.drive_uplink_source(&mut q);
        assert_eq!(cc.call(7).unwrap().tx_speech_frames, 0, "no source frames without the floor");
        assert!(
            q.iter().all(|m| !matches!(m.msg, SapMsgInner::TmdCircuitDataReq(_))),
            "no TMD source pushed to the MAC while not talking"
        );

        // Grant to self switches the U-plane on and makes us the talker
        // (cl. 14.5.1.4); CC-MS now sources uplink speech frames down to the MAC.
        cc.apply_transmission_grant(&mut q, 7, TransmissionGrant::Granted, None);
        let mut q2 = MessageQueue::new();
        cc.drive_uplink_source(&mut q2);
        assert_eq!(cc.call(7).unwrap().tx_speech_frames, 1, "one source frame supplied while talking");
        let pushed: Vec<_> = q2
            .iter()
            .filter(|m| m.dest == TetraEntity::Umac && matches!(m.msg, SapMsgInner::TmdCircuitDataReq(_)))
            .collect();
        assert_eq!(pushed.len(), 1, "exactly one TMD source frame pushed to the MAC");
        let SapMsgInner::TmdCircuitDataReq(req) = &pushed[0].msg else {
            unreachable!()
        };
        assert_eq!(req.data.len(), 35, "labelled deterministic 274-bit (packed) silence frame");
    }
}
