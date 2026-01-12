
use crate::common::bitbuffer::BitBuffer;
use crate::common::pdu_parse_error::PduParseErr;

/// SDS-TL PDUs are transported inside the User defined data-4 information element of U/D-SDS-DATA
/// and are intended to be octet-aligned (no O/P bits inside SDS-TL PDUs).
/// See EN 300 392-2 clause 29.4.2 and 29.4.3.*

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdsTlMessageType {
    /// 0000b
    SdsTransfer,
    /// 0001b
    SdsReport,
    /// 0010b
    SdsAck,
    /// 0011b..0111b reserved; 1xxx defined by application.
    Other(u8),
}

impl SdsTlMessageType {
    pub fn from_raw(v: u8) -> Self {
        match v {
            0 => Self::SdsTransfer,
            1 => Self::SdsReport,
            2 => Self::SdsAck,
            other => Self::Other(other),
        }
    }
    pub fn to_raw(self) -> u8 {
        match self {
            Self::SdsTransfer => 0,
            Self::SdsReport => 1,
            Self::SdsAck => 2,
            Self::Other(v) => v & 0x0F,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardAddressType {
    ShortNumberAddress, // 000
    Ssi,                // 001
    Tsi,                // 010
    ExternalSubscriberNumber, // 011 (we keep digits only)
    None,               // 111
    Reserved(u8),
}

impl ForwardAddressType {
    pub fn from_raw(v: u8) -> Self {
        match v & 0x07 {
            0b000 => Self::ShortNumberAddress,
            0b001 => Self::Ssi,
            0b010 => Self::Tsi,
            0b011 => Self::ExternalSubscriberNumber,
            0b111 => Self::None,
            other => Self::Reserved(other),
        }
    }
    pub fn to_raw(self) -> u8 {
        match self {
            Self::ShortNumberAddress => 0b000,
            Self::Ssi => 0b001,
            Self::Tsi => 0b010,
            Self::ExternalSubscriberNumber => 0b011,
            Self::None => 0b111,
            Self::Reserved(v) => v & 0x07,
        }
    }
}

/// Semi-parsed forward address used by SDS-TRANSFER / SDS-REPORT when Storage indicates storage service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardAddress {
    pub addr_type: ForwardAddressType,
    pub sna: Option<u8>,      // 8 bits
    pub ssi: Option<u32>,     // 24 bits
    pub ext_digits: Option<Vec<u8>>, // 4-bit digits (0..9, etc.) as in 14.8.20
}

impl ForwardAddress {
    pub fn none() -> Self {
        Self { addr_type: ForwardAddressType::None, sna: None, ssi: None, ext_digits: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdsTransfer {
    pub protocol_id: u8,
    pub delivery_report_request: u8, // 2 bits (table 435)
    pub short_form_report: bool,      // 1 bit
    pub storage: bool,                // 1 bit
    pub message_reference: u8,        // 8 bits
    // Present only if storage == true:
    pub validity_period: Option<u8>,  // 5 bits
    pub forward_address: Option<ForwardAddress>,
    // Rest of UD4 is user data (mandatory)
    pub user_data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdsReport {
    pub protocol_id: u8,
    pub acknowledgement_required: bool, // 1 bit (table 433)
    pub storage: bool,                 // 1 bit
    pub delivery_status: u8,           // 8 bits (table 434)
    pub message_reference: u8,         // 8 bits
    // Present only if storage == true:
    pub validity_period: Option<u8>,   // 5 bits
    pub forward_address: Option<ForwardAddress>,
    // Optional trailing user data (may be zero length, and there is intentionally no O-bit before it)
    pub user_data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdsAck {
    pub protocol_id: u8,
    pub delivery_status: u8,   // 8 bits
    pub message_reference: u8, // 8 bits
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdsTlPdu {
    Transfer(SdsTransfer),
    Report(SdsReport),
    Ack(SdsAck),
    /// Application-defined or reserved SDS-TL message types; raw payload preserved (after proto_id + msg_type)
    Unknown { protocol_id: u8, msg_type: u8, payload: Vec<u8> },
}

fn require_octet_aligned(b: &BitBuffer) -> Result<(), PduParseErr> {
    if (b.get_pos() & 7) != 0 {
        return Err(PduParseErr::InvalidTrailingMbitValue); // reuse a generic error; define a better one if you want
    }
    Ok(())
}

fn read_u8_aligned(buf: &mut BitBuffer) -> Result<u8, PduParseErr> {
    require_octet_aligned(buf)?;
    Ok(buf.read_field(8, "octet")? as u8)
}

fn drain_remaining_to_bytes(buf: &mut BitBuffer) -> Result<Vec<u8>, PduParseErr> {
    // SDS-TL is intended to be byte aligned. For safety, allow trailing bits but require they are 0 padding.
    let rem = buf.get_len_remaining();
    if rem == 0 { return Ok(vec![]); }

    // If not multiple of 8, read last partial and ensure it's zero.
    let full_bytes = rem / 8;
    let trailing = rem % 8;

    let mut out = Vec::with_capacity(full_bytes as usize + (if trailing > 0 { 1 } else { 0 }));
    for _ in 0..full_bytes {
        out.push(read_u8_aligned(buf)?);
    }
    if trailing > 0 {
        let bits = buf.read_field(trailing, "pad")?;
        if bits != 0 {
            return Err(PduParseErr::InvalidTrailingMbitValue);
        }
    }
    Ok(out)
}

fn parse_external_subscriber_digits(buf: &mut BitBuffer, n_digits: u8) -> Result<Vec<u8>, PduParseErr> {
    // Each digit is 4 bits; if odd, a dummy digit (4 bits) follows and must be 0.
    let mut digits = Vec::with_capacity(n_digits as usize);
    for _ in 0..n_digits {
        digits.push(buf.read_field(4, "ext_digit")? as u8);
    }
    if (n_digits & 1) == 1 {
        let dummy = buf.read_field(4, "dummy_digit")? as u8;
        if dummy != 0 {
            return Err(PduParseErr::InvalidTrailingMbitValue);
        }
    }
    Ok(digits)
}

fn parse_forward_address(buf: &mut BitBuffer) -> Result<ForwardAddress, PduParseErr> {
    let t = ForwardAddressType::from_raw(buf.read_field(3, "forward_address_type")? as u8);
    match t {
        ForwardAddressType::None => Ok(ForwardAddress::none()),
        ForwardAddressType::ShortNumberAddress => {
            let sna = buf.read_field(8, "forward_sna")? as u8;
            Ok(ForwardAddress { addr_type: t, sna: Some(sna), ssi: None, ext_digits: None })
        }
        ForwardAddressType::Ssi | ForwardAddressType::Tsi => {
            let ssi = buf.read_field(24, "forward_ssi_or_tsi")? as u32;
            Ok(ForwardAddress { addr_type: t, sna: None, ssi: Some(ssi), ext_digits: None })
        }
        ForwardAddressType::ExternalSubscriberNumber => {
            let n = buf.read_field(8, "n_ext_digits")? as u8;
            // Spec says 1..24; we just bound-check
            if n == 0 || n > 24 {
                return Err(PduParseErr::InvalidTrailingMbitValue);
            }
            let digits = parse_external_subscriber_digits(buf, n)?;
            Ok(ForwardAddress { addr_type: t, sna: None, ssi: None, ext_digits: Some(digits) })
        }
        ForwardAddressType::Reserved(_) => Ok(ForwardAddress::none()),
    }
}

impl SdsTlPdu {
    /// Parse an SDS-TL PDU from a BitBuffer containing exactly the User defined data-4 IE.
    pub fn from_ud4(ud4: &mut BitBuffer) -> Result<Self, PduParseErr> {
        // Byte aligned by design
        require_octet_aligned(ud4)?;

        let protocol_id = ud4.read_field(8, "protocol_identifier")? as u8;
        let msg_type_raw = ud4.read_field(4, "message_type")? as u8;
        let msg_type = SdsTlMessageType::from_raw(msg_type_raw);

        match msg_type {
            SdsTlMessageType::SdsTransfer => {
                let delivery_report_request = ud4.read_field(2, "delivery_report_request")? as u8;
                let short_form_report = ud4.read_field(1, "short_form_report")? == 1;
                let storage = ud4.read_field(1, "storage")? == 1;
                let message_reference = ud4.read_field(8, "message_reference")? as u8;

                let (validity_period, forward_address) = if storage {
                    let vp = ud4.read_field(5, "validity_period")? as u8;
                    let fwd = parse_forward_address(ud4)?;
                    (Some(vp), Some(fwd))
                } else {
                    (None, None)
                };

                let user_data = drain_remaining_to_bytes(ud4)?;
                Ok(SdsTlPdu::Transfer(SdsTransfer {
                    protocol_id,
                    delivery_report_request,
                    short_form_report,
                    storage,
                    message_reference,
                    validity_period,
                    forward_address,
                    user_data,
                }))
            }
            SdsTlMessageType::SdsReport => {
                let acknowledgement_required = ud4.read_field(1, "ack_required")? == 1;
                let _reserved2 = ud4.read_field(2, "reserved")? as u8; // default 0
                let storage = ud4.read_field(1, "storage")? == 1;
                let delivery_status = ud4.read_field(8, "delivery_status")? as u8;
                let message_reference = ud4.read_field(8, "message_reference")? as u8;

                let (validity_period, forward_address) = if storage {
                    let vp = ud4.read_field(5, "validity_period")? as u8;
                    let fwd = parse_forward_address(ud4)?;
                    (Some(vp), Some(fwd))
                } else {
                    (None, None)
                };

                // Remaining bits are "User data" (optional, may be 0 length by design)
                let user_data = drain_remaining_to_bytes(ud4)?;

                Ok(SdsTlPdu::Report(SdsReport {
                    protocol_id,
                    acknowledgement_required,
                    storage,
                    delivery_status,
                    message_reference,
                    validity_period,
                    forward_address,
                    user_data,
                }))
            }
            SdsTlMessageType::SdsAck => {
                let _reserved4 = ud4.read_field(4, "reserved")? as u8; // default 0
                let delivery_status = ud4.read_field(8, "delivery_status")? as u8;
                let message_reference = ud4.read_field(8, "message_reference")? as u8;
                // Remaining should be zero/padding
                let _ = drain_remaining_to_bytes(ud4)?;
                Ok(SdsTlPdu::Ack(SdsAck { protocol_id, delivery_status, message_reference }))
            }
            SdsTlMessageType::Other(other) => {
                let payload = drain_remaining_to_bytes(ud4)?;
                Ok(SdsTlPdu::Unknown { protocol_id, msg_type: other, payload })
            }
        }
    }

    /// Serialize back into UD4 bytes.
    /// (This is enough for forwarding: you can parse, maybe tweak fields, and re-emit.)
    pub fn to_ud4_bytes(&self) -> Result<Vec<u8>, PduParseErr> {
        let mut b = BitBuffer::new_autoexpand(256);
        match self {
            SdsTlPdu::Transfer(p) => {
                b.write_bits(p.protocol_id as u64, 8);
                b.write_bits(SdsTlMessageType::SdsTransfer.to_raw() as u64, 4);
                b.write_bits(p.delivery_report_request as u64, 2);
                b.write_bits(if p.short_form_report { 1 } else { 0 }, 1);
                b.write_bits(if p.storage { 1 } else { 0 }, 1);
                b.write_bits(p.message_reference as u64, 8);
                if p.storage {
                    b.write_bits(p.validity_period.unwrap_or(0) as u64, 5);
                    let fwd = p.forward_address.clone().unwrap_or_else(ForwardAddress::none);
                    b.write_bits(fwd.addr_type.to_raw() as u64, 3);
                    match fwd.addr_type {
                        ForwardAddressType::ShortNumberAddress => {
                            b.write_bits(fwd.sna.unwrap_or(0) as u64, 8);
                        }
                        ForwardAddressType::Ssi | ForwardAddressType::Tsi => {
                            b.write_bits(fwd.ssi.unwrap_or(0) as u64, 24);
                        }
                        ForwardAddressType::ExternalSubscriberNumber => {
                            let digits = fwd.ext_digits.unwrap_or_default();
                            let n = digits.len().min(24) as u8;
                            b.write_bits(n as u64, 8);
                            for i in 0..n as usize {
                                b.write_bits((digits[i] & 0xF) as u64, 4);
                            }
                            if (n & 1) == 1 { b.write_bits(0, 4); }
                        }
                        _ => {}
                    }
                }
                // user data bytes
                for byte in &p.user_data { b.write_bits(*byte as u64, 8); }
            }
            SdsTlPdu::Report(p) => {
                b.write_bits(p.protocol_id as u64, 8);
                b.write_bits(SdsTlMessageType::SdsReport.to_raw() as u64, 4);
                b.write_bits(if p.acknowledgement_required { 1 } else { 0 }, 1);
                b.write_bits(0, 2); // reserved
                b.write_bits(if p.storage { 1 } else { 0 }, 1);
                b.write_bits(p.delivery_status as u64, 8);
                b.write_bits(p.message_reference as u64, 8);
                if p.storage {
                    b.write_bits(p.validity_period.unwrap_or(0) as u64, 5);
                    let fwd = p.forward_address.clone().unwrap_or_else(ForwardAddress::none);
                    b.write_bits(fwd.addr_type.to_raw() as u64, 3);
                    match fwd.addr_type {
                        ForwardAddressType::ShortNumberAddress => b.write_bits(fwd.sna.unwrap_or(0) as u64, 8),
                        ForwardAddressType::Ssi | ForwardAddressType::Tsi => b.write_bits(fwd.ssi.unwrap_or(0) as u64, 24),
                        ForwardAddressType::ExternalSubscriberNumber => {
                            let digits = fwd.ext_digits.unwrap_or_default();
                            let n = digits.len().min(24) as u8;
                            b.write_bits(n as u64, 8);
                            for i in 0..n as usize { b.write_bits((digits[i] & 0xF) as u64, 4); }
                            if (n & 1) == 1 { b.write_bits(0, 4); }
                        }
                        _ => {}
                    }
                }
                for byte in &p.user_data { b.write_bits(*byte as u64, 8); }
            }
            SdsTlPdu::Ack(p) => {
                b.write_bits(p.protocol_id as u64, 8);
                b.write_bits(SdsTlMessageType::SdsAck.to_raw() as u64, 4);
                b.write_bits(0, 4); // reserved
                b.write_bits(p.delivery_status as u64, 8);
                b.write_bits(p.message_reference as u64, 8);
            }
            SdsTlPdu::Unknown { protocol_id, msg_type, payload } => {
                b.write_bits(*protocol_id as u64, 8);
                b.write_bits((*msg_type & 0x0F) as u64, 4);
                for byte in payload { b.write_bits(*byte as u64, 8); }
            }
        }
        // pad to octet boundary with 0s if needed
        let rem = b.get_pos() & 7;
        if rem != 0 {
            b.write_bits(0, 8 - rem);
        }
        b.seek(0);
        Ok(drain_remaining_to_bytes(&mut b)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMessageSdu {
    pub timestamp_used: bool,
    pub text_coding_scheme: u8, // 7-bit value (table 447)
    pub timestamp: Option<u32>, // 24-bit (if timestamp_used)
    pub text_bytes: Vec<u8>,
    pub text_latin1: Option<String>,
}

/// ISO-8859-1: bytes map 1:1 to Unicode codepoints U+0000..U+00FF.
fn latin1_bytes_to_string(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// Parse the "Text message transfer SDU" that sits inside SDS-TRANSFER user_data
/// for protocol_id == 130 (0x82). (Table 446)
///
/// Layout (byte-aligned):
///   Octet0: [timestamp_used:1][text_coding_scheme:7]
///   if timestamp_used: next 3 octets = timestamp (24-bit)
///   remaining octets = text (encoded per scheme)
pub fn parse_text_message_sdu(user_data: &[u8]) -> Option<TextMessageSdu> {
    if user_data.is_empty() {
        return None;
    }

    // Table 446: timestamp_used (1 bit) + text coding scheme (7 bits) share an octet.
    // Common convention: MSB is the 1-bit flag, remaining 7 bits are scheme.
    let b0 = user_data[0];
    let timestamp_used = (b0 & 0x80) != 0;
    let text_coding_scheme = b0 & 0x7F;

    let mut idx = 1;

    let timestamp = if timestamp_used {
        if user_data.len() < idx + 3 {
            return None;
        }
        let t = ((user_data[idx] as u32) << 16) | ((user_data[idx + 1] as u32) << 8) | (user_data[idx + 2] as u32);
        idx += 3;
        Some(t)
    } else {
        None
    };

    let text_bytes = user_data[idx..].to_vec();

    // Table 447: scheme=1 => ISO/IEC 8859-1 Latin-1
    let text_latin1 = if text_coding_scheme == 1 {
        Some(latin1_bytes_to_string(&text_bytes))
    } else {
        None
    };

    Some(TextMessageSdu {
        timestamp_used,
        text_coding_scheme,
        timestamp,
        text_bytes,
        text_latin1,
    })
}

impl SdsTlPdu {
    /// If this SDS-TL PDU is a Text Message (protocol_id=130) and uses Latin-1 (scheme=1),
    /// return decoded text.
    pub fn decoded_text_latin1(&self) -> Option<String> {
        const PROTOCOL_ID_TEXT_MESSAGING_SDS_TL: u8 = 130; // table 439

        match self {
            SdsTlPdu::Transfer(t) if t.protocol_id == PROTOCOL_ID_TEXT_MESSAGING_SDS_TL => {
                let tm = parse_text_message_sdu(&t.user_data)?;
                tm.text_latin1
            }
            SdsTlPdu::Report(r) if r.protocol_id == PROTOCOL_ID_TEXT_MESSAGING_SDS_TL => {
                let tm = parse_text_message_sdu(&r.user_data)?;
                tm.text_latin1
            }
            _ => None,
        }
    }
}

