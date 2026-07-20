use crate::net_control::ControlEndpoint;
use crate::net_telemetry::channel::TelemetrySink;
use crate::{MessageQueue, TetraEntityTrait};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::typed_pdu_fields::Type3FieldGeneric;
use tetra_core::{BitBuffer, Layer2Service, Sap, TdmaTime, TetraAddress, unimplemented_log};
use tetra_saps::lmm::{LmmMleActivateConf, LmmMleIdentitiesReq, LmmMleUnitdataReq};
use tetra_saps::{SapMsg, SapMsgInner};

use tetra_pdus::mm::enums::location_update_type::LocationUpdateType;
use tetra_pdus::mm::enums::location_update_accept_type::LocationUpdateAcceptType;
use tetra_pdus::mm::enums::mm_pdu_type_dl::MmPduTypeDl;
use tetra_pdus::mm::enums::reject_cause::RejectCause;
use tetra_pdus::mm::enums::type34_elem_id_ul::MmType34ElemIdUl;
use tetra_pdus::mm::pdus::d_location_update_accept::DLocationUpdateAccept;
use tetra_pdus::mm::pdus::d_location_update_command::DLocationUpdateCommand;
use tetra_pdus::mm::pdus::d_location_update_reject::DLocationUpdateReject;
use tetra_pdus::mm::pdus::u_location_update_demand::ULocationUpdateDemand;
use tetra_pdus::mm::pdus::u_itsi_detach::UItsiDetach;
use tetra_pdus::mm::fields::class_of_ms::ClassOfMs;
use tetra_pdus::mm::fields::group_identity_location_demand::GroupIdentityLocationDemand;
use tetra_pdus::mm::fields::group_identity_uplink::GroupIdentityUplink;

/// Timer T351 — "Registration response time" (ETSI TS 100 392-2 cl. 16.11.1.1):
/// the maximum time MM waits for a response (D-LOCATION-UPDATE-ACCEPT /
/// -PROCEEDING / -REJECT) to a registration request. The spec value is **10 s**.
/// Expressed here in downlink timeslots: one TETRA timeslot lasts 85/6 ≈ 14.167 ms
/// (cl. 9.4.4), so 10 s / 14.167 ms ≈ 706 slots. The MM `tick_start` hook is
/// driven once per received downlink slot, so this is the on-air timer value, not
/// an invented guard. On expiry MM may resend the demand (cl. 16.4.5).
const T351_TIMEOUT_SLOTS: u32 = 706;

/// Implementation cap on how many times MM resends the U-LOCATION-UPDATE-DEMAND
/// after successive T351 expiries before abandoning the attempt and returning to
/// idle to await a fresh cell (re)selection. Clause 16.4.5 permits MM, on T351
/// expiry, to "resend the U-LOCATION UPDATE DEMAND" and notes the MS "may wish to
/// select a new serving cell before further registration attempts"; the spec does
/// not fix a resend count, so this bound is an implementation choice (not an
/// on-air value). It is distinct from N351 below, which counts *rejections*.
const MAX_REGISTRATION_ATTEMPTS: u8 = 5;

/// N351 — "Maximum system rejection count" (ETSI TS 100 392-2 cl. 16.11.2.1):
/// when an MS has received N351 registration rejections of type "system rejection"
/// without a successful registration on a system, it shall leave the system and
/// shall not attempt to register again on that system until after a power cycle.
/// N351 has a range 1..4 with a **default value of 4**.
const N351_MAX_SYSTEM_REJECTIONS: u8 = 4;

/// Number of downlink timeslots to keep the stack running after a U-ITSI DETACH
/// has been queued at shutdown, so the MAC random-access procedure (cl. 23.5.1.4)
/// and the PHY have time to actually transmit the burst before the SDR streams
/// close. One TDMA multiframe (18 frames × 4 slots ≈ 1.02 s) is ~72 slots; this
/// is ~2 multiframes ≈ 2 s, enough for at least one random-access opportunity and
/// a retransmission. De-registration is best-effort (cl. 16.6.1: MM proceeds
/// whether the DLL reports the PDU "successfully or unsuccessfully transmitted"),
/// so this is a bounded drain, not a wait for acknowledgement. It is an
/// implementation guard, not a value transmitted on air.
const DETACH_DRAIN_SLOTS: u32 = 144;

/// Class of usage sent for each group attached at ITSI attach (ETSI TS 100 392-2
/// cl. 16.10.6, Table 16.32; the raw 3-bit field value). Per the standard the
/// class of usage "has meaning only for the user application" — the SwMI simply
/// echoes it back in the accept — so a fixed value is spec-valid. Value 4 matches
/// the reference (Motorola) radio observed attaching against this same BS.
const GROUP_CLASS_OF_USAGE: u8 = 4;

/// MS registration state (ETSI TS 100 392-2 cl. 16.4 location updating /
/// ITSI attach).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegState {
    /// Not registered and not currently attempting to register (no cell
    /// selected yet, or a previous attempt was abandoned).
    Idle,
    /// A U-LOCATION-UPDATE-DEMAND has been sent; awaiting a response
    /// (D-LOCATION-UPDATE-ACCEPT / -REJECT). Retransmits on timeout.
    Registering,
    /// Registration accepted by the SwMI.
    Registered,
    /// De-registration in progress: a U-ITSI DETACH has been sent at shutdown
    /// and the stack is being drained so the burst can be transmitted
    /// (cl. 16.6.1). Terminal — the MS is stopping.
    Detaching,
}

/// Identity of a cell as seen by MM: network (MCC/MNC) plus location area.
/// Used to apply the registration conditions of ETSI TS 100 392-2 cl. 18.3.4.7.1a
/// (network change -> migrating; LA outside the registered area -> roaming).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CellId {
    mcc: u16,
    mnc: u16,
    la: u16,
}

pub struct MmMs {
    config: SharedConfig,
    /// OUTBOUND external-interface handle (stack -> UI): TNMM indications
    /// (ETSI TS 100 392-2 cl. 15.3) are pushed here in later phases. Wired in
    /// Phase T0; `None` when no telemetry endpoint is configured.
    telemetry: Option<TelemetrySink>,
    /// INBOUND external-interface handle (UI -> stack): TNMM requests
    /// (cl. 15.3) are received here in later phases. Wired in Phase T0; `None`
    /// when no control endpoint is configured.
    control: Option<ControlEndpoint>,
    /// Registration procedure state (cl. 16.4).
    reg_state: RegState,
    /// Timer T351 (registration response time, cl. 16.11.1.1) countdown in
    /// downlink slots. Started when a demand is sent, stopped on ACCEPT/REJECT.
    /// On expiry MM may resend the demand (cl. 16.4.5).
    t351_countdown: u32,
    /// Number of demands sent for the current registration attempt.
    attempts: u8,
    /// Count of registration rejections of type "system rejection" received
    /// without an intervening successful registration (cl. 16.11.2.1, N351).
    /// When it reaches `N351_MAX_SYSTEM_REJECTIONS` the MS leaves the system.
    system_rejection_count: u8,
    /// Set once the MS has left the system after N351 system rejections
    /// (cl. 16.11.2.1): no further registration is attempted on this system
    /// until a power cycle (process restart).
    left_system: bool,
    /// Downlink slots remaining to drain after sending a U-ITSI DETACH at
    /// shutdown, giving the MAC/PHY time to transmit it (cl. 16.6.1).
    detach_countdown: u32,
    /// The cell MM currently believes it is camped on, from the most recent
    /// LMM-ACTIVATE confirm (cl. 17.3.2). Used to distinguish a genuine cell/LA
    /// change from a repeated confirmation for the same cell.
    current_cell: Option<CellId>,
    /// The cell in which the MS last completed a successful registration
    /// (cl. 16.4). Its LA is the (single-LA) "registered area" against which an
    /// LA change is evaluated (cl. 18.3.4.7.1a cond. 2). `None` until the first
    /// successful registration.
    registered_cell: Option<CellId>,
    /// The cell targeted by the in-flight registration attempt, promoted to
    /// `registered_cell` on a D-LOCATION-UPDATE-ACCEPT.
    pending_cell: Option<CellId>,
    /// Location-updating type of the in-flight attempt, so a T351 resend
    /// (cl. 16.4.5) repeats the same request type.
    pending_lu_type: LocationUpdateType,
    /// Set when the last accept carried the "Temporary registration" status
    /// (cl. 16.4.8, accept type 0012, Table 16.68). While held, a cell
    /// reselection requires re-registration even inside the registered area, and
    /// a cell returning to "normal mode" triggers a periodic location update.
    temporary_registration: bool,
    /// Location area of the serving cell as reported by the most recent
    /// LMM-ACTIVATE confirmation (cl. 17.3.2). Cached so the TNMM-REGISTRATION
    /// indication/confirm can report "LA (where registered)" (Table 15.5).
    /// Defaults to the configured cell LA until a serving cell is selected.
    serving_la: u16,
    /// Runtime plumbing needed by the Plane B management write/apply handlers
    /// (config file path + process restart signalling). `None` in unit tests
    /// and read-only deployments, in which case `SetConfig`/`ApplyConfig` are
    /// refused gracefully rather than acted upon. **NON-STANDARD** (Plane B).
    mgmt_ctx: Option<ManagementContext>,
    /// UI hint (**NON-STANDARD**, Plane B): a configuration change has been
    /// staged to disk via `SetConfig` that only takes effect after a controlled
    /// restart (`ApplyConfig`). Surfaced in the runtime-state snapshot.
    restart_required: bool,
    /// The full set of group identities (GSSIs) currently attached (cl. 16.8).
    /// Seeded from `[ms].attach_groups` and updated at runtime as groups are
    /// attached/detached (registration accept, standalone attach/detach). This
    /// is the authoritative set MM sends to the MLE via MLE-IDENTITIES (cl.
    /// 17.3.2) so the MAC downlink address filter (cl. 23.4.1.2.1) stays in
    /// sync, and the set re-requested on a re-registration.
    attached_gssis: BTreeSet<u32>,
}

/// Runtime plumbing for the Plane B management write/apply handlers
/// (**NON-STANDARD**). Carries the on-disk config path to persist to and the
/// process-level restart signalling shared with `main`.
struct ManagementContext {
    /// Path of the TOML config file that `SetConfig` writes and that the
    /// supervisor-respawned process reloads at startup.
    config_path: PathBuf,
    /// Set by `ApplyConfig` to request the supervisor-driven restart; polled by
    /// `main` after the stack run loop exits so it can exit with the documented
    /// restart exit code.
    restart_requested: Arc<AtomicBool>,
    /// Cleared by `ApplyConfig` to break the stack run loop and begin the
    /// graceful shutdown drain (the same `is_running` flag `main` owns).
    is_running: Arc<AtomicBool>,
}

impl MmMs {
    pub fn new(config: SharedConfig, telemetry: Option<TelemetrySink>, control: Option<ControlEndpoint>) -> Self {
        let serving_la = config.config().cell.location_area;
        // Seed the runtime attached-group set from config (cl. 16.8). Before any
        // registration this is the set the MS will request; after registration it
        // holds the SwMI-confirmed attached set.
        let attached_gssis = config
            .config()
            .ms
            .as_ref()
            .map(|ms| ms.attach_groups.iter().copied().collect::<BTreeSet<u32>>())
            .unwrap_or_default();
        Self {
            config,
            telemetry,
            control,
            reg_state: RegState::Idle,
            t351_countdown: 0,
            attempts: 0,
            system_rejection_count: 0,
            left_system: false,
            detach_countdown: 0,
            current_cell: None,
            registered_cell: None,
            pending_cell: None,
            pending_lu_type: LocationUpdateType::ItsiAttach,
            temporary_registration: false,
            serving_la,
            mgmt_ctx: None,
            restart_required: false,
            attached_gssis,
        }
    }

    /// Install the Plane B management context (**NON-STANDARD**): the config
    /// file path to persist `SetConfig` writes to, plus the shared restart /
    /// running flags `main` uses to drive a controlled restart on `ApplyConfig`.
    ///
    /// Provided via a setter (not a `new()` parameter) so the many existing
    /// test constructors are unaffected; when it is not installed the write /
    /// apply management commands are refused gracefully.
    pub fn set_management_context(
        &mut self,
        config_path: PathBuf,
        restart_requested: Arc<AtomicBool>,
        is_running: Arc<AtomicBool>,
    ) {
        self.mgmt_ctx = Some(ManagementContext { config_path, restart_requested, is_running });
    }

    /// Own Individual Short Subscriber Identity (ISSI) from the MS config
    /// section. This is the MS's own address used as the source of the uplink.
    fn own_issi(&self) -> u32 {
        self.config
            .config()
            .ms
            .as_ref()
            .expect("MS config section required in MS mode")
            .issi
    }

    /// Group identities (GSSIs) this MS currently intends to have attached
    /// (ETSI TS 100 392-2 cl. 16.8 group identity attachment). Returns the live
    /// runtime set (seeded from config, updated by attach/detach), sorted for
    /// deterministic PDU ordering. On an initial registration this equals the
    /// configured set; on a re-registration it reflects the current attachments.
    fn attach_groups(&self) -> Vec<u32> {
        self.attached_gssis.iter().copied().collect()
    }

    /// Own Mobile Network Identity (MNI) as the 24-bit value carried in the
    /// address extension information element (ETSI EN 300 392-1 clause 7): the
    /// 10-bit MCC in the high bits followed by the 14-bit MNC. Taken from the
    /// configured home network (`[net_info]`), which is the MNI of the MS ITSI.
    fn own_mni(&self) -> u32 {
        let net = &self.config.config().net;
        ((net.mcc as u32) << 14) | (net.mnc as u32 & 0x3FFF)
    }

    // ----------------------------------------------------------------------
    // TNMM-SAP indications (Plane A, OUTBOUND) — ETSI TS 100 392-2 cl. 15.3.
    //
    // These push standardized TNMM primitives (cl. 15.3.3 / 15.3.4) toward the
    // MS user application over the telemetry side-channel. They never touch the
    // on-air SapMsg queue and never change registration state — they only
    // report state that MM has already reached, so no air/TNMM behaviour is
    // invented.
    // ----------------------------------------------------------------------

    /// Push a TNMM indication/confirm to the user application, if a telemetry
    /// sink is wired. Fire-and-forget (lock-free).
    fn emit(&self, event: crate::net_telemetry::TelemetryEvent) {
        if let Some(sink) = &self.telemetry {
            sink.send(event);
        }
    }

    /// MCC where registered (Table 15.5). This single-network clear-mode MS only
    /// ever camps on / registers with cells of its configured network — a
    /// D-LOCATION-UPDATE-COMMAND carrying a different MNI is ignored (own_mni
    /// check) — so the registered MCC is the configured home MCC.
    fn where_registered_mcc(&self) -> u16 {
        self.config.config().net.mcc
    }

    /// MNC where registered (Table 15.5). See [`Self::where_registered_mcc`].
    fn where_registered_mnc(&self) -> u16 {
        self.config.config().net.mnc
    }

    /// Cell type where registered (Table 15.5). TNMM registration / location
    /// updating is a trunked-mode (V+D) procedure carried out on a CA cell; this
    /// stack implements no direct-mode (DA) registration path, so the cell where
    /// this MS registers is always a CA cell (`cl. 15.3.4` value "CA cell").
    fn where_registered_cell_type(&self) -> crate::tnmm::CellType {
        crate::tnmm::CellType::CaCell
    }

    /// Build the "group identities" parameter (Table 15.8) for a set of GSSIs
    /// that have just been attached at ITSI attach, as reported to the user
    /// application. GITI = Attachment, so the conditional lifetime + class of
    /// usage members are present; the detachment reason is absent (cl. 15.3.4).
    /// GTSI = MNI << 24 | GSSI (cl. 15.3.4 "GTSI").
    fn attached_group_identities(&self, gssis: &[u32]) -> Vec<crate::tnmm::GroupIdentity> {
        use crate::tnmm::{ClassOfUsage, GroupIdentity, GroupIdentityAttachDetachTypeIdentifier, GroupIdentityLifetime};
        let mni = self.own_mni() as u64;
        gssis
            .iter()
            .map(|gssi| GroupIdentity {
                gtsi: (mni << 24) | (*gssi as u64),
                group_identity_attach_detach_type_identifier: GroupIdentityAttachDetachTypeIdentifier::Attachment,
                // Attached at ITSI attach with the class of usage MM sends in the
                // demand (cl. 16.10.6, GROUP_CLASS_OF_USAGE = 4 -> "Class of Usage 4").
                group_identity_lifetime: Some(GroupIdentityLifetime::AttachmentNeededForNextItsiAttach),
                class_of_usage: Some(ClassOfUsage::ClassOfUsage4),
                group_identity_detachment_reason: None,
            })
            .collect()
    }

    /// Emit a TNMM-SERVICE indication (Table 15.6, cl. 15.3.3.8) reflecting the
    /// MS service state. Disable status is always "enabled": the disabling
    /// procedure is defined in ETSI EN 300 392-7 (Part 7, security), which is out
    /// of scope, so this MS is never in a temporary/permanently-disabled state.
    fn emit_service(&self, service_status: crate::tnmm::ServiceStatus) {
        use crate::net_telemetry::TelemetryEvent;
        use crate::tnmm::{DisableStatus, TnmmServiceIndication};
        self.emit(TelemetryEvent::TnmmServiceIndication(TnmmServiceIndication {
            service_status,
            disable_status: DisableStatus::Enabled,
        }));
    }

