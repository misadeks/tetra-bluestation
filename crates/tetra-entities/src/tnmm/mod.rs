//! TNMM-SAP message types (ETSI TS 100 392-2 v3.10.1, clause 15.3).
//!
//! This module is **Plane A** of the MS external interface: the standardized
//! TETRA Network Mobility Management Service Access Point (TNMM-SAP) between
//! Mobility Management (MM) and the MS user application (cl. 15.3). Here the
//! "user application" is an external UI process reached over the in-tree
//! telemetry (OUTBOUND: indications/confirms) and control (INBOUND: requests)
//! transports.
//!
//! Every type name, field name and enumerant in this module traces **verbatim**
//! to the spec:
//! - the primitive parameter tables of cl. 15.3.3 (Tables 15.1–15.7), and
//! - the value enumerations of cl. 15.3.4.
//!
//! No behaviour is invented here — these are pure message/value definitions.
//! Mandatory (M) parameters are plain fields; optional (O) and conditional (C)
//! parameters are `Option<...>`, with the spec condition noted in a doc comment.
//!
//! Out of scope (documented deferrals, per the project plan): the
//! TNMM-DISABLING / TNMM-ENABLING primitives are defined in ETSI EN 300 392-7
//! (Part 7, security) — see cl. 15.3.3.3 / 15.3.3.4 — and are not modelled here.
//! The energy-saving primitives (cl. 15.3.3.5) are defined below but dormant,
//! because the underlying energy-economy procedure (cl. 16.7) is not implemented.

use bitcode::{Decode, Encode};
use serde::{Deserialize, Serialize};

// ===========================================================================
// 15.3.4 Value enumerations (verbatim)
// ===========================================================================

/// `Cell load` (cl. 15.3.4). Used by the TNMM-STATUS primitive (Table 15.7).
///
/// Verbatim value list from cl. 15.3.4 (order preserved). `Reserved` is kept as
/// a distinct enumerant so the ordering — and any future raw-value mapping —
/// matches the specification exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum CellLoad {
    CellLoadUnknown,
    LowCellLoadForCa,
    MediumCellLoadForCa,
    HighCellLoadForCa,
    CellLoadInformationIsNotAvailableForCa,
    LowTchLoadDa,
    Reserved,
    HighTchLoadDa,
    CellLoadInformationIsNotAvailableForTchInDa,
    LowPdchLoadDa,
    HighPdchLoadDa,
    CellLoadInformationIsNotAvailableForPdchInDa,
    LowCchSdsLoadDa,
    HighCchSdsLoadDa,
    CellLoadInformationIsNotAvailableForCchSdsInDa,
}

/// `Cell type (where registered)` (cl. 15.3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum CellType {
    CaCell,
    DaCell,
}

/// `Class of usage` (cl. 15.3.4). Class of Usage 1..8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum ClassOfUsage {
    ClassOfUsage1,
    ClassOfUsage2,
    ClassOfUsage3,
    ClassOfUsage4,
    ClassOfUsage5,
    ClassOfUsage6,
    ClassOfUsage7,
    ClassOfUsage8,
}

/// `Disable status` (cl. 15.3.4). Used by TNMM-SERVICE (Table 15.6) and
/// TNMM-STATUS (Table 15.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum DisableStatus {
    Enabled,
    TemporaryDisabled,
    PermanentlyDisabled,
}

/// `Dual watch` (cl. 15.3.4). Used by TNMM-STATUS (Table 15.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum DualWatch {
    StartingDualWatchMode,
    ModifyOrResumeDualWatchMode,
    DualWatchModeAccepted,
    DualWatchModeRejected,
    DualWatchModeNotSupported,
    TerminatingDualWatchMode,
    TerminatingDualWatchModeResponse,
    DualWatchEnergyEconomyGroupChangedBySwmi,
    DualWatchModeTerminatedBySwmi,
}

/// `Direct mode` (cl. 15.3.4). Used by TNMM-STATUS (Table 15.7, Request).
///
/// Per the spec NOTE, a return to trunking mode is a normal registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum DirectMode {
    StartOfDirectModeOperation,
}

/// `Energy economy mode` (cl. 15.3.4).
///
/// Defined verbatim but dormant: the energy-economy procedure (cl. 16.7) is not
/// implemented, so these values are not currently produced or consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum EnergyEconomyMode {
    StayAlive,
    EnergyEconomyMode1,
    EnergyEconomyMode2,
    EnergyEconomyMode3,
    EnergyEconomyMode4,
    EnergyEconomyMode5,
    EnergyEconomyMode6,
    EnergyEconomyMode7,
}

