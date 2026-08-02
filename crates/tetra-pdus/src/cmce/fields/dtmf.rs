//! DTMF information element (ETSI TS 100 392-2 v3.10.1 cl. 14.8.19 / Tables
//! 14.56, 14.57, 14.58).
//!
//! Carries in-call DTMF signalling in the CMCE type-3 `Dtmf` element (U-INFO /
//! D-INFO). Layout of the "new" (v2+) mechanism:
//!   - a 3-bit **DTMF type** (Table 14.58): `000` tone start, `001` tone end,
//!     `010` not supported, `011` not subscribed, `100..111` reserved;
//!   - when the type is tone-start (`000`), `n` **DTMF digits** follow, each
//!     4 bits (Table 14.57: `0-9`, `*`=1010, `#`=1011, `A`=1100 .. `D`=1111),
//!     `n <= 254`. The digit count is `(element length - 3) / 4`.
//!
//! The total element length being **not** divisible by 4 is what distinguishes
//! this mechanism from the edition-1 (digits-only) form, so tone-start with `n`
//! digits is `3 + 4n` bits (never a multiple of 4).

use crate::cmce::enums::type3_elem_id::CmceType3ElemId;
use tetra_core::typed_pdu_fields::Type3FieldGeneric;

/// Maximum DTMF digits per element (cl. 14.8.19: `n <= 254`).
pub const MAX_DTMF_DIGITS: usize = 254;

/// DTMF type codes (Table 14.58).
pub const DTMF_TYPE_TONE_START: u8 = 0b000;
pub const DTMF_TYPE_TONE_END: u8 = 0b001;

/// Map a DTMF digit character to its 4-bit code (Table 14.57). `None` for an
/// unsupported character.
pub fn digit_code(c: char) -> Option<u8> {
    Some(match c {
        '0'..='9' => c as u8 - b'0',
        '*' => 0b1010,
        '#' => 0b1011,
        'A' => 0b1100,
        'B' => 0b1101,
        'C' => 0b1110,
        'D' => 0b1111,
        _ => return None,
    })
}

/// Inverse of [`digit_code`]: map a 4-bit code back to its digit character.
pub fn code_digit(code: u8) -> Option<char> {
    Some(match code {
        0..=9 => (b'0' + code) as char,
        0b1010 => '*',
        0b1011 => '#',
        0b1100 => 'A',
        0b1101 => 'B',
        0b1110 => 'C',
        0b1111 => 'D',
        _ => return None,
    })
}

/// A decoded DTMF element: the 3-bit type and, for a tone-start, its digit
/// nibbles (each `0..=15`, decode with [`code_digit`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DtmfIe {
    pub dtmf_type: u8,
    pub nibbles: Vec<u8>,
}

fn write_bits_msb_first(data: &mut [u8], bit_off: usize, value: u8, num_bits: usize) {
    for b in 0..num_bits {
        if (value >> (num_bits - 1 - b)) & 1 != 0 {
            let idx = bit_off + b;
            data[idx / 8] |= 1 << (7 - (idx % 8));
        }
    }
}

/// Encode a **tone-start** DTMF element (type `000`) carrying `nibbles`
/// (each digit `0..=15`, per Table 14.57). Returns `None` if `nibbles` is empty,
/// longer than 254, or contains a value `> 15`.
pub fn encode_tone_start(nibbles: &[u8]) -> Option<Type3FieldGeneric> {
    if nibbles.is_empty() || nibbles.len() > MAX_DTMF_DIGITS || nibbles.iter().any(|n| *n > 0x0F) {
        return None;
    }
    let len_bits = 3 + 4 * nibbles.len();
    let mut data = vec![0u8; len_bits.div_ceil(8)];
    write_bits_msb_first(&mut data, 0, DTMF_TYPE_TONE_START, 3);
    for (i, nib) in nibbles.iter().enumerate() {
        write_bits_msb_first(&mut data, 3 + 4 * i, *nib, 4);
    }
    Some(Type3FieldGeneric {
        field_id: CmceType3ElemId::Dtmf.into_raw(),
        len: len_bits,
        data,
    })
}

/// Encode a **tone-end** DTMF element (type `001`, no digits): a 3-bit element.
pub fn encode_tone_end() -> Type3FieldGeneric {
    let mut data = vec![0u8; 1];
    write_bits_msb_first(&mut data, 0, DTMF_TYPE_TONE_END, 3);
    Type3FieldGeneric {
        field_id: CmceType3ElemId::Dtmf.into_raw(),
        len: 3,
        data,
    }
}

