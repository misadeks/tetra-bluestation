//! TNSDS-SAP message types (ETSI TS 100 392-2 v3.10.1, clause 13.3).
//!
//! Plane A external Short Data Service SAP between CMCE/SDS and the MS user
//! application. Field names and optionality follow the primitive parameter
//! tables in cl. 13.3.2 (Table 13.1 TNSDS-STATUS, Table 13.2 TNSDS-REPORT,
//! Table 13.3 TNSDS-UNITDATA); parameter values follow cl. 13.3.3.
//!
//! Implemented subset (matching the BS peer `sds_bs.rs` and the available PDU
//! encoders): called/calling party type identifier = SSI, individual (ISSI) or
//! group (GSSI) addressing. Short-number, TSI/extension, external-subscriber and
//! DM-MS addressing, area selection and access priority are deferred (the BS
//! feature-checks reject them today); when added they map to the corresponding
//! `U-SDS-DATA`/`U-STATUS` optional fields.
//!
//! As in the TNCC/TNMM SAPs, the transport-level `handle` (a local SDU
//! identifier, cl. 13.3.3) is carried by the wrapper control command, not by the
//! primitive payload struct.

use bitcode::{Decode, Encode};
use serde::{Deserialize, Serialize};

use crate::control::enums::sds_user_data::SdsUserData;

/// TNSDS-UNITDATA request (Table 13.3, cl. 13.3.2.3): send a user-defined SDS
/// message to another user application (individual or group).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnsdsUnitdataRequest {
    /// Called party SSI (ISSI when individual, GSSI when group). cl. 13.3.3.
    pub called_party_ssi: u32,
    /// Called party address type (cl. 13.3.3): `true` = group (GSSI),
    /// `false` = individual (ISSI). Broadcast is not supported.
    pub called_party_is_group: bool,
    /// Short data type identifier + user-defined data 1..4 (Table 13.3).
    pub user_data: SdsUserData,
}

/// TNSDS-UNITDATA indication (Table 13.3, cl. 13.3.2.3): a user-defined SDS
/// message was received from another user application.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnsdsUnitdataIndication {
    /// Calling party SSI (ISSI) — mandatory in the indication (Table 13.3).
    pub calling_party_ssi: u32,
    /// Whether the message was received as a group message (GSSI-addressed).
    pub called_party_is_group: bool,
    /// Short data type identifier + user-defined data 1..4 (Table 13.3).
    pub user_data: SdsUserData,
}

/// TNSDS-STATUS request (Table 13.1, cl. 13.3.2.1): send a pre-coded status
/// message (selected by status number) to another user or users.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnsdsStatusRequest {
    /// Called party SSI (ISSI when individual, GSSI when group). cl. 13.3.3.
    pub called_party_ssi: u32,
    /// Called party address type (cl. 13.3.3): `true` = group (GSSI).
    pub called_party_is_group: bool,
    /// Pre-coded status number (Table 13.1, cl. 13.3.3 "Status number";
    /// pre-coded status value, cl. 14.8.34).
    pub status_number: u16,
}

/// TNSDS-STATUS indication (Table 13.1, cl. 13.3.2.1): a pre-coded status
/// message was received from another user application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnsdsStatusIndication {
    /// Calling party SSI (ISSI) — mandatory in the indication (Table 13.1).
    pub calling_party_ssi: u32,
    /// Pre-coded status number (cl. 13.3.3 / cl. 14.8.34).
    pub status_number: u16,
}
