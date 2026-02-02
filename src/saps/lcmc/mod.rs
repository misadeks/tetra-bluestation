// Clause 17.3.3 Service state diagram for the LCMC-SAP (MLE-CMCE)

#![allow(unused)]

use crate::common::{address::TetraAddress, bitbuffer::BitBuffer, tetra_common::Todo};

/// MLE-ACTIVITY request: this primitive shall be used by the CMCE to inform the MLE of the state of any circuit
/// mode call(s).
#[derive(Debug)]
pub struct LcmcMleActivityReq {
    pub call_state: Todo,
}

/// MLE-BREAK indication: this primitive shall be used by the MLE to inform the CMCE that access to the
/// communication resources is temporarily unavailable and that the data transfer service cannot be used. In the graceful
/// service degradation mode this primitive indicates which services can access communication resources.
#[derive(Debug)]
pub struct LcmcMleBreakInd {
    pub permitted_services_in_ms_graceful_service_degradation_mode: Todo
}

/// MLE-BUSY indication: this shall be used by the MLE to inform the CMCE that a MM protocol exchange is in
/// progress.
#[derive(Debug)]
pub struct LcmcMleBusyInd {}

/// MLE-CANCEL request: this primitive shall be used by the CMCE to delete a previous request issued but not yet
/// transmitted. The ability to cancel is removed when an MLE-REPORT indication is received indicating transmission
/// of the CMCE PDU.
#[derive(Debug)]
pub struct LcmcMleCancelReq {
    pub handle: Todo,
}

/// MLE-CLOSE indication: this primitive shall be used by the MLE to indicate to the CMCE that access to the
/// communications resources has been removed and that data transfer service cannot be used.
#[derive(Debug)]
pub struct LcmcMleCloseInd {}

/// MLE-CONFIGURE request: this primitive shall be used to pass inter layer management information relating to
/// circuit mode calls, e.g. whether Tx grant has been given, type of traffic, etc.
#[derive(Debug)]
pub struct LcmcMleConfigureReq {
    pub endpoint_id: Todo,
    pub chan_change_accepted: bool,
    pub chan_change_handle: Todo,
    pub call_release: Todo,
    pub encryption_flag: bool,
    pub circuit_mode_type: Todo,
    pub simplex_duplex: bool,
    pub add_temp_gssi: Todo,
    pub del_temp_gssi: Todo,
    pub tx_grant: Todo,
    pub switch_u_plane: Todo,
}

/// MLE-CONFIGURE indication: this primitive shall be used to pass inter layer management information relating to
/// circuit mode calls and packet data conflicts.
#[derive(Debug)]
pub struct LcmcMleConfigureInd {
    pub endpoint_id: Todo,
    pub chan_change_responce_required: bool,
    pub chan_change_handle: Todo,
    pub reason_for_config_indication: Todo,
    pub conflicting_endpoint_id: Todo,
}

/// MLE-DISABLE indication: this primitive shall be used by the MLE entity to instruct the CMCE entity to enter the
/// temporarily disabled state.
#[derive(Debug)]
pub struct LcmcMleDisableInd {
    pub permitted_services_in_temp_disabled_mode: Todo,
}

/// MLE-ENABLE indication: this primitive shall be used by the MLE entity to instruct the CMCE entity to recover from
/// the tamporarily [sic] disabled state.
#[derive(Debug)]
pub struct LcmcMleEnableInd {}

/// MLE-IDENTITIES request: this primitive shall be used by the CMCE to inform the MLE and layer 2 of a change to
/// the list of group identities.
#[derive(Debug)]
pub struct LcmcMleIdentitiesReq {
    pub gssi_list: Vec<Todo>
}

/// MLE-IDLE indication: this shall be used by the MLE to inform the CMCE that a MM protocol exchange has
/// completed.
#[derive(Debug)]
pub struct LcmcMleIdleInd {}

