//! SDS-TL (Short Data Service Transport Layer) PDUs — ETSI TS 100 392-2 v3.10.1
//! clause 29.4.
//!
//! SDS-TL is the end-to-end transport sublayer that rides **inside** the Type-4
//! user-defined data of a U/D-SDS-DATA PDU (and, for the short report, inside the
//! pre-coded status of a U/D-STATUS PDU). It adds a message reference and
//! delivery/read reporting on top of the bearer SDS. It is selected by the
//! Protocol identifier (cl. 29.4.3.9): this module handles the "Text Messaging"
//! protocol that uses SDS-TL ([`PROTOCOL_ID_TEXT_MESSAGING`], `1000 0010`).
//!
//! Only the common no-storage/forward, no-forward-address case is encoded
//! (Storage/forward control = 0, so the Validity period / Forward address
//! conditional IEs are absent); this is exactly the normal MS↔MS delivery-report
//! path. Every fixed header here is byte-aligned:
//!   - SDS-TRANSFER header = 24 bits (PI 8, type 4, DRR 2, service-sel 1,
//!     storage/forward 1, message reference 8) then the user data;
//!   - SDS-REPORT = 32 bits (PI, type, ack-required 1, reserved 2,
//!     storage/forward 1, delivery status 8, message reference 8);
//!   - SDS-ACK = 32 bits (PI, type, reserved 4, delivery status 8, message
//!     reference 8);
//!   - SDS-SHORT-REPORT = a 16-bit pre-coded status (`011111` + short report
//!     type 2 + message reference 8).

/// Protocol identifier: Text Messaging using SDS-TL (Table 29.21, `1000 0010`).
pub const PROTOCOL_ID_TEXT_MESSAGING: u8 = 0x82;

/// Message type values (Table 29.20), low nibble; high bit 0 ⇒ defined by SDS-TL.
pub const MSG_TYPE_SDS_TRANSFER: u8 = 0b0000;
pub const MSG_TYPE_SDS_REPORT: u8 = 0b0001;
pub const MSG_TYPE_SDS_ACK: u8 = 0b0010;

/// Delivery report request (Table 29.17), 2 bits.
pub const DRR_NONE: u8 = 0b00;
pub const DRR_RECEIVED: u8 = 0b01;
pub const DRR_CONSUMED: u8 = 0b10;
pub const DRR_RECEIVED_AND_CONSUMED: u8 = 0b11;

/// Selected delivery-status codes (Table 29.16), 8 bits. The full table is large;
/// these are the ones the MS generates/recognises. Any other value is passed
/// through opaquely to the user application.
pub const DELIVERY_RECEIPT_ACK_BY_DEST: u8 = 0x00; // SDS receipt acknowledged by destination
pub const DELIVERY_CONSUMED_BY_DEST: u8 = 0x02; // SDS consumed by destination

/// Short report type (Table 29.23), 2 bits.
pub const SHORT_REPORT_PROTOCOL_NOT_SUPPORTED: u8 = 0b00;
pub const SHORT_REPORT_DEST_MEMORY_FULL: u8 = 0b01;
pub const SHORT_REPORT_MESSAGE_RECEIVED: u8 = 0b10;
pub const SHORT_REPORT_MESSAGE_CONSUMED: u8 = 0b11;

/// The 6-bit prefix marking a pre-coded status value as an SDS-TL short report
/// (Table 29.13, `011111`).
pub const SHORT_REPORT_STATUS_PREFIX: u16 = 0b011111;

/// SDS-TRANSFER PDU (Table 29.14), no storage/forward, no forward address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdsTransfer {
    pub protocol_id: u8,
    pub delivery_report_request: u8,
    /// Service selection (uplink): `false` = individual, `true` = group/individual.
    pub service_selection: bool,
    pub message_reference: u8,
    /// Application user data (opaque to SDS-TL; e.g. a text-messaging body).
    pub user_data: Vec<u8>,
    /// Length of `user_data` in bits (may be a non-multiple of 8; the trailing
    /// bits of the last byte are padding).
    pub user_data_bits: u16,
}

