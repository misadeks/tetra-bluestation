use super::*;

impl CcMsSubentity {
    pub(super) fn send_u_setup(
        &mut self,
        queue: &mut MessageQueue,
        called_party: TetraAddress,
        basic_service: BasicServiceInformation,
        hook_method_selection: bool,
        simplex_duplex_selection: bool,
        request_to_transmit: bool,
        external_subscriber_number: Option<Type3FieldGeneric>,
    ) {
        // Uplink CMCE signalling to the SwMI travels over the individual,
        // acknowledged basic link identified by the MS's own individual short
        // subscriber identity (cl. 14.5 / basic link addressing cl. 21 & 23).
        // The called identity (ISSI or GSSI) is carried *inside* the U-SETUP as
        // the Called party SSI element (cl. 14.8.28), NOT as the layer-2
        // address: a group address here would force the LLC onto the
        // unacknowledged basic link (BL-UDATA), which the SwMI rejects.
        let Some(own_issi) = self.own_issi else {
            tracing::error!("CMCE-MS: cannot originate call — own ISSI not configured; dropping U-SETUP");
            return;
        };
        let source_address = TetraAddress::new(own_issi, SsiType::Issi);
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
            external_subscriber_number,
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
                main_address: source_address,
                handle: 0,
                endpoint_id: 0,
                link_id: 0,
            },
            false,
            false,
        );
    }

    pub(super) fn send_pdu<P: UplinkPdu>(
        &self,
        queue: &mut MessageQueue,
        pdu: &P,
        route: CallRoute,
        stealing: bool,
        stealing_repeats: bool,
    ) {
        let mut sdu = BitBuffer::new_autoexpand(96);
        pdu.write(&mut sdu).expect("failed to serialize CMCE uplink PDU");
        sdu.seek(0);
        // ACK-KEY / basic-link addressing (cl. 14.5 / basic-link addressing
        // cl. 21 & 23; ACK correlation cl. 22.3.2.3): ALL uplink CMCE signalling
        // to the SwMI travels over the MS's individual, acknowledged basic link,
        // so the layer-2 `main_address` must be the MS's OWN individual ISSI. For
        // a group call `route.main_address` (inherited from `call.route`) is the
        // group number; keying an acknowledged BL-DATA on it makes the SwMI's
        // ISSI-addressed BL-ACK unmatchable → the frame (e.g. U-DISCONNECT)
        // retransmits to exhaustion. The called/group identity is carried *inside*
        // the CMCE PDU as the Called-party / Call-identifier element
        // (cl. 14.8.28 / 14.8.4), NOT as the layer-2 address. Re-key here so every
        // sender (U-DISCONNECT/-RELEASE/-CONNECT/-ALERT/-CALL-RESTORE) is
        // consistent; U-SETUP and the floor route already address own ISSI, so
        // this is a no-op for them and for individual calls. On-air neutral: the
        // MAC source address is UMAC's config ISSI regardless of this field.
        let main_address = match self.own_issi {
            Some(issi) => TetraAddress::new(issi, SsiType::Issi),
            None => route.main_address,
        };
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
                main_address,
                chan_alloc: None,
                tx_reporter: None,
            }),
        });
    }
}

pub(super) trait UplinkPdu {
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

pub(super) fn kind_from_basic_service(basic: &BasicServiceInformation) -> MsCallKind {
    match basic.communication_type {
        CommunicationType::P2p => MsCallKind::Individual,
        CommunicationType::P2Mp => MsCallKind::Group,
        CommunicationType::P2MpAcked => MsCallKind::AcknowledgedGroup,
        CommunicationType::Broadcast => MsCallKind::Broadcast,
    }
}

pub(super) fn default_speech_basic_service() -> BasicServiceInformation {
    BasicServiceInformation {
        circuit_mode_type: CircuitModeType::TchS,
        encryption_flag: false,
        communication_type: CommunicationType::P2Mp,
        slots_per_frame: None,
        speech_service: Some(0),
    }
}

pub(super) fn tncc_basic_from_pdu(basic: &BasicServiceInformation) -> Option<tncc::TnccBasicServiceInformation> {
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

pub(super) fn pdu_basic_from_tncc(basic: &tncc::TnccBasicServiceInformation) -> Result<BasicServiceInformation, String> {
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

pub(super) fn tncc_call_timeout(timeout: CallTimeout) -> tncc::CallTimeout {
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

pub(super) fn tncc_setup_timeout(timeout: CallTimeoutSetupPhase) -> tncc::CallTimeoutSetupPhase {
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

pub(super) fn tncc_transmission_grant(grant: TransmissionGrant) -> tncc::TransmissionGrant {
    match grant {
        TransmissionGrant::Granted => tncc::TransmissionGrant::TransmissionGranted,
        TransmissionGrant::NotGranted => tncc::TransmissionGrant::TransmissionNotGranted,
        TransmissionGrant::RequestQueued => tncc::TransmissionGrant::TransmissionRequestQueued,
        TransmissionGrant::GrantedToOtherUser => tncc::TransmissionGrant::TransmissionGrantedToAnotherUser,
    }
}

pub(super) fn tncc_transmission_status_from_grant(grant: TransmissionGrant) -> tncc::TransmissionStatus {
    match grant {
        TransmissionGrant::Granted => tncc::TransmissionStatus::TransmissionGranted,
        TransmissionGrant::NotGranted => tncc::TransmissionStatus::TransmissionNotGranted,
        TransmissionGrant::RequestQueued => tncc::TransmissionStatus::TransmissionRequestQueued,
        TransmissionGrant::GrantedToOtherUser => tncc::TransmissionStatus::TransmissionGrantedToAnotherUser,
    }
}

pub(super) fn tncc_call_status(status: tetra_pdus::cmce::enums::call_status::CallStatus) -> Option<tncc::CallStatus> {
    Some(match status {
        tetra_pdus::cmce::enums::call_status::CallStatus::Callproceeding => tncc::CallStatus::CallIsProgressing,
        tetra_pdus::cmce::enums::call_status::CallStatus::Callqueued => tncc::CallStatus::CallIsQueued,
        tetra_pdus::cmce::enums::call_status::CallStatus::Requestedsubscriberpaged => tncc::CallStatus::RequestedSubscriberIsPaged,
        tetra_pdus::cmce::enums::call_status::CallStatus::Callcontinue => tncc::CallStatus::CallContinue,
        tetra_pdus::cmce::enums::call_status::CallStatus::Hangtimeexpired => tncc::CallStatus::HangTimerHasExpired,
    })
}

pub(super) fn tncc_call_status_raw(status: u8) -> Option<tncc::CallStatus> {
    Some(match status {
        0 => tncc::CallStatus::CallIsProgressing,
        1 => tncc::CallStatus::CallIsQueued,
        2 => tncc::CallStatus::RequestedSubscriberIsPaged,
        3 => tncc::CallStatus::CallContinue,
        4 => tncc::CallStatus::HangTimerHasExpired,
        _ => return None,
    })
}

pub(super) fn tncc_disconnect_cause(cause: DisconnectCause) -> tncc::DisconnectCause {
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

pub(super) fn pdu_disconnect_cause_from_tncc(cause: tncc::DisconnectCause) -> Result<DisconnectCause, String> {
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