/// MLE-INFO indication: this primitive shall be used by MLE to inform the CMCE of a change in system broadcast
/// parameters, to indicate whether there is any match between the subscriber class being broadcast by the SwMI and the
/// subscriber class of the MS, and to indicate if the present cell is a permitted cell.
#[derive(Debug)]
pub struct LcmcMleInfoInd {
    pub broadcast_params: Todo,
    pub subscriber_class_match: Todo,
    pub permitted_cell_info: Todo,
}

/// MLE-OPEN indication: this primitive shall be used by the MLE to inform the CMCE that it has access to the
/// communication resources and that the data transfer service can be used.
#[derive(Debug)]
pub struct LcmcMleOpenInd {
    pub mcc: Todo, // current network
    pub mnc: Todo, // current network
}

/// MLE-REOPEN indication: this primitive shall be used by the MLE to inform the CMCE that access to the
/// communication resources is once again available. MLE-REOPEN indication indicates the failure of current call
/// restoration to CMCE but does not prevent CMCE from restoring other circuit-mode calls. The data transfer service can
/// now be used.
#[derive(Debug)]
pub struct LcmcMleReopenInd {}

/// MLE-REPORT indication: this shall be used by the MLE to report on the completion of an MLE-UNITDATA
/// request procedure. The result of the transfer attempt shall be passed as a parameter.
#[derive(Debug)]
pub struct LcmcMleReportInd {
    pub handle: Todo,
    pub transfer_result: Todo,
    pub channel_change_response_required: bool,
    pub channel_change_handle: Todo,
}

/// MLE-RESTORE request: this primitive shall be used by the CMCE to restore a call after a successful cell reselection
#[derive(Debug)]
pub struct LcmcMleRestoreReq {
    pub sdu: Todo,
    pub handle: Todo,
    pub layer2service: Todo,
    pub pdu_prio: Todo,
    pub stealing_permission: bool,
    pub stealing_repeats_flag: bool,
}

/// MLE-RESTORE confirm: this primitive indicates the success or failure of call restoration to the CMCE as a result of
/// a previously issued MLE-RESTORE request.
#[derive(Debug)]
pub struct LcmcMleRestoreConf {
    pub sdu: Todo,
    pub handle: Todo,
}

/// MLE-RESUME indication: this primitive shall be used by the MLE to inform the CMCE that access to the
/// communication resources is once again available. The data transfer service can now be used and the CMCE may
/// attempt to restore any circuit mode calls.
#[derive(Debug)]
pub struct LcmcMleResumeInd {
    pub mcc: Todo, // current network
    pub mnc: Todo, // current network
}

/// MLE-UNITDATA request: this primitive shall be used by the CMCE to send unconfirmed data to a peer entity on the
/// TETRA infrastructure side. Parameter indicates which layer 2 service is required.
#[derive(Debug)]
pub struct LcmcMleUnitdataReq {
    pub sdu: BitBuffer,
    /// Destination identity (ISSI/GSSI) for air-interface routing.
    pub address: TetraAddress,
    pub handle: Todo,
    pub endpoint_id: Todo,
    pub link_id: Todo,
    pub layer2service: Todo,
    pub pdu_prio: Todo,
    pub layer2_qos: Todo,
    pub stealing_permission: bool,
    pub stealing_repeats_flag: bool,
    /// We use this to indicate it may be retransmitted
    /// This may differ from what ETSI envisioned
    pub eligible_for_graceful_degradation: bool,
}

/// MLE-UNITDATA indication: this primitive shall be used by the MLE to pass to the CMCE entity data which has
/// been received from a peer entity on the TETRA infrastructure side.
#[derive(Debug)]
pub struct LcmcMleUnitdataInd {
    pub sdu: BitBuffer,
    pub handle: Todo,
    pub endpoint_id: Todo,
    pub link_id: Todo,
    pub received_tetra_address: TetraAddress, // ITSI/GTSI
    // pub received_address_type: Todo,
    pub chan_change_resp_req: bool,
    pub chan_change_handle: Option<Todo>,
}