/// `Energy economy mode status` (cl. 15.3.4). Dormant (see [`EnergyEconomyMode`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum EnergyEconomyModeStatus {
    Accepted,
    Rejected,
}

/// `Group identity Attach/detach type identifier` (GITI) (cl. 15.3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum GroupIdentityAttachDetachTypeIdentifier {
    Attachment,
    Detachment,
}

/// `Group identity lifetime` (cl. 15.3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum GroupIdentityLifetime {
    PermanentAttachmentNotNeeded,
    AttachmentNeededForNextItsiAttach,
    AttachmentNotAllowedAfterNextItsiAttach,
    AttachmentNeededForNextLocationUpdate,
}

/// `Group identity detachment` (cl. 15.3.4). Carried as the detachment reason in
/// the `Group identities` parameter (Table 15.8) when GITI = Detachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum GroupIdentityDetachment {
    PermanentlyDetached,
    Temporary1Detached,
    Temporary2Detached,
    UnknownGroupIdentity,
}

/// `Group identity detachment request` (cl. 15.3.4). Carried in the
/// `Group identity request` parameter (Table 15.9) when GITI = Detachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum GroupIdentityDetachmentRequest {
    UserInitiatedDetachment,
}

/// `Group identity report` (cl. 15.3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum GroupIdentityReport {
    ReportRequested,
    ReportNotRequested,
}

/// `Group identity attach/detach mode` (cl. 15.3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum GroupIdentityAttachDetachMode {
    Amendment,
    DetachTheCurrentlyActiveGroupIdentities,
}

/// `Registration reject cause` (cl. 15.3.4).
///
/// Verbatim value list. Note this is the TNMM-SAP reject-cause enumeration, a
/// strict subset of the on-air `Reject cause` element (cl. 16.10.42): the
/// on-air "use CA/DA cell not permitted" causes have no TNMM-SAP enumerant and
/// therefore cannot be reported through this parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum RegistrationRejectCause {
    ItsiUnknown,
    IllegalMs,
    LaNotAllowed,
    LaUnknown,
    NetworkFailure,
    Congestion,
    ForwardRegistrationFailure,
    ServiceNotSubscribed,
    MandatoryElementError,
    MessageConsistencyError,
    RoamingNotSupported,
    MigrationNotSupported,
    NoCipherKsg,
    IdentifiedCipherKsgNotSupported,
    RequestedCipherKeyTypeNotAvailable,
    IdentifiedCipherKeyNotAvailable,
    CipheringRequired,
    AuthenticationFailure,
}

/// `Registration status` (cl. 15.3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum RegistrationStatus {
    Success,
    Failure,
    LaRegistrationExpired,
    NoPreferredCellFound,
    NoPermittedCellTypes,
}

/// `Registration type` (cl. 15.3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum RegistrationType {
    PeriodicRegistration,
    RegistrationToIndicatedCell,
}

/// `Service status` (cl. 15.3.4). Used by TNMM-SERVICE (Table 15.6) and
/// TNMM-STATUS (Table 15.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum ServiceStatus {
    InService,
    InGracefulServiceDegradationMode,
    InServiceWaitingForRegistration,
    OutOfService,
    MmBusy,
    MmIdle,
}

/// `Transfer result` (cl. 15.3.4). Used by TNMM-REPORT (Table 15.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum TransferResult {
    TransferSuccessfulDone,
    TransferFail,
}

// ===========================================================================
// Composite parameters
// ===========================================================================

/// One entry of the `Group identities` parameter (Table 15.8, cl. 15.3.4).
///
/// GTSI and the attach/detach type identifier (GITI) are Mandatory. Per the
/// table NOTE the remaining members are Conditional on GITI:
/// - GITI = Attachment: `Group Identity Lifetime` + `Class of Usage`;
/// - GITI = Detachment: `Group Identity Detachment Reason`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct GroupIdentity {
    /// GTSI — Group TETRA Subscriber Identity (cl. 15.3.4): the GSSI qualified
    /// by its MNI (MCC+MNC). Carried as the full 48-bit value (MNI << 24 | GSSI).
    pub gtsi: u64,
    /// `Group Identity Attach/detach Type Identifier` (GITI).
    pub group_identity_attach_detach_type_identifier: GroupIdentityAttachDetachTypeIdentifier,
    /// `Group Identity Lifetime` — conditional: present when GITI = Attachment.
    pub group_identity_lifetime: Option<GroupIdentityLifetime>,
    /// `Class of Usage` — conditional: present when GITI = Attachment.
    pub class_of_usage: Option<ClassOfUsage>,
    /// `Group Identity Detachment Reason` — conditional: present when
    /// GITI = Detachment.
    pub group_identity_detachment_reason: Option<GroupIdentityDetachment>,
}

