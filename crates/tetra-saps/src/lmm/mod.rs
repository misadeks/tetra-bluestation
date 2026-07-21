// Clause 17.3.2 Service primitives for the LMM-SAP
#![allow(unused)]
use tetra_core::{BitBuffer, Layer2Service, MleHandle, TetraAddress, Todo, TxReporter};

/// This shall be used as a request to initiate the selection of a cell for communications. The
/// request shall always be made after power on and may be made at any time thereafter.
#[derive(Debug, Clone)]
pub struct LmmMleActivateReq {
    pub mcc_list: Vec<u16>,
    pub mnc_list: Vec<u16>,
    pub la_list: Vec<u16>,
    pub cell_type_prefs: Option<Todo>,
}

#[derive(Debug, Clone)]
pub struct LmmMleActivateInd {
    pub cell_availability: Todo,
}

/// This shall be used as a confirmation to the MM entity that a cell has been selected with the
/// required characteristics.
///
/// Per ETSI TS 100 392-2 cl. 16.4.1.0 the MLE-LINK/ACTIVATE indication "supplies the MNC, MCC,
/// LA and cell type of the new cell" so MM can apply the registration conditions of cl. 18.3.4.7.1a
/// (different network -> migrating; LA outside the registered area -> roaming location updating).
#[derive(Debug, Clone)]
pub struct LmmMleActivateConf {
    pub registration_required: bool,
    pub mcc: u16,
    pub mnc: u16,
    pub la: u16,
    /// BS service details "system wide services" flag of the selected cell
    /// (cl. 18.5.2.1 Table 18.26). `false` means the cell advertises "system wide
    /// services temporarily not supported"; per cl. 16.4.1.0 cond. 5 the MS must
    /// then register even inside its registered area. A transition back to `true`
    /// (normal mode) while the MS holds a temporary registration triggers a
    /// periodic location update (cl. 16.4.8 / 16.4.1.0 NOTE).
    pub system_wide_services: bool,
    pub cell_type: Todo,
}

#[derive(Debug, Clone)]
pub struct LmmMleActivityReq {
    pub sleep_mode: Todo,
}

#[derive(Debug, Clone)]
pub struct LmmMleBusyReq {}

#[derive(Debug, Clone)]
pub struct LmmMleCancelReq {
    pub handle: Todo,
}

#[derive(Debug, Clone)]
pub struct LmmMleCloseReq {}
#[derive(Debug, Clone)]
pub struct LmmMleConfigureReq {
    pub periodic_reporting_timer: Todo,
}

#[derive(Debug, Clone)]
pub struct LmmMleConfigureInd {
    pub periodic_reporting_timer: Todo,
}

#[derive(Debug, Clone)]
pub struct LmmMleDeactivateReq {}

#[derive(Debug, Clone)]
pub struct LmmMleDisableReq {
    pub permitted_services_in_temp_disabled_mode: Todo,
}

#[derive(Debug, Clone)]
pub struct LmmMleEnableReq {}