    /// Map an on-air `Reject cause` (cl. 16.10.42) to the TNMM-SAP
    /// `Registration reject cause` enumeration (cl. 15.3.4). Returns `None` for
    /// the on-air causes that have no TNMM-SAP enumerant ("use CA/DA cell not
    /// permitted"), which therefore cannot be reported through this parameter.
    fn map_registration_reject_cause(cause: RejectCause) -> Option<crate::tnmm::RegistrationRejectCause> {
        use crate::tnmm::RegistrationRejectCause as T;
        Some(match cause {
            RejectCause::ItsiAtsiUnknown => T::ItsiUnknown,
            RejectCause::IllegalMs => T::IllegalMs,
            RejectCause::LaNotAllowed => T::LaNotAllowed,
            RejectCause::LaUnknown => T::LaUnknown,
            RejectCause::NetworkFailure => T::NetworkFailure,
            RejectCause::Congestion => T::Congestion,
            RejectCause::ForwardRegistrationFailure => T::ForwardRegistrationFailure,
            RejectCause::ServiceNotSubscribed => T::ServiceNotSubscribed,
            RejectCause::MandatoryElementError => T::MandatoryElementError,
            RejectCause::MessageConsistencyError => T::MessageConsistencyError,
            RejectCause::RoamingNotSupported => T::RoamingNotSupported,
            RejectCause::MigrationNotSupported => T::MigrationNotSupported,
            RejectCause::NoCipherKsg => T::NoCipherKsg,
            RejectCause::IdentifiedCipherKsgNotSupported => T::IdentifiedCipherKsgNotSupported,
            RejectCause::RequestedCipherKeyTypeNotAvailable => T::RequestedCipherKeyTypeNotAvailable,
            RejectCause::IdentifiedCipherKeyNotAvailable => T::IdentifiedCipherKeyNotAvailable,
            RejectCause::CipheringRequired => T::CipheringRequired,
            RejectCause::AuthenticationFailure => T::AuthenticationFailure,
            RejectCause::UseCaCellNotPermitted | RejectCause::UseDaCellNotPermitted => return None,
        })
    }

    /// Emit a TNMM-REGISTRATION indication reporting that MM has failed a
    /// registration procedure (Table 15.5; status = "failure" + reject cause,
    /// cl. 15.3.3.7 / NOTE 1). LA/MCC/MNC reflect the cell the attempt was made
    /// against. Followed by a TNMM-SERVICE "out of service" indication.
    fn emit_registration_failure(&self, cause: Option<RejectCause>) {
        use crate::net_telemetry::TelemetryEvent;
        use crate::tnmm::{RegistrationStatus, ServiceStatus, TnmmRegistrationIndication};
        let reject = cause.and_then(Self::map_registration_reject_cause);
        self.emit(TelemetryEvent::TnmmRegistrationIndication(Box::new(TnmmRegistrationIndication {
            registration_status: RegistrationStatus::Failure,
            registration_reject_cause: reject,
            cell_type_where_registered: self.where_registered_cell_type(),
            la_where_registered: self.serving_la,
            mcc_where_registered: self.where_registered_mcc(),
            mnc_where_registered: self.where_registered_mnc(),
            swmis_required_cell_types: None,
            energy_economy_mode: None,
            energy_economy_mode_status: None,
            group_identities: None,
            group_identity_attach_detach_mode: None,
        })));
        self.emit_service(ServiceStatus::OutOfService);
    }

    /// Emit the TNMM-SAP outbound primitives for a successfully carried-out
    /// registration (Tables 15.5 / 15.1 / 15.6):
    /// - TNMM-REGISTRATION indication and confirm (status = "success"), the
    ///   confirm informing the user application the MS is ready for use
    ///   (cl. 15.3.3.7);
    /// - TNMM-SERVICE indication "in service" (cl. 15.3.3.8);
    /// - TNMM-ATTACH DETACH GROUP IDENTITY indication reporting the groups the
    ///   SwMI attached, when any (cl. 15.3.3.1).
    ///
    /// `attached_gssis` are the GSSIs the SwMI confirmed attached in the
    /// D-LOCATION-UPDATE-ACCEPT (empty when none / group attachment failed).
    fn emit_registration_success(&self, attached_gssis: &[u32]) {
        use crate::net_telemetry::TelemetryEvent;
        use crate::tnmm::{
            RegistrationStatus, ServiceStatus, TnmmAttachDetachGroupIdentityIndication,
            TnmmRegistrationConfirm, TnmmRegistrationIndication,
        };

        let group_identities = self.attached_group_identities(attached_gssis);
        let groups_opt = if group_identities.is_empty() {
            None
        } else {
            Some(group_identities.clone())
        };

        self.emit(TelemetryEvent::TnmmRegistrationIndication(Box::new(TnmmRegistrationIndication {
            registration_status: RegistrationStatus::Success,
            registration_reject_cause: None,
            cell_type_where_registered: self.where_registered_cell_type(),
            la_where_registered: self.serving_la,
            mcc_where_registered: self.where_registered_mcc(),
            mnc_where_registered: self.where_registered_mnc(),
            swmis_required_cell_types: None,
            energy_economy_mode: None,
            energy_economy_mode_status: None,
            group_identities: groups_opt.clone(),
            group_identity_attach_detach_mode: None,
        })));

        self.emit(TelemetryEvent::TnmmRegistrationConfirm(Box::new(TnmmRegistrationConfirm {
            registration_status: RegistrationStatus::Success,
            registration_reject_cause: None,
            cell_type_where_registered: self.where_registered_cell_type(),
            la_where_registered: self.serving_la,
            mcc_where_registered: self.where_registered_mcc(),
            mnc_where_registered: self.where_registered_mnc(),
            energy_economy_mode: None,
            energy_economy_mode_status: None,
            group_identities: groups_opt,
            group_identity_attach_detach_mode: None,
        })));

        self.emit_service(ServiceStatus::InService);

        if !group_identities.is_empty() {
            self.emit(TelemetryEvent::TnmmAttachDetachGroupIdentityIndication(
                TnmmAttachDetachGroupIdentityIndication { group_identities },
            ));
        }
    }

    /// Build the "class of MS" element (ETSI TS 100 392-2 cl. 16.10.5, Table
    /// 16.31) describing this MS's radio capabilities. Clause 16.4.3 requires
    /// the U-LOCATION-UPDATE-DEMAND sent in response to a D-LOCATION-UPDATE-
    /// COMMAND to contain this element.
    ///
    /// The bits reflect the *actual* capabilities of this clear-mode voice SDR
    /// implementation — they are not invented capabilities: only `voice` is
    /// asserted, and `e2e_encryption_not_supported` is set (reversed polarity:
    /// true = E2E encryption **not** supported, which is correct — Part 7
    /// security is out of scope). All data / advanced-link / encryption /
    /// authentication / alternative-modulation capabilities are `false` because
    /// the stack does not implement them. `air_interface_version` = 3 is the AI
    /// protocol version in use on this network (directly observed from the
    /// reference radio registering against this same BS); it is an interworking
    /// version field, not a fabricated capability.
    fn build_class_of_ms(&self) -> ClassOfMs {
        ClassOfMs {
            freq_simplex_duplex: false,
            multislot_phase_mod: false,
            concurrent_multicarrier: false,
            voice: true,
            e2e_encryption_not_supported: true,
            circuit_mode_data: false,
            tetra_packet_data: false,
            fast_switching: false,
            dck_encryption: false,
            clch_needed: false,
            concurrent_circuit_mode: false,
            original_advanced_link: false,
            minimum_mode: false,
            carrier_specific_signalling: false,
            authentication: false,
            sck_encryption: false,
            air_interface_version: 3,
            common_scch: false,
            reserved_21: false,
            mac_d_blck: false,
            extended_advanced_link: false,
            d8psk: false,
        }
    }

    /// Build a "group report response" element (ETSI TS 100 392-2 cl. 16.10.27a,
    /// Table 16.59) indicating "group report complete" (length 1 bit, value 0).
    /// Sent in the command-response demand when all reported groups are carried
    /// in that single PDU (cl. 16.4.3).
    fn group_report_complete() -> Type3FieldGeneric {
        Type3FieldGeneric {
            field_id: MmType34ElemIdUl::GroupReportResponse as u64,
            len: 1,
            data: vec![0],
        }
    }

    /// Build the group identity location demand element carried inside the
    /// U-LOCATION-UPDATE-DEMAND at ITSI attach (cl. 16.10.24 / 16.8.2), from the
    /// configured `attach_groups`. Returns `None` when no groups are configured
    /// (the element is then omitted, as before).
    ///
    /// The attach/detach mode is set to "detach all currently attached group
    /// identities and attach the group identities defined here" (cl. 16.10.17,
    /// Table 16.49, value 1): at ITSI attach nothing is yet attached, so this
    /// cleanly establishes exactly the configured set. Each group is an
    /// attachment (`class_of_usage` present, `group_identity_detachment_uplink`
    /// absent) carried as a plain GSSI (address type 0). This mirrors the
    /// reference radio, which the BS accepts.
    fn build_group_identity_location_demand(&self) -> Option<GroupIdentityLocationDemand> {
        let groups = self.attach_groups();
        if groups.is_empty() {
            return None;
        }
        Some(GroupIdentityLocationDemand {
            group_identity_attach_detach_mode: 1,
            group_identity_uplink: Some(
                groups
                    .iter()
                    .map(|&gssi| GroupIdentityUplink {
                        class_of_usage: Some(GROUP_CLASS_OF_USAGE),
                        group_identity_detachment_uplink: None,
                        gssi: Some(gssi),
                        address_extension: None,
                        vgssi: None,
                    })
                    .collect(),
            ),
        })
    }

    /// LMM-ACTIVATE confirmation from MLE (cl. 17.3.2): a serving cell has been
    /// selected. Start the registration procedure if the cell requires it.
    fn rx_activate_conf(&mut self, queue: &mut MessageQueue, conf: &LmmMleActivateConf) {
        let cell = CellId {
            mcc: conf.mcc,
            mnc: conf.mnc,
            la: conf.la,
        };
        // Track the cell MM believes it is camped on, from the latest confirm.
        self.current_cell = Some(cell);
        // Cache the serving cell's LA (from the latest confirm) so the
        // TNMM-REGISTRATION indication/confirm can report "LA (where
        // registered)" (Table 15.5). Runs on every confirm regardless of the
        // registration-condition early returns below.
        self.serving_la = conf.la;

        if self.left_system {
            // Left the system after N351 system rejections (cl. 16.11.2.1); no
            // further registration until a power cycle (process restart).
            tracing::debug!("MM: activate-conf received but MS left the system (N351); ignoring");
            return;
        }
        if self.reg_state == RegState::Detaching {
            // Shutting down; do not start a new registration.
            tracing::debug!("MM: activate-conf received while detaching; ignoring");
            return;
        }
        if self.reg_state == RegState::Registering {
            // A registration attempt is already in flight; the current attempt
            // will complete or time out. Do not start a second one.
            tracing::debug!("MM: activate-conf received while a registration is in flight; ignoring");
            return;
        }

        // Apply the registration conditions of ETSI TS 100 392-2 cl. 18.3.4.7.1a
        // (see also cl. 16.4.1.0). Decide both *whether* to register and *which*
        // location-updating type applies.
        //
        // cl. 16.4.1.0 cond. 5 / cl. 16.4.8: a cell advertising "system wide
        // services temporarily not supported" (system_wide_services == false)
        // requires registration even inside the registered area and even if the
        // cell does not otherwise set the registration flag.
        let restricted = !conf.system_wide_services;
        let lu_type = if self.reg_state == RegState::Registered {
            match self.registered_cell {
                // Same cell/LA as where we are registered.
                Some(reg) if reg == cell => {
                    if self.temporary_registration && conf.system_wide_services {
                        // We held a temporary registration and the cell has
                        // returned to normal mode: perform a periodic location
                        // update (cl. 16.4.8 para 3 / cl. 16.4.1.0 NOTE).
                        tracing::info!(
                            "MM: cell returned to normal mode while temporarily registered; \
                             periodic location updating (cl. 16.4.8)"
                        );
                        LocationUpdateType::PeriodicLocationUpdating
                    } else if self.temporary_registration || restricted {
                        // Still holding a temporary registration (cond. 4) or the
                        // cell is in "system wide services temporarily not
                        // supported" state (cond. 5): registration is required even
                        // on the registered cell. Same network => roaming.
                        tracing::info!(
                            "MM: re-registration required on the registered cell \
                             (temporary_registration={}, system_wide_services={}); \
                             roaming location updating (cl. 16.4.1.0 cond. 4/5)",
                            self.temporary_registration, conf.system_wide_services
                        );
                        LocationUpdateType::RoamingLocationUpdating
                    } else {
                        // No condition applies: returning to / staying on the
                        // registered cell requires no location update
                        // (cl. 18.3.4.7.1a NOTE 2).
                        tracing::debug!(
                            "MM: activate-conf for the registered cell (MCC={} MNC={} LA={}); no re-registration",
                            cell.mcc, cell.mnc, cell.la
                        );
                        return;
                    }
                }
                // Different network (MNI): migrating location updating is required
                // regardless of the new cell's registration flag
                // (cl. 18.3.4.7.1a cond. 1).
                Some(reg) if reg.mcc != cell.mcc || reg.mnc != cell.mnc => {
                    tracing::info!(
                        "MM: network changed {}/{} -> {}/{}; migrating location updating (cl. 18.3.4.7.1a)",
                        reg.mcc, reg.mnc, cell.mcc, cell.mnc
                    );
                    LocationUpdateType::MigratingLocationUpdating
                }
                // Same network, LA outside the registered area: roaming location
                // updating is required if the new cell requires registration
                // (cond. 2), or if the cell is restricted (cond. 5), or if we hold
                // a temporary registration (cond. 4).
                _ => {
                    if !conf.registration_required && !restricted && !self.temporary_registration {
                        tracing::info!(
                            "MM: LA changed to {} but new cell does not require registration; \
                             not registering (cl. 18.3.4.7.1a cond. 2)",
                            cell.la
                        );
                        return;
                    }
                    tracing::info!(
                        "MM: LA outside registered area (registered LA={:?}, now LA={}); \
                         roaming location updating (cl. 18.3.4.7.1a)",
                        self.registered_cell.map(|c| c.la), cell.la
                    );
                    LocationUpdateType::RoamingLocationUpdating
                }
            }
        } else {
            // Not registered: initial registration / ITSI attach (cl. 16.9.3.4),
            // performed when the selected cell requires registration (registration
            // flag) or advertises "system wide services temporarily not supported"
            // (cl. 16.4.1.0 cond. 5).
            if !conf.registration_required && !restricted {
                tracing::info!(
                    "MM: serving cell (LA={}) does not require registration; not registering",
                    conf.la
                );
                return;
            }
            tracing::info!("MM: serving cell selected (LA={}), initiating ITSI attach registration", conf.la);
            LocationUpdateType::ItsiAttach
        };

        self.pending_cell = Some(cell);
        self.attempts = 0;
        self.send_location_update_demand(queue, lu_type);
    }

    /// Build and send a U-LOCATION-UPDATE-DEMAND (ITSI attach, cl. 16.9.3.4) down
    /// to MLE. MLE prepends its protocol discriminator and forwards it to LLC,
    /// from where it reaches the MAC and is transmitted on the uplink via random
    /// access (cl. 23.5.1.4).
    fn send_location_update_demand(&mut self, queue: &mut MessageQueue, lu_type: LocationUpdateType) {
        let issi = self.own_issi();
        // Remember the type in flight so a T351 resend repeats the same request
        // (cl. 16.4.5).
        self.pending_lu_type = lu_type;

        // Attach the configured group identities as part of the ITSI attach
        // (cl. 16.8.2): the SwMI affiliates the MS to these groups and will then
        // forward group-addressed traffic to it. Omitted when none configured.
        let group_identity_location_demand = self.build_group_identity_location_demand();

        // Minimal demand: no ciphering, no optional elements. The MS identity is
        // carried by the MAC-layer source address, so the ssi element is left
        // absent (cl. 16.9.3.4 note 2 / BS accepts this). `lu_type` reflects the
        // reason for the update: ITSI attach (initial), roaming (LA change) or
        // migrating (network change) per cl. 16.10.35 / 18.3.4.7.1a.
        let pdu = ULocationUpdateDemand {
            location_update_type: lu_type,
            request_to_append_la: false,
            cipher_control: false,
            ciphering_parameters: None,
            class_of_ms: None,
            energy_saving_mode: None,
            la_information: None,
            ssi: None,
            address_extension: None,
            group_identity_location_demand,
            group_report_response: None,
            authentication_uplink: None,
            extended_capabilities: None,
            proprietary: None,
        };

        let mut sdu = BitBuffer::new_autoexpand(4);
        pdu.to_bitbuf(&mut sdu).expect("U-LOCATION-UPDATE-DEMAND serialization");
        sdu.seek(0);
        tracing::info!(
            "MM: -> U-LOCATION-UPDATE-DEMAND ({:?}) for ISSI {}, attach_groups {:?} sdu {}",
            lu_type,
            issi,
            self.attach_groups(),
            sdu.dump_bin()
        );

        let m = SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mm,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LmmMleUnitdataReq(LmmMleUnitdataReq {
                sdu,
                handle: 0,
                address: TetraAddress::issi(issi),
                layer2service: Layer2Service::Acknowledged,
                stealing_permission: false,
                stealing_repeats_flag: false,
                encryption_flag: false,
                is_null_pdu: false,
                tx_reporter: None,
            }),
        };
        queue.push_back(m);

        self.reg_state = RegState::Registering;
        self.t351_countdown = T351_TIMEOUT_SLOTS;
        self.attempts += 1;