/// The `Group identity request` parameter (Table 15.9, cl. 15.3.4).
///
/// GTSI and GITI are Mandatory. Per the table NOTE:
/// - GITI = Attachment: `Class of Usage`;
/// - GITI = Detachment: `Group Identity Detachment Request`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct GroupIdentityRequest {
    /// GTSI — Group TETRA Subscriber Identity (see [`GroupIdentity::gtsi`]).
    pub gtsi: u64,
    /// `Group Identity Attach/detach Type Identifier` (GITI).
    pub group_identity_attach_detach_type_identifier: GroupIdentityAttachDetachTypeIdentifier,
    /// `Class of Usage` — conditional: present when GITI = Attachment.
    pub class_of_usage: Option<ClassOfUsage>,
    /// `Group Identity Detachment Request` — conditional: present when
    /// GITI = Detachment.
    pub group_identity_detachment_request: Option<GroupIdentityDetachmentRequest>,
}

// ===========================================================================
// 15.3.3 Primitive parameter sets — OUTBOUND primitives (indication / confirm)
//
// These carry the parameters marked for the Indication / Confirm columns of the
// respective tables. Request-column parameters (INBOUND) are added with the
// control-command types in Phase T2.
// ===========================================================================

/// TNMM-REGISTRATION **indication** parameters (Table 15.5, Indication column).
///
/// Emitted when MM has carried out a registration procedure (successfully or
/// unsuccessfully) or when LA registration has expired (cl. 15.3.3.7).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmRegistrationIndication {
    /// `Registration status` — M.
    pub registration_status: RegistrationStatus,
    /// `Registration reject cause` — C: present when Registration Status =
    /// "failure" (Table 15.5, NOTE 1).
    pub registration_reject_cause: Option<RegistrationRejectCause>,
    /// `Cell type (where registered)` — M.
    pub cell_type_where_registered: CellType,
    /// `LA (where registered)` — M.
    pub la_where_registered: u16,
    /// `MCC (where registered)` — M.
    pub mcc_where_registered: u16,
    /// `MNC (where registered)` — M.
    pub mnc_where_registered: u16,
    /// `SwMI's required cell types` — O: present when Registration Status =
    /// "no permitted cell types" (Table 15.5, NOTE 5). Indication column only.
    pub swmis_required_cell_types: Option<Vec<CellType>>,
    /// `Energy economy mode` — O (dormant).
    pub energy_economy_mode: Option<EnergyEconomyMode>,
    /// `Energy economy mode status` — O (dormant).
    pub energy_economy_mode_status: Option<EnergyEconomyModeStatus>,
    /// `Group identities` — O.
    pub group_identities: Option<Vec<GroupIdentity>>,
    /// `Group identity attach/detach mode` — O.
    pub group_identity_attach_detach_mode: Option<GroupIdentityAttachDetachMode>,
}

/// TNMM-REGISTRATION **confirm** parameters (Table 15.5, Confirm column).
///
/// Informs the user application that registration is confirmed / the MS is ready
/// for use (cl. 15.3.3.7). Identical to the indication except the Indication-only
/// `SwMI's required cell types` parameter is absent.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmRegistrationConfirm {
    /// `Registration status` — M.
    pub registration_status: RegistrationStatus,
    /// `Registration reject cause` — C: present when Registration Status =
    /// "failure" (Table 15.5, NOTE 1).
    pub registration_reject_cause: Option<RegistrationRejectCause>,
    /// `Cell type (where registered)` — M.
    pub cell_type_where_registered: CellType,
    /// `LA (where registered)` — M.
    pub la_where_registered: u16,
    /// `MCC (where registered)` — M.
    pub mcc_where_registered: u16,
    /// `MNC (where registered)` — M.
    pub mnc_where_registered: u16,
    /// `Energy economy mode` — O (dormant).
    pub energy_economy_mode: Option<EnergyEconomyMode>,
    /// `Energy economy mode status` — O (dormant).
    pub energy_economy_mode_status: Option<EnergyEconomyModeStatus>,
    /// `Group identities` — O.
    pub group_identities: Option<Vec<GroupIdentity>>,
    /// `Group identity attach/detach mode` — O.
    pub group_identity_attach_detach_mode: Option<GroupIdentityAttachDetachMode>,
}

