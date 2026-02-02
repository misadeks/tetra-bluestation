//! SDS (Short Data Service) codec utilities.
//!
//! Supports:
//! - ETSI EN 300 392-2: Simple Text Messaging (PID=0x02)
//! - ETSI EN 300 392-2: SDS-TL Text Messaging (PID=0x82, SDS-TRANSFER)
//!
//! This module is intentionally self-contained (pure Rust, no codegen) and unit-testable.

use crate::common::bitbuffer::BitBuffer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextCodingScheme {
    Gsm7 = 0x00,
    Latin1 = 0x01,
    Ucs2 = 0x1A,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedSds {
    SimpleText { tcs: TextCodingScheme, text: String },
    SdsTlText { tcs: TextCodingScheme, text: String, message_reference: u8, delivery_report_request: bool },
    Unknown { pid: u8, payload_bits: usize },
}

/// Maximum allowed SDS Type-4 payload length in bits (11-bit Length Indicator).
/// Values above this cannot be encoded into D-SDS-DATA (length_indicator is 11 bits).
pub const MAX_SDS_TYPE4_BITS: usize = 2047;

/// Many real terminals have a *much smaller* effective limit for SDS text payload
/// (often close to 140 octets for SMS-like storage). The stack allows overriding
/// the per-part payload size via an env var so long texts are split into multiple
/// SDS parts instead of being dropped by terminals.
///
/// Env: TETRA_SDS_MAX_TEXT_PAYLOAD_BYTES (default: 120)
pub fn max_text_payload_bits_configured() -> usize {
    let def_bytes: usize = 120;
    let bytes = std::env::var("TETRA_SDS_MAX_TEXT_PAYLOAD_BYTES")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(def_bytes);
    (bytes * 8).min(MAX_SDS_TYPE4_BITS)
}

pub fn max_text_payload_bytes_configured() -> usize {
    max_text_payload_bits_configured() / 8
}

/// Whether to prefix multi-part SDS text with "(i/n) ".
/// Env: TETRA_SDS_PART_HEADER (default: true)
fn part_header_enabled() -> bool {
    match std::env::var("TETRA_SDS_PART_HEADER") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !(v == "0" || v == "false" || v == "off" || v == "no")
        }
        Err(_) => true,
    }
}

fn mk_part_header(idx_1based: usize, total: usize) -> String {
    format!("({}/{}) ", idx_1based, total)
}

/// Split a Unicode string into chunks such that each chunk, once encoded by `payload_bits_fn`,
/// fits within `max_bits`. This is used to avoid generating SDS Type-4 payloads that exceed
/// the 11-bit Length Indicator limit (2047 bits).
fn split_text_by_payload_limit<F>(text: &str, max_bits: usize, mut payload_bits_fn: F) -> Vec<String>
where
    F: FnMut(&str) -> usize,
{
    if text.is_empty() {
        return vec![String::new()];
    }

    // Map char index -> byte offset for safe UTF-8 slicing.
    let mut starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    starts.push(text.len());

    let mut out: Vec<String> = Vec::new();
    let mut pos: usize = 0;

    while pos < starts.len() - 1 {
        let remaining = (starts.len() - 1) - pos;
        let mut lo: usize = 1;
        let mut hi: usize = remaining;
        let mut best: usize = 0;

        while lo <= hi {
            let mid = (lo + hi) / 2;
            let end_byte = starts[pos + mid];
            let slice = &text[starts[pos]..end_byte];
            let bits = payload_bits_fn(slice);

            if bits <= max_bits {
                best = mid;
                lo = mid + 1;
            } else {
                hi = mid - 1;
            }
        }

        if best == 0 {
            // Should not happen for supported encodings, but avoid infinite loops.
            best = 1;
        }

        let end_byte = starts[pos + best];
        out.push(text[starts[pos]..end_byte].to_string());
        pos += best;
    }

    out
}

