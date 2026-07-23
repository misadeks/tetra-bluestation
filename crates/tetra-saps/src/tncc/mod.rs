//! TNCC-SAP message types (ETSI TS 100 392-2 v3.10.1, clause 11.3).
//!
//! Plane A external call-control SAP between CMCE/CC and the MS user application.
//! Field names and optionality follow the primitive parameter tables in
//! cl. 11.3.3 (Tables 11.1, 11.2 and 11.5 through 11.9); values follow
//! cl. 11.3.4. Mandatory (M) parameters are plain fields. Optional (O) and
//! conditional (C) parameters are `Option<...>` with the table condition noted.
//!
//! `call_identifier` is intentionally not part of these primitive payloads: when
//! used by the in-tree UI transport it is carried by the wrapper event/command as
//! a local TNCC-SAP instance selector, analogous to the TNMM transport handle.

use bitcode::{Decode, Encode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum AccessPriority {
    LowPriority,
    HighPriority,
    EmergencyPriority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum AreaSelection {
    AreaNotDefined,
    Area1,
    Area2,
    Area3,
    Area4,
    Area5,
    Area6,
    Area7,
    Area8,
    Area9,
    Area10,
    Area11,
    Area12,
    Area13,
    Area14,
    AllAreasInThisSystem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum CircuitModeService {
    DataService,
    SpeechService,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum CommunicationType {
    PointToPoint,
    PointToMultipoint,
    PointToMultipointAcknowledged,
    Broadcast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum DataService {
    Unprotected72KbitsNoInterleaving,
    LowProtection48KbitsShortInterleavingDepth1,
    LowProtection48KbitsMediumInterleavingDepth4,
    LowProtection48KbitsLongInterleavingDepth8,
    HighProtection24KbitsShortInterleavingDepth1,
    HighProtection24KbitsMediumInterleavingDepth4,
    HighProtection24KbitsLongInterleavingDepth8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum DataCallCapacity {
    OneTimeSlot,
    TwoTimeSlots,
    ThreeTimeSlots,
    FourTimeSlots,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum EncryptionFlag {
    ClearEndToEndTransmission,
    EncryptedEndToEndTransmission,
}

impl EncryptionFlag {
    pub fn from_bool(encrypted: bool) -> Self {
        if encrypted {
            Self::EncryptedEndToEndTransmission
        } else {
            Self::ClearEndToEndTransmission
        }
    }
    pub fn as_bool(self) -> bool {
        matches!(self, Self::EncryptedEndToEndTransmission)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum SpeechService {
    TetraEncodedOneTimeslotSpeech,
    ProprietaryEncodedOneTimeslotSpeech,
}

/// `Basic service information` parameter set (cl. 11.3.4).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccBasicServiceInformation {
    pub circuit_mode_service: CircuitModeService,
    pub communication_type: CommunicationType,
    /// Conditional: present for data service.
    pub data_service: Option<DataService>,
    /// Conditional: present for data service.
    pub data_call_capacity: Option<DataCallCapacity>,
    pub encryption_flag: EncryptionFlag,
    /// Conditional: present for speech service.
    pub speech_service: Option<SpeechService>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum CallAmalgamation {
    CallNotAmalgamated,
    CallAmalgamated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum CallOwnership {
    ACallOwner,
    NotACallOwner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum CallPriority {
    PriorityNotDefined,
    LowestPriority,
    Priority2,
    Priority3,
    Priority4,
    Priority5,
    Priority6,
    HighestNonPreEmptivePriority,
    LowestPreEmptivePriority,
    PreEmptivePriority9,
    PreEmptivePriority10,
    PreEmptivePriority11,
    PreEmptivePriority12,
    PreEmptivePriority13,
    SecondHighestPreEmptivePriority,
    EmergencyPreEmptivePriority,
}

impl CallPriority {
    pub fn from_raw(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::PriorityNotDefined,
            1 => Self::LowestPriority,
            2 => Self::Priority2,
            3 => Self::Priority3,
            4 => Self::Priority4,
            5 => Self::Priority5,
            6 => Self::Priority6,
            7 => Self::HighestNonPreEmptivePriority,
            8 => Self::LowestPreEmptivePriority,
            9 => Self::PreEmptivePriority9,
            10 => Self::PreEmptivePriority10,
            11 => Self::PreEmptivePriority11,
            12 => Self::PreEmptivePriority12,
            13 => Self::PreEmptivePriority13,
            14 => Self::SecondHighestPreEmptivePriority,
            15 => Self::EmergencyPreEmptivePriority,
            _ => return None,
        })
    }
    pub fn into_raw(self) -> u8 {
        match self {
            Self::PriorityNotDefined => 0,
            Self::LowestPriority => 1,
            Self::Priority2 => 2,
            Self::Priority3 => 3,
            Self::Priority4 => 4,
            Self::Priority5 => 5,
            Self::Priority6 => 6,
            Self::HighestNonPreEmptivePriority => 7,
            Self::LowestPreEmptivePriority => 8,
            Self::PreEmptivePriority9 => 9,
            Self::PreEmptivePriority10 => 10,
            Self::PreEmptivePriority11 => 11,
            Self::PreEmptivePriority12 => 12,
            Self::PreEmptivePriority13 => 13,
            Self::SecondHighestPreEmptivePriority => 14,
            Self::EmergencyPreEmptivePriority => 15,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum CallQueued {
    CallIsNotQueued,
    CallIsQueued,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum CallStatus {
    CallStatusUnknown,
    CallIsProgressing,
    CallIsQueued,
    RequestedSubscriberIsPaged,
    CallContinue,
    HangTimerHasExpired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum CallTimeout {
    Infinite,
    Value1,
    Value2,
    Value3,
    Value4,
    Value5,
    Value6,
    Value7,
    Value8,
    Value9,
    Value10,
    Value11,
    Value12,
    Value13,
    Value14,
    Value15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum CallTimeoutSetupPhase {
    PreDefined,
    Value1,
    Value2,
    Value3,
    Value4,
    Value5,
    Value6,
    Value7,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum CalledPartyTypeIdentifier {
    Sna,
    Ssi,
    Tsi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum ClirControl {
    NotImplementedOrUseDefaultMode,
    PresentationNotRestricted,
    PresentationRestricted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum DisconnectCause {
    CauseNotDefinedOrUnknown,
    UserRequestedDisconnection,
    NonCallOwnerRequestedDisconnection,
    CalledPartyBusy,
    CalledPartyNotReachable,
    CalledPartyDoesNotSupportEncryption,
    CongestionInInfrastructure,
    NotAllowedTrafficCase,
    IncompatibleTrafficCase,
    RequestedServiceNotAvailable,
    PreEmptiveUseOfResource,
    InvalidCallIdentifier,
    CallRejectedByTheCalledParty,
    NoIdleCcEntity,
    ExpiryOfTimer,
    SwmiRequestedDisconnection,
    AcknowledgedServiceNotCompleted,
    LossOfResources,
    UsageMarkerFailure,
    CalledPartyRequiresEncryption,
    ConcurrentSetUpNotSupported,
    CalledPartyIsUnderTheSameDmGateOfTheCallingParty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum DisconnectStatus {
    DisconnectionSuccessful,
    DisconnectionUnsuccessfulTheUserIsReleasedFromTheCall,
    DisconnectionUnsuccessfulNotTheCallOwnerTheUserIsReleasedFromTheCall,
    DisconnectionUnsuccessfulUserNotAuthorizedTheUserIsReleasedFromTheCall,
    TheUserIsReleasedFromTheCall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum DisconnectType {
    DisconnectCall,
    LeaveCallWithoutDisconnection,
    LeaveCallTemporarily,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum DtmfDigit {
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    DigitStar,
    DigitHash,
    DigitA,
    DigitB,
    DigitC,
    DigitD,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum DtmfResult {
    DtmfNotSupported,
    DtmfNotSubscribed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum DtmfToneDelimiter {
    Dtmf,
    ToneEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum HookMethodSelection {
    NoHookSignallingDirectThroughConnect,
    HookOnHookOffSignallingOrCallAcceptanceSignalling,
}

impl HookMethodSelection {
    pub fn from_bool(hook: bool) -> Self {
        if hook {
            Self::HookOnHookOffSignallingOrCallAcceptanceSignalling
        } else {
            Self::NoHookSignallingDirectThroughConnect
        }
    }
    pub fn as_bool(self) -> bool {
        matches!(self, Self::HookOnHookOffSignallingOrCallAcceptanceSignalling)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum RequestToTransmitSendData {
    RequestToTransmitSendData,
    RequestThatOtherMsLsMayTransmitSendData,
}

impl RequestToTransmitSendData {
    pub fn as_bool(self) -> bool {
        matches!(self, Self::RequestToTransmitSendData)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum SimplexDuplexSelection {
    SimplexOperation,
    DuplexOperation,
}

impl SimplexDuplexSelection {
    pub fn from_bool(duplex: bool) -> Self {
        if duplex { Self::DuplexOperation } else { Self::SimplexOperation }
    }
    pub fn as_bool(self) -> bool {
        matches!(self, Self::DuplexOperation)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum TrafficStealing {
    DoNotStealTraffic,
    StealTraffic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum TransmissionCondition {
    RequestToTransmit,
    TransmissionCeased,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum TransmissionGrant {
    TransmissionGranted,
    TransmissionNotGranted,
    TransmissionRequestQueued,
    TransmissionGrantedToAnotherUser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum TransmissionRequestPermission {
    AllowedToRequestForTransmission,
    NotAllowedToRequestForTransmission,
}

impl TransmissionRequestPermission {
    pub fn from_bool(allowed: bool) -> Self {
        if allowed {
            Self::AllowedToRequestForTransmission
        } else {
            Self::NotAllowedToRequestForTransmission
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum TransmissionStatus {
    TransmissionCeased,
    TransmissionGranted,
    TransmissionNotGranted,
    TransmissionRequestQueued,
    TransmissionGrantedToAnotherUser,
    TransmissionInterrupt,
    TransmissionWait,
    TransmissionRequestFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum TxDemandPriority {
    LowPriority,
    HighPriority,
    PreEmptivePriority,
    EmergencyPreEmptivePriority,
}

impl TxDemandPriority {
    pub fn into_raw(self) -> u8 {
        match self {
            Self::LowPriority => 0,
            Self::HighPriority => 1,
            Self::PreEmptivePriority => 2,
            Self::EmergencyPreEmptivePriority => 3,
        }
    }
}

/// TNCC-ALERT indication parameters (Table 11.1, cl. 11.3.3.1).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccAlertIndication {
    pub basic_service_information_offered: Option<TnccBasicServiceInformation>,
    pub call_queued: Option<CallQueued>,
    pub call_time_out_set_up_phase: CallTimeoutSetupPhase,
    pub notification_indicator: Option<u8>,
    pub simplex_duplex: SimplexDuplexSelection,
}

/// TNCC-COMPLETE request parameters (Table 11.2, cl. 11.3.3.2).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccCompleteRequest {
    pub access_priority: Option<AccessPriority>,
    pub basic_service_information_offered: Option<TnccBasicServiceInformation>,
    pub hook_method: HookMethodSelection,
    pub simplex_duplex: SimplexDuplexSelection,
    pub traffic_stealing: Option<TrafficStealing>,
}

/// TNCC-COMPLETE indication parameters (Table 11.2, cl. 11.3.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccCompleteIndication {
    pub call_time_out: CallTimeout,
    pub notification_indicator: Option<u8>,
    pub transmission_grant: TransmissionGrant,
    pub transmission_request_permission: TransmissionRequestPermission,
    pub transmission_status: TransmissionStatus,
}

/// TNCC-COMPLETE confirm parameters (Table 11.2, cl. 11.3.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccCompleteConfirm {
    pub call_time_out: CallTimeout,
    pub notification_indicator: Option<u8>,
    pub transmission_grant: TransmissionGrant,
    pub transmission_request_permission: TransmissionRequestPermission,
    pub transmission_status: TransmissionStatus,
}

/// TNCC-DTMF request parameters (Table 11.3, cl. 11.3.3.3).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccDtmfRequest {
    pub access_priority: Option<AccessPriority>,
    pub dtmf_tone_delimiter: DtmfToneDelimiter,
    /// Conditional: present when DTMF tone delimiter = "DTMF" (Table 11.3 NOTE 1).
    pub number_of_dtmf_digits: Option<u8>,
    /// Conditional: present when DTMF tone delimiter = "DTMF" (Table 11.3 NOTE 1).
    pub dtmf_digits: Option<Vec<DtmfDigit>>,
    pub traffic_stealing: Option<TrafficStealing>,
}

/// TNCC-DTMF indication parameters (Table 11.3, cl. 11.3.3.3).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccDtmfIndication {
    pub dtmf_tone_delimiter: Option<DtmfToneDelimiter>,
    /// Conditional: present when DTMF tone delimiter is absent (Table 11.3 NOTE 3).
    pub dtmf_result: Option<DtmfResult>,
    /// Conditional: present when DTMF tone delimiter is present and = "DTMF" (Table 11.3 NOTE 2).
    pub number_of_dtmf_digits: Option<u8>,
    /// Conditional: present when DTMF tone delimiter is present and = "DTMF" (Table 11.3 NOTE 2).
    pub dtmf_digits: Option<Vec<DtmfDigit>>,
}

/// TNCC-MODIFY request parameters (Table 11.4, cl. 11.3.3.4).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccModifyRequest {
    pub access_priority: Option<AccessPriority>,
    pub basic_service_information_new: Option<TnccBasicServiceInformation>,
    pub simplex_duplex: Option<SimplexDuplexSelection>,
    pub traffic_stealing: Option<TrafficStealing>,
}

/// TNCC-MODIFY indication parameters (Table 11.4, cl. 11.3.3.4).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccModifyIndication {
    pub basic_service_information_new: Option<TnccBasicServiceInformation>,
    pub call_time_out: Option<CallTimeout>,
    pub notification_indicator: Option<u8>,
    pub simplex_duplex: Option<SimplexDuplexSelection>,
}

/// TNCC-NOTIFY indication parameters (Table 11.5, cl. 11.3.3.5).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccNotifyIndication {
    pub call_status: Option<CallStatus>,
    pub call_time_out_in_set_up_phase: Option<CallTimeoutSetupPhase>,
    pub call_time_out: Option<CallTimeout>,
    pub call_ownership: Option<CallOwnership>,
    pub notification_indicator: Option<u8>,
    pub poll_response_percentage: Option<u8>,
    pub poll_response_number: Option<u8>,
    pub poll_response_addresses: Option<Vec<u64>>,
    pub poll_request: Option<bool>,
}

/// TNCC-PROCEED indication parameters (Table 11.6, cl. 11.3.3.6).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccProceedIndication {
    pub basic_service_information_offered: Option<TnccBasicServiceInformation>,
    pub call_status: Option<CallStatus>,
    pub hook_method: Option<HookMethodSelection>,
    pub notification_indicator: Option<u8>,
    pub simplex_duplex: Option<SimplexDuplexSelection>,
}

/// TNCC-RELEASE request parameters (Table 11.7, cl. 11.3.3.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccReleaseRequest {
    pub access_priority: Option<AccessPriority>,
    pub disconnect_cause: DisconnectCause,
    pub disconnect_type: DisconnectType,
    pub traffic_stealing: Option<TrafficStealing>,
}

/// TNCC-RELEASE indication parameters (Table 11.7, cl. 11.3.3.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccReleaseIndication {
    pub disconnect_cause: DisconnectCause,
    pub notification_indicator: Option<u8>,
}

/// TNCC-RELEASE confirm parameters (Table 11.7, cl. 11.3.3.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccReleaseConfirm {
    pub disconnect_cause: DisconnectCause,
    pub disconnect_status: DisconnectStatus,
    pub notification_indicator: Option<u8>,
}

/// TNCC-SETUP request parameters (Table 11.8, cl. 11.3.3.8).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccSetupRequest {
    pub access_priority: Option<AccessPriority>,
    pub area_selection: Option<AreaSelection>,
    pub basic_service_information: TnccBasicServiceInformation,
    pub call_priority: CallPriority,
    pub called_party_type_identifier: CalledPartyTypeIdentifier,
    pub called_party_sna: Option<u32>,
    pub called_party_ssi: Option<u32>,
    pub called_party_extension: Option<u32>,
    pub external_subscriber_number_called: Option<String>,
    pub clir_control: Option<ClirControl>,
    pub hook_method_selection: HookMethodSelection,
    pub request_to_transmit_send_data: RequestToTransmitSendData,
    pub simplex_duplex_selection: SimplexDuplexSelection,
    pub traffic_stealing: Option<TrafficStealing>,
}

/// TNCC-SETUP indication parameters (Table 11.8, cl. 11.3.3.8).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccSetupIndication {
    pub basic_service_information: TnccBasicServiceInformation,
    pub call_priority: CallPriority,
    pub call_time_out: CallTimeout,
    pub called_party_ssi: u32,
    pub called_party_extension: Option<u32>,
    pub calling_party_ssi: Option<u32>,
    pub calling_party_extension: Option<u32>,
    pub external_subscriber_number_calling: Option<String>,
    pub clir_control: Option<ClirControl>,
    pub hook_method_selection: HookMethodSelection,
    pub notification_indicator: Option<u8>,
    pub simplex_duplex_selection: SimplexDuplexSelection,
    pub transmission_grant: TransmissionGrant,
    pub transmission_request_permission: TransmissionRequestPermission,
}

/// TNCC-SETUP response parameters (Table 11.8, cl. 11.3.3.8).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccSetupResponse {
    pub access_priority: Option<AccessPriority>,
    pub basic_service_information: Option<TnccBasicServiceInformation>,
    pub clir_control: Option<ClirControl>,
    pub hook_method_selection: HookMethodSelection,
    pub simplex_duplex_selection: SimplexDuplexSelection,
    pub traffic_stealing: Option<TrafficStealing>,
}

/// TNCC-SETUP confirm parameters (Table 11.8, cl. 11.3.3.8).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccSetupConfirm {
    pub basic_service_information: TnccBasicServiceInformation,
    pub call_priority: Option<CallPriority>,
    pub call_ownership: CallOwnership,
    pub call_amalgamation: CallAmalgamation,
    pub call_time_out: CallTimeout,
    pub hook_method_selection: HookMethodSelection,
    pub notification_indicator: Option<u8>,
    pub simplex_duplex_selection: SimplexDuplexSelection,
    pub transmission_grant: TransmissionGrant,
    pub transmission_request_permission: TransmissionRequestPermission,
}

/// TNCC-TX request parameters (Table 11.9, cl. 11.3.3.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccTxRequest {
    pub access_priority: Option<AccessPriority>,
    pub encryption_flag: EncryptionFlag,
    pub traffic_stealing: Option<TrafficStealing>,
    pub transmission_condition: TransmissionCondition,
    pub tx_demand_priority: TxDemandPriority,
}

/// TNCC-TX indication parameters (Table 11.9, cl. 11.3.3.9).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccTxIndication {
    pub encryption_flag: EncryptionFlag,
    pub notification_indicator: Option<u8>,
    pub transmitting_party_ssi: Option<u32>,
    pub transmitting_party_extension: Option<u32>,
    pub external_subscriber_number: Option<String>,
    pub transmit_request_permission: TransmissionRequestPermission,
    pub transmission_status: TransmissionStatus,
}

/// TNCC-TX confirm parameters (Table 11.9, cl. 11.3.3.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnccTxConfirm {
    pub encryption_flag: EncryptionFlag,
    pub transmit_request_permission: TransmissionRequestPermission,
    pub transmission_status: TransmissionStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tncc_setup_request_json_and_bitcode_roundtrip() {
        let req = TnccSetupRequest {
            access_priority: Some(AccessPriority::LowPriority),
            area_selection: Some(AreaSelection::AreaNotDefined),
            basic_service_information: TnccBasicServiceInformation {
                circuit_mode_service: CircuitModeService::SpeechService,
                communication_type: CommunicationType::PointToMultipoint,
                data_service: None,
                data_call_capacity: None,
                encryption_flag: EncryptionFlag::ClearEndToEndTransmission,
                speech_service: Some(SpeechService::TetraEncodedOneTimeslotSpeech),
            },
            call_priority: CallPriority::PriorityNotDefined,
            called_party_type_identifier: CalledPartyTypeIdentifier::Ssi,
            called_party_sna: None,
            called_party_ssi: Some(91),
            called_party_extension: None,
            external_subscriber_number_called: None,
            clir_control: Some(ClirControl::NotImplementedOrUseDefaultMode),
            hook_method_selection: HookMethodSelection::NoHookSignallingDirectThroughConnect,
            request_to_transmit_send_data: RequestToTransmitSendData::RequestToTransmitSendData,
            simplex_duplex_selection: SimplexDuplexSelection::SimplexOperation,
            traffic_stealing: Some(TrafficStealing::DoNotStealTraffic),
        };
        let json = serde_json::to_vec(&req).unwrap();
        assert_eq!(serde_json::from_slice::<TnccSetupRequest>(&json).unwrap(), req);
        let bc = bitcode::encode(&req);
        assert_eq!(bitcode::decode::<TnccSetupRequest>(&bc).unwrap(), req);
    }

    #[test]
    fn tncc_dtmf_and_modify_json_and_bitcode_roundtrip() {
        let dtmf = TnccDtmfRequest {
            access_priority: Some(AccessPriority::LowPriority),
            dtmf_tone_delimiter: DtmfToneDelimiter::Dtmf,
            number_of_dtmf_digits: Some(2),
            dtmf_digits: Some(vec![DtmfDigit::Digit1, DtmfDigit::Digit2]),
            traffic_stealing: None,
        };
        let json = serde_json::to_vec(&dtmf).unwrap();
        assert_eq!(serde_json::from_slice::<TnccDtmfRequest>(&json).unwrap(), dtmf);
        let bc = bitcode::encode(&dtmf);
        assert_eq!(bitcode::decode::<TnccDtmfRequest>(&bc).unwrap(), dtmf);

        let modify = TnccModifyIndication {
            basic_service_information_new: None,
            call_time_out: Some(CallTimeout::Value1),
            notification_indicator: Some(1),
            simplex_duplex: Some(SimplexDuplexSelection::SimplexOperation),
        };
        let json = serde_json::to_vec(&modify).unwrap();
        assert_eq!(serde_json::from_slice::<TnccModifyIndication>(&json).unwrap(), modify);
        let bc = bitcode::encode(&modify);
        assert_eq!(bitcode::decode::<TnccModifyIndication>(&bc).unwrap(), modify);
    }
}
