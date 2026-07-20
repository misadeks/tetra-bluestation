use tetra_core::{BitBuffer, EndpointId, Todo};

/// BS only
/// TL-SAP and TMB-SAP merged into TLMB-SAP
#[derive(Debug, Clone)]
pub struct TlmbSyncReq {
    pub endpoint_id: EndpointId,
    pub tl_sdu: BitBuffer,
    pub priority: Todo,
}

/// MS only
/// TL-SAP and TMB-SAP merged into TLMB-SAP
#[derive(Debug, Clone)]
pub struct TlmbSyncInd {
    pub endpoint_id: EndpointId,
    pub tl_sdu: BitBuffer,
}

/// BS only
/// TL-SAP and TMB-SAP merged into TLMB-SAP
#[derive(Debug, Clone)]
pub struct TlmbSysinfoReq {
    pub endpoint_id: EndpointId,
    pub tl_sdu: BitBuffer,
    pub mac_broadcast_info: Option<Todo>,
    pub priority: Todo,
}

/// MS only
/// TL-SAP and TMB-SAP merged into TLMB-SAP
#[derive(Debug, Clone)]
pub struct TlmbSysinfoInd {
    pub endpoint_id: EndpointId,
    pub tl_sdu: BitBuffer,
    pub mac_broadcast_info: Option<Todo>,
}

/// MS only — internal serving-cell downlink monitoring indication.
///
/// NOT an over-the-air PDU: this is the stack's own IPC primitive by which the
/// MS PHY reports the health of the serving-cell downlink to the MLE. It carries
/// the result of the physical downlink-decode surveillance the MLE uses to
/// detect radio link failure (ETSI TS 100 392-2 cl. 18.3.4.5.3 — AACH/training
/// sequence decode failure) and to re-open the link once the downlink recovers
/// (cl. 18.3.4.7). `downlink_available == false` signals a declared downlink
/// failure; `true` signals recovery. The MLE turns this into the standardized
/// MLE-BREAK / MLE-REOPEN primitives towards the upper layers.
#[derive(Debug, Clone)]
pub struct TlmbMonitorInd {
    pub downlink_available: bool,
}