/// Split text into SDS parts such that each part, once encoded *including a part header*,
/// fits within `max_bits`.
///
/// The header is only added when multiple parts are needed.
fn split_text_by_payload_limit_with_part_headers<F>(text: &str, max_bits: usize, mut payload_bits_fn: F) -> Vec<String>
where
    F: FnMut(&str) -> usize,
{
    if text.is_empty() {
        return vec![String::new()];
    }

    // Fast path: fits in one message without header.
    if payload_bits_fn(text) <= max_bits {
        return vec![text.to_string()];
    }

    // If headers are disabled, fall back to plain splitting.
    if !part_header_enabled() {
        return split_text_by_payload_limit(text, max_bits, payload_bits_fn);
    }

    // Map char index -> byte offset for safe UTF-8 slicing.
    let mut starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    starts.push(text.len());

    // Iterate until the estimated total stabilizes (header length depends on total digits).
    let mut total_guess: usize = 2;
    for _ in 0..8 {
        let mut out: Vec<String> = Vec::new();
        let mut pos: usize = 0;
        let mut part_idx_1based: usize = 1;

        while pos < starts.len() - 1 {
            let remaining = (starts.len() - 1) - pos;
            let mut lo: usize = 1;
            let mut hi: usize = remaining;
            let mut best: usize = 0;

            while lo <= hi {
                let mid = (lo + hi) / 2;
                let end_byte = starts[pos + mid];
                let slice = &text[starts[pos]..end_byte];
                let candidate = format!("{}{}", mk_part_header(part_idx_1based, total_guess), slice);
                let bits = payload_bits_fn(&candidate);

                if bits <= max_bits {
                    best = mid;
                    lo = mid + 1;
                } else {
                    hi = mid - 1;
                }
            }

            if best == 0 {
                // Prevent infinite loop: force at least 1 char.
                best = 1;
            }
            let end_byte = starts[pos + best];
            let slice = &text[starts[pos]..end_byte];
            out.push(format!("{}{}", mk_part_header(part_idx_1based, total_guess), slice));

            pos += best;
            part_idx_1based += 1;
        }

        let actual = out.len();
        if actual == total_guess {
            // Fix-up: rewrite headers to match actual (in case of digit change).
            let mut fixed: Vec<String> = Vec::with_capacity(out.len());
            for (i, part) in out.into_iter().enumerate() {
                // strip the old header: find first space after ')'
                let after = part.find(") ").map(|p| p + 2).unwrap_or(0);
                let body = &part[after..];
                fixed.push(format!("{}{}", mk_part_header(i + 1, actual), body));
            }
            return fixed;
        }
        total_guess = actual;
    }

    // Fallback if convergence fails.
    split_text_by_payload_limit(text, max_bits, payload_bits_fn)
}

/// Encode Simple Text (PID=0x02) and automatically split into multiple SDS messages if needed.
pub fn encode_simple_text_multi(text: &str, tcs: TextCodingScheme) -> Vec<BitBuffer> {
    let max_bits = max_text_payload_bits_configured();
    let chunks = split_text_by_payload_limit_with_part_headers(text, max_bits, |s| encode_simple_text(s, tcs).get_len());
    chunks.into_iter().map(|s| encode_simple_text(&s, tcs)).collect()
}

/// Encode SDS-TL Text (PID=0x82) and automatically split into multiple SDS messages if needed.
/// Returns (message_reference, payload) pairs.
pub fn encode_sds_tl_text_multi(
    text: &str,
    tcs: TextCodingScheme,
    message_reference: u8,
    delivery_report_request: bool,
) -> Vec<(u8, BitBuffer)> {
    let max_bits = max_text_payload_bits_configured();
    let chunks = split_text_by_payload_limit_with_part_headers(text, max_bits, |s| {
        encode_sds_tl_text(s, tcs, message_reference, delivery_report_request).get_len()
    });

    chunks
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let mr = message_reference.wrapping_add(i as u8);
            (mr, encode_sds_tl_text(&s, tcs, mr, delivery_report_request))
        })
        .collect()
}

