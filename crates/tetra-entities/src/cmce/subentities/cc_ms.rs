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
    telemetry: Option<TelemetrySink>,
}

impl CcMsSubentity {
    pub fn new(telemetry: Option<TelemetrySink>) -> Self {
        Self {
            own_issi: None,
            dltime: TdmaTime::default(),
            calls: HashMap::new(),
            pending_originations: Vec::new(),
            telemetry,
        }
    }

    pub fn new_with_config(config: SharedConfig, telemetry: Option<TelemetrySink>) -> Self {
        let mut s = Self::new(telemetry);
        s.set_config(config);
        s
    }

    pub fn set_telemetry(&mut self, telemetry: Option<TelemetrySink>) {
        self.telemetry = telemetry;
    }

    fn emit(&self, event: TelemetryEvent) {
        if let Some(sink) = &self.telemetry {
            sink.send(event);
        }
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

    /// TNCC-SETUP request (Table 11.8, cl. 11.3.3.8) adapter: build U-SETUP
    /// through the Phase-1 CC engine; no call-control behaviour is duplicated.
    pub fn handle_tncc_setup_request(&mut self, queue: &mut MessageQueue, request: &tncc::TnccSetupRequest) -> Result<(), String> {
        let Some(called_party_ssi) = request.called_party_ssi else {
            return Err("TNCC-SETUP request without called party SSI is not supported by this MS CC engine".to_string());
        };
        let basic = pdu_basic_from_tncc(&request.basic_service_information)?;
        match request.called_party_type_identifier {
            tncc::CalledPartyTypeIdentifier::Ssi => {
                if request.basic_service_information.communication_type == tncc::CommunicationType::PointToPoint {
                    self.originate_individual_call(
                        queue,
                        called_party_ssi,
                        basic,
                        request.simplex_duplex_selection.as_bool(),
                        request.request_to_transmit_send_data.as_bool(),
                    );
                } else {
                    self.originate_group_call(queue, called_party_ssi, basic, request.request_to_transmit_send_data.as_bool());
                }
                Ok(())
            }
            tncc::CalledPartyTypeIdentifier::Sna | tncc::CalledPartyTypeIdentifier::Tsi => {
                Err("TNCC-SETUP SNA/TSI called-party addressing is not implemented by the Phase-1 engine".to_string())
            }
        }
    }

    /// TNCC-SETUP response / TNCC-COMPLETE request adapter (Tables 11.8/11.2).
    pub fn handle_tncc_answer(&mut self, queue: &mut MessageQueue, call_identifier: u16) -> bool {
        self.answer_call(queue, call_identifier, false)
    }

    /// TNCC-TX request adapter (Table 11.9).
    pub fn handle_tncc_tx_request(&mut self, queue: &mut MessageQueue, call_identifier: u16, request: tncc::TnccTxRequest) -> bool {
        match request.transmission_condition {
            tncc::TransmissionCondition::RequestToTransmit => {
                self.request_tx(queue, call_identifier, request.tx_demand_priority.into_raw())
            }
            tncc::TransmissionCondition::TransmissionCeased => self.cease_tx(queue, call_identifier),
        }
    }

    /// TNCC-RELEASE request adapter (Table 11.7).
    pub fn handle_tncc_release_request(
        &mut self,
        queue: &mut MessageQueue,
        call_identifier: u16,
        request: tncc::TnccReleaseRequest,
    ) -> Result<(), String> {
        let cause = pdu_disconnect_cause_from_tncc(request.disconnect_cause)?;
        let acted = match request.disconnect_type {
            tncc::DisconnectType::DisconnectCall => self.disconnect_call(queue, call_identifier, cause),
            tncc::DisconnectType::LeaveCallWithoutDisconnection | tncc::DisconnectType::LeaveCallTemporarily => {
                self.release_call(queue, call_identifier, cause)
            }
        };
        if acted {
            Ok(())
        } else {
            Err("unknown call identifier".to_string())
        }
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
        let setup_basic_for_event = pdu.basic_service_information.clone();
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
        if let Some(basic) = tncc_basic_from_pdu(&setup_basic_for_event) {
            self.emit(TelemetryEvent::TnccSetupIndication {
                call_identifier: pdu.call_identifier,
                indication: Box::new(tncc::TnccSetupIndication {
                    basic_service_information: basic,
                    call_priority: tncc::CallPriority::from_raw(pdu.call_priority).unwrap_or(tncc::CallPriority::PriorityNotDefined),
                    call_time_out: tncc_call_timeout(pdu.call_time_out),
                    called_party_ssi: route.main_address.ssi,
                    called_party_extension: None,
                    calling_party_ssi: pdu.calling_party_address_ssi,
                    calling_party_extension: pdu.calling_party_extension,
                    external_subscriber_number_calling: None,
                    clir_control: None,
                    hook_method_selection: tncc::HookMethodSelection::from_bool(pdu.hook_method_selection),
                    notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                    simplex_duplex_selection: tncc::SimplexDuplexSelection::from_bool(pdu.simplex_duplex_selection),
                    transmission_grant: tncc_transmission_grant(pdu.transmission_grant),
                    transmission_request_permission: tncc::TransmissionRequestPermission::from_bool(pdu.transmission_request_permission),
                }),
            });
        } else {
            tracing::warn!(
                call_identifier = pdu.call_identifier,
                "CMCE-MS: unsupported TNCC basic service value; suppressing TNCC-SETUP indication"
            );
        }
        if kind != MsCallKind::Individual {
            self.apply_transmission_grant(queue, pdu.call_identifier, pdu.transmission_grant, pdu.calling_party_address_ssi);
        }
    }

    fn rx_d_call_proceeding(&mut self, _queue: &mut MessageQueue, pdu: DCallProceeding, route: CallRoute) {
        let pending = self.pending_originations.pop();
        let basic = pdu
            .basic_service_information
            .clone()
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
        self.emit(TelemetryEvent::TnccProceedIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccProceedIndication {
                basic_service_information_offered: pdu.basic_service_information.as_ref().and_then(tncc_basic_from_pdu),
                call_status: pdu.call_status.and_then(tncc_call_status),
                hook_method: Some(tncc::HookMethodSelection::from_bool(pdu.hook_method_selection)),
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                simplex_duplex: Some(tncc::SimplexDuplexSelection::from_bool(pdu.simplex_duplex_selection)),
            },
        });
    }

