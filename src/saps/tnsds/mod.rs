//! TN-SDS-SAP (CMCE <-> User/application).
//!
//! This SAP is used by external interfaces (e.g. PEI/AT, command server, web UI)
//! to request SDS transmissions via CMCE.

#![allow(unused)]

use crate::common::{address::TetraAddress, bitbuffer::BitBuffer, sds_job::SdsTraceMeta};

#[derive(Debug, Clone)]
pub struct TnsdsUnitdataReq {
    /// Destination identity (ISSI/GSSI).
    pub dst: TetraAddress,
    /// Calling party SSI to be encoded into SDS PDUs.
    pub calling_ssi: u32,
    /// Optional ISS routing (B3): infrastructure link selection.
    pub link_id: u16,
    /// Optional ISS routing (B3): endpoint selection.
    pub endpoint_id: u16,
    /// Type-4 payload bitbuffer (includes PID as first octet).
    pub type4_payload: BitBuffer,
    /// Delivery report request (SDS-TL) hint.
    pub delivery_report_request: bool,
    /// Message reference (SDS-TL).
    pub message_reference: u8,

    /// Optional trace metadata used by the WebUI/API to show per-part send status.
    pub trace: Option<SdsTraceMeta>,
}

#[derive(Debug, Clone)]
pub struct TnsdsUnitdataInd {
    pub src: TetraAddress,
    pub decoded: Option<String>,
}