/// TNMM-ATTACH DETACH GROUP IDENTITY **indication** parameters (Table 15.1,
/// Indication column). Sent when the SwMI has (de)activated one or more defined
/// group identities in the MS/LS (cl. 15.3.3.1).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmAttachDetachGroupIdentityIndication {
    /// `Group identities` — M.
    pub group_identities: Vec<GroupIdentity>,
}

/// TNMM-ATTACH DETACH GROUP IDENTITY **confirm** parameters (Table 15.1,
/// Confirm column).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmAttachDetachGroupIdentityConfirm {
    /// `Group identity attach detach mode` — M.
    pub group_identity_attach_detach_mode: GroupIdentityAttachDetachMode,
    /// `Group identity report` — O.
    pub group_identity_report: Option<GroupIdentityReport>,
    /// `Group identities` — M.
    pub group_identities: Vec<GroupIdentity>,
}

/// TNMM-REPORT **indication** parameters (Table 15.4). Informs the user
/// application of a successful or unsuccessful transmission of U-ITSI DETACH
/// (cl. 15.3.3.6).
///
/// **Emitted by MM during the de-registration drain (cl. 16.6.1).** MM creates a
/// [`TxReporter`](tetra_core::TxReporter) for the shutdown U-ITSI DETACH and
/// shares it with the LLC acknowledged-mode outbound entry (cl. 22.3.2.3). The
/// LLC/MAC drive its state as the burst is actually transmitted (random-access
/// success), acknowledged, discarded (congestion), or lost (acknowledged
/// transfer gave up). MM polls the receipt each slot of the detach drain and
/// emits this indication exactly once with the resolved `TransferResult`:
/// `TransferSuccessfulDone` when acknowledged/transmitted, `TransferFail` when
/// discarded or lost. The earlier LLC head-of-line-blocking defect that made the
/// detach fail to transmit was fixed in the acknowledged-mode uplink wedge fix
/// (early-ack acceptance in MS mode), so the receipt now carries a real result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmReportIndication {
    /// `Transfer result` — M.
    pub transfer_result: TransferResult,
}

/// TNMM-SERVICE **indication** parameters (Table 15.6). Reflects the service
/// state of the MS (cl. 15.3.3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmServiceIndication {
    /// `Service status` — M.
    pub service_status: ServiceStatus,
    /// `Disable status` — M.
    pub disable_status: DisableStatus,
}

/// TNMM-STATUS **indication** parameters (Table 15.7, Indication column).
/// Indicates a mobility management service or action request (cl. 15.3.3.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmStatusIndication {
    /// `Service status` — M.
    pub service_status: ServiceStatus,
    /// `Disable status` — M.
    pub disable_status: DisableStatus,
    /// `Dual watch` — O.
    pub dual_watch: Option<DualWatch>,
    /// `Energy economy mode` — O (dormant).
    pub energy_economy_mode: Option<EnergyEconomyMode>,
    /// `Cell load` — O.
    pub cell_load: Option<CellLoad>,
}

/// TNMM-STATUS **confirm** parameters (Table 15.7, Confirm column). Indicates the
/// result of a request (cl. 15.3.3.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmStatusConfirm {
    /// `Dual watch` — O.
    pub dual_watch: Option<DualWatch>,
    /// `Energy economy mode` — O (dormant).
    pub energy_economy_mode: Option<EnergyEconomyMode>,
}

/// TNMM-ENERGY SAVING **indication** parameters (Table 15.3, Indication column).
///
/// Dormant: the energy-economy procedure (cl. 16.7) is not implemented, so this
/// primitive is defined for completeness but never emitted (cl. 15.3.3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmEnergySavingIndication {
    /// `Energy economy mode` — O.
    pub energy_economy_mode: Option<EnergyEconomyMode>,
    /// `Energy economy mode status` — O.
    pub energy_economy_mode_status: Option<EnergyEconomyModeStatus>,
}

