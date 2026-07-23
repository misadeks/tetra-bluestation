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
};

use crate::MessageQueue;

const TIMESLOT_DURATION_MS: f64 = 170.0 / 12.0;

/// CMCE CC-MS call state (ETSI TS 100 392-2 cl. 14.5.2 and 14.5.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsCcState {
    Idle,
    MoCallSetup,
    MtCallSetup,
    CallActive,
    Wait,
    Restore,
    Disconnect,
    Release,
}

/// Call kind from Basic service information / communication type (cl. 14.8.2,
/// 14.8.17c).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsCallKind {
    Individual,
    Group,
    AcknowledgedGroup,
    Broadcast,
}

/// Local interpretation of Transmission grant (cl. 14.8.42) in the MS call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsTxGrantState {
    None,
    GrantedSelf,
    RequestQueued,
    GrantedOther,
    Waiting,
    Interrupted,
}

/// Last U-plane CONFIGURE request state (cl. 14.5.1.4; LCMC-SAP cl. 17.3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsUPlaneState {
    pub switch_u_plane: bool,
    pub tx_grant: bool,
    pub simplex_duplex: bool,
}

#[derive(Clone, Debug)]
pub struct MsCallTimers {
    /// T301/T302/T303 setup-phase deadline from cl. 14.5.2.1 / 14.8.17.
    pub setup_phase_deadline: Option<TdmaTime>,
    /// T310 call-timeout deadline from cl. 14.8.16.
    pub call_deadline: Option<TdmaTime>,
    pub setup_timeout: Option<CallTimeoutSetupPhase>,
    pub call_timeout: CallTimeout,
}

impl Default for MsCallTimers {
    fn default() -> Self {
        Self {
            setup_phase_deadline: None,
            call_deadline: None,
            setup_timeout: None,
            call_timeout: CallTimeout::Infinite,
        }
    }
}

/// Per-call CC-MS state keyed by the 14-bit Call identifier IE (cl. 14.8.4).
#[derive(Clone, Debug)]
pub struct MsCall {
    pub call_identifier: u16,
    pub state: MsCcState,
    pub kind: MsCallKind,
    pub basic_service: BasicServiceInformation,
    pub current_speaker_ssi: Option<u32>,
    pub tx_grant_state: MsTxGrantState,
    pub transmission_request_allowed: bool,
    pub timers: MsCallTimers,
    pub disconnect_cause: Option<DisconnectCause>,
    pub last_uplane: Option<MsUPlaneState>,
    route: CallRoute,
    simplex_duplex_selection: bool,
    pending_tx_request: bool,
    uplane_before_wait: Option<MsUPlaneState>,
}

#[derive(Clone, Copy, Debug)]
struct CallRoute {
    main_address: TetraAddress,
    handle: MleHandle,
    endpoint_id: EndpointId,
    link_id: LinkId,
}

#[derive(Clone, Debug)]
struct PendingOrigination {
    called_party: TetraAddress,
    basic_service: BasicServiceInformation,
    simplex_duplex_selection: bool,
}

/// Clause 14 Call Control CMCE sub-entity, MS side.
pub struct CcMsSubentity {
    own_issi: Option<u32>,
    dltime: TdmaTime,
    calls: HashMap<u16, MsCall>,
    pending_originations: Vec<PendingOrigination>,
}

impl CcMsSubentity {
    pub fn new() -> Self {
        Self {
            own_issi: None,
            dltime: TdmaTime::default(),
            calls: HashMap::new(),
            pending_originations: Vec::new(),
        }
    }

    pub fn new_with_config(config: SharedConfig) -> Self {
        let mut s = Self::new();
        s.set_config(config);
        s
    }

    pub fn set_config(&mut self, config: SharedConfig) {
        self.own_issi = config.config().ms.as_ref().map(|ms| ms.issi);
    }

    pub fn call(&self, call_identifier: u16) -> Option<&MsCall> {
        self.calls.get(&call_identifier)
    }

    pub fn call_count(&self) -> usize {
        self.calls.len()
    }

    pub fn pending_origination_count(&self) -> usize {
        self.pending_originations.len()
    }