    fn rx_d_alert(&mut self, _queue: &mut MessageQueue, pdu: DAlert, route: CallRoute) {
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            if let Some(basic) = &pdu.basic_service_information {
                call.basic_service = basic.clone();
            }
            if let Ok(timeout) = CallTimeoutSetupPhase::try_from(pdu.call_time_out_set_up_phase as u64) {
                call.start_setup_timer(self.dltime, timeout);
            }
        }
        self.emit(TelemetryEvent::TnccAlertIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccAlertIndication {
                basic_service_information_offered: pdu.basic_service_information.as_ref().and_then(tncc_basic_from_pdu),
                call_queued: Some(if pdu.call_queued {
                    tncc::CallQueued::CallIsQueued
                } else {
                    tncc::CallQueued::CallIsNotQueued
                }),
                call_time_out_set_up_phase: tncc_setup_timeout(
                    CallTimeoutSetupPhase::try_from(pdu.call_time_out_set_up_phase as u64).unwrap_or(CallTimeoutSetupPhase::Predefined),
                ),
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                simplex_duplex: tncc::SimplexDuplexSelection::from_bool(pdu.simplex_duplex_selection),
            },
        });
    }

    fn rx_d_connect(&mut self, queue: &mut MessageQueue, pdu: DConnect, route: CallRoute) {
        let pending = self.pending_originations.pop();
        let basic = pdu
            .basic_service_information
            .clone()
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
        let confirm_basic = call.basic_service.clone();
        let _ = call;
        self.apply_transmission_grant(queue, pdu.call_identifier, pdu.transmission_grant, None);
        if let Some(basic) = tncc_basic_from_pdu(&confirm_basic) {
            self.emit(TelemetryEvent::TnccSetupConfirm {
                call_identifier: pdu.call_identifier,
                confirm: Box::new(tncc::TnccSetupConfirm {
                    basic_service_information: basic,
                    call_priority: pdu.call_priority.and_then(|v| tncc::CallPriority::from_raw(v as u8)),
                    call_ownership: if pdu.call_ownership {
                        tncc::CallOwnership::ACallOwner
                    } else {
                        tncc::CallOwnership::NotACallOwner
                    },
                    call_amalgamation: tncc::CallAmalgamation::CallNotAmalgamated,
                    call_time_out: tncc_call_timeout(pdu.call_time_out),
                    hook_method_selection: tncc::HookMethodSelection::from_bool(pdu.hook_method_selection),
                    notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                    simplex_duplex_selection: tncc::SimplexDuplexSelection::from_bool(pdu.simplex_duplex_selection),
                    transmission_grant: tncc_transmission_grant(pdu.transmission_grant),
                    transmission_request_permission: tncc::TransmissionRequestPermission::from_bool(pdu.transmission_request_permission),
                }),
            });
        }
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
            if let Some(call) = self.calls.get(&pdu.call_identifier) {
                self.emit(TelemetryEvent::TnccCompleteConfirm {
                    call_identifier: pdu.call_identifier,
                    confirm: tncc::TnccCompleteConfirm {
                        call_time_out: tncc_call_timeout(call.timers.call_timeout),
                        notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                        transmission_grant: tncc_transmission_grant(grant),
                        transmission_request_permission: tncc::TransmissionRequestPermission::from_bool(
                            pdu.transmission_request_permission,
                        ),
                        transmission_status: tncc_transmission_status_from_grant(grant),
                    },
                });
            }
        }
    }

    fn rx_d_tx_granted(&mut self, queue: &mut MessageQueue, pdu: DTxGranted, route: CallRoute) {
        let pending_before = self.calls.get(&pdu.call_identifier).map(|c| c.pending_tx_request).unwrap_or(false);
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
            if let Some(call) = self.calls.get(&pdu.call_identifier) {
                if pending_before && grant == TransmissionGrant::Granted {
                    self.emit(TelemetryEvent::TnccTxConfirm {
                        call_identifier: pdu.call_identifier,
                        confirm: tncc::TnccTxConfirm {
                            encryption_flag: tncc::EncryptionFlag::from_bool(pdu.encryption_control),
                            transmit_request_permission: tncc::TransmissionRequestPermission::from_bool(
                                pdu.transmission_request_permission,
                            ),
                            transmission_status: tncc_transmission_status_from_grant(grant),
                        },
                    });
                } else {
                    self.emit(TelemetryEvent::TnccTxIndication {
                        call_identifier: pdu.call_identifier,
                        indication: tncc::TnccTxIndication {
                            encryption_flag: tncc::EncryptionFlag::from_bool(pdu.encryption_control),
                            notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                            transmitting_party_ssi: pdu.transmitting_party_address_ssi.map(|v| v as u32).or(call.current_speaker_ssi),
                            transmitting_party_extension: pdu.transmitting_party_extension.map(|v| v as u32),
                            external_subscriber_number: None,
                            transmit_request_permission: tncc::TransmissionRequestPermission::from_bool(
                                pdu.transmission_request_permission,
                            ),
                            transmission_status: tncc_transmission_status_from_grant(grant),
                        },
                    });
                }
            }
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
        self.emit(TelemetryEvent::TnccTxIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccTxIndication {
                encryption_flag: tncc::EncryptionFlag::ClearEndToEndTransmission,
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                transmitting_party_ssi: None,
                transmitting_party_extension: None,
                external_subscriber_number: None,
                transmit_request_permission: tncc::TransmissionRequestPermission::from_bool(pdu.transmission_request_permission),
                transmission_status: tncc::TransmissionStatus::TransmissionCeased,
            },
        });
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
        self.emit(TelemetryEvent::TnccTxIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccTxIndication {
                encryption_flag: tncc::EncryptionFlag::ClearEndToEndTransmission,
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                transmitting_party_ssi: None,
                transmitting_party_extension: None,
                external_subscriber_number: None,
                transmit_request_permission: tncc::TransmissionRequestPermission::from_bool(pdu.transmission_request_permission),
                transmission_status: tncc::TransmissionStatus::TransmissionWait,
            },
        });
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
        self.emit(TelemetryEvent::TnccTxIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccTxIndication {
                encryption_flag: tncc::EncryptionFlag::ClearEndToEndTransmission,
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                transmitting_party_ssi: None,
                transmitting_party_extension: None,
                external_subscriber_number: None,
                transmit_request_permission: tncc::TransmissionRequestPermission::from_bool(pdu.transmission_request_permission),
                transmission_status: if pdu.do_continue {
                    tncc::TransmissionStatus::TransmissionGranted
                } else {
                    tncc::TransmissionStatus::TransmissionCeased
                },
            },
        });
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
        self.emit(TelemetryEvent::TnccTxIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccTxIndication {
                encryption_flag: tncc::EncryptionFlag::ClearEndToEndTransmission,
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                transmitting_party_ssi: pdu.transmitting_party_address_ssi.map(|v| v as u32),
                transmitting_party_extension: pdu.transmitting_party_extension.map(|v| v as u32),
                external_subscriber_number: None,
                transmit_request_permission: tncc::TransmissionRequestPermission::from_bool(pdu.transmission_request_permission),
                transmission_status: tncc::TransmissionStatus::TransmissionInterrupt,
            },
        });
    }

    fn rx_d_disconnect(&mut self, queue: &mut MessageQueue, pdu: DDisconnect, route: CallRoute) {
        self.emit(TelemetryEvent::TnccReleaseIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccReleaseIndication {
                disconnect_cause: tncc_disconnect_cause(pdu.disconnect_cause),
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
            },
        });
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::Disconnect;
            call.disconnect_cause = Some(pdu.disconnect_cause);
        }
        let _ = self.release_call(queue, pdu.call_identifier, pdu.disconnect_cause);
    }

    fn rx_d_release(&mut self, queue: &mut MessageQueue, pdu: DRelease, route: CallRoute) {
        let was_local_disconnect = self
            .calls
            .get(&pdu.call_identifier)
            .map(|c| c.state == MsCcState::Disconnect || c.state == MsCcState::Release)
            .unwrap_or(false);
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
        if was_local_disconnect {
            self.emit(TelemetryEvent::TnccReleaseConfirm {
                call_identifier: pdu.call_identifier,
                confirm: tncc::TnccReleaseConfirm {
                    disconnect_cause: tncc_disconnect_cause(pdu.disconnect_cause),
                    disconnect_status: tncc::DisconnectStatus::DisconnectionSuccessful,
                    notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                },
            });
        } else {
            self.emit(TelemetryEvent::TnccReleaseIndication {
                call_identifier: pdu.call_identifier,
                indication: tncc::TnccReleaseIndication {
                    disconnect_cause: tncc_disconnect_cause(pdu.disconnect_cause),
                    notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                },
            });
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
        self.emit(TelemetryEvent::TnccNotifyIndication {
            call_identifier: new_key.unwrap_or(pdu.call_identifier),
            indication: tncc::TnccNotifyIndication {
                call_status: pdu.call_status.and_then(|v| tncc_call_status_raw(v as u8)),
                call_time_out_in_set_up_phase: pdu
                    .call_time_out_set_up_phase_t301_t302_
                    .and_then(|v| CallTimeoutSetupPhase::try_from(v).ok())
                    .map(tncc_setup_timeout),
                call_time_out: pdu.call_time_out.and_then(|v| CallTimeout::try_from(v).ok()).map(tncc_call_timeout),
                call_ownership: pdu.call_ownership.map(|v| {
                    if v == 0 {
                        tncc::CallOwnership::NotACallOwner
                    } else {
                        tncc::CallOwnership::ACallOwner
                    }
                }),
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                poll_response_percentage: pdu.poll_response_percentage.map(|v| v as u8),
                poll_response_number: pdu.poll_response_number.map(|v| v as u8),
                poll_response_addresses: None,
                poll_request: Some(pdu.poll_request),
            },
        });
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

fn tncc_basic_from_pdu(basic: &BasicServiceInformation) -> Option<tncc::TnccBasicServiceInformation> {
    let communication_type = match basic.communication_type {
        CommunicationType::P2p => tncc::CommunicationType::PointToPoint,
        CommunicationType::P2Mp => tncc::CommunicationType::PointToMultipoint,
        CommunicationType::P2MpAcked => tncc::CommunicationType::PointToMultipointAcknowledged,
        CommunicationType::Broadcast => tncc::CommunicationType::Broadcast,
    };
    let encryption_flag = tncc::EncryptionFlag::from_bool(basic.encryption_flag);
    if basic.circuit_mode_type == CircuitModeType::TchS {
        let speech_service = match basic.speech_service? {
            0 => tncc::SpeechService::TetraEncodedOneTimeslotSpeech,
            3 => tncc::SpeechService::ProprietaryEncodedOneTimeslotSpeech,
            _ => return None,
        };
        Some(tncc::TnccBasicServiceInformation {
            circuit_mode_service: tncc::CircuitModeService::SpeechService,
            communication_type,
            data_service: None,
            data_call_capacity: None,
            encryption_flag,
            speech_service: Some(speech_service),
        })
    } else {
        let data_service = match basic.circuit_mode_type {
            CircuitModeType::Tch72 => tncc::DataService::Unprotected72KbitsNoInterleaving,
            CircuitModeType::Tch48n1 => tncc::DataService::LowProtection48KbitsShortInterleavingDepth1,
            CircuitModeType::Tch48n4 => tncc::DataService::LowProtection48KbitsMediumInterleavingDepth4,
            CircuitModeType::Tch48n8 => tncc::DataService::LowProtection48KbitsLongInterleavingDepth8,
            CircuitModeType::Tch24n1 => tncc::DataService::HighProtection24KbitsShortInterleavingDepth1,
            CircuitModeType::Tch24n4 => tncc::DataService::HighProtection24KbitsMediumInterleavingDepth4,
            CircuitModeType::Tch24n8 => tncc::DataService::HighProtection24KbitsLongInterleavingDepth8,
            CircuitModeType::TchS => return None,
        };
        let data_call_capacity = match basic.slots_per_frame? {
            0 => tncc::DataCallCapacity::OneTimeSlot,
            1 => tncc::DataCallCapacity::TwoTimeSlots,
            2 => tncc::DataCallCapacity::ThreeTimeSlots,
            3 => tncc::DataCallCapacity::FourTimeSlots,
            _ => return None,
        };
        Some(tncc::TnccBasicServiceInformation {
            circuit_mode_service: tncc::CircuitModeService::DataService,
            communication_type,
            data_service: Some(data_service),
            data_call_capacity: Some(data_call_capacity),
            encryption_flag,
            speech_service: None,
        })
    }
}

fn pdu_basic_from_tncc(basic: &tncc::TnccBasicServiceInformation) -> Result<BasicServiceInformation, String> {
    let communication_type = match basic.communication_type {
        tncc::CommunicationType::PointToPoint => CommunicationType::P2p,
        tncc::CommunicationType::PointToMultipoint => CommunicationType::P2Mp,
        tncc::CommunicationType::PointToMultipointAcknowledged => CommunicationType::P2MpAcked,
        tncc::CommunicationType::Broadcast => CommunicationType::Broadcast,
    };
    match basic.circuit_mode_service {
        tncc::CircuitModeService::SpeechService => {
            let speech_service = match basic
                .speech_service
                .ok_or("speech_service is required for TNCC speech basic service")?
            {
                tncc::SpeechService::TetraEncodedOneTimeslotSpeech => 0,
                tncc::SpeechService::ProprietaryEncodedOneTimeslotSpeech => 3,
            };
            Ok(BasicServiceInformation {
                circuit_mode_type: CircuitModeType::TchS,
                encryption_flag: basic.encryption_flag.as_bool(),
                communication_type,
                slots_per_frame: None,
                speech_service: Some(speech_service),
            })
        }
        tncc::CircuitModeService::DataService => {
            let circuit_mode_type = match basic.data_service.ok_or("data_service is required for TNCC data basic service")? {
                tncc::DataService::Unprotected72KbitsNoInterleaving => CircuitModeType::Tch72,
                tncc::DataService::LowProtection48KbitsShortInterleavingDepth1 => CircuitModeType::Tch48n1,
                tncc::DataService::LowProtection48KbitsMediumInterleavingDepth4 => CircuitModeType::Tch48n4,
                tncc::DataService::LowProtection48KbitsLongInterleavingDepth8 => CircuitModeType::Tch48n8,
                tncc::DataService::HighProtection24KbitsShortInterleavingDepth1 => CircuitModeType::Tch24n1,
                tncc::DataService::HighProtection24KbitsMediumInterleavingDepth4 => CircuitModeType::Tch24n4,
                tncc::DataService::HighProtection24KbitsLongInterleavingDepth8 => CircuitModeType::Tch24n8,
            };
            let slots_per_frame = match basic
                .data_call_capacity
                .ok_or("data_call_capacity is required for TNCC data basic service")?
            {
                tncc::DataCallCapacity::OneTimeSlot => 0,
                tncc::DataCallCapacity::TwoTimeSlots => 1,
                tncc::DataCallCapacity::ThreeTimeSlots => 2,
                tncc::DataCallCapacity::FourTimeSlots => 3,
            };
            Ok(BasicServiceInformation {
                circuit_mode_type,
                encryption_flag: basic.encryption_flag.as_bool(),
                communication_type,
                slots_per_frame: Some(slots_per_frame),
                speech_service: None,
            })
        }
    }
}

fn tncc_call_timeout(timeout: CallTimeout) -> tncc::CallTimeout {
    match timeout {
        CallTimeout::Infinite => tncc::CallTimeout::Infinite,
        CallTimeout::T30s => tncc::CallTimeout::Value1,
        CallTimeout::T45s => tncc::CallTimeout::Value2,
        CallTimeout::T60s => tncc::CallTimeout::Value3,
        CallTimeout::T2m => tncc::CallTimeout::Value4,
        CallTimeout::T3m => tncc::CallTimeout::Value5,
        CallTimeout::T4m => tncc::CallTimeout::Value6,
        CallTimeout::T5m => tncc::CallTimeout::Value7,
        CallTimeout::T6m => tncc::CallTimeout::Value8,
        CallTimeout::T8m => tncc::CallTimeout::Value9,
        CallTimeout::T10m => tncc::CallTimeout::Value10,
        CallTimeout::T12m => tncc::CallTimeout::Value11,
        CallTimeout::T15m => tncc::CallTimeout::Value12,
        CallTimeout::T20m => tncc::CallTimeout::Value13,
        CallTimeout::T30m => tncc::CallTimeout::Value14,
        CallTimeout::Reserved => tncc::CallTimeout::Value15,
    }
}

fn tncc_setup_timeout(timeout: CallTimeoutSetupPhase) -> tncc::CallTimeoutSetupPhase {
    match timeout {
        CallTimeoutSetupPhase::Predefined => tncc::CallTimeoutSetupPhase::PreDefined,
        CallTimeoutSetupPhase::T1s => tncc::CallTimeoutSetupPhase::Value1,
        CallTimeoutSetupPhase::T2s => tncc::CallTimeoutSetupPhase::Value2,
        CallTimeoutSetupPhase::T5s => tncc::CallTimeoutSetupPhase::Value3,
        CallTimeoutSetupPhase::T10s => tncc::CallTimeoutSetupPhase::Value4,
        CallTimeoutSetupPhase::T20s => tncc::CallTimeoutSetupPhase::Value5,
        CallTimeoutSetupPhase::T30s => tncc::CallTimeoutSetupPhase::Value6,
        CallTimeoutSetupPhase::T60s => tncc::CallTimeoutSetupPhase::Value7,
    }
}

fn tncc_transmission_grant(grant: TransmissionGrant) -> tncc::TransmissionGrant {
    match grant {
        TransmissionGrant::Granted => tncc::TransmissionGrant::TransmissionGranted,
        TransmissionGrant::NotGranted => tncc::TransmissionGrant::TransmissionNotGranted,
        TransmissionGrant::RequestQueued => tncc::TransmissionGrant::TransmissionRequestQueued,
        TransmissionGrant::GrantedToOtherUser => tncc::TransmissionGrant::TransmissionGrantedToAnotherUser,
    }
}

fn tncc_transmission_status_from_grant(grant: TransmissionGrant) -> tncc::TransmissionStatus {
    match grant {
        TransmissionGrant::Granted => tncc::TransmissionStatus::TransmissionGranted,
        TransmissionGrant::NotGranted => tncc::TransmissionStatus::TransmissionNotGranted,
        TransmissionGrant::RequestQueued => tncc::TransmissionStatus::TransmissionRequestQueued,
        TransmissionGrant::GrantedToOtherUser => tncc::TransmissionStatus::TransmissionGrantedToAnotherUser,
    }
}

fn tncc_call_status(status: tetra_pdus::cmce::enums::call_status::CallStatus) -> Option<tncc::CallStatus> {
    Some(match status {
        tetra_pdus::cmce::enums::call_status::CallStatus::Callproceeding => tncc::CallStatus::CallIsProgressing,
        tetra_pdus::cmce::enums::call_status::CallStatus::Callqueued => tncc::CallStatus::CallIsQueued,
        tetra_pdus::cmce::enums::call_status::CallStatus::Requestedsubscriberpaged => tncc::CallStatus::RequestedSubscriberIsPaged,
        tetra_pdus::cmce::enums::call_status::CallStatus::Callcontinue => tncc::CallStatus::CallContinue,
        tetra_pdus::cmce::enums::call_status::CallStatus::Hangtimeexpired => tncc::CallStatus::HangTimerHasExpired,
    })
}

fn tncc_call_status_raw(status: u8) -> Option<tncc::CallStatus> {
    Some(match status {
        0 => tncc::CallStatus::CallIsProgressing,
        1 => tncc::CallStatus::CallIsQueued,
        2 => tncc::CallStatus::RequestedSubscriberIsPaged,
        3 => tncc::CallStatus::CallContinue,
        4 => tncc::CallStatus::HangTimerHasExpired,
        _ => return None,
    })
}

fn tncc_disconnect_cause(cause: DisconnectCause) -> tncc::DisconnectCause {
    match cause {
        DisconnectCause::CauseNotDefinedOrUnknown => tncc::DisconnectCause::CauseNotDefinedOrUnknown,
        DisconnectCause::UserRequestedDisconnection => tncc::DisconnectCause::UserRequestedDisconnection,
        DisconnectCause::NonCallOwnerRequestedDisconnection => tncc::DisconnectCause::NonCallOwnerRequestedDisconnection,
        DisconnectCause::CalledPartyBusy => tncc::DisconnectCause::CalledPartyBusy,
        DisconnectCause::CalledPartyNotReachable => tncc::DisconnectCause::CalledPartyNotReachable,
        DisconnectCause::CalledPartyDoesNotSupportEncryption => tncc::DisconnectCause::CalledPartyDoesNotSupportEncryption,
        DisconnectCause::CongestionInInfrastructure => tncc::DisconnectCause::CongestionInInfrastructure,
        DisconnectCause::NotAllowedTrafficCase => tncc::DisconnectCause::NotAllowedTrafficCase,
        DisconnectCause::IncompatibleTrafficCase => tncc::DisconnectCause::IncompatibleTrafficCase,
        DisconnectCause::RequestedServiceNotAvailable => tncc::DisconnectCause::RequestedServiceNotAvailable,
        DisconnectCause::PreEmptiveUseOfResource => tncc::DisconnectCause::PreEmptiveUseOfResource,
        DisconnectCause::InvalidCallIdentifier => tncc::DisconnectCause::InvalidCallIdentifier,
        DisconnectCause::CallRejectedByTheCalledParty => tncc::DisconnectCause::CallRejectedByTheCalledParty,
        DisconnectCause::NoIdleCcEntity => tncc::DisconnectCause::NoIdleCcEntity,
        DisconnectCause::ExpiryOfTimer => tncc::DisconnectCause::ExpiryOfTimer,
        DisconnectCause::SwmiRequestedDisconnection => tncc::DisconnectCause::SwmiRequestedDisconnection,
        DisconnectCause::AcknowledgedServiceNotComplete => tncc::DisconnectCause::AcknowledgedServiceNotCompleted,
        DisconnectCause::CalledPartyRequiresEncryption => tncc::DisconnectCause::CalledPartyRequiresEncryption,
        DisconnectCause::ConcurrentSetUpNotSupported => tncc::DisconnectCause::ConcurrentSetUpNotSupported,
        DisconnectCause::CalledPartyIsUnderTheSameDmGateOfTheCallingParty => {
            tncc::DisconnectCause::CalledPartyIsUnderTheSameDmGateOfTheCallingParty
        }
        DisconnectCause::UnknownTetraIdentity
        | DisconnectCause::SsSpecificDisconnection
        | DisconnectCause::UnknownExternalSubscriberIdentity
        | DisconnectCause::CallRestorationOfTheOtherUserFailed => tncc::DisconnectCause::CauseNotDefinedOrUnknown,
    }
}

fn pdu_disconnect_cause_from_tncc(cause: tncc::DisconnectCause) -> Result<DisconnectCause, String> {
    Ok(match cause {
        tncc::DisconnectCause::CauseNotDefinedOrUnknown => DisconnectCause::CauseNotDefinedOrUnknown,
        tncc::DisconnectCause::UserRequestedDisconnection => DisconnectCause::UserRequestedDisconnection,
        tncc::DisconnectCause::NonCallOwnerRequestedDisconnection => DisconnectCause::NonCallOwnerRequestedDisconnection,
        tncc::DisconnectCause::CalledPartyBusy => DisconnectCause::CalledPartyBusy,
        tncc::DisconnectCause::CalledPartyNotReachable => DisconnectCause::CalledPartyNotReachable,
        tncc::DisconnectCause::CalledPartyDoesNotSupportEncryption => DisconnectCause::CalledPartyDoesNotSupportEncryption,
        tncc::DisconnectCause::CongestionInInfrastructure => DisconnectCause::CongestionInInfrastructure,
        tncc::DisconnectCause::NotAllowedTrafficCase => DisconnectCause::NotAllowedTrafficCase,
        tncc::DisconnectCause::IncompatibleTrafficCase => DisconnectCause::IncompatibleTrafficCase,
        tncc::DisconnectCause::RequestedServiceNotAvailable => DisconnectCause::RequestedServiceNotAvailable,
        tncc::DisconnectCause::PreEmptiveUseOfResource => DisconnectCause::PreEmptiveUseOfResource,
        tncc::DisconnectCause::InvalidCallIdentifier => DisconnectCause::InvalidCallIdentifier,
        tncc::DisconnectCause::CallRejectedByTheCalledParty => DisconnectCause::CallRejectedByTheCalledParty,
        tncc::DisconnectCause::NoIdleCcEntity => DisconnectCause::NoIdleCcEntity,
        tncc::DisconnectCause::ExpiryOfTimer => DisconnectCause::ExpiryOfTimer,
        tncc::DisconnectCause::SwmiRequestedDisconnection => DisconnectCause::SwmiRequestedDisconnection,
        tncc::DisconnectCause::AcknowledgedServiceNotCompleted => DisconnectCause::AcknowledgedServiceNotComplete,
        tncc::DisconnectCause::CalledPartyRequiresEncryption => DisconnectCause::CalledPartyRequiresEncryption,
        tncc::DisconnectCause::ConcurrentSetUpNotSupported => DisconnectCause::ConcurrentSetUpNotSupported,
        tncc::DisconnectCause::CalledPartyIsUnderTheSameDmGateOfTheCallingParty => {
            DisconnectCause::CalledPartyIsUnderTheSameDmGateOfTheCallingParty
        }
        tncc::DisconnectCause::LossOfResources | tncc::DisconnectCause::UsageMarkerFailure => {
            return Err("TNCC disconnect cause has no Phase-1 U-RELEASE/U-DISCONNECT mapping".to_string());
        }
    })
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
