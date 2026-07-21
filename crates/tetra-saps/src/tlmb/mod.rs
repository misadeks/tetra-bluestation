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
///
/// It additionally carries the most recent serving-cell downlink signal level
/// (`rssi_dbfs`), which the PHY refreshes periodically while camped. The MLE
/// uses signal strength as a reselection input (cl. 18.3.4) and it is surfaced
/// to the MS management UI as a receive-level indicator. `None` before the first
/// downlink slot has been measured. The value is uncalibrated dBFS relative to
/// the demodulator full-scale magnitude, not absolute antenna power.
#[derive(Debug, Clone)]
pub struct TlmbMonitorInd {
    pub downlink_available: bool,
    pub rssi_dbfs: Option<f32>,
}

/// MS only — internal scan-dwell-elapsed indication (**[impl policy]**).
///
/// NOT an over-the-air PDU. While the MS is NOT camped (the downlink demodulator
/// is un-synchronized, ETSI TS 100 392-2 cl. 18.3.4 initial cell selection), the
/// PHY cannot emit the per-slot [`TlmbMonitorInd`] it produces while camped
/// (the demodulator exposes no slots until it locks). This primitive is the
/// PHY's heartbeat during acquisition: it is raised once a scan *dwell window*
/// has elapsed on the currently-tuned candidate carrier without acquiring a
/// serving-cell downlink. The MLE's scanning cell-selection engine uses it to
/// advance to the next candidate frequency (cl. 18.3.4). `rssi_dbfs` is the
/// current receive level (the candidate's noise floor when nothing is present),
/// uncalibrated dBFS relative to demodulator full-scale, or `None` before the
/// first measurement.
#[derive(Debug, Clone)]
pub struct TlmbScanDwellInd {
    pub rssi_dbfs: Option<f32>,
}