/// Encode a tone-start element from digit characters (Table 14.57). Convenience
/// over [`encode_tone_start`]; whitespace is ignored. `None` on an unsupported
/// character or an empty/over-long sequence.
pub fn encode_tone_start_digits(digits: &str) -> Option<Type3FieldGeneric> {
    let nibbles: Vec<u8> = digits.chars().filter(|c| !c.is_whitespace()).map(digit_code).collect::<Option<_>>()?;
    encode_tone_start(&nibbles)
}

fn read_bits_msb_first(field: &Type3FieldGeneric, start_bit: usize, num_bits: usize) -> Option<u8> {
    if start_bit + num_bits > field.len {
        return None;
    }
    let mut value = 0u8;
    for i in 0..num_bits {
        let idx = start_bit + i;
        let byte = *field.data.get(idx / 8)?;
        let bit = (byte >> (7 - (idx % 8))) & 1;
        value = (value << 1) | bit;
    }
    Some(value)
}

/// Decode a DTMF element (new mechanism, cl. 14.8.19). Returns the 3-bit type
/// and, for a tone-start, its digit nibbles. Returns `None` if the element is
/// not a `Dtmf` field, is shorter than the 3-bit type, or is a tone-start whose
/// digit region is not a whole number of 4-bit digits (or exceeds 254 digits).
pub fn decode(field: &Type3FieldGeneric) -> Option<DtmfIe> {
    if field.field_id != CmceType3ElemId::Dtmf.into_raw() || field.len < 3 {
        return None;
    }
    let dtmf_type = read_bits_msb_first(field, 0, 3)?;
    let mut nibbles = Vec::new();
    if dtmf_type == DTMF_TYPE_TONE_START {
        let tail = field.len - 3;
        if tail == 0 || tail % 4 != 0 || tail / 4 > MAX_DTMF_DIGITS {
            return None;
        }
        for i in 0..tail / 4 {
            nibbles.push(read_bits_msb_first(field, 3 + 4 * i, 4)?);
        }
    }
    Some(DtmfIe { dtmf_type, nibbles })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_start_round_trips_all_digit_symbols() {
        for s in ["0", "1234567890", "*0#", "ABCD", "12*34#"] {
            let ie = encode_tone_start_digits(s).expect("valid digits");
            assert_eq!(ie.field_id, CmceType3ElemId::Dtmf.into_raw());
            assert_eq!(ie.len, 3 + 4 * s.chars().count(), "len = 3 + 4n and not a multiple of 4");
            assert_ne!(ie.len % 4, 0, "new mechanism length must not be divisible by 4");
            let decoded = decode(&ie).expect("decodes");
            assert_eq!(decoded.dtmf_type, DTMF_TYPE_TONE_START);
            let back: String = decoded.nibbles.iter().map(|n| code_digit(*n).unwrap()).collect();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn tone_end_is_three_bits_no_digits() {
        let ie = encode_tone_end();
        assert_eq!(ie.len, 3);
        let decoded = decode(&ie).expect("decodes");
        assert_eq!(decoded.dtmf_type, DTMF_TYPE_TONE_END);
        assert!(decoded.nibbles.is_empty());
    }

    #[test]
    fn encode_rejects_empty_overlong_and_bad_nibble() {
        assert!(encode_tone_start(&[]).is_none());
        assert!(encode_tone_start(&vec![1u8; MAX_DTMF_DIGITS + 1]).is_none());
        assert!(encode_tone_start(&[16]).is_none(), "nibble > 15 invalid");
        assert!(encode_tone_start_digits("12E4").is_none(), "'E' is not a DTMF digit");
        assert!(encode_tone_start(&vec![9u8; MAX_DTMF_DIGITS]).is_some(), "254 digits is the maximum");
    }

    #[test]
    fn decode_rejects_wrong_id_and_ragged_tone_start() {
        let mut ie = encode_tone_start_digits("12").unwrap();
        ie.field_id = CmceType3ElemId::Facility.into_raw();
        assert!(decode(&ie).is_none());
        // Tone-start type but a tail that is not a whole number of digits.
        let ragged = Type3FieldGeneric {
            field_id: CmceType3ElemId::Dtmf.into_raw(),
            len: 3 + 2, // 2 tail bits — not a 4-bit digit
            data: vec![0x00],
        };
        assert!(decode(&ragged).is_none());
    }
}