/// TNMM-ENERGY SAVING **confirm** parameters (Table 15.3, Confirm column).
/// Dormant (see [`TnmmEnergySavingIndication`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmEnergySavingConfirm {
    /// `Energy economy mode` — M.
    pub energy_economy_mode: EnergyEconomyMode,
    /// `Energy economy mode status` — M.
    pub energy_economy_mode_status: EnergyEconomyModeStatus,
}

// ===========================================================================
// 15.3.3 Primitive parameter sets — INBOUND primitives (request)
//
// These carry the parameters marked for the Request column of the respective
// tables (cl. 15.3.3). They are transported over the control channel as
// `ControlCommand` variants (Phase T2). Request-only parameters that select
// features not implemented in this stack (preferred cell/LA/MCC/MNC lists,
// energy economy) are modelled for completeness with their spec-mandated
// optionality, but are not acted upon (documented at the handler).
// ===========================================================================

/// TNMM-REGISTRATION **request** parameters (Table 15.5, Request column).
///
/// Used by the user application to initiate attachment and registration of the
/// terminal (cl. 15.3.3.7).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmRegistrationRequest {
    /// `Registration type` — M.
    pub registration_type: RegistrationType,
    /// `Required cell type list` — O.
    pub required_cell_type_list: Option<Vec<CellType>>,
    /// `Preferred cell type list` — O (Table 15.5, NOTE 4: if present with the
    /// required list, must be a subset of it).
    pub preferred_cell_type_list: Option<Vec<CellType>>,
    /// `Preferred LA list` — O (Table 15.5, NOTE 2).
    pub preferred_la_list: Option<Vec<u16>>,
    /// `Preferred MCC list` — O (Table 15.5, NOTE 3).
    pub preferred_mcc_list: Option<Vec<u16>>,
    /// `Preferred MNC list` — O (Table 15.5, NOTE 3).
    pub preferred_mnc_list: Option<Vec<u16>>,
    /// `ISSI` — M.
    pub issi: u32,
    /// `MCC (of the ISSI)` — M.
    pub mcc_of_issi: u16,
    /// `MNC (of the ISSI)` — M.
    pub mnc_of_issi: u16,
    /// `Energy economy mode` — O (dormant).
    pub energy_economy_mode: Option<EnergyEconomyMode>,
    /// `Group identity request` — O.
    pub group_identity_request: Option<Vec<GroupIdentityRequest>>,
    /// `Group identity attach/detach mode` — O.
    pub group_identity_attach_detach_mode: Option<GroupIdentityAttachDetachMode>,
}

/// TNMM-DEREGISTRATION **request** parameters (Table 15.2).
///
/// Cancels the registration (log-off / ITSI removal / power-off) (cl. 15.3.3.2).
/// Per the table NOTE, when all attached ITSIs are detached the parameters need
/// not be present — hence all are Optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmDeregistrationRequest {
    /// `ISSI` — O.
    pub issi: Option<u32>,
    /// `MCC` — O.
    pub mcc: Option<u16>,
    /// `MNC` — O.
    pub mnc: Option<u16>,
}

/// TNMM-ATTACH DETACH GROUP IDENTITY **request** parameters (Table 15.1,
/// Request column). Activates/deactivates group identities or asks for a group
/// report (cl. 15.3.3.1).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmAttachDetachGroupIdentityRequest {
    /// `Group identity attach detach mode` — M.
    pub group_identity_attach_detach_mode: GroupIdentityAttachDetachMode,
    /// `Group identity request` — M.
    pub group_identity_request: Vec<GroupIdentityRequest>,
    /// `Group identity report` — O.
    pub group_identity_report: Option<GroupIdentityReport>,
}

/// TNMM-STATUS **request** parameters (Table 15.7, Request column). Requests
/// various mobility management services (cl. 15.3.3.9).
///
/// Dormant: `direct_mode` (DMO), `dual_watch` and `energy_economy_mode` all
/// select features not implemented in this stack, so a STATUS request is
/// accepted but not acted upon (documented at the handler).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmStatusRequest {
    /// `Direct mode` — O.
    pub direct_mode: Option<DirectMode>,
    /// `Dual watch` — O.
    pub dual_watch: Option<DualWatch>,
    /// `Energy economy mode` — O (dormant).
    pub energy_economy_mode: Option<EnergyEconomyMode>,
}

/// TNMM-ENERGY SAVING **request** parameters (Table 15.3, Request column).
/// Dormant (see [`TnmmEnergySavingIndication`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnmmEnergySavingRequest {
    /// `Energy economy mode` — M.
    pub energy_economy_mode: EnergyEconomyMode,
}