pub fn decode_type4_payload(mut payload: BitBuffer) -> Option<DecodedSds> {
    payload.seek(0);
    let pid = payload.read_bits(8)? as u8;
    match pid {
        0x02 => decode_simple_text(pid, payload),
        0x82 => decode_sds_tl_text(pid, payload),
        _ => Some(DecodedSds::Unknown { pid, payload_bits: payload.get_len() }),
    }
}

fn decode_simple_text(_pid: u8, mut payload: BitBuffer) -> Option<DecodedSds> {
    let _reserved = payload.read_bits(1)?;
    let tcs_raw = payload.read_bits(7)? as u8;
    let tcs = tcs_from_raw(tcs_raw)?;
    let text_bits = payload.get_len_remaining();
    let text = decode_text_bits(tcs, &mut payload, text_bits)?;
    Some(DecodedSds::SimpleText { tcs, text })
}

fn decode_sds_tl_text(_pid: u8, mut payload: BitBuffer) -> Option<DecodedSds> {
    // SDS-TL header
    let msg_type = payload.read_bits(4)? as u8;
    if msg_type != 0 {
        return Some(DecodedSds::Unknown { pid: 0x82, payload_bits: payload.get_len() });
    }
    let drr = payload.read_bits(2)? as u8;
    let _service_sel = payload.read_bits(1)?;
    let storage = payload.read_bits(1)? as u8;
    let mr = payload.read_bits(8)? as u8;

    if storage != 0 {
        // storage/forward fields not implemented yet
        return Some(DecodedSds::Unknown { pid: 0x82, payload_bits: payload.get_len() });
    }

    // Text transfer SDU
    let ts_used = payload.read_bits(1)? as u8;
    let tcs_raw = payload.read_bits(7)? as u8;
    let tcs = tcs_from_raw(tcs_raw)?;
    if ts_used != 0 {
        // timestamp 24 bits
        let _ts = payload.read_bits(24)?;
    }
    let text_bits = payload.get_len_remaining();
    let text = decode_text_bits(tcs, &mut payload, text_bits)?;
    Some(DecodedSds::SdsTlText { tcs, text, message_reference: mr, delivery_report_request: drr != 0 })
}

pub fn encode_simple_text(text: &str, tcs: TextCodingScheme) -> BitBuffer {
    let mut payload = BitBuffer::new_autoexpand(2048);
    payload.write_bits(0x02, 8);
    payload.write_bits(0, 1);
    payload.write_bits(tcs as u64, 7);
    encode_text_into(tcs, text, &mut payload);
    payload.seek(0);
    payload
}

pub fn encode_sds_tl_text(text: &str, tcs: TextCodingScheme, message_reference: u8, delivery_report_request: bool) -> BitBuffer {
    let mut payload = BitBuffer::new_autoexpand(2048);
    payload.write_bits(0x82, 8);

    // message type = SDS-TRANSFER (0)
    payload.write_bits(0, 4);

    // delivery report request (2 bits)
    payload.write_bits(if delivery_report_request { 0b01 } else { 0b00 }, 2);

    // service selection (1) - 1 = TETRA network (default)
    payload.write_bits(1, 1);

    // storage (0)
    payload.write_bits(0, 1);

    // message reference
    payload.write_bits(message_reference as u64, 8);

    // Text SDU
    payload.write_bits(0, 1); // timestamp_used
    payload.write_bits(tcs as u64, 7);
    encode_text_into(tcs, text, &mut payload);

    payload.seek(0);
    payload
}

fn tcs_from_raw(v: u8) -> Option<TextCodingScheme> {
    match v {
        0x00 => Some(TextCodingScheme::Gsm7),
        0x01 => Some(TextCodingScheme::Latin1),
        0x1A => Some(TextCodingScheme::Ucs2),
        _ => None,
    }
}