        // TNMM-SERVICE indication (Table 15.6): the MS is now in service but
        // awaiting the registration response (cl. 15.3.4 "in service waiting for
        // registration").
        self.emit_service(crate::tnmm::ServiceStatus::InServiceWaitingForRegistration);
    }

    /// Handle a D-LOCATION-UPDATE-COMMAND (ETSI TS 100 392-2 cl. 16.4.3): the
    /// SwMI has demanded that the MS (re)registers ("infrastructure initiated
    /// registration"). On receipt, if the MNI in the address extension matches
    /// the MS's own MNI (or is absent), the MS responds with a
    /// U-LOCATION-UPDATE-DEMAND of type "demand location updating" and starts
    /// T351.
    fn rx_d_location_update_command(&mut self, queue: &mut MessageQueue, mut sdu: BitBuffer) {
        let pdu = match DLocationUpdateCommand::from_bitbuf(&mut sdu) {
            Ok(pdu) => pdu,
            Err(e) => {
                tracing::warn!("Failed parsing DLocationUpdateCommand: {:?} {}", e, sdu.dump_bin());
                return;
            }
        };

        // cl. 16.4.3: "extract the MNI from the address extension information
        // element. If the MNI is the same as the MNI of the MS ITSI (or the MNI
        // is not present), then [proceed]". A command carrying a different MNI is
        // not for us.
        if let Some(mni) = pdu.address_extension {
            if mni != self.own_mni() as u64 {
                tracing::debug!(
                    "MM: <- D-LOCATION-UPDATE-COMMAND for MNI {} != own MNI {}; ignoring",
                    mni,
                    self.own_mni()
                );
                return;
            }
        }

        if self.left_system {
            tracing::debug!("MM: D-LOCATION-UPDATE-COMMAND received but MS left the system (N351); ignoring");
            return;
        }
        if self.reg_state == RegState::Detaching {
            tracing::debug!("MM: D-LOCATION-UPDATE-COMMAND received while detaching; ignoring");
            return;
        }

        // Coalesce redundant infrastructure-initiated registrations (cl. 16.4.3):
        // if a registration is already in progress (T351 active) — typically the
        // SwMI is re-sending the command because our in-flight
        // U-LOCATION-UPDATE-DEMAND has not yet been acknowledged — do not queue
        // another demand. The outstanding demand (and its T351-driven
        // retransmission, cl. 16.4.5) already satisfies the command; a second
        // demand would only pile up behind the first on the acknowledged basic
        // link (stop-and-wait, cl. 22.3.2.3) and never make progress. The
        // command's basic-link frame is still acknowledged independently by the
        // LLC (BL-ACK).
        if self.reg_state == RegState::Registering && self.t351_countdown > 0 {
            tracing::debug!(
                "MM: <- D-LOCATION-UPDATE-COMMAND while a registration is already in progress \
                 (T351 active, {} slots left); coalescing — not sending a duplicate demand",
                self.t351_countdown
            );
            return;
        }

        // cl. 16.4.3: if the "cell type control" element is present the MS would
        // update its cell-type preference (cl. 16.4.12) and, if the serving cell
        // is no longer permitted, issue an MLE-LINK request to reselect a cell
        // (possibly MLE-CLOSE / out of service). That MLE cell-type plumbing is
        // not implemented; this BS sends no cell-type control, so we log and
        // proceed to register on the current serving cell.
        if pdu.cell_type_control.is_some() {
            tracing::warn!(
                "MM: D-LOCATION-UPDATE-COMMAND carries a cell type control element; \
                 cell-type-preference update / MLE-LINK reselection (cl. 16.4.12) is not \
                 implemented — proceeding with the current serving cell"
            );
        }

        tracing::info!(
            "MM: <- D-LOCATION-UPDATE-COMMAND (infrastructure-initiated registration), \
             group_report_request={}",
            pdu.group_identity_report
        );

        self.attempts = 0;
        self.send_demand_location_update(queue, pdu.group_identity_report);
    }

    /// Build and send the U-LOCATION-UPDATE-DEMAND that answers a
    /// D-LOCATION-UPDATE-COMMAND (ETSI TS 100 392-2 cl. 16.4.3). Distinct from
    /// the plain ITSI-attach demand (`send_location_update_demand`): per
    /// cl. 16.4.3 this demand carries the MNI in the address extension and the
    /// ISSI in the SSI element (the true ITSI), the class of MS element, a
    /// location-update type of "demand location updating", and group handling
    /// that depends on whether the command requested a group report.
    fn send_demand_location_update(&mut self, queue: &mut MessageQueue, group_report_requested: bool) {
        let issi = self.own_issi();
        let groups = self.attach_groups();

        // Group handling (cl. 16.4.3):
        //  - With a group report request: the MS shall regard all group
        //    identities as no longer attached. If it wishes to keep receiving
        //    group signalling it may include the attachments here; the first
        //    attachment uses attach/detach mode "detach all + attach defined"
        //    (which `build_group_identity_location_demand` already sets, value 1)
        //    and report "not report request". As all reported groups fit in this
        //    single PDU, a "group report response = group report complete"
        //    element is included. If there are no groups, only the "group report
        //    complete" element is sent. (Timer T353 would start here; the group
        //    identity response timer is a later MM slice — R3.)
        //  - Without a group report request: the MS may still include an
        //    attachment request for its configured groups (optional), and no
        //    group report response element is sent.
        let (group_identity_location_demand, group_report_response) = if group_report_requested {
            (
                self.build_group_identity_location_demand(),
                Some(Self::group_report_complete()),
            )
        } else {
            (self.build_group_identity_location_demand(), None)
        };

        // cl. 16.4.3: location update type = "demand location updating" (the MS
        // is enabled; "disabled MS updating" would be used if the MS were
        // disabled — enable/disable is EN 300 392-7, out of scope). The demand
        // shall contain the MNI (address extension) and ISSI (SSI element) — the
        // true ITSI — the class of MS element, shall not request to append the
        // LA, and shall not carry LA information (this is not a forward
        // registration). Extended capabilities are omitted (the MS supports none
        // of the listed items beyond CA); no energy saving mode (not activated).
        let pdu = ULocationUpdateDemand {
            location_update_type: LocationUpdateType::DemandLocationUpdating,
            request_to_append_la: false,
            cipher_control: false,
            ciphering_parameters: None,
            class_of_ms: Some(self.build_class_of_ms()),
            energy_saving_mode: None,
            la_information: None,
            ssi: Some(issi as u64),
            address_extension: Some(self.own_mni() as u64),
            group_identity_location_demand,
            group_report_response,
            authentication_uplink: None,
            extended_capabilities: None,
            proprietary: None,
        };

        let mut sdu = BitBuffer::new_autoexpand(4);
        pdu.to_bitbuf(&mut sdu)
            .expect("U-LOCATION-UPDATE-DEMAND (demand location updating) serialization");
        sdu.seek(0);
        tracing::info!(
            "MM: -> U-LOCATION-UPDATE-DEMAND (demand location updating) ISSI {} MNI {} \
             group_report_requested {} attach_groups {:?} sdu {}",
            issi,
            self.own_mni(),
            group_report_requested,
            groups,
            sdu.dump_bin()
        );

        // NOTE: cl. 16.4.3 assigns this PDU priority 6 (or 3 when a group report
        // response is included). The LMM-UNITDATA request primitive carries no
        // PDU-priority parameter in this stack, so the priority cannot be
        // signalled to lower layers — a documented limitation, not an on-air
        // deviation of the PDU contents.
        let m = SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mm,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LmmMleUnitdataReq(LmmMleUnitdataReq {
                sdu,
                handle: 0,
                address: TetraAddress::issi(issi),
                layer2service: Layer2Service::Acknowledged,
                stealing_permission: false,
                stealing_repeats_flag: false,
                encryption_flag: false,
                is_null_pdu: false,
                tx_reporter: None,
            }),
        };
        queue.push_back(m);

        // Track the target cell so a subsequent ACCEPT promotes it to the
        // registered cell (the demand re-registers on the current serving cell).
        self.pending_cell = self.current_cell;
        self.reg_state = RegState::Registering;
        self.t351_countdown = T351_TIMEOUT_SLOTS;
        self.attempts += 1;

        // TNMM-SERVICE indication (Table 15.6): in service, awaiting the
        // registration response (cl. 15.3.4 "in service waiting for registration").
        self.emit_service(crate::tnmm::ServiceStatus::InServiceWaitingForRegistration);
    }

    /// Build and send a U-ITSI DETACH (cl. 16.9.3.3) down to MLE as part of the
    /// de-registration procedure (cl. 16.6.1). Sent with an MLE-UNITDATA request
    /// over acknowledged basic link (TL-DATA, per Figure 16.7); the MS identity
    /// is carried by the MAC-layer source address, so the optional MNI address
    /// extension is omitted (cl. 16.6.1 note: it cannot fully safeguard the
    /// identity anyway as there is no ISSI element in the PDU). De-registration
    /// is best-effort — MM does not wait for a D-MM STATUS response.
    fn send_itsi_detach(&mut self, queue: &mut MessageQueue) {
        let issi = self.own_issi();

        // Minimal detach: no MNI address extension, no proprietary element.
        let pdu = UItsiDetach {
            address_extension: None,
            proprietary: None,
        };

        let mut sdu = BitBuffer::new_autoexpand(4);
        pdu.to_bitbuf(&mut sdu).expect("U-ITSI DETACH serialization");
        sdu.seek(0);
        tracing::info!(
            "MM: -> U-ITSI DETACH (de-registration) for ISSI {} sdu {}",
            issi,
            sdu.dump_bin()
        );

        let m = SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mm,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LmmMleUnitdataReq(LmmMleUnitdataReq {
                sdu,
                handle: 0,
                address: TetraAddress::issi(issi),
                layer2service: Layer2Service::Acknowledged,
                stealing_permission: false,
                stealing_repeats_flag: false,
                encryption_flag: false,
                is_null_pdu: false,
                tx_reporter: None,
            }),
        };
        queue.push_back(m);
    }

    fn rx_lmm_mle_unitdata_ind(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LmmMleUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };

        let Some(bits) = prim.sdu.peek_bits(4) else {
            tracing::warn!("insufficient bits: {}", prim.sdu.dump_bin());
            return;
        };

        let Ok(pdu_type) = MmPduTypeDl::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, prim.sdu.dump_bin());
            return;
        };

        match pdu_type {
            MmPduTypeDl::DOtar => unimplemented_log!("DOtar"),
            MmPduTypeDl::DAuthentication => unimplemented_log!("DAuthentication"),
            MmPduTypeDl::DCkChangeDemand => unimplemented_log!("DCkChangeDemand"),
            MmPduTypeDl::DDisable => unimplemented_log!("DDisable"),
            MmPduTypeDl::DEnable => unimplemented_log!("DEnable"),
            MmPduTypeDl::DLocationUpdateAccept => self.rx_d_location_update_accept(queue, prim.sdu.clone()),
            MmPduTypeDl::DLocationUpdateCommand => {
                self.rx_d_location_update_command(queue, prim.sdu.clone())
            }
            MmPduTypeDl::DLocationUpdateReject => self.rx_d_location_update_reject(prim.sdu.clone()),
            MmPduTypeDl::DLocationUpdateProceeding => unimplemented_log!("DLocationUpdateProceeding"),
            MmPduTypeDl::DAttachDetachGroupIdentity => unimplemented_log!("DAttachDetachGroupIdentity"),
            MmPduTypeDl::DAttachDetachGroupIdentityAcknowledgement => {
                unimplemented_log!("DAttachDetachGroupIdentityAcknowledgement")
            }
            MmPduTypeDl::DMmStatus => unimplemented_log!("DMmStatus"),
            MmPduTypeDl::MmPduFunctionNotSupported => unimplemented_log!("MmPduFunctionNotSupported"),
        };
    }

    /// Handle D-LOCATION-UPDATE-ACCEPT (cl. 16.9.2.7): the SwMI has accepted our
    /// registration. Mark the MS as registered and stop the retry timer.
    fn rx_d_location_update_accept(&mut self, queue: &mut MessageQueue, mut sdu: BitBuffer) {
        let pdu = match DLocationUpdateAccept::from_bitbuf(&mut sdu) {
            Ok(pdu) => pdu,
            Err(e) => {
                tracing::warn!("Failed parsing DLocationUpdateAccept: {:?} {}", e, sdu.dump_bin());
                return;
            }
        };

        if self.reg_state == RegState::Registered {
            tracing::debug!("MM: <- D-LOCATION-UPDATE-ACCEPT (duplicate); already registered");
            return;
        }

        // Reinterpret the accepted location-update type through the *accept-type*
        // enumeration (cl. 16.10.35a, Table 16.68). The shared D-LOCATION-UPDATE-
        // ACCEPT PDU types this field as `LocationUpdateType` (the *demand* type,
        // Table 16.67), but the two tables diverge at raw values 1 and 5 — most
        // importantly raw 0012 means "Temporary registration" on accept versus
        // "migrating" on demand. Recover the true accepted type MS-side without
        // touching the shared PDU/BS decode.
        let accept_type = LocationUpdateAcceptType::try_from(pdu.location_update_accept_type.into_raw());

        // cl. 16.4.8: an accept with status "Temporary registration" means the
        // registration was accepted only temporarily. Remember it so a later cell
        // reselection / a return of the cell to normal mode triggers the required
        // re-registration (cl. 16.4.1.0 cond. 4 / periodic update).
        self.temporary_registration =
            matches!(accept_type, Ok(LocationUpdateAcceptType::TemporaryRegistration));

        tracing::info!(
            "MM: <- D-LOCATION-UPDATE-ACCEPT type={} ssi={:?}: registration COMPLETE{}",
            accept_type
                .map(|t| t.to_string())
                .unwrap_or_else(|_| format!("raw={}", pdu.location_update_accept_type.into_raw())),
            pdu.ssi,
            if self.temporary_registration { " (TEMPORARY registration, cl. 16.4.8)" } else { "" },
        );

        // Report the outcome of the group identity attachment requested in the
        // demand (cl. 16.8.2 / 16.10.24). The SwMI returns the accepted groups
        // (each carrying a group_identity_attachment) and/or rejected ones.
        let mut attached_gssis: Vec<u32> = Vec::new();
        let mut group_accept_present = false;
        if let Some(accept) = &pdu.group_identity_location_accept {
            group_accept_present = true;
            attached_gssis = accept
                .group_identity_downlink
                .as_ref()
                .map(|v| {
                    v.iter()
                        .filter(|g| g.group_identity_attachment.is_some())
                        .filter_map(|g| g.gssi)
                        .collect()
                })
                .unwrap_or_default();
            tracing::info!(
                "MM: group attachment result (accept_reject={}): attached GSSIs {:?}",
                accept.group_identity_accept_reject,
                attached_gssis
            );
        } else if !self.attach_groups().is_empty() {
            // cl. 16.4.1.1: "In case the group identity location accept
            // information element is not present in the D-LOCATION UPDATE ACCEPT
            // PDU, the MS shall assume the group attachment/detachment failed.
            // The MS shall treat the failure as equivalent to T353 timer expiry."
            // The ITSI registration itself is still accepted; only the group
            // attachment failed. (T353 handling / re-attach is part of the group
            // identity procedures, cl. 16.8 — a later MM slice.)
            tracing::warn!(
                "MM: registration accepted but no group identity location accept returned; \
                 assuming group attachment failed (cl. 16.4.1.1, equivalent to T353 expiry) \
                 for configured groups {:?}",
                self.attach_groups()
            );
        }

        // Successful registration: stop timer T351 (cl. 16.4.1.1) and clear the
        // N351 system-rejection counter (cl. 16.11.2.1 counts rejections "without
        // a successful registration"). Promote the pending cell to the registered
        // cell so its LA becomes the registered area against which a later LA
        // change is evaluated (cl. 18.3.4.7.1a).
        self.reg_state = RegState::Registered;
        if self.pending_cell.is_some() {
            self.registered_cell = self.pending_cell;
        }
        self.t351_countdown = 0;
        self.system_rejection_count = 0;

        // Reconcile the runtime attached-group set with what the SwMI confirmed,
        // then push it to the MLE so the MAC downlink address filter matches the
        // actual affiliation (cl. 16.8.2 last paragraph / cl. 18.441-442: MM
        // sends the accepted-and-thus-attached group identities to the MLE with
        // the MLE-IDENTITIES request). If the SwMI returned no group accept
        // element the attached set is left as-is (config-seeded) so the filter is
        // unchanged, preserving prior behaviour.
        if group_accept_present {
            self.attached_gssis = attached_gssis.iter().copied().collect();
        }
        self.send_mle_identities(queue, &[]);

        // TNMM-SAP (cl. 15.3) outbound indications to the user application. MM has
        // just carried out the registration procedure successfully.
        self.emit_registration_success(&attached_gssis);
    }

    /// Send an MLE-IDENTITIES request (cl. 17.3.2) carrying the MS's own ISSI and
    /// the complete current attached-group set. The MLE uses it to (re)configure
    /// the MAC downlink address filter (cl. 23.4.1.2.1). `detached_gssis` is
    /// purely informational (for logging on the MLE side); the authoritative
    /// post-update set is the attached set. Called after a registration accept
    /// and after every successful standalone group attach/detach (cl. 16.8.2).
    fn send_mle_identities(&mut self, queue: &mut MessageQueue, detached_gssis: &[u32]) {
        let issi = self.own_issi();
        let attached: Vec<u32> = self.attach_groups();
        tracing::info!(
            "MM: -> MLE-IDENTITIES (issi={} attached_gssis={:?} detached_gssis={:?})",
            issi,
            attached,
            detached_gssis
        );
        let m = SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mm,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LmmMleIdentitiesReq(LmmMleIdentitiesReq {
                issi,
                assi: None,
                attached_gssis: attached,
                detached_gssis: detached_gssis.to_vec(),
            }),
        };
        queue.push_back(m);
    }

    /// Handle D-LOCATION-UPDATE-REJECT (cl. 16.9.2.9 / 16.4.1.1): the SwMI
    /// refused our registration. Per cl. 16.4.1.1 the reject is acted upon only
    /// while timer T351 (or T354) is active; MM stops T351/T352, then analyses
    /// the reject cause and either re-tries, abandons the cell (awaiting cell
    /// reselection), or — after N351 "system rejection" results — leaves the
    /// system (cl. 16.11.2.1).
    fn rx_d_location_update_reject(&mut self, mut sdu: BitBuffer) {
        let pdu = match DLocationUpdateReject::from_bitbuf(&mut sdu) {
            Ok(pdu) => pdu,
            Err(e) => {
                tracing::warn!("Failed parsing DLocationUpdateReject: {:?} {}", e, sdu.dump_bin());
                return;
            }
        };

        // cl. 16.4.1.1: the reject is processed only if timer T351 (or T354) is
        // active — i.e. we are awaiting a registration response. A stray reject
        // outside a registration attempt is ignored.
        if self.reg_state != RegState::Registering {
            tracing::debug!(
                "MM: <- D-LOCATION-UPDATE-REJECT with no registration outstanding (T351 not active); ignoring"
            );
            return;
        }

        // Stop timers T351 and T352 (cl. 16.4.1.1).
        self.t351_countdown = 0;

        let cause = RejectCause::try_from(pdu.reject_cause as u64);
        match &cause {
            Ok(c) => tracing::warn!(
                "MM: <- D-LOCATION-UPDATE-REJECT type={:?} reject_cause={} ({})",
                pdu.location_update_type,
                pdu.reject_cause,
                c
            ),
            Err(_) => tracing::warn!(
                "MM: <- D-LOCATION-UPDATE-REJECT type={:?} unknown reject_cause={}",
                pdu.location_update_type,
                pdu.reject_cause
            ),
        }

        // TNMM-REGISTRATION indication ("failure" + reject cause, cl. 16.4.1.1)
        // to the user application is emitted from the terminal branches below —
        // only where MM actually abandons the registration (not while a re-try is
        // still outstanding, which keeps the procedure in progress).
        let cause_opt = cause.ok();

        let action = cause.map(Self::analyse_reject_cause).unwrap_or(RejectAction::Abandon);
        match action {
            RejectAction::Retry => {
                if self.attempts < MAX_REGISTRATION_ATTEMPTS {
                    tracing::info!(
                        "MM: reject cause permits re-try; resending U-LOCATION-UPDATE-DEMAND (attempt {})",
                        self.attempts + 1
                    );
                    // send_location_update_demand re-arms T351 and Registering.
                    // (caller state already Registering)
                    self.resend_after_reject();
                } else {
                    tracing::warn!(
                        "MM: reject cause permits re-try but resend cap ({}) reached; abandoning",
                        MAX_REGISTRATION_ATTEMPTS
                    );
                    self.reg_state = RegState::Idle;
                    // Terminal: registration procedure carried out unsuccessfully.
                    self.emit_registration_failure(cause_opt);
                }
            }
            RejectAction::Abandon => {
                // The spec issues an MLE-UPDATE request (LA / cell-type / cell
                // rejection) so MLE runs cell reselection (cl. 18.3.4.7). MLE-side
                // reselection is a later slice; MM abandons this attempt and awaits
                // the next LMM-ACTIVATE confirmation.
                tracing::info!("MM: reject cause requires cell reselection / abandon; returning to idle");
                self.reg_state = RegState::Idle;
                // Terminal: registration procedure carried out unsuccessfully.
                self.emit_registration_failure(cause_opt);
            }
            RejectAction::SystemRejection => {
                self.system_rejection_count = self.system_rejection_count.saturating_add(1);
                if self.system_rejection_count >= N351_MAX_SYSTEM_REJECTIONS {
                    tracing::error!(
                        "MM: reached N351={} system rejections; leaving the system (cl. 16.11.2.1), \
                         no further registration until power cycle",
                        N351_MAX_SYSTEM_REJECTIONS
                    );
                    self.left_system = true;
                } else {
                    tracing::warn!(
                        "MM: system rejection {}/{}; abandoning attempt, MLE cell reselection pending",
                        self.system_rejection_count,
                        N351_MAX_SYSTEM_REJECTIONS
                    );
                }
                self.reg_state = RegState::Idle;
                // Terminal: registration procedure carried out unsuccessfully.
                self.emit_registration_failure(cause_opt);
            }
        }
    }

    /// Resend the U-LOCATION-UPDATE-DEMAND after a recoverable reject cause. Kept
    /// separate from the timeout resend so the log context differs; both re-arm
    /// T351 and increment the attempt count via `send_location_update_demand`.
    fn resend_after_reject(&mut self) {
        // A fresh MessageQueue is not available here; the reject handler is
        // invoked from rx_lmm_mle_unitdata_ind which does not thread the queue
        // down. The demand is instead re-sent from tick_start on the next slot by
        // leaving reg_state = Registering with an expired T351. Setting the
        // countdown to 0 makes tick_start resend immediately (cl. 16.4.5).
        self.reg_state = RegState::Registering;
        self.t351_countdown = 0;
    }

    /// Classify a D-LOCATION-UPDATE-REJECT cause into the action MM must take,
    /// strictly per the reject-cause analysis in cl. 16.4.1.1. Causes not
    /// enumerated there for normal registration (e.g. security/ciphering causes,
    /// which belong to ETSI EN 300 392-7) are treated as `Abandon` and logged;
    /// they are not counted as N351 "system rejections".
    fn analyse_reject_cause(cause: RejectCause) -> RejectAction {
        match cause {
            // "may re-try registration after a suitable time" (else cell rejection).
            RejectCause::Congestion | RejectCause::NetworkFailure => RejectAction::Retry,
            // "the MS shall be allowed at least one registration re-try".
            RejectCause::MandatoryElementError | RejectCause::MessageConsistencyError => {
                RejectAction::Retry
            }
            // "cell type rejection" → MLE-UPDATE → cell reselection.
            RejectCause::UseCaCellNotPermitted | RejectCause::UseDaCellNotPermitted => {
                RejectAction::Abandon
            }
            // "LA rejection" → MLE-UPDATE → cell reselection.
            RejectCause::LaNotAllowed
            | RejectCause::ServiceNotSubscribed
            | RejectCause::RoamingNotSupported => RejectAction::Abandon,
            // "system rejection" type causes (counted against N351). Our MAC
            // header carries the ISSI (no ASSI/(V)ASSI has been issued), so the
            // ITSI/ATSI-unknown ASSI-retry branch does not apply.
            RejectCause::ItsiAtsiUnknown
            | RejectCause::IllegalMs
            | RejectCause::MigrationNotSupported => RejectAction::SystemRejection,
            // "not applicable to normal registration".
            RejectCause::ForwardRegistrationFailure => RejectAction::Abandon,
            // All other causes (LA unknown, and the security/ciphering causes
            // defined in ETSI EN 300 392-7) are outside the scope of the normal
            // registration reject analysis of cl. 16.4.1.1.
            _ => RejectAction::Abandon,
        }
    }

    /// Drain and handle inbound external-interface commands (UI -> stack).
    ///
    /// The control endpoint is polled once per downlink tick (mirroring
    /// `MmBs::tick_start`). Commands are drained first so the immutable borrow of
    /// `self.control` is released before any handler mutates MM state.
    ///
    /// Unlike the BS placeholder handler this never panics: commands originate
    /// from an external UI process and an unexpected/unsupported variant must not
    /// be able to crash the MS stack.
    fn poll_control(&mut self, queue: &mut MessageQueue) {
        let mut commands = Vec::new();
        if let Some(cep) = &self.control {
            while let Some(cmd) = cep.try_recv() {
                commands.push(cmd);
            }
        }
        for cmd in commands {
            self.handle_control_command(queue, cmd);
        }
    }

    /// Send a control response back to the UI, if a control endpoint is wired.
    fn respond(&self, response: crate::net_control::ControlResponse) {
        if let Some(cep) = &self.control {
            cep.respond(response);
        }
    }

    /// Handle one inbound control command. TNMM-SAP requests (Plane A, cl. 15.3)
    /// are acted upon here; the TNMM *result* is reported asynchronously through
    /// the outbound TNMM-SAP indications/confirms on the telemetry channel
    /// (cl. 15.3.2), so the control response only acknowledges whether MM acted.
    fn handle_control_command(&mut self, queue: &mut MessageQueue, cmd: crate::net_control::ControlCommand) {
        use crate::net_control::{ControlCommand, ControlResponse};
        match cmd {
            // TNMM-REGISTRATION request (Table 15.5, cl. 15.3.3.7): initiate
            // attachment and registration of the terminal.
            ControlCommand::TnmmRegistration { handle, request } => {
                self.handle_tnmm_registration_request(queue, handle, &request);
            }
            // TNMM-DEREGISTRATION request (Table 15.2, cl. 15.3.3.2): cancel the
            // registration. Reuses the shutdown de-registration path (cl. 16.6.1),
            // which sends U-ITSI DETACH and emits TNMM-SERVICE "out of service".
            ControlCommand::TnmmDeregistration { handle, request } => {
                // Table 15.2 NOTE: with all attached ITSIs detached the ISSI/MCC/
                // MNC need not be present. When present, they must select this
                // MS's own ITSI (single-ITSI stack).
                if let Some(issi) = request.issi {
                    if issi != self.own_issi() {
                        tracing::warn!(
                            "MM(MS): TNMM-DEREGISTRATION for ISSI {} != own ISSI {}; ignoring",
                            issi,
                            self.own_issi()
                        );
                        self.respond(ControlResponse::TnmmAck {
                            handle,
                            accepted: false,
                            detail: Some("ISSI does not match the configured ITSI".to_string()),
                        });
                        return;
                    }
                }
                let acted = self.begin_deregistration(queue);
                let detail = if acted { None } else { Some("not registered; nothing to detach".to_string()) };
                self.respond(ControlResponse::TnmmAck { handle, accepted: true, detail });
            }
            // TNMM-ATTACH DETACH GROUP IDENTITY request (Table 15.1, cl. 15.3.3.1):
            // the standalone U-ATTACH/DETACH GROUP IDENTITY procedure (cl. 16.9.3)
            // is not implemented in this stack — group attachment is only carried
            // bundled in the ITSI-attach registration (cl. 16.8.2). Documented
            // deferral: accepted = false so the UI knows no action was taken.
            ControlCommand::TnmmAttachDetachGroupIdentity { handle, request } => {
                tracing::warn!(
                    "MM(MS): TNMM-ATTACH DETACH GROUP IDENTITY request received but the standalone \
                     group identity procedure (cl. 16.9.3) is not implemented; {} entrie(s) ignored",
                    request.group_identity_request.len()
                );
                self.respond(ControlResponse::TnmmAck {
                    handle,
                    accepted: false,
                    detail: Some(
                        "standalone group attach/detach (cl. 16.9.3) not implemented; \
                         groups are attached at registration"
                            .to_string(),
                    ),
                });
            }
            // TNMM-STATUS request (Table 15.7, cl. 15.3.3.9): selects direct mode
            // / dual watch / energy economy — none implemented in this stack.
            ControlCommand::TnmmStatus { handle, .. } => {
                tracing::warn!(
                    "MM(MS): TNMM-STATUS request received but direct mode / dual watch / energy \
                     economy are not implemented; ignoring"
                );
                self.respond(ControlResponse::TnmmAck {
                    handle,
                    accepted: false,
                    detail: Some("direct mode / dual watch / energy economy not implemented".to_string()),
                });
            }
            // TNMM-ENERGY SAVING request (Table 15.3, cl. 15.3.3.5): dormant —
            // the energy-economy procedure (cl. 16.7) is not implemented.
            ControlCommand::TnmmEnergySaving { handle, .. } => {
                tracing::warn!("MM(MS): TNMM-ENERGY SAVING request received but energy economy (cl. 16.7) is not implemented; ignoring");
                self.respond(ControlResponse::TnmmAck {
                    handle,
                    accepted: false,
                    detail: Some("energy economy (cl. 16.7) not implemented".to_string()),
                });
            }
            // Management / provisioning (Plane B, **NON-STANDARD**). Served here
            // because MM is the single writer of MS runtime state. See
            // `crate::management` for the standards disclaimer.
            ControlCommand::Management(mgmt) => {
                self.handle_management_command(mgmt, queue);
            }
            // Non-TNMM commands are not addressed to MM(MS); log and drop.
            other => {
                tracing::warn!("MM(MS): received non-TNMM control command with no MS handler, dropping: {:?}", other);
            }
        }
    }

    /// Act on a TNMM-REGISTRATION request (Table 15.5). Validates the requested
    /// ITSI against the configured one (single-ITSI stack) and, when the MS is
    /// idle, initiates the ITSI-attach registration by sending a
    /// U-LOCATION-UPDATE-DEMAND — the same path taken on serving-cell selection
    /// (rx_activate_conf). The registration *result* is reported later through
    /// the TNMM-REGISTRATION indication/confirm.
    fn handle_tnmm_registration_request(
        &mut self,
        queue: &mut MessageQueue,
        handle: u32,
        request: &crate::tnmm::TnmmRegistrationRequest,
    ) {
        use crate::net_control::ControlResponse;

        // The request identifies the ITSI to register (ISSI + MNI). This stack
        // manages a single configured ITSI, so a mismatch cannot be honoured.
        let own_mcc = self.config.config().net.mcc;
        let own_mnc = self.config.config().net.mnc;
        if request.issi != self.own_issi() || request.mcc_of_issi != own_mcc || request.mnc_of_issi != own_mnc {
            tracing::warn!(
                "MM(MS): TNMM-REGISTRATION request for ITSI {}/{}/{} != configured {}/{}/{}; ignoring",
                request.mcc_of_issi,
                request.mnc_of_issi,
                request.issi,
                own_mcc,
                own_mnc,
                self.own_issi()
            );
            self.respond(ControlResponse::TnmmAck {
                handle,
                accepted: false,
                detail: Some("ISSI/MNI does not match the configured ITSI".to_string()),
            });
            return;
        }

        if self.left_system {
            self.respond(ControlResponse::TnmmAck {
                handle,
                accepted: false,
                detail: Some("MS left the system (N351); registration requires a restart".to_string()),
            });
            return;
        }

        match self.reg_state {
            RegState::Idle => {
                // Initiate the ITSI-attach registration. Note: MLE must have a
                // serving cell for the demand to actually be transmitted; the
                // demand reuses the exact air path of rx_activate_conf.
                tracing::info!("MM(MS): TNMM-REGISTRATION request; initiating ITSI attach registration");
                self.attempts = 0;
                self.send_location_update_demand(queue, LocationUpdateType::ItsiAttach);
                self.respond(ControlResponse::TnmmAck { handle, accepted: true, detail: None });
            }
            RegState::Registering => {
                self.respond(ControlResponse::TnmmAck {
                    handle,
                    accepted: false,
                    detail: Some("registration already in progress".to_string()),
                });
            }
            RegState::Registered => {
                self.respond(ControlResponse::TnmmAck {
                    handle,
                    accepted: false,
                    detail: Some("already registered".to_string()),
                });
            }
            RegState::Detaching => {
                self.respond(ControlResponse::TnmmAck {
                    handle,
                    accepted: false,
                    detail: Some("de-registration in progress".to_string()),
                });
            }
        }
    }

    // ----------------------------------------------------------------------
    // Management / provisioning handlers (Plane B, **NON-STANDARD**).
    //
    // Implementation-defined stack provisioning + runtime-state reads. NOT part
    // of any ETSI standard (see `crate::management`). Carried over the reused
    // control transport in the dedicated `Management` variant.
    // ----------------------------------------------------------------------

    /// Handle one inbound management command (Plane B, non-standard).
    fn handle_management_command(&mut self, cmd: crate::management::ManagementCommand, queue: &mut MessageQueue) {
        use crate::management::{ManagementCommand, ManagementResponse};
        use crate::net_control::ControlResponse;
        match cmd {
            // Read-only runtime state snapshot; always serviceable.
            ManagementCommand::GetState { handle } => {
                let state = self.runtime_snapshot();
                self.respond(ControlResponse::Management(ManagementResponse::State {
                    handle,
                    state: Box::new(state),
                }));
            }
            // Frozen interface schema version discovery; always serviceable.
            ManagementCommand::GetInterfaceVersion { handle } => {
                self.respond(ControlResponse::Management(ManagementResponse::InterfaceVersion {
                    handle,
                    version: crate::management::MS_INTERFACE_SCHEMA_VERSION.to_string(),
                }));
            }
            // Read the active configuration as canonical TOML (always serviceable).
            // Secrets are redacted on the wire; the real values never leave the
            // process (restored on SetConfig write-back from the current config).
            ManagementCommand::GetConfig { handle } => {
                let cfg = self.config.config();
                match tetra_config::bluestation::to_toml_string_redacted(&cfg) {
                    Ok(toml) => {
                        self.respond(ControlResponse::Management(ManagementResponse::Config { handle, toml }));
                    }
                    Err(e) => {
                        tracing::error!("MM(MS): failed to serialize active config: {e}");
                        self.respond(ControlResponse::Management(ManagementResponse::Error {
                            handle,
                            message: format!("failed to serialize active config: {e}"),
                        }));
                    }
                }
            }
            // Stage a new configuration: validate through the exact startup
            // validator, then persist to disk. Structural changes take effect on
            // the next `ApplyConfig`; the process is NOT bounced here.
            ManagementCommand::SetConfig { handle, toml } => {
                self.handle_set_config(handle, toml);
            }
            // Apply staged configuration: drain-and-restart under the external
            // supervisor. Reuses the graceful de-registration drain (cl. 16.6.1).
            ManagementCommand::ApplyConfig { handle } => {
                self.handle_apply_config(handle, queue);
            }
        }
    }

    /// Validate + persist a staged configuration (Plane B, non-standard).
    ///
    /// The payload is parsed through [`tetra_config::bluestation::from_toml_str`]
    /// — the exact validator the stack runs at startup — so a config that would
    /// fail to load is rejected here rather than bricking the radio on restart.
    /// On success it is re-serialized to canonical form and written to the
    /// config file, and `restart_required` is set so the UI shows a pending
    /// restart. The running process keeps its current (old) config until an
    /// explicit `ApplyConfig`.
    fn handle_set_config(&mut self, handle: u32, toml: String) {
        use crate::management::ManagementResponse;
        use crate::net_control::ControlResponse;

        let ctx = match &self.mgmt_ctx {
            Some(ctx) => ctx,
            None => {
                tracing::warn!("MM(MS): SetConfig received but no management context installed; refusing");
                self.respond(ControlResponse::Management(ManagementResponse::Ack {
                    handle,
                    accepted: false,
                    restart_required: self.restart_required,
                    message: "configuration write-back is not available in this deployment".to_string(),
                }));
                return;
            }
        };

        // 1) Validate: parse through the startup validator.
        let parsed = match tetra_config::bluestation::from_toml_str(&toml) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!("MM(MS): SetConfig rejected invalid configuration: {e}");
                self.respond(ControlResponse::Management(ManagementResponse::Ack {
                    handle,
                    accepted: false,
                    restart_required: self.restart_required,
                    message: format!("configuration rejected: {e}"),
                }));
                return;
            }
        };

        // 2) Merge redacted secrets: a UI that read the config via GetConfig sees
        //    secrets as the REDACTED_SECRET sentinel. Preserve the live on-disk
        //    value for any secret returned unchanged (sentinel), so a benign
        //    round-trip edit never clobbers a credential; genuinely-new secrets
        //    (not the sentinel) are kept as supplied.
        let current = self.config.config();
        let parsed = tetra_config::bluestation::restore_redacted_secrets(parsed, &current);

        // 3) Normalize: re-serialize so the on-disk file is canonical and
        //    provably re-parses to the validated config.
        let normalized = match tetra_config::bluestation::to_toml_string(&parsed) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("MM(MS): SetConfig failed to normalize configuration: {e}");
                self.respond(ControlResponse::Management(ManagementResponse::Ack {
                    handle,
                    accepted: false,
                    restart_required: self.restart_required,
                    message: format!("configuration could not be serialized: {e}"),
                }));
                return;
            }
        };

        // 4) Persist to the config file path.
        let path = ctx.config_path.clone();
        if let Err(e) = std::fs::write(&path, normalized) {
            tracing::error!("MM(MS): SetConfig failed to write {}: {e}", path.display());
            self.respond(ControlResponse::Management(ManagementResponse::Ack {
                handle,
                accepted: false,
                restart_required: self.restart_required,
                message: format!("failed to write configuration file: {e}"),
            }));
            return;
        }

        self.restart_required = true;
        tracing::info!("MM(MS): staged new configuration to {} (restart required to apply)", path.display());
        self.respond(ControlResponse::Management(ManagementResponse::Ack {
            handle,
            accepted: true,
            restart_required: true,
            message: "configuration staged; apply (restart) required to take effect".to_string(),
        }));
    }

    /// Apply staged configuration by draining and requesting a controlled
    /// restart (Plane B, non-standard).
    ///
    /// This reuses the existing graceful shutdown path: if the MS is registered
    /// it begins de-registration (U-ITSI DETACH, cl. 16.6.1) so the drain sends
    /// it, then clears the shared `is_running` flag to break the stack run loop
    /// and sets `restart_requested` so `main` exits with the restart exit code.
    /// The external supervisor respawns the process with the new config. It does
    /// **not** relaunch in-process (the SDR handle and thread graph are owned by
    /// `main`).
    fn handle_apply_config(&mut self, handle: u32, queue: &mut MessageQueue) {
        use crate::management::ManagementResponse;
        use crate::net_control::ControlResponse;

        let ctx = match &self.mgmt_ctx {
            Some(ctx) => ctx,
            None => {
                tracing::warn!("MM(MS): ApplyConfig received but no management context installed; refusing");
                self.respond(ControlResponse::Management(ManagementResponse::Ack {
                    handle,
                    accepted: false,
                    restart_required: self.restart_required,
                    message: "restart/apply is not available in this deployment".to_string(),
                }));
                return;
            }
        };

        // Signal main to exit with the restart code once the loop unwinds, and
        // clear the running flag to break the loop and start the drain.
        ctx.restart_requested.store(true, Ordering::SeqCst);
        ctx.is_running.store(false, Ordering::SeqCst);

        // Reuse the graceful de-registration drain so a registered MS cleanly
        // detaches before the restart (best-effort, cl. 16.6.1).
        let detaching = self.begin_deregistration(queue);

        tracing::info!(
            "MM(MS): ApplyConfig -> restart requested (detach initiated: {}); supervisor will respawn with new config",
            detaching
        );
        self.respond(ControlResponse::Management(ManagementResponse::Ack {
            handle,
            accepted: true,
            restart_required: self.restart_required,
            message: "restart initiated; the stack will de-register and respawn with the staged configuration".to_string(),
        }));
    }

    /// Build an [`crate::management::MsRuntimeState`] snapshot from MM's own
    /// state and the active configuration (Plane B, non-standard). MM is the
    /// single writer of MS runtime state, so no locking is required.
    fn runtime_snapshot(&self) -> crate::management::MsRuntimeState {
        use crate::management::{MsRuntimeState, RegistrationState};
        use crate::tnmm::ServiceStatus;

        let registration_state = match self.reg_state {
            RegState::Idle => RegistrationState::Idle,
            RegState::Registering => RegistrationState::Registering,
            RegState::Registered => RegistrationState::Registered,
            RegState::Detaching => RegistrationState::Detaching,
        };
        // Derived service status: mirrors the vocabulary of the TNMM-SERVICE
        // indication (Plane A) without asserting a standardized primitive.
        let service_status = match self.reg_state {
            RegState::Registered => ServiceStatus::InService,
            RegState::Registering => ServiceStatus::InServiceWaitingForRegistration,
            RegState::Idle | RegState::Detaching => ServiceStatus::OutOfService,
        };
        let cfg = self.config.config();
        MsRuntimeState {
            registration_state,
            service_status,
            own_issi: self.own_issi(),
            home_mcc: cfg.net.mcc,
            home_mnc: cfg.net.mnc,
            serving_la: self.serving_la,
            colour_code: cfg.cell.colour_code,
            attached_groups: self.attach_groups(),
            restart_required: self.restart_required,
        }
    }
}
/// Action MM takes after analysing a D-LOCATION-UPDATE-REJECT cause
/// (ETSI TS 100 392-2 cl. 16.4.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RejectAction {
    /// Recoverable cause — MM may re-try the registration.
    Retry,
    /// Cause requires abandoning the cell and awaiting cell reselection
    /// (the spec's LA / cell-type / cell rejection MLE-UPDATE results).
    Abandon,
    /// A registration result of type "system rejection" — counts toward N351;
    /// once N351 is reached the MS leaves the system (cl. 16.11.2.1).
    SystemRejection,
}

