//! External subscriber number information element (ETSI TS 100 392-2 v3.10.1
//! cl. 14.8.20, Table 14.59).
//!
//! Carries a dialled subscriber number between a TETRA subscriber and a gateway
//! (PABX/PSTN). The number is `n` digits (`n <= 24`), each encoded in 4 bits, in
//! dialled order. It rides the CMCE type-3 `ExtSubscriberNum` element; the digit
//! count is `element length / 4`, so the type-3 wrapper's length field carries
//! `n * 4` bits and this module only produces/consumes the digit payload.

use crate::cmce::enums::type3_elem_id::CmceType3ElemId;
use tetra_core::typed_pdu_fields::Type3FieldGeneric;

/// Maximum digits in an external subscriber number (cl. 14.8.20: `n <= 24`).
pub const MAX_EXTERNAL_SUBSCRIBER_DIGITS: usize = 24;

/// Map a dialled character to its 4-bit code (Table 14.59). `None` for an
/// unsupported character.
pub fn digit_code(c: char) -> Option<u8> {
    Some(match c {
        '0'..='9' => c as u8 - b'0',
        '*' => 0b1010,
        '#' => 0b1011,
        '+' => 0b1100,
        _ => return None,
    })
}

/// Inverse of [`digit_code`]: map a 4-bit code back to its dialled character.
/// `None` for the reserved codes (0b1101..=0b1111).
pub fn code_digit(code: u8) -> Option<char> {
    Some(match code {
        0..=9 => (b'0' + code) as char,
        0b1010 => '*',
        0b1011 => '#',
        0b1100 => '+',
        _ => return None,
    })
}

/// Encode dialled digits into an External subscriber number type-3 element
/// (cl. 14.8.20 / Table 14.59). Whitespace in `digits` is ignored (a display
/// convenience). Returns `None` if any significant character is unsupported or
/// the resulting count is 0 or greater than 24.
pub fn encode(digits: &str) -> Option<Type3FieldGeneric> {
    let codes: Vec<u8> = digits.chars().filter(|c| !c.is_whitespace()).map(digit_code).collect::<Option<_>>()?;
    if codes.is_empty() || codes.len() > MAX_EXTERNAL_SUBSCRIBER_DIGITS {
        return None;
    }
    let len_bits = codes.len() * 4;
    let mut data = vec![0u8; len_bits.div_ceil(8)];
    for (i, code) in codes.iter().enumerate() {
        // Each digit is 4 bits, packed MSB-first (matching how the generic
        // type-3 writer/reader walks the payload bit-by-bit).
        let bit_off = i * 4;
        for b in 0..4 {
            if (code >> (3 - b)) & 1 != 0 {
                let idx = bit_off + b;
                data[idx / 8] |= 1 << (7 - (idx % 8));
            }
        }
    }
    Some(Type3FieldGeneric {
        field_id: CmceType3ElemId::ExtSubscriberNum.into_raw(),
        len: len_bits,
        data,
    })
}

/// Decode an External subscriber number type-3 element back to its dialled
/// digit string (cl. 14.8.20 / Table 14.59). Returns `None` if the element is
/// not an `ExtSubscriberNum`, its length is not a whole number of 4-bit digits,
/// its digit count exceeds 24, or it contains a reserved code.
pub fn decode(field: &Type3FieldGeneric) -> Option<String> {
    if field.field_id != CmceType3ElemId::ExtSubscriberNum.into_raw() {
        return None;
    }
    if field.len == 0 || field.len % 4 != 0 {
        return None;
    }
    let n = field.len / 4;
    if n > MAX_EXTERNAL_SUBSCRIBER_DIGITS {
        return None;
    }
    let mut out = String::with_capacity(n);
    for i in 0..n {
        let bit_off = i * 4;
        let mut code = 0u8;
        for b in 0..4 {
            let idx = bit_off + b;
            let byte = *field.data.get(idx / 8)?;
            let bit = (byte >> (7 - (idx % 8))) & 1;
            code = (code << 1) | bit;
        }
        out.push(code_digit(code)?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_digits_stars_hashes_plus() {
        for s in ["0", "1234567890", "*21#", "+3859123456", "911"] {
            let ie = encode(s).expect("valid digits encode");
            assert_eq!(ie.field_id, CmceType3ElemId::ExtSubscriberNum.into_raw());
            assert_eq!(ie.len, s.chars().count() * 4, "length is digits * 4 bits");
            assert_eq!(decode(&ie).as_deref(), Some(s), "round-trips");
        }
    }

    #[test]
    fn whitespace_is_ignored() {
        let ie = encode("012 345 678").expect("spaced digits encode");
        assert_eq!(decode(&ie).as_deref(), Some("012345678"));
    }

    #[test]
    fn rejects_empty_overlong_and_bad_chars() {
        assert!(encode("").is_none());
        assert!(encode("   ").is_none());
        assert!(encode(&"1".repeat(25)).is_none(), "25 digits exceeds the 24-digit limit");
        assert!(encode(&"9".repeat(24)).is_some(), "24 digits is the maximum");
        assert!(encode("12A4").is_none(), "hex letter is not a dial digit");
        assert!(encode("call").is_none());
    }

    #[test]
    fn decode_rejects_wrong_id_ragged_len_and_reserved() {
        // Wrong element id.
        let mut ie = encode("123").unwrap();
        ie.field_id = CmceType3ElemId::Facility.into_raw();
        assert!(decode(&ie).is_none());
        // Length not a whole number of digits.
        let ragged = Type3FieldGeneric {
            field_id: CmceType3ElemId::ExtSubscriberNum.into_raw(),
            len: 6,
            data: vec![0xFF],
        };
        assert!(decode(&ragged).is_none());
        // Reserved digit code 0b1101.
        let reserved = Type3FieldGeneric {
            field_id: CmceType3ElemId::ExtSubscriberNum.into_raw(),
            len: 4,
            data: vec![0b1101_0000],
        };
        assert!(decode(&reserved).is_none());
    }
}
