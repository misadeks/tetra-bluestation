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

/// Frozen schema version of the MS external interface (**NON-STANDARD**,
/// Plane A TNMM-SAP indications/requests + Plane B management), independent of
/// the transport-level WebSocket subprotocol handshake string
/// (`CONTROL_PROTOCOL_VERSION` / `TELEMETRY_PROTOCOL_VERSION`), which is shared
/// with the BS and therefore intentionally NOT bumped here.
///
/// A UI discovers this at runtime via [`ManagementCommand::GetInterfaceVersion`]
/// so it can gate on the message catalog it was built against. Bump this (and
/// the documented message catalog) whenever the Plane A/B message shapes change
/// `3` adds the manual cell-survey / register-to-cell commands
/// ([`ManagementCommand::SetCellSelectionMode`], `StartCellScan`,
/// `StopCellScan`, `CampOnCell`), the `selection_mode_manual` field on
/// [`MsRuntimeState`], and the `MsScanResult` / `MsScanComplete` telemetry
/// events. `2` adds the scan-list management command
/// ([`ManagementCommand::ActivateScanlist`]) and the `active_scanlists` field
/// on [`MsRuntimeState`]; `1` was the first frozen revision covering T1 TNMM
/// indications, T2 TNMM requests and T3 management read/write.
pub const MS_INTERFACE_SCHEMA_VERSION: &str = "bluestation-ms-interface-4";

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
///
/// Note: `Eq` is intentionally NOT derived because `rssi_dbfs` is an `f32`.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Serialize, Deserialize)]
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
    /// Serving-cell downlink receive level in uncalibrated dBFS relative to the
    /// demodulator full-scale magnitude (**non-standard**, Plane B telemetry;
    /// an MLE reselection input per cl. 18.3.4 surfaced for a UI receive-level
    /// meter). `None` before the first measurement or while out of service.
    pub rssi_dbfs: Option<f32>,
    /// Colour code of the configured serving cell (`[cell_info] colour_code`).
    pub colour_code: u8,
    /// GSSIs configured for attachment at registration (`[ms] attach_groups`).
    pub attached_groups: Vec<u32>,
    /// Names of the scan lists currently active (a runtime superset control over
    /// group affiliation; **non-standard**, Plane B). Activating a scan list
    /// affiliates its talkgroups (cl. 16.8.2 group attach); deactivating detaches
    /// the groups no other active scan list still needs. Initialised from the
    /// codeplug scan lists whose programmed default is `active`.
    pub active_scanlists: Vec<String>,
    /// True when a configuration change has been staged (via `SetConfig`) that
    /// only takes effect after a controlled restart (`ApplyConfig`). Purely a
    /// UI hint so the operator can see a "pending restart" indication.
    pub restart_required: bool,
    /// Cell-selection mode (**non-standard**, Plane B operator control on top of
    /// ETSI cl. 18.3.4). `false` = automatic (the MS auto-camps on the first
    /// suitable cell, cl. 18.3.4.6); `true` = manual (auto-camp is suppressed;
    /// the operator drives a survey and an explicit camp). Set via
    /// [`ManagementCommand::SetCellSelectionMode`].
    pub selection_mode_manual: bool,
}

