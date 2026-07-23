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
mod tncc_adapters;
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
}