/// SDS-REPORT PDU (Table 29.12), no storage/forward, no user data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdsReport {
    pub protocol_id: u8,
    pub ack_required: bool,
    pub delivery_status: u8,
    pub message_reference: u8,
}

/// SDS-ACK PDU (Table 29.11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdsAck {
    pub protocol_id: u8,
    pub delivery_status: u8,
    pub message_reference: u8,
}

/// SDS-SHORT-REPORT (Table 29.13) carried in a pre-coded status value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdsShortReport {
    pub short_report_type: u8,
    pub message_reference: u8,
}

/// A parsed SDS-TL PDU from a Type-4 user-data payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdsTlPdu {
    Transfer(SdsTransfer),
    Report(SdsReport),
    Ack(SdsAck),
}

impl SdsTransfer {
    /// Encode to `(len_bits, bytes)` for a Type-4 user-data field.
    pub fn encode(&self) -> (u16, Vec<u8>) {
        let mut out = Vec::with_capacity(3 + self.user_data.len());
        out.push(self.protocol_id);
        // msg type (4) | DRR (2) | service selection (1) | storage/forward (1=0)
        out.push((MSG_TYPE_SDS_TRANSFER << 4) | ((self.delivery_report_request & 0x3) << 2) | ((self.service_selection as u8) << 1));
        out.push(self.message_reference);
        out.extend_from_slice(&self.user_data);
        let len_bits = 24u32 + self.user_data_bits as u32;
        (len_bits.min(u16::MAX as u32) as u16, out)
    }
}

impl SdsReport {
    pub fn encode(&self) -> (u16, Vec<u8>) {
        let byte1 = (MSG_TYPE_SDS_REPORT << 4) | ((self.ack_required as u8) << 3); // reserved (2) = 0, storage/forward (1) = 0
        (32, vec![self.protocol_id, byte1, self.delivery_status, self.message_reference])
    }
}

impl SdsAck {
    pub fn encode(&self) -> (u16, Vec<u8>) {
        let byte1 = MSG_TYPE_SDS_ACK << 4; // reserved (4) = 0
        (32, vec![self.protocol_id, byte1, self.delivery_status, self.message_reference])
    }
}

impl SdsShortReport {
    /// Encode as a 16-bit pre-coded status value (Table 29.13).
    pub fn encode_status(&self) -> u16 {
        (SHORT_REPORT_STATUS_PREFIX << 10) | (((self.short_report_type & 0x3) as u16) << 8) | self.message_reference as u16
    }

    /// Decode from a 16-bit pre-coded status. `None` when the value is not an
    /// SDS-TL short report (its top 6 bits are not `011111`).
    pub fn decode_status(status: u16) -> Option<Self> {
        if (status >> 10) != SHORT_REPORT_STATUS_PREFIX {
            return None;
        }
        Some(SdsShortReport {
            short_report_type: ((status >> 8) & 0x3) as u8,
            message_reference: (status & 0xFF) as u8,
        })
    }
}