    /// U-SETUP for MO group call (cl. 14.5.2.1.2; PDU cl. 14.7.2.10).
    pub fn originate_group_call(
        &mut self,
        queue: &mut MessageQueue,
        called_gssi: u32,
        basic_service: BasicServiceInformation,
        request_to_transmit: bool,
    ) {
        self.send_u_setup(
            queue,
            TetraAddress::new(called_gssi, SsiType::Gssi),
            basic_service,
            false,
            false,
            request_to_transmit,
        );
    }

    /// U-SETUP for MO individual call (cl. 14.5.6.2; PDU cl. 14.7.2.10).
    pub fn originate_individual_call(
        &mut self,
        queue: &mut MessageQueue,
        called_issi: u32,
        basic_service: BasicServiceInformation,
        simplex_duplex_selection: bool,
        request_to_transmit: bool,
    ) {
        self.send_u_setup(
            queue,
            TetraAddress::new(called_issi, SsiType::Issi),
            basic_service,
            true,
            simplex_duplex_selection,
            request_to_transmit,
        );
    }

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
        let route = call.route;
        call.pending_tx_request = true;
        self.send_pdu(queue, &pdu, route, false, false);
        true
    }

    /// U-TX CEASED (cl. 14.5.2.2.1 e; PDU cl. 14.7.2.11).
    pub fn cease_tx(&mut self, queue: &mut MessageQueue, call_identifier: u16) -> bool {
        let Some(call) = self.calls.get_mut(&call_identifier) else {
            return false;
        };
        let route = call.route;
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
            true,
            true,
        );
        self.configure_uplane(queue, call_identifier, false, false, simplex_duplex);
        true
    }

    /// U-ALERT/U-CONNECT answer path for MT individual calls (cl. 14.5.6.5).
    pub fn answer_call(&mut self, queue: &mut MessageQueue, call_identifier: u16, alert_first: bool) -> bool {
        let Some(call) = self.calls.get(&call_identifier) else {
            return false;
        };
        let route = call.route;
        if alert_first {
            self.send_pdu(
                queue,
                &UAlert {
                    call_identifier,
                    reserved: true,
                    simplex_duplex_selection: call.simplex_duplex_selection,
                    basic_service_information: Some(call.basic_service.clone()),
                    facility: None,
                    proprietary: None,
                },
                route,
                false,
                false,
            );
        }
        self.send_pdu(
            queue,
            &UConnect {
                call_identifier,
                hook_method_selection: false,
                simplex_duplex_selection: call.simplex_duplex_selection,
                basic_service_information: Some(call.basic_service.clone()),
                facility: None,
                proprietary: None,
            },
            route,
            false,
            false,
        );
        true
    }

    /// U-DISCONNECT initiated clearing (cl. 14.5.2.3.1; PDU cl. 14.7.2.4).
    pub fn disconnect_call(&mut self, queue: &mut MessageQueue, call_identifier: u16, cause: DisconnectCause) -> bool {
        let Some(call) = self.calls.get_mut(&call_identifier) else {
            return false;
        };
        let route = call.route;
        let simplex_duplex = call.simplex_duplex_selection;
        call.state = MsCcState::Disconnect;
        call.disconnect_cause = Some(cause);
        self.send_pdu(
            queue,
            &UDisconnect {
                call_identifier,
                disconnect_cause: cause,
                facility: None,
                proprietary: None,
            },
            route,
            false,
            false,
        );
        self.configure_uplane(queue, call_identifier, false, false, simplex_duplex);
        true
    }

    /// U-RELEASE response to D-DISCONNECT (D-DISCONNECT cl. 14.7.1.6; U-RELEASE cl. 14.7.2.8).
    pub fn release_call(&mut self, queue: &mut MessageQueue, call_identifier: u16, cause: DisconnectCause) -> bool {
        let Some(call) = self.calls.get_mut(&call_identifier) else {
            return false;
        };
        let route = call.route;
        let simplex_duplex = call.simplex_duplex_selection;
        call.state = MsCcState::Release;
        call.disconnect_cause = Some(cause);
        self.send_pdu(
            queue,
            &URelease {
                call_identifier,
                disconnect_cause: cause,
                facility: None,
                proprietary: None,
            },
            route,
            false,
            false,
        );
        self.configure_uplane(queue, call_identifier, false, false, simplex_duplex);
        true
    }

    /// U-CALL RESTORE (cl. 14.5.2.2.4; PDU cl. 14.7.2.2). Phase 1 wires this
    /// seam but only receives MLE-REOPEN, the unsuccessful restoration indication.
    pub fn request_call_restore(&self, queue: &mut MessageQueue, call_identifier: u16) -> bool {
        let Some(call) = self.calls.get(&call_identifier) else {
            return false;
        };
        let pdu = UCallRestore {
            call_identifier,
            request_to_transmit_send_data: call.pending_tx_request || call.tx_grant_state == MsTxGrantState::GrantedSelf,
            other_party_type_identifier: PartyTypeIdentifier::Ssi.into_raw() as u8,
            other_party_short_number_address: None,
            other_party_ssi: Some(call.route.main_address.ssi as u64),
            other_party_extension: None,
            basic_service_information: Some(call.basic_service.clone()),
            facility: None,
            dm_ms_address: None,
            proprietary: None,
        };
        self.send_pdu(queue, &pdu, call.route, false, false);
        true
    }

    pub fn tick_start(&mut self, queue: &mut MessageQueue, ts: TdmaTime) {
        self.dltime = ts;
        let expired: Vec<(u16, bool)> = self
            .calls
            .iter()
            .filter_map(|(id, call)| {
                if call.timers.setup_phase_deadline.map_or(false, |deadline| deadline.age(ts) >= 0) {
                    Some((*id, true))
                } else if call.timers.call_deadline.map_or(false, |deadline| deadline.age(ts) >= 0) {
                    Some((*id, false))
                } else {
                    None
                }
            })
            .collect();
        for (id, setup_phase) in expired {
            tracing::warn!(call_identifier = id, setup_phase, "CMCE-MS: call timer expired");
            let _ = self.disconnect_call(queue, id, DisconnectCause::ExpiryOfTimer);
        }
    }

    pub fn handle_break(&mut self, queue: &mut MessageQueue) {
        // cl. 14.5.1.4.2 e / 14.5.2.2.4: BREAK switches U-plane off; a current
        // self grant is treated as ended as if U-TX CEASED had been sent.
        for id in self.calls.keys().copied().collect::<Vec<_>>() {
            let simplex_duplex = if let Some(call) = self.calls.get_mut(&id) {
                call.state = MsCcState::Restore;
                if call.tx_grant_state == MsTxGrantState::GrantedSelf {
                    call.tx_grant_state = MsTxGrantState::None;
                    call.pending_tx_request = false;
                }
                Some(call.simplex_duplex_selection)
            } else {
                None
            };
            if let Some(simplex_duplex) = simplex_duplex {
                self.configure_uplane(queue, id, false, false, simplex_duplex);
            }
        }
    }

    pub fn handle_reopen(&mut self, queue: &mut MessageQueue) {
        // cl. 17.3.3 MLE-REOPEN plus cl. 14.5.2.2.4: REOPEN indicates failed
        // restoration; clear the call cleanly.
        for id in self.calls.keys().copied().collect::<Vec<_>>() {
            let simplex_duplex = if let Some(call) = self.calls.get_mut(&id) {
                call.state = MsCcState::Release;
                call.disconnect_cause = Some(DisconnectCause::CallRestorationOfTheOtherUserFailed);
                Some(call.simplex_duplex_selection)
            } else {
                None
            };
            if let Some(simplex_duplex) = simplex_duplex {
                self.configure_uplane(queue, id, false, false, simplex_duplex);
            }
            self.calls.remove(&id);
        }
    }

    pub fn route_rd_deliver(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        let Some(bits) = prim.sdu.peek_bits(5) else {
            tracing::warn!("insufficient bits: {}", prim.sdu.dump_bin());
            return;
        };
        let Ok(pdu_type) = CmcePduTypeDl::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, prim.sdu.dump_bin());
            return;
        };
        let route = CallRoute {
            main_address: prim.received_tetra_address,
            handle: prim.handle,
            endpoint_id: prim.endpoint_id,
            link_id: prim.link_id,
        };
        macro_rules! parse {
            ($ty:ty, $handler:ident) => {
                match <$ty>::from_bitbuf(&mut prim.sdu) {
                    Ok(pdu) => self.$handler(queue, pdu, route),
                    Err(e) => tracing::warn!("CMCE-MS: failed parsing {:?}: {:?} {}", pdu_type, e, prim.sdu.dump_bin()),
                }
            };
        }
        match pdu_type {
            CmcePduTypeDl::DAlert => parse!(DAlert, rx_d_alert),
            CmcePduTypeDl::DCallProceeding => parse!(DCallProceeding, rx_d_call_proceeding),
            CmcePduTypeDl::DCallRestore => parse!(DCallRestore, rx_d_call_restore),
            CmcePduTypeDl::DConnect => parse!(DConnect, rx_d_connect),
            CmcePduTypeDl::DConnectAcknowledge => parse!(DConnectAcknowledge, rx_d_connect_ack),
            CmcePduTypeDl::DDisconnect => parse!(DDisconnect, rx_d_disconnect),
            CmcePduTypeDl::DInfo => parse!(DInfo, rx_d_info),
            CmcePduTypeDl::DRelease => parse!(DRelease, rx_d_release),
            CmcePduTypeDl::DSetup => parse!(DSetup, rx_d_setup),
            CmcePduTypeDl::DTxCeased => parse!(DTxCeased, rx_d_tx_ceased),
            CmcePduTypeDl::DTxContinue => parse!(DTxContinue, rx_d_tx_continue),
            CmcePduTypeDl::DTxGranted => parse!(DTxGranted, rx_d_tx_granted),
            CmcePduTypeDl::DTxInterrupt => parse!(DTxInterrupt, rx_d_tx_interrupt),
            CmcePduTypeDl::DTxWait => parse!(DTxWait, rx_d_tx_wait),
            _ => panic!(),
        }
    }

    fn rx_d_setup(&mut self, queue: &mut MessageQueue, pdu: DSetup, route: CallRoute) {
        let kind = kind_from_basic_service(&pdu.basic_service_information);
        let state = if kind == MsCallKind::Individual {
            MsCcState::MtCallSetup
        } else {
            MsCcState::CallActive
        };
        let mut call = MsCall::new(
            pdu.call_identifier,
            state,
            kind,
            pdu.basic_service_information,
            pdu.simplex_duplex_selection,
            route,
            pdu.transmission_request_permission,
        );
        call.current_speaker_ssi = pdu.calling_party_address_ssi;
        call.start_call_timer(self.dltime, pdu.call_time_out);
        self.calls.insert(pdu.call_identifier, call);
        if kind != MsCallKind::Individual {
            self.apply_transmission_grant(queue, pdu.call_identifier, pdu.transmission_grant, pdu.calling_party_address_ssi);
        }
    }

    fn rx_d_call_proceeding(&mut self, _queue: &mut MessageQueue, pdu: DCallProceeding, route: CallRoute) {
        let pending = self.pending_originations.pop();
        let basic = pdu
            .basic_service_information
            .or_else(|| pending.as_ref().map(|p| p.basic_service.clone()))
            .unwrap_or_else(default_speech_basic_service);
        let simplex = pending
            .as_ref()
            .map(|p| p.simplex_duplex_selection)
            .unwrap_or(pdu.simplex_duplex_selection);
        let kind = kind_from_basic_service(&basic);
        let call = self
            .calls
            .entry(pdu.call_identifier)
            .or_insert_with(|| MsCall::new(pdu.call_identifier, MsCcState::MoCallSetup, kind, basic, simplex, route, true));
        call.state = MsCcState::MoCallSetup;
        call.route = route;
        call.start_setup_timer(self.dltime, pdu.call_time_out_set_up_phase);
    }

    fn rx_d_alert(&mut self, _queue: &mut MessageQueue, pdu: DAlert, route: CallRoute) {
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            if let Some(basic) = pdu.basic_service_information {
                call.basic_service = basic;
            }
            if let Ok(timeout) = CallTimeoutSetupPhase::try_from(pdu.call_time_out_set_up_phase as u64) {
                call.start_setup_timer(self.dltime, timeout);
            }
        }
    }

    fn rx_d_connect(&mut self, queue: &mut MessageQueue, pdu: DConnect, route: CallRoute) {
        let pending = self.pending_originations.pop();
        let basic = pdu
            .basic_service_information
            .or_else(|| pending.as_ref().map(|p| p.basic_service.clone()))
            .unwrap_or_else(default_speech_basic_service);
        let simplex = pending
            .as_ref()
            .map(|p| p.simplex_duplex_selection)
            .unwrap_or(pdu.simplex_duplex_selection);
        let kind = kind_from_basic_service(&basic);
        let call = self.calls.entry(pdu.call_identifier).or_insert_with(|| {
            MsCall::new(
                pdu.call_identifier,
                MsCcState::CallActive,
                kind,
                basic,
                simplex,
                route,
                pdu.transmission_request_permission,
            )
        });
        call.state = MsCcState::CallActive;
        call.route = route;
        call.transmission_request_allowed = pdu.transmission_request_permission;
        call.simplex_duplex_selection = pdu.simplex_duplex_selection;
        call.start_call_timer(self.dltime, pdu.call_time_out);
        call.timers.setup_phase_deadline = None;
        self.apply_transmission_grant(queue, pdu.call_identifier, pdu.transmission_grant, None);
    }

    fn rx_d_connect_ack(&mut self, queue: &mut MessageQueue, pdu: DConnectAcknowledge, route: CallRoute) {
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.state = MsCcState::CallActive;
            call.route = route;
            call.transmission_request_allowed = pdu.transmission_request_permission;
            call.timers.setup_phase_deadline = None;
            if let Ok(timeout) = CallTimeout::try_from(pdu.call_time_out as u64) {
                call.start_call_timer(self.dltime, timeout);
            }
        }
        if let Ok(grant) = TransmissionGrant::try_from(pdu.transmission_grant as u64) {
            self.apply_transmission_grant(queue, pdu.call_identifier, grant, None);
        }
    }

    fn rx_d_tx_granted(&mut self, queue: &mut MessageQueue, pdu: DTxGranted, route: CallRoute) {
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::CallActive;
            call.transmission_request_allowed = pdu.transmission_request_permission;
            call.current_speaker_ssi = pdu.transmitting_party_address_ssi.map(|v| v as u32);
        }
        if let Ok(grant) = TransmissionGrant::try_from(pdu.transmission_grant as u64) {
            let speaker = pdu.transmitting_party_address_ssi.map(|v| v as u32);
            if grant == TransmissionGrant::GrantedToOtherUser && speaker == self.own_issi {
                tracing::warn!(call_identifier = pdu.call_identifier, "CMCE-MS: explicit self grant still required");
                return;
            }
            self.apply_transmission_grant(queue, pdu.call_identifier, grant, speaker);
        }
    }

    fn rx_d_tx_ceased(&mut self, queue: &mut MessageQueue, pdu: DTxCeased, route: CallRoute) {
        let simplex_duplex = if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::CallActive;
            call.tx_grant_state = MsTxGrantState::None;
            call.current_speaker_ssi = None;
            call.transmission_request_allowed = pdu.transmission_request_permission;
            call.pending_tx_request = false;
            Some(call.simplex_duplex_selection)
        } else {
            None
        };
        if let Some(simplex_duplex) = simplex_duplex {
            self.configure_uplane(queue, pdu.call_identifier, false, false, simplex_duplex);
        }
    }

    fn rx_d_tx_wait(&mut self, queue: &mut MessageQueue, pdu: DTxWait, route: CallRoute) {
        let simplex_duplex = if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::Wait;
            call.transmission_request_allowed = pdu.transmission_request_permission;
            call.tx_grant_state = MsTxGrantState::Waiting;
            call.uplane_before_wait = call.last_uplane.filter(|u| u.switch_u_plane);
            Some(call.simplex_duplex_selection)
        } else {
            None
        };
        if let Some(simplex_duplex) = simplex_duplex {
            self.configure_uplane(queue, pdu.call_identifier, false, false, simplex_duplex);
        }
    }

    fn rx_d_tx_continue(&mut self, queue: &mut MessageQueue, pdu: DTxContinue, route: CallRoute) {
        let restore = if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::CallActive;
            call.transmission_request_allowed = pdu.transmission_request_permission;
            let restore = if pdu.do_continue { call.uplane_before_wait.take() } else { None };
            if restore.is_none() {
                call.tx_grant_state = MsTxGrantState::None;
            }
            restore
        } else {
            None
        };
        if let Some(u) = restore {
            self.configure_uplane(queue, pdu.call_identifier, u.switch_u_plane, u.tx_grant, u.simplex_duplex);
            if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
                call.tx_grant_state = if u.tx_grant {
                    MsTxGrantState::GrantedSelf
                } else {
                    MsTxGrantState::GrantedOther
                };
            }
        }
    }

    fn rx_d_tx_interrupt(&mut self, queue: &mut MessageQueue, pdu: DTxInterrupt, route: CallRoute) {
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::CallActive;
            call.transmission_request_allowed = pdu.transmission_request_permission;
            call.current_speaker_ssi = pdu.transmitting_party_address_ssi.map(|v| v as u32);
        }
        if let Ok(grant) = TransmissionGrant::try_from(pdu.transmission_grant as u64) {
            if grant == TransmissionGrant::GrantedToOtherUser {
                self.apply_transmission_grant(
                    queue,
                    pdu.call_identifier,
                    grant,
                    pdu.transmitting_party_address_ssi.map(|v| v as u32),
                );
            } else if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
                call.tx_grant_state = MsTxGrantState::Interrupted;
                let simplex_duplex = call.simplex_duplex_selection;
                let _ = call;
                self.configure_uplane(queue, pdu.call_identifier, false, false, simplex_duplex);
            }
        }
    }

    fn rx_d_disconnect(&mut self, queue: &mut MessageQueue, pdu: DDisconnect, route: CallRoute) {
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::Disconnect;
            call.disconnect_cause = Some(pdu.disconnect_cause);
        }
        let _ = self.release_call(queue, pdu.call_identifier, pdu.disconnect_cause);
    }

    fn rx_d_release(&mut self, queue: &mut MessageQueue, pdu: DRelease, route: CallRoute) {
        let simplex_duplex = if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::Release;
            call.disconnect_cause = Some(pdu.disconnect_cause);
            Some(call.simplex_duplex_selection)
        } else {
            None
        };
        if let Some(simplex_duplex) = simplex_duplex {
            self.configure_uplane(queue, pdu.call_identifier, false, false, simplex_duplex);
        }
        self.calls.remove(&pdu.call_identifier);
    }

    fn rx_d_info(&mut self, _queue: &mut MessageQueue, pdu: DInfo, route: CallRoute) {
        let mut new_key = None;
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            if let Some(timeout) = pdu
                .call_time_out_set_up_phase_t301_t302_
                .and_then(|v| CallTimeoutSetupPhase::try_from(v).ok())
            {
                call.start_setup_timer(self.dltime, timeout);
            }
            if pdu.reset_call_time_out_timer_t310_ {
                if let Some(timeout) = pdu.call_time_out.and_then(|v| CallTimeout::try_from(v).ok()) {
                    call.start_call_timer(self.dltime, timeout);
                } else if call.timers.call_timeout != CallTimeout::Infinite {
                    call.start_call_timer(self.dltime, call.timers.call_timeout);
                }
            } else if let Some(timeout) = pdu.call_time_out.and_then(|v| CallTimeout::try_from(v).ok()) {
                call.timers.call_timeout = timeout;
            }
            new_key = pdu.new_call_identifier.map(|id| id as u16);
        }
        if let Some(id) = new_key {
            if let Some(mut call) = self.calls.remove(&pdu.call_identifier) {
                call.call_identifier = id;
                self.calls.insert(id, call);
            }
        }
    }

    fn rx_d_call_restore(&mut self, queue: &mut MessageQueue, pdu: DCallRestore, route: CallRoute) {
        let mut key = pdu.call_identifier;
        if let Some(new_id) = pdu.new_call_identifier {
            if let Some(mut call) = self.calls.remove(&pdu.call_identifier) {
                key = new_id as u16;
                call.call_identifier = key;
                self.calls.insert(key, call);
            }
        }
        if let Some(call) = self.calls.get_mut(&key) {
            call.route = route;
            call.state = MsCcState::CallActive;
            if pdu.reset_call_time_out_timer_t310_ {
                if let Some(timeout) = pdu.call_time_out.and_then(|v| CallTimeout::try_from(v).ok()) {
                    call.start_call_timer(self.dltime, timeout);
                } else if call.timers.call_timeout != CallTimeout::Infinite {
                    call.start_call_timer(self.dltime, call.timers.call_timeout);
                }
            }
        }
        if let Ok(grant) = TransmissionGrant::try_from(pdu.transmission_grant as u64) {
            self.apply_transmission_grant(queue, key, grant, None);
        }
    }

    fn apply_transmission_grant(&mut self, queue: &mut MessageQueue, call_identifier: u16, grant: TransmissionGrant, speaker: Option<u32>) {
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

    fn configure_uplane(
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

    fn send_u_setup(
        &mut self,
        queue: &mut MessageQueue,
        called_party: TetraAddress,
        basic_service: BasicServiceInformation,
        hook_method_selection: bool,
        simplex_duplex_selection: bool,
        request_to_transmit: bool,
    ) {
        let pdu = USetup {
            area_selection: 0,
            hook_method_selection,
            simplex_duplex_selection,
            basic_service_information: basic_service.clone(),
            request_to_transmit_send_data: request_to_transmit,
            call_priority: 0,
            clir_control: 0,
            called_party_type_identifier: PartyTypeIdentifier::Ssi,
            called_party_short_number_address: None,
            called_party_ssi: Some(called_party.ssi as u64),
            called_party_extension: None,
            external_subscriber_number: None,
            facility: None,
            dm_ms_address: None,
            proprietary: None,
        };
        self.pending_originations.push(PendingOrigination {
            called_party,
            basic_service,
            simplex_duplex_selection,
        });
        self.send_pdu(
            queue,
            &pdu,
            CallRoute {
                main_address: called_party,
                handle: 0,
                endpoint_id: 0,
                link_id: 0,
            },
            false,
            false,
        );
    }

    fn send_pdu<P: UplinkPdu>(&self, queue: &mut MessageQueue, pdu: &P, route: CallRoute, stealing: bool, stealing_repeats: bool) {
        let mut sdu = BitBuffer::new_autoexpand(96);
        pdu.write(&mut sdu).expect("failed to serialize CMCE uplink PDU");
        sdu.seek(0);
        queue.push_back(SapMsg {
            sap: Sap::LcmcSap,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LcmcMleUnitdataReq(LcmcMleUnitdataReq {
                sdu,
                handle: route.handle,
                endpoint_id: route.endpoint_id,
                link_id: route.link_id,
                layer2service: Layer2Service::Todo,
                pdu_prio: 0,
                layer2_qos: 0,
                stealing_permission: stealing,
                stealing_repeats_flag: stealing_repeats,
                main_address: route.main_address,
                chan_alloc: None,
                tx_reporter: None,
            }),
        });
    }
}

impl MsCall {
    fn new(
        call_identifier: u16,
        state: MsCcState,
        kind: MsCallKind,
        basic_service: BasicServiceInformation,
        simplex_duplex_selection: bool,
        route: CallRoute,
        transmission_request_allowed: bool,
    ) -> Self {
        Self {
            call_identifier,
            state,
            kind,
            basic_service,
            current_speaker_ssi: None,
            tx_grant_state: MsTxGrantState::None,
            transmission_request_allowed,
            timers: MsCallTimers::default(),
            disconnect_cause: None,
            last_uplane: None,
            route,
            simplex_duplex_selection,
            pending_tx_request: false,
            uplane_before_wait: None,
        }
    }

    fn start_setup_timer(&mut self, now: TdmaTime, timeout: CallTimeoutSetupPhase) {
        self.timers.setup_timeout = Some(timeout);
        self.timers.setup_phase_deadline = setup_timeout_to_timeslots(timeout).map(|slots| now.add_timeslots(slots));
        if self.timers.setup_phase_deadline.is_none() {
            tracing::warn!(
                call_identifier = self.call_identifier,
                "CMCE-MS: predefined setup timer has no codeplug value; not armed"
            );
        }
    }

    fn start_call_timer(&mut self, now: TdmaTime, timeout: CallTimeout) {
        self.timers.call_timeout = timeout;
        self.timers.call_deadline = call_timeout_to_timeslots(timeout).map(|slots| now.add_timeslots(slots));
        if timeout == CallTimeout::Reserved {
            tracing::warn!(call_identifier = self.call_identifier, "CMCE-MS: reserved T310 value; not armed");
        }
    }
}

fn kind_from_basic_service(basic: &BasicServiceInformation) -> MsCallKind {
    match basic.communication_type {
        CommunicationType::P2p => MsCallKind::Individual,
        CommunicationType::P2Mp => MsCallKind::Group,
        CommunicationType::P2MpAcked => MsCallKind::AcknowledgedGroup,
        CommunicationType::Broadcast => MsCallKind::Broadcast,
    }
}

fn default_speech_basic_service() -> BasicServiceInformation {
    BasicServiceInformation {
        circuit_mode_type: CircuitModeType::TchS,
        encryption_flag: false,
        communication_type: CommunicationType::P2Mp,
        slots_per_frame: None,
        speech_service: Some(0),
    }
}

#[inline]
fn seconds_to_timeslots(seconds: i32) -> i32 {
    (f64::from(seconds) * 1_000.0 / TIMESLOT_DURATION_MS) as i32
}

/// cl. 14.8.17; predefined is not invented without a codeplug value.
fn setup_timeout_to_timeslots(timeout: CallTimeoutSetupPhase) -> Option<i32> {
    match timeout {
        CallTimeoutSetupPhase::Predefined => None,
        CallTimeoutSetupPhase::T1s => Some(seconds_to_timeslots(1)),
        CallTimeoutSetupPhase::T2s => Some(seconds_to_timeslots(2)),
        CallTimeoutSetupPhase::T5s => Some(seconds_to_timeslots(5)),
        CallTimeoutSetupPhase::T10s => Some(seconds_to_timeslots(10)),
        CallTimeoutSetupPhase::T20s => Some(seconds_to_timeslots(20)),
        CallTimeoutSetupPhase::T30s => Some(seconds_to_timeslots(30)),
        CallTimeoutSetupPhase::T60s => Some(seconds_to_timeslots(60)),
    }
}

/// cl. 14.8.16 T310 values.
fn call_timeout_to_timeslots(timeout: CallTimeout) -> Option<i32> {
    match timeout {
        CallTimeout::Infinite | CallTimeout::Reserved => None,
        CallTimeout::T30s => Some(seconds_to_timeslots(30)),
        CallTimeout::T45s => Some(seconds_to_timeslots(45)),
        CallTimeout::T60s => Some(seconds_to_timeslots(60)),
        CallTimeout::T2m => Some(seconds_to_timeslots(120)),
        CallTimeout::T3m => Some(seconds_to_timeslots(180)),
        CallTimeout::T4m => Some(seconds_to_timeslots(240)),
        CallTimeout::T5m => Some(seconds_to_timeslots(300)),
        CallTimeout::T6m => Some(seconds_to_timeslots(360)),
        CallTimeout::T8m => Some(seconds_to_timeslots(480)),
        CallTimeout::T10m => Some(seconds_to_timeslots(600)),
        CallTimeout::T12m => Some(seconds_to_timeslots(720)),
        CallTimeout::T15m => Some(seconds_to_timeslots(900)),
        CallTimeout::T20m => Some(seconds_to_timeslots(1200)),
        CallTimeout::T30m => Some(seconds_to_timeslots(1800)),
    }
}

trait UplinkPdu {
    fn write(&self, buffer: &mut BitBuffer) -> Result<(), tetra_core::PduParseErr>;
}

macro_rules! uplink {
    ($ty:ty) => {
        impl UplinkPdu for $ty {
            fn write(&self, buffer: &mut BitBuffer) -> Result<(), tetra_core::PduParseErr> {
                self.to_bitbuf(buffer)
            }
        }
    };
}

uplink!(USetup);
uplink!(UTxDemand);
uplink!(UTxCeased);
uplink!(UConnect);
uplink!(UAlert);
uplink!(UDisconnect);
uplink!(URelease);
uplink!(UCallRestore);

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
        let mut cc = CcMsSubentity::new();
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