/// Management command (UI -> stack), **non-standard** Plane B.
///
/// Carried inside `ControlCommand::Management`. `handle` is a transport-level
/// correlation id so the UI can match responses.
///
/// Config apply model is **HYBRID**: structural radio parameters (MCC/MNC,
/// carrier/band/duplex, ISSI, SDR device) are staged to the on-disk TOML by
/// `SetConfig` and only take effect on a controlled restart (`ApplyConfig`);
/// operational TNMM actions (group attach/detach, energy saving,
/// register/deregister) are carried on Plane A (`crate::tnmm`) and apply live.
#[derive(Debug, Clone, Encode, Decode, Serialize, Deserialize)]
pub enum ManagementCommand {
    /// Read the current MS runtime state (live/anytime).
    GetState { handle: u32 },
    /// Discover the frozen MS interface schema version (live/anytime) so the UI
    /// can gate on the message catalog it was built against. Independent of the
    /// transport handshake subprotocol string.
    GetInterfaceVersion { handle: u32 },
    /// Read the active stack configuration, serialized as canonical TOML
    /// (the same on-disk schema the stack loads at startup).
    GetConfig { handle: u32 },
    /// Stage a new stack configuration. The payload is a full TOML document in
    /// the on-disk schema. It is validated through the exact startup validator
    /// and, if valid, written to the config file; it does **not** bounce the
    /// process. Structural changes need a subsequent `ApplyConfig` to take
    /// effect (sets `restart_required`).
    SetConfig { handle: u32, toml: String },
    /// Apply staged configuration by performing a graceful de-registration
    /// drain and then requesting a controlled process restart (the external
    /// supervisor respawns the stack with the new config). No-op if nothing is
    /// staged is still honored as an explicit restart request.
    ApplyConfig { handle: u32 },
    /// Activate or deactivate a programmed scan list at runtime (live). The
    /// stack resolves the change to a standalone group attach/detach (cl. 16.8.2)
    /// so the affected talkgroups start/stop being monitored. `name` must match
    /// a codeplug scan list; an unknown name is answered with `Ack{accepted:
    /// false}`. Applies immediately when registered; otherwise it just updates
    /// the desired set so the groups are affiliated at the next registration.
    ActivateScanlist { handle: u32, name: String, active: bool },
    /// Set the cell-selection mode (**non-standard**, Plane B). `manual = true`
    /// switches to manual selection: the MS stops auto-camping and waits for the
    /// operator to survey and pick a cell; `false` restores automatic selection
    /// (cl. 18.3.4.6). Rejected (`Ack{accepted:false}`) while a call is active.
    SetCellSelectionMode { handle: u32, manual: bool },
    /// Start a receive-only survey of every candidate downlink carrier in the
    /// codeplug `[[frequency_list]]`s. Each found cell is reported via a
    /// `MsScanResult` telemetry event, followed by a single `MsScanComplete`.
    /// The survey transmits nothing and never camps/registers. Rejected while a
    /// call is active.
    StartCellScan { handle: u32 },
    /// Abort an in-progress cell survey. A `MsScanComplete` is still emitted with
    /// the count of cells found so far.
    StopCellScan { handle: u32 },
    /// Camp on (and optionally register to) a specific carrier chosen by the
    /// operator from the survey results. `register = true` forces a registration
    /// (cl. 16.4) even if the cell advertises registration-not-required;
    /// `register = false` camps only. Rejected while a call is active.
    CampOnCell { handle: u32, carrier_hz: u32, register: bool },
}

/// Management response (stack -> UI), **non-standard** Plane B.
///
/// Carried inside `ControlResponse::Management`.
#[derive(Debug, Clone, Encode, Decode, Serialize, Deserialize)]
pub enum ManagementResponse {
    /// Response to [`ManagementCommand::GetState`].
    State { handle: u32, state: Box<MsRuntimeState> },
    /// Response to [`ManagementCommand::GetInterfaceVersion`]: the frozen
    /// [`MS_INTERFACE_SCHEMA_VERSION`].
    InterfaceVersion { handle: u32, version: String },
    /// Response to [`ManagementCommand::GetConfig`]: the active configuration
    /// serialized as canonical TOML.
    Config { handle: u32, toml: String },
    /// Acknowledgement of a write/apply command.
    ///
    /// For `SetConfig`: `accepted` reflects whether the config validated and was
    /// persisted; `restart_required` echoes whether a restart is now pending.
    /// For `ApplyConfig`: `accepted` reflects whether a restart was initiated.
    Ack { handle: u32, accepted: bool, restart_required: bool, message: String },
    /// A management command could not be served.
    Error { handle: u32, message: String },
}