/// Decode an SDS-TL PDU from a Type-4 user-data payload `(len_bits, bytes)`.
///
/// Returns `None` (⇒ the caller treats the payload as opaque non-SDS-TL data)
/// when the payload is too short, the message type is application-defined
/// (high bit set, Table 29.20), or a storage/forward variant we do not decode is
/// present.
pub fn decode(len_bits: u16, bytes: &[u8]) -> Option<SdsTlPdu> {
    if bytes.len() < 2 {
        return None;
    }
    let protocol_id = bytes[0];
    let msg_type = bytes[1] >> 4;
    if msg_type & 0b1000 != 0 {
        return None; // application-defined message type — not SDS-TL framed
    }
    match msg_type {
        MSG_TYPE_SDS_TRANSFER => {
            if bytes.len() < 3 {
                return None;
            }
            let drr = (bytes[1] >> 2) & 0x3;
            let service_selection = (bytes[1] >> 1) & 0x1 != 0;
            let storage_forward = bytes[1] & 0x1 != 0;
            if storage_forward {
                return None; // storage/forward + forward-address not decoded
            }
            let message_reference = bytes[2];
            let user_data = bytes[3..].to_vec();
            let user_data_bits = len_bits.saturating_sub(24);
            Some(SdsTlPdu::Transfer(SdsTransfer {
                protocol_id,
                delivery_report_request: drr,
                service_selection,
                message_reference,
                user_data,
                user_data_bits,
            }))
        }
        MSG_TYPE_SDS_REPORT => {
            if bytes.len() < 4 {
                return None;
            }
            let ack_required = (bytes[1] >> 3) & 0x1 != 0;
            let storage_forward = bytes[1] & 0x1 != 0;
            if storage_forward {
                return None;
            }
            Some(SdsTlPdu::Report(SdsReport {
                protocol_id,
                ack_required,
                delivery_status: bytes[2],
                message_reference: bytes[3],
            }))
        }
        MSG_TYPE_SDS_ACK => {
            if bytes.len() < 4 {
                return None;
            }
            Some(SdsTlPdu::Ack(SdsAck {
                protocol_id,
                delivery_status: bytes[2],
                message_reference: bytes[3],
            }))
        }
        _ => None, // reserved SDS-TL message type
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_round_trips() {
        let t = SdsTransfer {
            protocol_id: PROTOCOL_ID_TEXT_MESSAGING,
            delivery_report_request: DRR_RECEIVED_AND_CONSUMED,
            service_selection: false,
            message_reference: 42,
            user_data: vec![0x01, b'H', b'i'],
            user_data_bits: 24,
        };
        let (len_bits, bytes) = t.encode();
        assert_eq!(len_bits, 24 + 24, "24-bit header + 24-bit body");
        assert_eq!(decode(len_bits, &bytes), Some(SdsTlPdu::Transfer(t)));
    }

    #[test]
    fn report_and_ack_round_trip() {
        let r = SdsReport {
            protocol_id: PROTOCOL_ID_TEXT_MESSAGING,
            ack_required: true,
            delivery_status: DELIVERY_CONSUMED_BY_DEST,
            message_reference: 7,
        };
        let (lb, b) = r.encode();
        assert_eq!(lb, 32);
        assert_eq!(decode(lb, &b), Some(SdsTlPdu::Report(r)));

        let a = SdsAck {
            protocol_id: PROTOCOL_ID_TEXT_MESSAGING,
            delivery_status: DELIVERY_RECEIPT_ACK_BY_DEST,
            message_reference: 7,
        };
        let (lb, b) = a.encode();
        assert_eq!(decode(lb, &b), Some(SdsTlPdu::Ack(a)));
    }

    #[test]
    fn short_report_round_trips_and_rejects_non_prefix() {
        let sr = SdsShortReport { short_report_type: SHORT_REPORT_MESSAGE_CONSUMED, message_reference: 200 };
        let status = sr.encode_status();
        assert_eq!(status >> 10, SHORT_REPORT_STATUS_PREFIX);
        assert_eq!(SdsShortReport::decode_status(status), Some(sr));
        // A normal pre-coded status (e.g. 0x8002) is not an SDS-TL short report.
        assert!(SdsShortReport::decode_status(0x8002).is_none());
    }

    #[test]
    fn decode_passes_through_application_defined_and_short_payloads() {
        // Application-defined message type (high bit set) ⇒ not SDS-TL.
        assert!(decode(16, &[PROTOCOL_ID_TEXT_MESSAGING, 0x80]).is_none());
        // Too short.
        assert!(decode(8, &[0x02]).is_none());
        // Storage/forward set ⇒ not decoded here.
        let sf = [PROTOCOL_ID_TEXT_MESSAGING, (MSG_TYPE_SDS_TRANSFER << 4) | 0x1, 5, 0xAA];
        assert!(decode(32, &sf).is_none());
    }
}
