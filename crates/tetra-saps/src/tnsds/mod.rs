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

// ---------------------------------------------------------------------------
// SDS-TL (ETSI TS 100 392-2 cl. 29) transport-layer messaging: text messages
// with a message reference and end-to-end delivery/read reporting. Layered on
// top of the Type-4 SDS bearer (U/D-SDS-DATA) and, for the short report, the
// pre-coded status (U/D-STATUS). Distinct from the opaque `TnsdsUnitdata*`
// path above, which carries Type-1..4 data with no transport layer.
// ---------------------------------------------------------------------------

/// Delivery report request (SDS-TRANSFER, Table 29.17): what end-to-end report
/// the sender wants back for a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub enum DeliveryReportRequest {
    /// No delivery report requested.
    None,
    /// Report when the message has been received by the destination.
    Received,
    /// Report when the message has been consumed (e.g. read) by the destination.
    Consumed,
    /// Report on both received and consumed.
    ReceivedAndConsumed,
}

impl DeliveryReportRequest {
    /// The 2-bit on-air value (Table 29.17).
    pub fn to_bits(self) -> u8 {
        match self {
            DeliveryReportRequest::None => 0b00,
            DeliveryReportRequest::Received => 0b01,
            DeliveryReportRequest::Consumed => 0b10,
            DeliveryReportRequest::ReceivedAndConsumed => 0b11,
        }
    }

    /// From the 2-bit on-air value (Table 29.17).
    pub fn from_bits(bits: u8) -> Self {
        match bits & 0x3 {
            0b00 => DeliveryReportRequest::None,
            0b01 => DeliveryReportRequest::Received,
            0b10 => DeliveryReportRequest::Consumed,
            _ => DeliveryReportRequest::ReceivedAndConsumed,
        }
    }

    /// Whether a "received" report is requested.
    pub fn wants_received(self) -> bool {
        matches!(self, DeliveryReportRequest::Received | DeliveryReportRequest::ReceivedAndConsumed)
    }

    /// Whether a "consumed" (read) report is requested.
    pub fn wants_consumed(self) -> bool {
        matches!(self, DeliveryReportRequest::Consumed | DeliveryReportRequest::ReceivedAndConsumed)
    }
}

/// TNSDS-UNITDATA request carrying an SDS-TL SDS-TRANSFER (cl. 29.4.2.4): send a
/// text/user message with a message reference and optional delivery reporting.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnsdsMessageRequest {
    /// Called party SSI (ISSI when individual, GSSI when group).
    pub called_party_ssi: u32,
    pub called_party_is_group: bool,
    /// SDS-TL protocol identifier (cl. 29.4.3.9), e.g. `0x82` text messaging.
    pub protocol_id: u8,
    /// End-to-end report requested for this message.
    pub delivery_report_request: DeliveryReportRequest,
    /// Application-chosen message reference (0..255, cl. 29.4.3.7). Echoed in any
    /// delivery/read report so the UI can correlate.
    pub message_reference: u8,
    /// The application message body (opaque to SDS-TL; e.g. a text-coding-scheme
    /// byte followed by characters).
    pub user_data: Vec<u8>,
    /// Length of `user_data` in bits (usually `user_data.len() * 8`).
    pub user_data_bits: u16,
}

/// TNSDS-UNITDATA indication for a received SDS-TL SDS-TRANSFER (cl. 29.4.2.4).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnsdsMessageIndication {
    pub calling_party_ssi: u32,
    pub called_party_is_group: bool,
    pub protocol_id: u8,
    pub delivery_report_request: DeliveryReportRequest,
    pub message_reference: u8,
    pub user_data: Vec<u8>,
    pub user_data_bits: u16,
}

/// TNSDS-REPORT request (Table 13.2, cl. 13.3.2.2): send an SDS-TL delivery/read
/// report (SDS-REPORT, cl. 29.4.2.2) for a previously received message — e.g.
/// "consumed" once the user reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnsdsReportRequest {
    /// The original sender (destination of this report).
    pub called_party_ssi: u32,
    /// The message reference being reported on (from the received message).
    pub message_reference: u8,
    /// Delivery-status code (Table 29.16), e.g. `0x00` received, `0x02` consumed.
    pub delivery_status: u8,
}

/// TNSDS-REPORT indication (Table 13.2, cl. 13.3.2.2): a delivery/read report was
/// received for a message this MS sent (SDS-REPORT / SDS-ACK / SDS-SHORT-REPORT).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnsdsReportIndication {
    /// The reporting party (the original destination).
    pub calling_party_ssi: u32,
    /// The message reference being reported on.
    pub message_reference: u8,
    /// Delivery-status code (Table 29.16) — for a short report this is mapped
    /// from the short report type (Table 29.23).
    pub delivery_status: u8,
    /// `true` when the report arrived as an SDS-SHORT-REPORT (U/D-STATUS).
    pub short_form: bool,
}

/// TNSDS-CANCEL: stop tracking a locally-outstanding SDS-TL message (one that was
/// sent with a delivery-report request and has not yet been reported on).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
pub struct TnsdsCancelRequest {
    pub message_reference: u8,
}