impl TetraEntityTrait for MmMs {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Mm
    }

    fn set_config(&mut self, config: SharedConfig) {
        self.config = config;
    }

    fn tick_start(&mut self, queue: &mut MessageQueue, _ts: TdmaTime) {
        // Drain any inbound external-interface (UI -> stack) commands first.
        // Phase T0 wires the endpoint but defines no MS control commands yet;
        // TNMM requests (ETSI TS 100 392-2 cl. 15.3) are handled here in T2.
        self.poll_control(queue);

        // While detaching at shutdown, count down the bounded drain that gives
        // the MAC/PHY time to transmit the U-ITSI DETACH (cl. 16.6.1).
        if self.reg_state == RegState::Detaching {
            self.detach_countdown = self.detach_countdown.saturating_sub(1);
            return;
        }

        // Drive timer T351 (registration response time, cl. 16.11.1.1). Only
        // active while awaiting a response to a sent demand. On expiry MM resends
        // the U-LOCATION-UPDATE-DEMAND (cl. 16.4.5 permits resending), bounded by
        // an implementation resend cap, after which it returns to idle to await a
        // fresh cell (re)selection.
        if self.reg_state != RegState::Registering {
            return;
        }
        if self.t351_countdown > 0 {
            self.t351_countdown -= 1;
            return;
        }
        if self.attempts >= MAX_REGISTRATION_ATTEMPTS {
            tracing::error!(
                "MM: registration failed after {} attempts (T351 expiries); abandoning until next cell (re)selection",
                self.attempts
            );
            self.reg_state = RegState::Idle;
            return;
        }
        tracing::warn!(
            "MM: T351 expired with no registration response; resending U-LOCATION-UPDATE-DEMAND (attempt {})",
            self.attempts + 1
        );
        self.send_location_update_demand(queue, self.pending_lu_type);
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);

        // There is only one SAP for MM
        assert!(message.sap == Sap::LmmSap);

        match &message.msg {
            SapMsgInner::LmmMleUnitdataInd(_) => {
                self.rx_lmm_mle_unitdata_ind(queue, message);
            }
            SapMsgInner::LmmMleActivateConf(conf) => {
                let conf = conf.clone();
                self.rx_activate_conf(queue, &conf);
            }
            _ => {
                panic!();
            }
        }
    }

    /// Shutdown hook (cl. 16.6.1): if the MS is registered, emit a U-ITSI DETACH
    /// and ask the router to keep running so it can be transmitted. Returns
    /// `true` only when a detach was actually initiated.
    fn begin_deregistration(&mut self, queue: &mut MessageQueue) -> bool {
        if self.reg_state != RegState::Registered {
            tracing::info!("MM: shutdown while not registered; no de-registration needed");
            return false;
        }
        tracing::info!("MM: shutdown while registered; sending U-ITSI DETACH (de-registration)");
        self.send_itsi_detach(queue);
        self.reg_state = RegState::Detaching;
        self.detach_countdown = DETACH_DRAIN_SLOTS;

        // TNMM-SERVICE indication (Table 15.6): the MS is de-registering and
        // therefore going out of service (cl. 15.3.4 "out of service").
        self.emit_service(crate::tnmm::ServiceStatus::OutOfService);
        true
    }

    fn deregistration_pending(&self) -> bool {
        self.reg_state == RegState::Detaching && self.detach_countdown > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net_control::ControlCommand;
    use crate::net_telemetry::TelemetryEvent;
    use tetra_config::bluestation::from_toml_str;
    use tetra_saps::lmm::LmmMleUnitdataInd;
    use tetra_pdus::mm::pdus::d_location_update_accept::DLocationUpdateAccept;
    use tetra_pdus::mm::pdus::u_itsi_detach::UItsiDetach;

    const MS_ISSI: u32 = 1000001;

    const MS_TOML: &str = r#"
config_version = "0.6"
stack_mode = "Ms"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
tx_freq = 439825000
rx_freq = 430425000
ppm_err = 0
device = "driver=sx"
sample_rate = 600000
rx_antenna = "RX"
tx_antenna = "TX"
rx_gain_lna = 48.0
rx_gain_pga = 8.0
tx_gain_dac = 0.0
tx_gain_mixer = 0.0

[net_info]
mcc = 901
mnc = 9999

[cell_info]
freq_band = 4
main_carrier = 1593
duplex_spacing = 7
custom_duplex_spacing = 9400000
freq_offset = 0
reverse_operation = false
location_area = 1
colour_code = 1

[ms]
issi = 1000001
subscriber_class = 1
attach_groups = []
"#;

    fn ms_mm() -> MmMs {
        let cfg = from_toml_str(MS_TOML).expect("valid MS test config");
        MmMs::new(SharedConfig::from_parts(cfg, None), None, None)
    }

    fn ms_mm_with_groups(groups: &[u32]) -> MmMs {
        let list = groups.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
        let toml = MS_TOML.replace("attach_groups = []", &format!("attach_groups = [{list}]"));
        let cfg = from_toml_str(&toml).expect("valid MS test config");
        MmMs::new(SharedConfig::from_parts(cfg, None), None, None)
    }

    fn activate_conf(registration_required: bool) -> LmmMleActivateConf {
        activate_conf_cell(registration_required, 901, 9999, 1)
    }

    fn activate_conf_cell(registration_required: bool, mcc: u16, mnc: u16, la: u16) -> LmmMleActivateConf {
        activate_conf_cell_sws(registration_required, mcc, mnc, la, true)
    }

    fn activate_conf_cell_sws(
        registration_required: bool,
        mcc: u16,
        mnc: u16,
        la: u16,
        system_wide_services: bool,
    ) -> LmmMleActivateConf {
        LmmMleActivateConf {
            registration_required,
            mcc,
            mnc,
            la,
            system_wide_services,
            cell_type: 0,
        }
    }

    /// Deliver a downlink MM PDU (already positioned at the MM PDU type) to MM as
    /// an LMM-UNITDATA indication, mirroring what MLE forwards.
    fn deliver_dl(mm: &mut MmMs, queue: &mut MessageQueue, mut sdu: BitBuffer) {
        sdu.seek(0);
        let msg = SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Mm,
            msg: SapMsgInner::LmmMleUnitdataInd(LmmMleUnitdataInd {
                sdu,
                handle: 0,
                received_address: TetraAddress::issi(MS_ISSI),
            }),
        };
        mm.rx_prim(queue, msg);
    }

    fn build_accept() -> BitBuffer {
        let pdu = DLocationUpdateAccept {
            location_update_accept_type: LocationUpdateType::ItsiAttach,
            ssi: Some(MS_ISSI as u64),
            address_extension: None,
            subscriber_class: None,
            energy_saving_information: None,
            scch_information_and_distribution_on_18th_frame: None,
            new_registered_area: None,
            security_downlink: None,
            group_identity_location_accept: None,
            default_group_attachment_lifetime: None,
            authentication_downlink: None,
            group_identity_security_related_information: None,
            cell_type_control: None,
            proprietary: None,
        };
        let mut sdu = BitBuffer::new_autoexpand(8);
        pdu.to_bitbuf(&mut sdu).unwrap();
        sdu
    }

    /// The demand MM emits on cell selection must be a valid, BS-acceptable
    /// U-LOCATION-UPDATE-DEMAND (ITSI attach, no ciphering, no optional fields):
    /// it round-trips through the exact parser the BS uses (cl. 16.9.3.4).
    #[test]
    fn test_registration_demand_on_activate_conf() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();

        mm.rx_activate_conf(&mut q, &activate_conf(true));
        assert_eq!(mm.reg_state, RegState::Registering);
        assert_eq!(mm.attempts, 1);

        let msg = q.pop_front().expect("a demand must be emitted");
        assert!(q.pop_front().is_none(), "exactly one message emitted");
        assert_eq!(msg.sap, Sap::LmmSap);
        assert_eq!(msg.dest, TetraEntity::Mle);
        let SapMsgInner::LmmMleUnitdataReq(mut req) = msg.msg else {
            panic!("expected LmmMleUnitdataReq");
        };
        assert_eq!(req.address.ssi, MS_ISSI);

        // The BS parses exactly this on the uplink; it must decode cleanly.
        req.sdu.seek(0);
        let pdu = ULocationUpdateDemand::from_bitbuf(&mut req.sdu).expect("BS must parse the demand");
        assert_eq!(pdu.location_update_type, LocationUpdateType::ItsiAttach);
        assert!(!pdu.request_to_append_la);
        assert!(!pdu.cipher_control);
        assert!(pdu.ciphering_parameters.is_none());
        assert!(pdu.class_of_ms.is_none());
        assert!(pdu.group_identity_location_demand.is_none());
        assert_eq!(req.sdu.get_len_remaining(), 0, "demand fully consumed");
    }

    /// With groups configured, the ITSI-attach demand must carry a group identity
    /// location demand element (cl. 16.8.2 / 16.10.24) attaching exactly those
    /// GSSIs — mode 1 ("detach all + attach these"), each an attachment with a
    /// class of usage — and it must round-trip through the BS parser.
    #[test]
    fn test_registration_demand_carries_group_attachment() {
        let mut mm = ms_mm_with_groups(&[91, 220]);
        let mut q = MessageQueue::new();

        mm.rx_activate_conf(&mut q, &activate_conf(true));

        let msg = q.pop_front().expect("a demand must be emitted");
        let SapMsgInner::LmmMleUnitdataReq(mut req) = msg.msg else {
            panic!("expected LmmMleUnitdataReq");
        };
        req.sdu.seek(0);
        let pdu = ULocationUpdateDemand::from_bitbuf(&mut req.sdu).expect("BS must parse the demand");

        let gild = pdu
            .group_identity_location_demand
            .expect("group identity location demand present");
        assert_eq!(gild.group_identity_attach_detach_mode, 1);
        let ul = gild.group_identity_uplink.expect("group entries present");
        assert_eq!(ul.len(), 2);
        let gssis: Vec<u32> = ul.iter().filter_map(|g| g.gssi).collect();
        assert_eq!(gssis, vec![91, 220]);
        assert!(ul.iter().all(|g| g.class_of_usage == Some(GROUP_CLASS_OF_USAGE)));
        assert!(ul.iter().all(|g| g.group_identity_detachment_uplink.is_none()));
        assert_eq!(req.sdu.get_len_remaining(), 0, "demand fully consumed");
    }

    /// Own MNI for the test config (mcc=901, mnc=9999): (901<<14)|9999.
    const MS_MNI: u64 = (901 << 14) | 9999;

    /// Build a D-LOCATION-UPDATE-COMMAND (cl. 16.9.2.8) with the given group
    /// report request flag and optional MNI address extension. Minimal: no
    /// ciphering, no cell type control, no proprietary element (as this BS
    /// sends).
    fn build_command(group_report: bool, address_extension: Option<u64>) -> BitBuffer {
        let pdu = DLocationUpdateCommand {
            group_identity_report: group_report,
            cipher_control: false,
            ciphering_parameters: None,
            address_extension,
            cell_type_control: None,
            proprietary: None,
        };
        let mut sdu = BitBuffer::new_autoexpand(8);
        pdu.to_bitbuf(&mut sdu).unwrap();
        sdu
    }

    /// A D-LOCATION-UPDATE-COMMAND (infrastructure-initiated registration,
    /// cl. 16.4.3) makes MM send a U-LOCATION-UPDATE-DEMAND of type "demand
    /// location updating" that contains the MNI (address extension), the ISSI
    /// (SSI element) and the class of MS element, and it must round-trip through
    /// the exact parser the BS uses.
    #[test]
    fn test_command_triggers_demand_location_updating() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();

        deliver_dl(&mut mm, &mut q, build_command(false, None));
        assert_eq!(mm.reg_state, RegState::Registering);
        assert_eq!(mm.attempts, 1);

        let msg = q.pop_front().expect("a demand must be emitted");
        assert!(q.pop_front().is_none(), "exactly one message emitted");
        let SapMsgInner::LmmMleUnitdataReq(mut req) = msg.msg else {
            panic!("expected LmmMleUnitdataReq");
        };
        assert_eq!(req.address.ssi, MS_ISSI);

        req.sdu.seek(0);
        let pdu = ULocationUpdateDemand::from_bitbuf(&mut req.sdu).expect("BS must parse the demand");
        assert_eq!(pdu.location_update_type, LocationUpdateType::DemandLocationUpdating);
        assert!(!pdu.request_to_append_la, "cl. 16.4.3: shall not request append LA");
        assert!(pdu.la_information.is_none(), "cl. 16.4.3: LA information shall be absent");
        assert_eq!(pdu.ssi, Some(MS_ISSI as u64), "cl. 16.4.3: ISSI in SSI element");
        assert_eq!(pdu.address_extension, Some(MS_MNI), "cl. 16.4.3: MNI in address extension");
        let class = pdu.class_of_ms.expect("cl. 16.4.3: class of MS shall be present");
        assert!(class.voice, "voice-capable radio");
        assert!(class.e2e_encryption_not_supported, "no E2E encryption");
        assert!(pdu.group_report_response.is_none(), "no group report requested");
        assert!(pdu.group_identity_location_demand.is_none(), "no groups configured");
        assert_eq!(req.sdu.get_len_remaining(), 0, "demand fully consumed");
    }

    /// A command WITH a group report request makes MM regard groups as
    /// un-attached and, since all reported groups fit in this one PDU, include a
    /// "group report response = group report complete" element (cl. 16.4.3 /
    /// 16.10.27a). Configured groups are (re)attached in the same demand.
    #[test]
    fn test_command_with_group_report_request() {
        let mut mm = ms_mm_with_groups(&[91]);
        let mut q = MessageQueue::new();

        deliver_dl(&mut mm, &mut q, build_command(true, Some(MS_MNI)));
        let msg = q.pop_front().expect("a demand must be emitted");
        let SapMsgInner::LmmMleUnitdataReq(mut req) = msg.msg else {
            panic!("expected LmmMleUnitdataReq");
        };
        req.sdu.seek(0);
        let pdu = ULocationUpdateDemand::from_bitbuf(&mut req.sdu).expect("BS must parse the demand");

        let grr = pdu.group_report_response.expect("group report response present");
        assert_eq!(grr.len, 1, "group report response is 1 bit");
        assert_eq!(grr.data, vec![0], "value 0 = group report complete (Table 16.59)");

        let gild = pdu
            .group_identity_location_demand
            .expect("configured group re-attached in the demand");
        assert_eq!(gild.group_identity_attach_detach_mode, 1);
        let gssis: Vec<u32> = gild
            .group_identity_uplink
            .expect("group entries present")
            .iter()
            .filter_map(|g| g.gssi)
            .collect();
        assert_eq!(gssis, vec![91]);
        assert_eq!(req.sdu.get_len_remaining(), 0, "demand fully consumed");
    }

    /// With a group report request but no configured groups, MM sends only the
    /// "group report complete" element and no group identity location demand
    /// (cl. 16.4.3: "If the MS has no groups to attach, it shall send
    /// U-LOCATION UPDATE DEMAND PDU containing a group report response
    /// information element indicating 'group report complete'").
    #[test]
    fn test_command_group_report_no_groups() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();

        deliver_dl(&mut mm, &mut q, build_command(true, None));
        let msg = q.pop_front().expect("a demand must be emitted");
        let SapMsgInner::LmmMleUnitdataReq(mut req) = msg.msg else {
            panic!("expected LmmMleUnitdataReq");
        };
        req.sdu.seek(0);
        let pdu = ULocationUpdateDemand::from_bitbuf(&mut req.sdu).expect("BS must parse the demand");
        assert!(pdu.group_report_response.is_some(), "group report complete sent");
        assert!(pdu.group_identity_location_demand.is_none(), "no groups to attach");
    }

    /// A command whose MNI address extension does not match the MS's own MNI is
    /// not for this MS and shall be ignored (cl. 16.4.3), leaving MM idle.
    #[test]
    fn test_command_wrong_mni_ignored() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();

        deliver_dl(&mut mm, &mut q, build_command(false, Some(MS_MNI ^ 0x1)));
        assert_eq!(mm.reg_state, RegState::Idle, "command for a different MNI ignored");
        assert!(q.pop_front().is_none(), "no demand emitted for a foreign MNI");
    }

    /// A second D-LOCATION-UPDATE-COMMAND arriving while a registration is
    /// already in progress (T351 active) is coalesced: MM does not emit a
    /// duplicate U-LOCATION-UPDATE-DEMAND (cl. 16.4.3 / 22.3.2.3 stop-and-wait).
    #[test]
    fn test_command_coalesced_while_registering() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();

        // First command → registration starts, one demand emitted.
        deliver_dl(&mut mm, &mut q, build_command(false, Some(MS_MNI)));
        assert_eq!(mm.reg_state, RegState::Registering);
        assert!(q.pop_front().is_some(), "first command emits a demand");
        assert!(q.pop_front().is_none(), "exactly one demand for the first command");

        // Second command while still Registering with T351 active → coalesced.
        assert!(mm.t351_countdown > 0, "T351 running after the first command");
        deliver_dl(&mut mm, &mut q, build_command(false, Some(MS_MNI)));
        assert_eq!(mm.reg_state, RegState::Registering, "still registering");
        assert!(
            q.pop_front().is_none(),
            "duplicate command coalesced — no second demand queued"
        );
    }

    /// If the serving cell does not require registration, MM stays idle.
    #[test]
    fn test_no_registration_when_not_required() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(false));
        assert_eq!(mm.reg_state, RegState::Idle);
        assert!(q.pop_front().is_none(), "no demand when registration not required");
    }

    /// A D-LOCATION-UPDATE-ACCEPT completes the registration and stops retries.
    #[test]
    fn test_registration_accept_completes() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();

        deliver_dl(&mut mm, &mut q, build_accept());
        assert_eq!(mm.reg_state, RegState::Registered);
        drain_mle_identities(&mut q);

        // No further retransmission even after the retry window elapses.
        for _ in 0..=T351_TIMEOUT_SLOTS {
            mm.tick_start(&mut q, TdmaTime::default());
        }
        assert!(q.pop_front().is_none(), "registered MS must not retransmit");
    }

    /// With no response, MM retransmits the demand after the retry window.
    #[test]
    fn test_registration_retransmits_on_timeout() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        assert_eq!(mm.attempts, 1);

        // Countdown slots: no retransmit until the tick after T351 reaches zero.
        for _ in 0..=T351_TIMEOUT_SLOTS {
            mm.tick_start(&mut q, TdmaTime::default());
        }
        let msg = q.pop_front().expect("demand retransmitted after timeout");
        assert!(matches!(msg.msg, SapMsgInner::LmmMleUnitdataReq(_)));
        assert_eq!(mm.attempts, 2);
    }

    /// Build a D-LOCATION-UPDATE-REJECT with the given raw reject cause.
    fn build_reject(cause: u8) -> BitBuffer {
        let pdu = DLocationUpdateReject {
            location_update_type: LocationUpdateType::ItsiAttach,
            reject_cause: cause,
            cipher_control: false,
            ciphering_parameters: None,
            address_extension: None,
            cell_type_control: None,
            proprietary: None,
        };
        let mut sdu = BitBuffer::new_autoexpand(8);
        pdu.to_bitbuf(&mut sdu).unwrap();
        sdu
    }

    /// A recoverable reject cause (Congestion) makes MM resend the demand
    /// (cl. 16.4.1.1: "may re-try registration"); attempt count increases.
    #[test]
    fn test_reject_recoverable_cause_retries() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        assert_eq!(mm.attempts, 1);

        deliver_dl(&mut mm, &mut q, build_reject(RejectCause::Congestion as u8));
        // Reject-driven resend is deferred to the next tick (T351 set to 0).
        assert_eq!(mm.reg_state, RegState::Registering);
        mm.tick_start(&mut q, TdmaTime::default());
        let msg = q.pop_front().expect("demand resent after recoverable reject");
        assert!(matches!(msg.msg, SapMsgInner::LmmMleUnitdataReq(_)));
        assert_eq!(mm.attempts, 2);
    }

    /// An LA-rejection cause (LA not allowed) abandons the attempt → Idle,
    /// awaiting a fresh cell (re)selection (cl. 16.4.1.1).
    #[test]
    fn test_reject_la_cause_abandons() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();

        deliver_dl(&mut mm, &mut q, build_reject(RejectCause::LaNotAllowed as u8));
        assert_eq!(mm.reg_state, RegState::Idle);
        assert!(!mm.left_system);
        // No resend on the next tick.
        mm.tick_start(&mut q, TdmaTime::default());
        assert!(q.pop_front().is_none());
    }

    /// After N351 "system rejection" causes (Illegal MS) without a successful
    /// registration, the MS leaves the system and ignores further activate-confs
    /// (cl. 16.11.2.1).
    #[test]
    fn test_reject_system_rejection_leaves_after_n351() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        for i in 1..=N351_MAX_SYSTEM_REJECTIONS {
            mm.rx_activate_conf(&mut q, &activate_conf(true));
            let _ = q.pop_front();
            deliver_dl(&mut mm, &mut q, build_reject(RejectCause::IllegalMs as u8));
            assert_eq!(mm.system_rejection_count, i);
        }
        assert!(mm.left_system, "MS must leave the system after N351 system rejections");
        // Further cell selection must not trigger a new registration.
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        assert_eq!(mm.reg_state, RegState::Idle);
        assert!(q.pop_front().is_none(), "no registration after leaving the system");
    }

    /// A successful registration clears the accumulated system-rejection count
    /// (cl. 16.11.2.1 counts rejections "without a successful registration").
    #[test]
    fn test_accept_resets_system_rejection_count() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        deliver_dl(&mut mm, &mut q, build_reject(RejectCause::IllegalMs as u8));
        assert_eq!(mm.system_rejection_count, 1);

        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        deliver_dl(&mut mm, &mut q, build_accept());
        assert_eq!(mm.reg_state, RegState::Registered);
        assert_eq!(mm.system_rejection_count, 0);
    }

    /// On shutdown while registered, MM emits a valid, BS-parseable U-ITSI DETACH
    /// (cl. 16.6.1 / 16.9.3.3) and enters the bounded detach drain.
    #[test]
    fn test_detach_on_shutdown_when_registered() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();

        // Get to the Registered state.
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        deliver_dl(&mut mm, &mut q, build_accept());
        assert_eq!(mm.reg_state, RegState::Registered);
        drain_mle_identities(&mut q);

        // Shutdown: a detach must be initiated.
        assert!(mm.begin_deregistration(&mut q), "detach must be initiated when registered");
        assert_eq!(mm.reg_state, RegState::Detaching);
        assert!(mm.deregistration_pending());

        let msg = q.pop_front().expect("a U-ITSI DETACH must be emitted");
        assert!(q.pop_front().is_none(), "exactly one message emitted");
        assert_eq!(msg.sap, Sap::LmmSap);
        assert_eq!(msg.dest, TetraEntity::Mle);
        let SapMsgInner::LmmMleUnitdataReq(mut req) = msg.msg else {
            panic!("expected LmmMleUnitdataReq");
        };
        assert_eq!(req.address.ssi, MS_ISSI);
        assert_eq!(req.layer2service, Layer2Service::Acknowledged);

        // The BS parses exactly this on the uplink; it must decode cleanly as a
        // minimal detach (no MNI, no proprietary).
        req.sdu.seek(0);
        let pdu = UItsiDetach::from_bitbuf(&mut req.sdu).expect("BS must parse the detach");
        assert!(pdu.address_extension.is_none());
        assert!(pdu.proprietary.is_none());
        assert_eq!(req.sdu.get_len_remaining(), 0, "detach fully consumed");
    }

    /// The detach drain counts down over `DETACH_DRAIN_SLOTS` ticks, then reports
    /// no longer pending so the router can stop.
    #[test]
    fn test_detach_drain_bounded() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        deliver_dl(&mut mm, &mut q, build_accept());
        drain_mle_identities(&mut q);
        assert!(mm.begin_deregistration(&mut q));
        let _ = q.pop_front();

        // Pending for the full drain window, then clears.
        for _ in 0..DETACH_DRAIN_SLOTS {
            assert!(mm.deregistration_pending(), "still draining");
            mm.tick_start(&mut q, TdmaTime::default());
        }
        assert!(!mm.deregistration_pending(), "drain complete");

        // No accidental uplinks emitted during the drain.
        assert!(q.pop_front().is_none(), "detach drain emits nothing further");
    }

    /// On shutdown while not registered, MM does not attempt a detach.
    #[test]
    fn test_no_detach_when_not_registered() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        assert!(!mm.begin_deregistration(&mut q), "no detach when idle");
        assert_eq!(mm.reg_state, RegState::Idle);
        assert!(!mm.deregistration_pending());
        assert!(q.pop_front().is_none(), "no PDU emitted when not registered");
    }

    /// Helper: drive MM to Registered on the default config cell (901/9999, LA 1)
    /// via an ITSI-attach activate-conf + a D-LOCATION-UPDATE-ACCEPT, draining the
    /// emitted demand. Leaves the queue empty.
    fn register_on_default_cell(mm: &mut MmMs, q: &mut MessageQueue) {
        mm.rx_activate_conf(q, &activate_conf(true));
        let _ = q.pop_front().expect("registration demand");
        assert!(q.pop_front().is_none());
        deliver_dl(mm, q, build_accept());
        assert_eq!(mm.reg_state, RegState::Registered);
        drain_mle_identities(q);
        assert!(q.pop_front().is_none());
    }

    /// Drain (and validate) the MLE-IDENTITIES request MM emits after a
    /// successful registration accept (cl. 17.3.2): it carries the MS's own ISSI
    /// and the confirmed attached-group set so the MLE can configure the MAC
    /// downlink address filter (cl. 23.4.1.2.1).
    fn drain_mle_identities(q: &mut MessageQueue) {
        let msg = q.pop_front().expect("MLE-IDENTITIES emitted after registration accept");
        assert_eq!(msg.sap, Sap::LmmSap);
        assert_eq!(msg.dest, TetraEntity::Mle);
        assert!(
            matches!(msg.msg, SapMsgInner::LmmMleIdentitiesReq(_)),
            "expected an MLE-IDENTITIES request"
        );
    }

    /// Helper: pop exactly one U-LOCATION-UPDATE-DEMAND off the queue and return its
    /// location-update type (parsed through the BS's own decoder).
    fn pop_demand_lu_type(q: &mut MessageQueue) -> LocationUpdateType {
        let msg = q.pop_front().expect("a demand must be emitted");
        assert!(q.pop_front().is_none(), "exactly one message emitted");
        assert_eq!(msg.dest, TetraEntity::Mle);
        let SapMsgInner::LmmMleUnitdataReq(mut req) = msg.msg else {
            panic!("expected LmmMleUnitdataReq");
        };
        req.sdu.seek(0);
        ULocationUpdateDemand::from_bitbuf(&mut req.sdu)
            .expect("BS must parse the demand")
            .location_update_type
    }

    /// R1 (cl. 18.3.4.7.1a NOTE 2): re-selecting the SAME cell (same MCC/MNC/LA)
    /// while already registered must NOT trigger a new location update.
    #[test]
    fn test_same_cell_no_reregistration() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        register_on_default_cell(&mut mm, &mut q);

        // A repeated activate-conf for the identical cell: no LU, stays Registered.
        mm.rx_activate_conf(&mut q, &activate_conf_cell(true, 901, 9999, 1));
        assert_eq!(mm.reg_state, RegState::Registered);
        assert!(q.pop_front().is_none(), "same-cell return emits no demand");
    }

    /// R1 (cl. 18.3.4.7.1a cond 2): an LA change within the same network, when the
    /// new cell requires registration, triggers a roaming location update.
    #[test]
    fn test_la_change_triggers_roaming_lu() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        register_on_default_cell(&mut mm, &mut q);

        mm.rx_activate_conf(&mut q, &activate_conf_cell(true, 901, 9999, 2));
        assert_eq!(mm.reg_state, RegState::Registering);
        assert_eq!(
            pop_demand_lu_type(&mut q),
            LocationUpdateType::RoamingLocationUpdating
        );
    }

    /// R1 (cl. 18.3.4.7.1a cond 1): a network (MCC/MNC) change triggers a migrating
    /// location update regardless of the new cell's registration-required flag.
    #[test]
    fn test_network_change_triggers_migrating_lu() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        register_on_default_cell(&mut mm, &mut q);

        mm.rx_activate_conf(&mut q, &activate_conf_cell(false, 902, 1, 5));
        assert_eq!(mm.reg_state, RegState::Registering);
        assert_eq!(
            pop_demand_lu_type(&mut q),
            LocationUpdateType::MigratingLocationUpdating
        );
    }

    /// R1 (cl. 18.3.4.7.1a cond 2): an LA change where the new cell does NOT require
    /// registration must not trigger a location update.
    #[test]
    fn test_la_change_without_registration_required_skips() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        register_on_default_cell(&mut mm, &mut q);

        mm.rx_activate_conf(&mut q, &activate_conf_cell(false, 901, 9999, 2));
        assert_eq!(mm.reg_state, RegState::Registered);
        assert!(q.pop_front().is_none(), "no LU when new cell needs no registration");
    }

    /// R1 (cl. 16.4.1.0 / 18.3.4.7.1a): the registered area (LA where registration
    /// completed) is captured on the accept, so a later same-LA return is a no-op.
    #[test]
    fn test_registered_cell_captured_on_accept() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        register_on_default_cell(&mut mm, &mut q);

        assert_eq!(
            mm.registered_cell,
            Some(CellId {
                mcc: 901,
                mnc: 9999,
                la: 1
            })
        );
    }

    /// R2 helper: build a D-LOCATION-UPDATE-ACCEPT whose accept-type field carries
    /// the given raw value. The shared PDU types this field as `LocationUpdateType`
    /// (Table 16.67), but on the wire it is a raw 3-bit value the MS reinterprets
    /// through `LocationUpdateAcceptType` (Table 16.68). Passing
    /// `LocationUpdateType::MigratingLocationUpdating` (raw 1) therefore simulates
    /// a "Temporary registration" accept (accept-type raw 1).
    fn build_accept_type(raw_type: LocationUpdateType) -> BitBuffer {
        let pdu = DLocationUpdateAccept {
            location_update_accept_type: raw_type,
            ssi: Some(MS_ISSI as u64),
            address_extension: None,
            subscriber_class: None,
            energy_saving_information: None,
            scch_information_and_distribution_on_18th_frame: None,
            new_registered_area: None,
            security_downlink: None,
            group_identity_location_accept: None,
            default_group_attachment_lifetime: None,
            authentication_downlink: None,
            group_identity_security_related_information: None,
            cell_type_control: None,
            proprietary: None,
        };
        let mut sdu = BitBuffer::new_autoexpand(8);
        pdu.to_bitbuf(&mut sdu).unwrap();
        sdu
    }

    /// R2 (cl. 16.4.8 / Table 16.68): an accept whose accept-type field is raw 0012
    /// means "Temporary registration" — the MS must reinterpret it through the
    /// accept-type table (not the demand table, where raw 1 = migrating) and hold
    /// the temporary-registration state. A normal ITSI-attach accept (raw 3) does
    /// not set the flag.
    #[test]
    fn test_temporary_registration_accept_sets_flag() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        // Raw 1 on the wire → "Temporary registration" per the accept table.
        deliver_dl(&mut mm, &mut q, build_accept_type(LocationUpdateType::MigratingLocationUpdating));
        assert_eq!(mm.reg_state, RegState::Registered);
        assert!(mm.temporary_registration, "raw-1 accept is a temporary registration");

        // A normal ITSI-attach accept (raw 3) leaves the flag clear.
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        deliver_dl(&mut mm, &mut q, build_accept_type(LocationUpdateType::ItsiAttach));
        assert!(!mm.temporary_registration, "raw-3 accept is a full registration");
    }

    /// R2 (cl. 16.4.8 para 3 / 16.4.1.0 NOTE): a temporarily-registered MS whose
    /// cell returns to normal mode ("system wide services" restored) performs a
    /// periodic location update on the same cell.
    #[test]
    fn test_temporary_registration_triggers_periodic_on_normal_mode() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        deliver_dl(&mut mm, &mut q, build_accept_type(LocationUpdateType::MigratingLocationUpdating));
        assert!(mm.temporary_registration);
        drain_mle_identities(&mut q);
        assert!(q.pop_front().is_none());

        // Same cell, normal mode (system_wide_services = true): periodic LU.
        mm.rx_activate_conf(&mut q, &activate_conf_cell_sws(true, 901, 9999, 1, true));
        assert_eq!(mm.reg_state, RegState::Registering);
        assert_eq!(
            pop_demand_lu_type(&mut q),
            LocationUpdateType::PeriodicLocationUpdating
        );
    }

    /// R2 (cl. 16.4.1.0 cond. 5): a cell advertising "system wide services
    /// temporarily not supported" (system_wide_services = false) forces
    /// registration on the registered cell even though nothing else changed.
    #[test]
    fn test_system_wide_services_false_forces_registration() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        register_on_default_cell(&mut mm, &mut q);
        assert!(!mm.temporary_registration);

        // Same cell, but now restricted: roaming LU required (cond. 5).
        mm.rx_activate_conf(&mut q, &activate_conf_cell_sws(true, 901, 9999, 1, false));
        assert_eq!(mm.reg_state, RegState::Registering);
        assert_eq!(
            pop_demand_lu_type(&mut q),
            LocationUpdateType::RoamingLocationUpdating
        );
    }

    /// R2 (cl. 16.4.1.0 cond. 5): even when the cell does NOT set the registration
    /// flag, a restricted cell (system_wide_services = false) still requires an
    /// initial registration.
    #[test]
    fn test_restricted_cell_forces_initial_registration() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        // registration_required = false, but system_wide_services = false.
        mm.rx_activate_conf(&mut q, &activate_conf_cell_sws(false, 901, 9999, 1, false));
        assert_eq!(mm.reg_state, RegState::Registering);
        assert_eq!(pop_demand_lu_type(&mut q), LocationUpdateType::ItsiAttach);
    }

    // --- Phase T0: MS external-interface wiring (enabler; no TNMM behaviour) ---

    /// Build an MS MM wired to a telemetry sink and a control endpoint, and
    /// return the far ends (telemetry source + command dispatcher) so a test can
    /// act as the external UI process.
    fn ms_mm_wired() -> (MmMs, crate::net_telemetry::TelemetrySource, crate::net_control::CommandDispatcher) {
        use crate::net_control::channel::make_control_link;
        use crate::net_telemetry::telemetry_channel;
        let cfg = from_toml_str(MS_TOML).expect("valid MS test config");
        let (sink, source) = telemetry_channel();
        let (dispatcher, endpoint) = make_control_link();
        let mm = MmMs::new(SharedConfig::from_parts(cfg, None), Some(sink), Some(endpoint));
        (mm, source, dispatcher)
    }

    /// T0: `MmMs::new` accepts and stores the telemetry sink + control endpoint
    /// (mirroring `MmBs`), so `build_ms_stack` can wire the external interface.
    #[test]
    fn test_ms_mm_accepts_telemetry_and_control() {
        let (mm, _source, _dispatcher) = ms_mm_wired();
        assert!(mm.telemetry.is_some(), "telemetry sink wired");
        assert!(mm.control.is_some(), "control endpoint wired");
    }

    /// T0: MM drains the control endpoint each tick. No MS control command is
    /// defined yet, so an inbound command must be consumed and dropped without
    /// panicking or altering registration state (cl. 15.3 requests land in T2).
    #[test]
    fn test_ms_mm_drains_control_without_panic() {
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        dispatcher.send(ControlCommand::CommandA { handle: 7, parameter: 1 });
        mm.tick_start(&mut q, TdmaTime::default());

        // Command consumed (nothing left for the endpoint to receive), state
        // unchanged, and no uplink PDU produced as a side effect.
        assert_eq!(mm.reg_state, RegState::Idle, "control command must not change reg state");
        assert!(q.pop_front().is_none(), "no PDU emitted for an unhandled control command");
    }

    // -----------------------------------------------------------------------
    // T1: TNMM-SAP outbound indications (cl. 15.3.3 / 15.3.4). Drive MM state
    // transitions through a wired telemetry sink and assert the exact primitives
    // reach the (simulated) user application.
    // -----------------------------------------------------------------------

    /// Collect all telemetry events currently queued on the source.
    fn drain(source: &crate::net_telemetry::TelemetrySource) -> Vec<TelemetryEvent> {
        let mut out = Vec::new();
        while let Some(e) = source.try_recv() {
            out.push(e);
        }
        out
    }

    /// Successful registration emits, in order: a TNMM-SERVICE "in service
    /// waiting for registration" indication when the demand is sent, then on
    /// D-LOCATION-UPDATE-ACCEPT a TNMM-REGISTRATION indication + confirm
    /// (status = success) and a TNMM-SERVICE "in service" indication
    /// (cl. 15.3.3.7 / 15.3.3.8).
    #[test]
    fn test_accept_emits_registration_success_and_service() {
        use crate::tnmm::{RegistrationStatus, ServiceStatus};
        let (mut mm, source, _dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let waiting = drain(&source);
        assert!(
            waiting.iter().any(|e| matches!(
                e,
                TelemetryEvent::TnmmServiceIndication(s)
                    if s.service_status == ServiceStatus::InServiceWaitingForRegistration
            )),
            "demand must emit 'in service waiting for registration'"
        );

        deliver_dl(&mut mm, &mut q, build_accept());
        assert_eq!(mm.reg_state, RegState::Registered);
        let events = drain(&source);

        let reg_ind = events.iter().find_map(|e| match e {
            TelemetryEvent::TnmmRegistrationIndication(i) => Some(i),
            _ => None,
        });
        let reg_ind = reg_ind.expect("TNMM-REGISTRATION indication emitted on accept");
        assert_eq!(reg_ind.registration_status, RegistrationStatus::Success);
        assert_eq!(reg_ind.registration_reject_cause, None);
        assert_eq!(reg_ind.cell_type_where_registered, crate::tnmm::CellType::CaCell);
        assert_eq!(reg_ind.la_where_registered, 1);
        assert_eq!(reg_ind.mcc_where_registered, 901);
        assert_eq!(reg_ind.mnc_where_registered, 9999);

        assert!(
            events.iter().any(|e| matches!(
                e,
                TelemetryEvent::TnmmRegistrationConfirm(c) if c.registration_status == RegistrationStatus::Success
            )),
            "TNMM-REGISTRATION confirm emitted on accept"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                TelemetryEvent::TnmmServiceIndication(s) if s.service_status == ServiceStatus::InService
            )),
            "TNMM-SERVICE 'in service' emitted on accept"
        );
    }

    /// `emit_registration_success` builds one `GroupIdentity` per attached GSSI
    /// with GITI = Attachment and GTSI = (own MNI << 24) | GSSI (cl. 15.3.4), and
    /// emits a TNMM-ATTACH DETACH GROUP IDENTITY indication (cl. 15.3.3.1).
    #[test]
    fn test_registration_success_emits_group_identities() {
        use crate::tnmm::GroupIdentityAttachDetachTypeIdentifier as Giti;
        let (mm, source, _dispatcher) = ms_mm_wired();
        let gssi = 100u32;
        mm.emit_registration_success(&[gssi]);
        let events = drain(&source);

        let group_ind = events.iter().find_map(|e| match e {
            TelemetryEvent::TnmmAttachDetachGroupIdentityIndication(i) => Some(i),
            _ => None,
        });
        let group_ind = group_ind.expect("group identity indication emitted for attached GSSIs");
        assert_eq!(group_ind.group_identities.len(), 1);
        let g = &group_ind.group_identities[0];
        assert_eq!(g.group_identity_attach_detach_type_identifier, Giti::Attachment);
        let expected_gtsi = ((mm.own_mni() as u64) << 24) | (gssi as u64);
        assert_eq!(g.gtsi, expected_gtsi);
        assert!(g.group_identity_lifetime.is_some(), "GITI=Attachment carries lifetime");
        assert!(g.class_of_usage.is_some(), "GITI=Attachment carries class of usage");
        assert!(g.group_identity_detachment_reason.is_none());
    }

    /// A terminal reject (Illegal MS → abandon) emits a TNMM-REGISTRATION
    /// indication with status = failure + the mapped reject cause, then a
    /// TNMM-SERVICE "out of service" indication (cl. 15.3.3.7 / 15.3.4).
    #[test]
    fn test_reject_emits_registration_failure_and_out_of_service() {
        use crate::tnmm::{RegistrationRejectCause, RegistrationStatus, ServiceStatus};
        let (mut mm, source, _dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = drain(&source); // discard the "waiting" service indication

        // Illegal MS (cause 2) is an abandon cause (cl. 16.4.1.1): terminal.
        deliver_dl(&mut mm, &mut q, build_reject(2));
        assert_eq!(mm.reg_state, RegState::Idle);
        let events = drain(&source);

        let reg_ind = events.iter().find_map(|e| match e {
            TelemetryEvent::TnmmRegistrationIndication(i) => Some(i),
            _ => None,
        });
        let reg_ind = reg_ind.expect("TNMM-REGISTRATION failure indication emitted on terminal reject");
        assert_eq!(reg_ind.registration_status, RegistrationStatus::Failure);
        assert_eq!(reg_ind.registration_reject_cause, Some(RegistrationRejectCause::IllegalMs));

        assert!(
            events.iter().any(|e| matches!(
                e,
                TelemetryEvent::TnmmServiceIndication(s) if s.service_status == ServiceStatus::OutOfService
            )),
            "TNMM-SERVICE 'out of service' emitted on terminal reject"
        );
    }

    /// A recoverable reject (Congestion → re-try) keeps the registration in
    /// progress, so no failure indication is emitted (cl. 16.4.1.1).
    #[test]
    fn test_recoverable_reject_emits_no_failure() {
        let (mut mm, source, _dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = drain(&source);

        // Congestion (cause 6) permits a re-try: still Registering, not terminal.
        deliver_dl(&mut mm, &mut q, build_reject(6));
        let events = drain(&source);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, TelemetryEvent::TnmmRegistrationIndication(_))),
            "no failure indication while a re-try is still outstanding"
        );
    }

    /// The on-air reject causes "use CA/DA cell not permitted" (cl. 16.10.42)
    /// have no TNMM-SAP enumerant (cl. 15.3.4): the mapping returns `None`.
    #[test]
    fn test_reject_cause_without_tnmm_enumerant_maps_to_none() {
        assert_eq!(
            MmMs::map_registration_reject_cause(RejectCause::UseCaCellNotPermitted),
            None
        );
        assert_eq!(
            MmMs::map_registration_reject_cause(RejectCause::UseDaCellNotPermitted),
            None
        );
        assert_eq!(
            MmMs::map_registration_reject_cause(RejectCause::IllegalMs),
            Some(crate::tnmm::RegistrationRejectCause::IllegalMs)
        );
    }

    /// De-registration at shutdown emits a TNMM-SERVICE "out of service"
    /// indication (cl. 15.3.3.8).
    #[test]
    fn test_deregistration_emits_out_of_service() {
        use crate::tnmm::ServiceStatus;
        let (mut mm, source, _dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        mm.rx_activate_conf(&mut q, &activate_conf(true));
        deliver_dl(&mut mm, &mut q, build_accept());
        let _ = drain(&source); // discard registration/service events

        assert!(mm.begin_deregistration(&mut q));
        let events = drain(&source);
        assert!(
            events.iter().any(|e| matches!(
                e,
                TelemetryEvent::TnmmServiceIndication(s) if s.service_status == ServiceStatus::OutOfService
            )),
            "de-registration emits 'out of service'"
        );
    }

    // -----------------------------------------------------------------------
    // T2: TNMM-SAP requests (INBOUND, cl. 15.3.3). Drive requests through the
    // wired control link (dispatcher = UI side) and assert MM's action + the
    // TnmmAck transport response.
    // -----------------------------------------------------------------------

    /// Build the MS's own valid TNMM-REGISTRATION request (matching ITSI/MNI).
    fn own_registration_request() -> crate::tnmm::TnmmRegistrationRequest {
        use crate::tnmm::{RegistrationType, TnmmRegistrationRequest};
        TnmmRegistrationRequest {
            registration_type: RegistrationType::RegistrationToIndicatedCell,
            required_cell_type_list: None,
            preferred_cell_type_list: None,
            preferred_la_list: None,
            preferred_mcc_list: None,
            preferred_mnc_list: None,
            issi: MS_ISSI,
            mcc_of_issi: 901,
            mnc_of_issi: 9999,
            energy_economy_mode: None,
            group_identity_request: None,
            group_identity_attach_detach_mode: None,
        }
    }

    fn last_ack(dispatcher: &crate::net_control::CommandDispatcher) -> (bool, Option<String>) {
        let resps = dispatcher.try_recv_responses();
        let ack = resps
            .into_iter()
            .find_map(|r| match r {
                crate::net_control::ControlResponse::TnmmAck { accepted, detail, .. } => Some((accepted, detail)),
                _ => None,
            })
            .expect("a TnmmAck response");
        ack
    }

    /// A TNMM-REGISTRATION request (Table 15.5) while idle initiates the ITSI
    /// attach: MM sends a U-LOCATION-UPDATE-DEMAND and moves to Registering, and
    /// acknowledges acceptance (cl. 15.3.3.7).
    #[test]
    fn test_tnmm_registration_request_triggers_demand() {
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        dispatcher.send(ControlCommand::TnmmRegistration {
            handle: 11,
            request: Box::new(own_registration_request()),
        });
        mm.tick_start(&mut q, TdmaTime::default());

        assert_eq!(mm.reg_state, RegState::Registering);
        let msg = q.pop_front().expect("registration demand queued");
        assert!(matches!(msg.msg, SapMsgInner::LmmMleUnitdataReq(_)));
        let (accepted, _detail) = last_ack(&dispatcher);
        assert!(accepted, "registration request accepted");
    }

    /// A TNMM-REGISTRATION request for a different ITSI is rejected (single-ITSI
    /// stack) with no state change.
    #[test]
    fn test_tnmm_registration_request_wrong_itsi_rejected() {
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        let mut req = own_registration_request();
        req.issi = MS_ISSI + 1;
        dispatcher.send(ControlCommand::TnmmRegistration { handle: 12, request: Box::new(req) });
        mm.tick_start(&mut q, TdmaTime::default());

        assert_eq!(mm.reg_state, RegState::Idle, "no registration for a foreign ITSI");
        assert!(q.pop_front().is_none(), "no demand for a foreign ITSI");
        let (accepted, detail) = last_ack(&dispatcher);
        assert!(!accepted);
        assert!(detail.is_some());
    }

    /// A TNMM-DEREGISTRATION request (Table 15.2) while registered runs the
    /// de-registration: MM sends U-ITSI DETACH, moves to Detaching, emits
    /// TNMM-SERVICE "out of service", and acknowledges (cl. 15.3.3.2 / 16.6.1).
    #[test]
    fn test_tnmm_deregistration_request_detaches() {
        use crate::tnmm::{ServiceStatus, TnmmDeregistrationRequest};
        let (mut mm, source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        // Register first.
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        deliver_dl(&mut mm, &mut q, build_accept());
        while q.pop_front().is_some() {}
        let _ = drain(&source);

        dispatcher.send(ControlCommand::TnmmDeregistration {
            handle: 21,
            request: TnmmDeregistrationRequest { issi: Some(MS_ISSI), mcc: None, mnc: None },
        });
        mm.tick_start(&mut q, TdmaTime::default());

        assert_eq!(mm.reg_state, RegState::Detaching);
        let msg = q.pop_front().expect("U-ITSI DETACH queued");
        assert!(matches!(msg.msg, SapMsgInner::LmmMleUnitdataReq(_)));
        let events = drain(&source);
        assert!(
            events.iter().any(|e| matches!(
                e,
                TelemetryEvent::TnmmServiceIndication(s) if s.service_status == ServiceStatus::OutOfService
            )),
            "de-registration emits 'out of service'"
        );
        let (accepted, _detail) = last_ack(&dispatcher);
        assert!(accepted);
    }

    /// A TNMM-DEREGISTRATION request while not registered is a no-op that is
    /// acknowledged with a documented detail.
    #[test]
    fn test_tnmm_deregistration_request_when_not_registered() {
        use crate::tnmm::TnmmDeregistrationRequest;
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        dispatcher.send(ControlCommand::TnmmDeregistration {
            handle: 22,
            request: TnmmDeregistrationRequest { issi: None, mcc: None, mnc: None },
        });
        mm.tick_start(&mut q, TdmaTime::default());

        assert_eq!(mm.reg_state, RegState::Idle);
        assert!(q.pop_front().is_none());
        let (accepted, detail) = last_ack(&dispatcher);
        assert!(accepted);
        assert!(detail.is_some(), "documented 'nothing to detach' detail");
    }

    /// The standalone group attach/detach procedure (cl. 16.9.3) is not
    /// implemented: a TNMM-ATTACH DETACH GROUP IDENTITY request is acknowledged
    /// as not-accepted with a documented deferral, and changes no state.
    #[test]
    fn test_tnmm_group_identity_request_is_deferred() {
        use crate::tnmm::{
            ClassOfUsage, GroupIdentityAttachDetachMode, GroupIdentityAttachDetachTypeIdentifier, GroupIdentityRequest,
            TnmmAttachDetachGroupIdentityRequest,
        };
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        dispatcher.send(ControlCommand::TnmmAttachDetachGroupIdentity {
            handle: 31,
            request: TnmmAttachDetachGroupIdentityRequest {
                group_identity_attach_detach_mode: GroupIdentityAttachDetachMode::Amendment,
                group_identity_request: vec![GroupIdentityRequest {
                    gtsi: 0x01,
                    group_identity_attach_detach_type_identifier: GroupIdentityAttachDetachTypeIdentifier::Attachment,
                    class_of_usage: Some(ClassOfUsage::ClassOfUsage4),
                    group_identity_detachment_request: None,
                }],
                group_identity_report: None,
            },
        });
        mm.tick_start(&mut q, TdmaTime::default());

        assert_eq!(mm.reg_state, RegState::Idle);
        assert!(q.pop_front().is_none());
        let (accepted, detail) = last_ack(&dispatcher);
        assert!(!accepted, "deferred procedure is not accepted");
        assert!(detail.is_some());
    }

    /// Dormant primitives (STATUS / ENERGY SAVING requests) are acknowledged as
    /// not-accepted with a documented reason and change no state.
    #[test]
    fn test_tnmm_status_and_energy_saving_requests_are_dormant() {
        use crate::tnmm::{EnergyEconomyMode, TnmmEnergySavingRequest, TnmmStatusRequest};
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        dispatcher.send(ControlCommand::TnmmStatus {
            handle: 41,
            request: TnmmStatusRequest { direct_mode: None, dual_watch: None, energy_economy_mode: None },
        });
        dispatcher.send(ControlCommand::TnmmEnergySaving {
            handle: 42,
            request: TnmmEnergySavingRequest { energy_economy_mode: EnergyEconomyMode::EnergyEconomyMode1 },
        });
        mm.tick_start(&mut q, TdmaTime::default());

        assert_eq!(mm.reg_state, RegState::Idle);
        let resps = dispatcher.try_recv_responses();
        let acks: Vec<_> = resps
            .into_iter()
            .filter_map(|r| match r {
                crate::net_control::ControlResponse::TnmmAck { handle, accepted, .. } => Some((handle, accepted)),
                _ => None,
            })
            .collect();
        assert_eq!(acks.len(), 2);
        assert!(acks.iter().all(|(_, accepted)| !*accepted), "dormant requests not accepted");
    }

    // -----------------------------------------------------------------------
    // T3: management / provisioning (Plane B, NON-STANDARD). Read-path slice:
    // GetState returns an MS runtime-state snapshot built from MM state + config.
    // -----------------------------------------------------------------------

    /// Extract the last `Management(State{..})` response from a dispatcher.
    fn last_state(dispatcher: &crate::net_control::CommandDispatcher) -> (u32, crate::management::MsRuntimeState) {
        use crate::management::ManagementResponse;
        use crate::net_control::ControlResponse;
        dispatcher
            .try_recv_responses()
            .into_iter()
            .find_map(|r| match r {
                ControlResponse::Management(ManagementResponse::State { handle, state }) => Some((handle, *state)),
                _ => None,
            })
            .expect("a Management State response")
    }

    /// GetState before any cell selection reports Idle / OutOfService and echoes
    /// the configured identity/network/cell fields (non-standard Plane B).
    #[test]
    fn test_management_get_state_idle_snapshot() {
        use crate::management::{ManagementCommand, RegistrationState};
        use crate::tnmm::ServiceStatus;
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        dispatcher.send(ControlCommand::Management(ManagementCommand::GetState { handle: 71 }));
        mm.tick_start(&mut q, TdmaTime::default());

        let (handle, state) = last_state(&dispatcher);
        assert_eq!(handle, 71);
        assert_eq!(state.registration_state, RegistrationState::Idle);
        assert_eq!(state.service_status, ServiceStatus::OutOfService);
        assert_eq!(state.own_issi, 1000001);
        assert_eq!(state.home_mcc, 901);
        assert_eq!(state.home_mnc, 9999);
        assert_eq!(state.serving_la, 1);
        assert_eq!(state.colour_code, 1);
        assert!(state.attached_groups.is_empty());
        // GetState is read-only: registration state must be untouched.
        assert_eq!(mm.reg_state, RegState::Idle);
        assert!(q.pop_front().is_none(), "GetState must not emit any PDU");
    }

    /// After successful registration, GetState reflects Registered / InService.
    #[test]
    fn test_management_get_state_registered_snapshot() {
        use crate::management::{ManagementCommand, RegistrationState};
        use crate::tnmm::ServiceStatus;
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        mm.rx_activate_conf(&mut q, &activate_conf(true));
        deliver_dl(&mut mm, &mut q, build_accept());
        assert_eq!(mm.reg_state, RegState::Registered);
        while q.pop_front().is_some() {}

        dispatcher.send(ControlCommand::Management(ManagementCommand::GetState { handle: 72 }));
        mm.tick_start(&mut q, TdmaTime::default());

        let (_handle, state) = last_state(&dispatcher);
        assert_eq!(state.registration_state, RegistrationState::Registered);
        assert_eq!(state.service_status, ServiceStatus::InService);
    }

    // -----------------------------------------------------------------------
    // T3: management / provisioning (Plane B, NON-STANDARD). Write-path slice:
    // GetConfig / SetConfig (validate+persist) / ApplyConfig (drain+restart).
    // -----------------------------------------------------------------------

    /// Extract the last `Management(Config{..})` response from a dispatcher.
    fn last_config(dispatcher: &crate::net_control::CommandDispatcher) -> (u32, String) {
        use crate::management::ManagementResponse;
        use crate::net_control::ControlResponse;
        dispatcher
            .try_recv_responses()
            .into_iter()
            .find_map(|r| match r {
                ControlResponse::Management(ManagementResponse::Config { handle, toml }) => Some((handle, toml)),
                _ => None,
            })
            .expect("a Management Config response")
    }

    /// Extract the last `Management(Ack{..})` response from a dispatcher.
    fn last_mgmt_ack(dispatcher: &crate::net_control::CommandDispatcher) -> (u32, bool, bool) {
        use crate::management::ManagementResponse;
        use crate::net_control::ControlResponse;
        dispatcher
            .try_recv_responses()
            .into_iter()
            .find_map(|r| match r {
                ControlResponse::Management(ManagementResponse::Ack { handle, accepted, restart_required, .. }) => {
                    Some((handle, accepted, restart_required))
                }
                _ => None,
            })
            .expect("a Management Ack response")
    }

    /// GetInterfaceVersion returns the frozen schema-version string (live).
    #[test]
    fn test_management_get_interface_version() {
        use crate::management::{ManagementCommand, ManagementResponse, MS_INTERFACE_SCHEMA_VERSION};
        use crate::net_control::ControlResponse;
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        dispatcher.send(ControlCommand::Management(ManagementCommand::GetInterfaceVersion { handle: 90 }));
        mm.tick_start(&mut q, TdmaTime::default());

        let (handle, version) = dispatcher
            .try_recv_responses()
            .into_iter()
            .find_map(|r| match r {
                ControlResponse::Management(ManagementResponse::InterfaceVersion { handle, version }) => {
                    Some((handle, version))
                }
                _ => None,
            })
            .expect("an InterfaceVersion response");
        assert_eq!(handle, 90);
        assert_eq!(version, MS_INTERFACE_SCHEMA_VERSION);
        assert_eq!(mm.reg_state, RegState::Idle);
    }

    /// GetConfig returns canonical TOML that re-parses through the validator.
    #[test]
    fn test_management_get_config_roundtrips() {
        use crate::management::ManagementCommand;
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        dispatcher.send(ControlCommand::Management(ManagementCommand::GetConfig { handle: 80 }));
        mm.tick_start(&mut q, TdmaTime::default());

        let (handle, toml) = last_config(&dispatcher);
        assert_eq!(handle, 80);
        let reparsed = from_toml_str(&toml).expect("GetConfig TOML must re-parse");
        assert_eq!(reparsed.net.mcc, 901);
        assert_eq!(reparsed.net.mnc, 9999);
        assert_eq!(reparsed.ms.as_ref().unwrap().issi, 1000001);
        // Read-only: no PDU emitted, state untouched.
        assert!(q.pop_front().is_none());
        assert_eq!(mm.reg_state, RegState::Idle);
    }

    /// SetConfig without an installed management context is refused gracefully
    /// (accepted = false), never panics, and does not touch state.
    #[test]
    fn test_management_set_config_without_context_refused() {
        use crate::management::ManagementCommand;
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        dispatcher.send(ControlCommand::Management(ManagementCommand::SetConfig {
            handle: 81,
            toml: MS_TOML.to_string(),
        }));
        mm.tick_start(&mut q, TdmaTime::default());

        let (handle, accepted, restart_required) = last_mgmt_ack(&dispatcher);
        assert_eq!(handle, 81);
        assert!(!accepted, "no context => refused");
        assert!(!restart_required);
        assert_eq!(mm.reg_state, RegState::Idle);
    }

    /// SetConfig with a context validates + persists to disk and flags a pending
    /// restart; the runtime snapshot then reports `restart_required = true`.
    #[test]
    fn test_management_set_config_persists_and_flags_restart() {
        use crate::management::ManagementCommand;
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        let path = std::env::temp_dir().join(format!("tetra-ms-setcfg-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let restart_requested = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let is_running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        mm.set_management_context(path.clone(), restart_requested.clone(), is_running.clone());

        dispatcher.send(ControlCommand::Management(ManagementCommand::SetConfig {
            handle: 82,
            toml: MS_TOML.to_string(),
        }));
        mm.tick_start(&mut q, TdmaTime::default());

        let (handle, accepted, restart_required) = last_mgmt_ack(&dispatcher);
        assert_eq!(handle, 82);
        assert!(accepted, "valid config accepted");
        assert!(restart_required, "restart flagged after staging");
        // File written and re-parses.
        let written = std::fs::read_to_string(&path).expect("config file written");
        assert!(from_toml_str(&written).is_ok(), "persisted config must re-parse");
        // SetConfig must NOT bounce the process.
        assert!(is_running.load(std::sync::atomic::Ordering::SeqCst), "SetConfig must not stop the loop");
        assert!(!restart_requested.load(std::sync::atomic::Ordering::SeqCst), "SetConfig must not request restart");
        // Snapshot reflects the pending restart.
        let snap = mm.runtime_snapshot();
        assert!(snap.restart_required);

        let _ = std::fs::remove_file(&path);
    }

    /// SetConfig rejects a config that fails the startup validator (accepted =
    /// false) and does not write the file or flag a restart.
    #[test]
    fn test_management_set_config_rejects_invalid() {
        use crate::management::ManagementCommand;
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        let path = std::env::temp_dir().join(format!("tetra-ms-badcfg-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let restart_requested = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let is_running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        mm.set_management_context(path.clone(), restart_requested, is_running);

        dispatcher.send(ControlCommand::Management(ManagementCommand::SetConfig {
            handle: 83,
            toml: "this = is not = valid toml {{{".to_string(),
        }));
        mm.tick_start(&mut q, TdmaTime::default());

        let (handle, accepted, restart_required) = last_mgmt_ack(&dispatcher);
        assert_eq!(handle, 83);
        assert!(!accepted, "invalid config rejected");
        assert!(!restart_required);
        assert!(!path.exists(), "no file written for rejected config");
        assert!(!mm.runtime_snapshot().restart_required);
    }

    // An MS config that additionally configures a control endpoint with HTTP
    // Basic credentials, so the secret redaction/restore paths carry a secret.
    const MS_TOML_WITH_SECRET: &str = r#"
config_version = "0.6"
stack_mode = "Ms"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
tx_freq = 439825000
rx_freq = 430425000
ppm_err = 0
device = "driver=sx"
sample_rate = 600000
rx_antenna = "RX"
tx_antenna = "TX"
rx_gain_lna = 48.0
rx_gain_pga = 8.0
tx_gain_dac = 0.0
tx_gain_mixer = 0.0

[net_info]
mcc = 901
mnc = 9999

[cell_info]
freq_band = 4
main_carrier = 1593
duplex_spacing = 7
custom_duplex_spacing = 9400000
freq_offset = 0
reverse_operation = false
location_area = 1
colour_code = 1

[ms]
issi = 1000001
subscriber_class = 1
attach_groups = []

[command]
host = "127.0.0.1"
port = 9000
username = "ui-operator"
password = "supersecret"
"#;

    fn ms_mm_wired_with_secret(
    ) -> (MmMs, crate::net_telemetry::TelemetrySource, crate::net_control::CommandDispatcher) {
        use crate::net_control::channel::make_control_link;
        use crate::net_telemetry::telemetry_channel;
        let cfg = from_toml_str(MS_TOML_WITH_SECRET).expect("valid MS test config with secret");
        let (sink, source) = telemetry_channel();
        let (dispatcher, endpoint) = make_control_link();
        let mm = MmMs::new(SharedConfig::from_parts(cfg, None), Some(sink), Some(endpoint));
        (mm, source, dispatcher)
    }

    /// End-to-end secret handling (Plane B, non-standard):
    /// - GetConfig redacts the control password on the wire (never leaks it).
    /// - SetConfig with the redacted document back preserves the real on-disk
    ///   secret (a benign round-trip must not clobber the credential).
    /// - SetConfig with a genuinely new password overwrites it.
    #[test]
    fn test_management_config_redacts_and_preserves_secret() {
        use crate::management::ManagementCommand;
        use tetra_config::bluestation::REDACTED_SECRET;

        let (mut mm, _source, dispatcher) = ms_mm_wired_with_secret();
        let mut q = MessageQueue::new();

        let path = std::env::temp_dir().join(format!("tetra-ms-secret-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let restart_requested = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let is_running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        mm.set_management_context(path.clone(), restart_requested, is_running);

        // 1) GetConfig must redact the secret on the wire.
        dispatcher.send(ControlCommand::Management(ManagementCommand::GetConfig { handle: 84 }));
        mm.tick_start(&mut q, TdmaTime::default());
        let (_h, wire_toml) = last_config(&dispatcher);
        assert!(!wire_toml.contains("supersecret"), "GetConfig must not leak the password");
        assert!(wire_toml.contains(REDACTED_SECRET), "GetConfig carries the redaction sentinel");
        assert!(wire_toml.contains("ui-operator"), "non-secret username still travels");

        // 2) UI posts the redacted document back unchanged => on-disk secret preserved.
        dispatcher.send(ControlCommand::Management(ManagementCommand::SetConfig {
            handle: 85,
            toml: wire_toml.clone(),
        }));
        mm.tick_start(&mut q, TdmaTime::default());
        let (_h, accepted, _rr) = last_mgmt_ack(&dispatcher);
        assert!(accepted, "redacted round-trip config accepted");
        let written = std::fs::read_to_string(&path).expect("config written");
        assert!(written.contains("supersecret"), "real secret preserved on disk");
        assert!(!written.contains(REDACTED_SECRET), "sentinel never persisted to disk");

        // 3) A genuinely new password is written through.
        let rotated = wire_toml.replace(REDACTED_SECRET, "rotated-pass");
        dispatcher.send(ControlCommand::Management(ManagementCommand::SetConfig {
            handle: 86,
            toml: rotated,
        }));
        mm.tick_start(&mut q, TdmaTime::default());
        let (_h, accepted, _rr) = last_mgmt_ack(&dispatcher);
        assert!(accepted, "rotated config accepted");
        let written = std::fs::read_to_string(&path).expect("config written");
        assert!(written.contains("rotated-pass"), "new secret written to disk");
        assert!(!written.contains("supersecret"), "old secret replaced");

        let _ = std::fs::remove_file(&path);
    }

    /// ApplyConfig requests the controlled restart: sets the shared
    /// `restart_requested` flag and clears `is_running` so `main` exits with the
    /// restart code, and (when registered) initiates the detach drain.
    #[test]
    fn test_management_apply_config_requests_restart() {
        use crate::management::ManagementCommand;
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        // Register first so ApplyConfig exercises the detach drain.
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        deliver_dl(&mut mm, &mut q, build_accept());
        assert_eq!(mm.reg_state, RegState::Registered);
        while q.pop_front().is_some() {}

        let path = std::env::temp_dir().join(format!("tetra-ms-apply-{}.toml", std::process::id()));
        let restart_requested = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let is_running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        mm.set_management_context(path, restart_requested.clone(), is_running.clone());

        dispatcher.send(ControlCommand::Management(ManagementCommand::ApplyConfig { handle: 84 }));
        mm.tick_start(&mut q, TdmaTime::default());

        let (handle, accepted, _restart_required) = last_mgmt_ack(&dispatcher);
        assert_eq!(handle, 84);
        assert!(accepted, "apply accepted");
        assert!(restart_requested.load(std::sync::atomic::Ordering::SeqCst), "restart requested");
        assert!(!is_running.load(std::sync::atomic::Ordering::SeqCst), "run loop stopped");
        // Registered MS begins de-registration (U-ITSI DETACH) on apply.
        assert_eq!(mm.reg_state, RegState::Detaching);
    }

    /// ApplyConfig without a management context is refused gracefully.
    #[test]
    fn test_management_apply_config_without_context_refused() {
        use crate::management::ManagementCommand;
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        dispatcher.send(ControlCommand::Management(ManagementCommand::ApplyConfig { handle: 85 }));
        mm.tick_start(&mut q, TdmaTime::default());

        let (handle, accepted, _r) = last_mgmt_ack(&dispatcher);
        assert_eq!(handle, 85);
        assert!(!accepted, "no context => refused");
        assert_eq!(mm.reg_state, RegState::Idle);
    }

    /// T4 (mock-transport CI): a management command survives a full JSON
    /// encode -> decode -> MmMs handle -> response encode -> decode loop, proving
    /// the wire path is portable end to end over the mock control transport.
    #[test]
    fn test_management_end_to_end_over_json_codec() {
        use crate::management::{ManagementCommand, ManagementResponse};
        use crate::net_control::ControlResponse;
        use crate::net_control::codec::ControlCodecJson;
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();
        let codec = ControlCodecJson;

        // UI side: build a command, serialize it, and (mock transport) deserialize.
        let wire = codec.encode_command(&ControlCommand::Management(ManagementCommand::GetInterfaceVersion { handle: 7 }));
        let cmd = codec.decode_command(&wire).expect("decode command");
        dispatcher.send(cmd);

        // Stack side: MM handles it and emits a response.
        mm.tick_start(&mut q, TdmaTime::default());

        // Response travels back over the mock transport: serialize + deserialize.
        let resp = dispatcher
            .try_recv_responses()
            .into_iter()
            .next()
            .expect("a response");
        let round = codec.decode_response(&codec.encode_response(&resp)).expect("decode response");
        let ControlResponse::Management(ManagementResponse::InterfaceVersion { handle, version }) = round else {
            panic!("expected InterfaceVersion response");
        };
        assert_eq!(handle, 7);
        assert_eq!(version, crate::management::MS_INTERFACE_SCHEMA_VERSION);
    }
}