fn decode_text_bits(tcs: TextCodingScheme, buf: &mut BitBuffer, num_bits: usize) -> Option<String> {
    match tcs {
        TextCodingScheme::Gsm7 => {
            let bytes = read_bits_to_bytes(buf, num_bits)?;
            Some(gsm7_unpack_to_string(&bytes))
        }
        TextCodingScheme::Latin1 => {
            let bytes = read_bits_to_bytes(buf, num_bits)?;
            Some(bytes.into_iter().map(|b| b as char).collect())
        }
        TextCodingScheme::Ucs2 => {
            let bytes = read_bits_to_bytes(buf, num_bits)?;
            if bytes.len() % 2 != 0 { return None; }
            let mut out = String::new();
            for ch in bytes.chunks(2) {
                let code = u16::from_be_bytes([ch[0], ch[1]]);
                out.push(char::from_u32(code as u32)?);
            }
            Some(out)
        }
    }
}

fn encode_text_into(tcs: TextCodingScheme, text: &str, out: &mut BitBuffer) {
    match tcs {
        TextCodingScheme::Gsm7 => {
            let packed = gsm7_pack_from_string(text);
            for b in packed {
                out.write_bits(b as u64, 8);
            }
        }
        TextCodingScheme::Latin1 => {
            for ch in text.bytes() {
                out.write_bits(ch as u64, 8);
            }
        }
        TextCodingScheme::Ucs2 => {
            for ch in text.chars() {
                let code = ch as u32;
                let u = if code <= 0xFFFF { code as u16 } else { 0x003F }; // '?'
                let be = u.to_be_bytes();
                out.write_bits(be[0] as u64, 8);
                out.write_bits(be[1] as u64, 8);
            }
        }
    }
}

fn read_bits_to_bytes(buf: &mut BitBuffer, num_bits: usize) -> Option<Vec<u8>> {
    let num_bytes = (num_bits + 7) / 8;
    let mut out = Vec::with_capacity(num_bytes);
    for _ in 0..num_bytes {
        let bits = if buf.get_len_remaining() >= 8 { 8 } else { buf.get_len_remaining() };
        let v = buf.read_bits(bits)? as u8;
        out.push(v);
        if bits < 8 {
            break;
        }
    }
    Some(out)
}

// GSM 7-bit packing/unpacking (basic ASCII subset)
fn gsm7_pack_from_string(s: &str) -> Vec<u8> {
    let septets: Vec<u8> = s.bytes().map(|b| b & 0x7F).collect();
    let mut out: Vec<u8> = Vec::new();
    let mut carry: u16 = 0;
    let mut carry_bits: u8 = 0;

    for septet in septets {
        carry |= (septet as u16) << carry_bits;
        carry_bits += 7;
        while carry_bits >= 8 {
            out.push((carry & 0xFF) as u8);
            carry >>= 8;
            carry_bits -= 8;
        }
    }
    if carry_bits > 0 {
        out.push((carry & 0xFF) as u8);
    }
    out
}

fn gsm7_unpack_to_string(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut acc: u32 = 0;
    let mut acc_bits: u8 = 0;

    for &b in bytes {
        acc |= (b as u32) << acc_bits;
        acc_bits += 8;
        while acc_bits >= 7 {
            let septet = (acc & 0x7F) as u8;
            out.push(septet as char);
            acc >>= 7;
            acc_bits -= 7;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gsm7_roundtrip() {
        let s = "HELLO";
        let packed = gsm7_pack_from_string(s);
        let u = gsm7_unpack_to_string(&packed);
        assert!(u.starts_with("HELLO"));
    }

    #[test]
    fn encode_decode_sds_tl() {
        let p = encode_sds_tl_text("HI", TextCodingScheme::Gsm7, 1, false);
        let dec = decode_type4_payload(p).unwrap();
        match dec {
            DecodedSds::SdsTlText{ text, .. } => assert!(text.starts_with("HI")),
            _ => panic!("unexpected"),
        }
    }
}
