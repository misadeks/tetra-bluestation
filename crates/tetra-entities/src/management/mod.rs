//! MS management / provisioning message types (**NON-STANDARD**, Plane B).
//!
//! This module is **Plane B** of the MS external interface: the "codeplug" /
//! provisioning interface between the MS user application (external UI) and the
//! stack. It carries reads and writes of stack settings (network, cell, MS
//! identity, SDR/PHY, endpoints) plus MS runtime state.
//!
//! **This plane is implementation-defined and is NOT part of any ETSI
//! standard.** ETSI does not standardize radio programming over the air
//! interface; the standardized *peripheral* interface is the PEI (ETSI
//! TS 100 392-5, AT-command set), which is noted as prior art but deliberately
//! **not adopted** here — the in-tree WebSocket+JSON framework is more portable
//! for modern UIs. Everything in this module is therefore a local design choice
//! and must never be confused with the standardized TNMM-SAP (Plane A, see
//! `crate::tnmm`).
//!
//! Namespacing: these types are carried over the control/telemetry transport
//! wrapped in the dedicated `Management` variants of `ControlCommand` /
//! `ControlResponse`, keeping Plane B strictly separate from Plane A on the wire.

use bitcode::{Decode, Encode};
use serde::{Deserialize, Serialize};

use crate::tnmm::ServiceStatus;

/// MS registration state, mirrored for the management snapshot (non-standard).
///
/// A serializable view of the internal MM registration state machine (ETSI
/// TS 100 392-2 cl. 16.4 location updating / ITSI attach). Kept as its own type
/// so the internal `RegState` enum stays private to the MM implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum RegistrationState {
    /// Not registered and not currently attempting to register.
    Idle,
    /// A U-LOCATION-UPDATE-DEMAND has been sent; awaiting a response.
    Registering,
    /// Registration accepted by the SwMI.
    Registered,
    /// De-registration in progress (U-ITSI DETACH sent, draining).
    Detaching,
}

/// A snapshot of MS runtime state (**non-standard**, Plane B).
///
/// Read-only view for the UI, built on demand by MM from its own state and the
/// active configuration (single-writer: MM). Feeds `GetState` and mirrors the
/// information the standardized TNMM-SERVICE/REGISTRATION indications carry
/// (Plane A), but is itself an implementation-defined convenience type.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct MsRuntimeState {
    /// Current MM registration state.
    pub registration_state: RegistrationState,
    /// Derived TETRA service status (Plane A enum reused for a common vocabulary).
    pub service_status: ServiceStatus,
    /// Own Individual Short Subscriber Identity (from `[ms] issi`).
    pub own_issi: u32,
    /// Home network MCC (from `[net_info] mcc`).
    pub home_mcc: u16,
    /// Home network MNC (from `[net_info] mnc`).
    pub home_mnc: u16,
    /// Location Area of the serving cell (cached from the last LMM-ACTIVATE
    /// confirmation; defaults to the configured cell LA before cell selection).
    pub serving_la: u16,
    /// Colour code of the configured serving cell (`[cell_info] colour_code`).
    pub colour_code: u8,
    /// GSSIs configured for attachment at registration (`[ms] attach_groups`).
    pub attached_groups: Vec<u32>,
}

/// Management command (UI -> stack), **non-standard** Plane B.
///
/// Carried inside `ControlCommand::Management`. `handle` is a transport-level
/// correlation id so the UI can match responses. Write/apply commands
/// (`SetConfig`, `ApplyConfig`) are added in a later slice.
#[derive(Debug, Clone, Encode, Decode, Serialize, Deserialize)]
pub enum ManagementCommand {
    /// Read the current MS runtime state (live/anytime).
    GetState { handle: u32 },
}

/// Management response (stack -> UI), **non-standard** Plane B.
///
/// Carried inside `ControlResponse::Management`.
#[derive(Debug, Clone, Encode, Decode, Serialize, Deserialize)]
pub enum ManagementResponse {
    /// Response to [`ManagementCommand::GetState`].
    State { handle: u32, state: Box<MsRuntimeState> },
    /// A management command could not be served.
    Error { handle: u32, message: String },
}