/// MLE-IDENTITIES request (cl. 17.3.2). MM uses this to tell the MLE the set of
/// identities by which the MS is currently known, so the MLE (and, through it,
/// the MAC downlink address filter, cl. 23.4.1.2.1) accepts traffic addressed to
/// them. Per cl. 16.8.2 (last paragraph) MM sends the accepted-and-thus-attached
/// group identities with this primitive after a successful group
/// attach/detach; it is also sent after registration to seed the attached set.
#[derive(Debug, Clone)]
pub struct LmmMleIdentitiesReq {
    /// The MS's own Individual Short Subscriber Identity (ISSI).
    pub issi: u32,
    /// Assigned/alias SSI, if the SwMI has allocated one (cl. 16.4.7). `None`
    /// when the MS is known only by its ISSI (this stack's clear-mode default).
    pub assi: Option<u32>,
    /// The complete, authoritative set of group identities (GSSIs) currently
    /// attached after the update. The MLE replaces its group-identity set with
    /// this (and configures the MAC filter accordingly), so it is a full
    /// snapshot rather than an increment.
    pub attached_gssis: Vec<u32>,
    /// Group identities (GSSIs) removed by this update, for logging/observability.
    /// The authoritative post-update set is `attached_gssis`.
    pub detached_gssis: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct LmmMleIdleReq {}

#[derive(Debug, Clone)]
pub struct LmmMleInfoReq {
    pub subscriber_class: Todo,
    pub scch_config: Todo,
    pub energy_economy_config: Todo,
    pub minimal_mode_config: Todo,
    pub dual_watch_config: Todo,
}

#[derive(Debug, Clone)]
pub struct LmmMleInfoInd {
    pub broadcast_params: Todo,
    pub subscriber_class_match: Todo,
}

#[derive(Debug, Clone)]
pub struct LmmMleLinkReq {
    pub mcc: Todo,
    pub mnc: Todo,
    pub la_list: Vec<u16>,
    pub cell_type_prefs: Option<Todo>,
}

#[derive(Debug, Clone)]
pub struct LmmMleLinkInd {
    pub mcc: Todo,
    pub mnc: Todo,
    pub la: u16,
    pub registration_type: Todo,
    pub security_params: Todo,
    pub cell_type: Todo,
}

#[derive(Debug, Clone)]
pub struct LmmMleOpen {}

#[derive(Debug, Clone)]
pub struct LmmMlePrepareReq {
    pub sdu: Todo,
    pub handle: Todo,
    pub layer2service: Layer2Service,
    pub pdu_prio: Todo,
    pub stealing_permission: bool,
    pub stealing_repeats_flag: bool,
}

#[derive(Debug, Clone)]
pub struct LmmMlePrepareConfirm {
    pub sdu: Todo,
    pub handle: Todo,
}

#[derive(Debug, Clone)]
pub struct LmmMleReportInd {
    pub handle: MleHandle,
    pub transfer_result: Todo,
}

/// MLE -> MM break indication (ETSI TS 100 392-2 cl. 18.3.3 / cl. 17.x MLE
/// service description): access to the communication resources is temporarily
/// unavailable (serving-cell radio link failure, cl. 18.3.4.5.3), so MM should
/// regard itself as out of service and inform the TNMM-SAP user (TNMM-SERVICE
/// "out of service", cl. 15.3.4). The MLE offers MLE-BREAK at every upper-layer
/// SAP (LCMC to CMCE, LMM to MM); this is the LMM-SAP form. No graceful
/// service-degradation service list is modelled.
#[derive(Debug, Clone)]
pub struct LmmMleBreakInd {}

/// MLE -> MM reopen indication (ETSI TS 100 392-2 cl. 18.3.4.7): access to the
/// communication resources has been restored (serving-cell downlink recovered).
/// MM leaves out-of-service; the subsequent cell re-selection / registration
/// re-evaluation determines the final service state. LMM-SAP counterpart of the
/// LCMC `LcmcMleReopenInd`.
#[derive(Debug, Clone)]
pub struct LmmMleReopenInd {}

/// MLE -> MM serving-cell receive-level indication (**implementation-defined**,
/// NOT an ETSI LMM-SAP primitive).
///
/// TETRA does not carry a signal-strength value to MM over the standardized
/// LMM-SAP: RSSI/path-loss is an MLE-internal reselection input (cl. 18.3.4),
/// consumed by the MLE, not surfaced up. This primitive is a local Plane B
/// convenience so MM can include the current serving-cell downlink level in the
/// management runtime-state snapshot the UI reads (a receive-level meter, as in
/// the flowstation reference). `rssi_dbfs` is uncalibrated dBFS relative to the
/// demodulator full-scale magnitude; `None` while the serving cell is out of
/// service or before the first measurement.
#[derive(Debug, Clone)]
pub struct LmmMleRssiInd {
    pub rssi_dbfs: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct LmmMleUnitdataReq {
    pub sdu: BitBuffer,
    pub handle: MleHandle,
    // pub address_type: Todo,
    pub address: TetraAddress,
    pub layer2service: Layer2Service,
    // pub pdu_prio: Todo, // Optional feature
    pub stealing_permission: bool,
    pub stealing_repeats_flag: bool,
    pub encryption_flag: bool,
    pub is_null_pdu: bool, // Prio should be lowest and may not steal
    pub tx_reporter: Option<TxReporter>,
}

#[derive(Debug, Clone)]
pub struct LmmMleUnitdataInd {
    pub sdu: BitBuffer,
    pub handle: MleHandle,
    pub received_address: TetraAddress,
    // pub received_address_type: Todo,
}

#[derive(Debug, Clone)]
pub struct LmmMleUpdateReq {
    pub mcc: Todo,
    pub mnc: Todo,
    pub ra: Todo,
    pub cell_type_prefs: Option<Todo>,
    pub registration_result: Todo,
}
