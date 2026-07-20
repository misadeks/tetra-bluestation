use core::fmt;

use tetra_core::{BitBuffer, pdu_parse_error::PduParseErr};

use crate::umac::{enums::access_assign_ul_usage::AccessAssignUlUsage, pdus::access_assign::AccessField};

/// Clause 21.4.7.2 ACCESS-ASSIGN
/// TODO FIXME technically not part of this SAP, but part of the MAC
#[derive(Debug)]
pub struct AccessAssignFr18 {
    // 2, kept for debugging purposes
    pub _header: u8,
    // 6
    // pub dl_usage: AccessAssignDlUsage,
    pub ul_usage: AccessAssignUlUsage,

    /// Populated when header == 0, 1 or 2
    /// Provides access rights on UL subslot 1
    pub f1_af1: Option<AccessField>,

    /// Populated when header == 3
    pub f1_traf_um: Option<AccessAssignUlUsage>,

    /// Populated when header == 0, 1 or 2
    /// Provides access rights on UL subslot 2
    pub f2_af2: Option<AccessField>,

    /// Populated when header == 3
    /// Provides access rights on both UL subslots
    pub f2_af: Option<AccessField>,
    // pub f2_ul_um: Option<AccessAssignUlUsage>,
}

impl Default for AccessAssignFr18 {
    fn default() -> Self {
        AccessAssignFr18 {
            _header: 0,

            ul_usage: AccessAssignUlUsage::CommonOnly,

            f1_af1: None,
            f1_traf_um: None,
            f2_af2: None,
            f2_af: None,
        }
    }
}

impl AccessAssignFr18 {
    pub fn from_bitbuf(buf: &mut BitBuffer) -> Result<Self, PduParseErr> {
        let mut s = AccessAssignFr18 {
            _header: buf.read_field(2, "_header")? as u8,
            ..Default::default()
        };

        let field1 = buf.read_field(6, "field1")? as u8;
        let field2 = buf.read_field(6, "field2")? as u8;

        match s._header {
            0 => {
                s.ul_usage = AccessAssignUlUsage::CommonOnly;
                s.f1_af1 = Some(AccessField {
                    access_code: (field1 >> 4) & 0x3,
                    base_frame_len: field1 & 0xF,
                });
                s.f2_af2 = Some(AccessField {
                    access_code: (field2 >> 4) & 0x3,
                    base_frame_len: field2 & 0xF,
                });
            }
            1 => {
                s.ul_usage = AccessAssignUlUsage::CommonAndAssigned;
                s.f1_af1 = Some(AccessField {
                    access_code: (field1 >> 4) & 0x3,
                    base_frame_len: field1 & 0xF,
                });
                s.f2_af2 = Some(AccessField {
                    access_code: (field2 >> 4) & 0x3,
                    base_frame_len: field2 & 0xF,
                });
            }
            2 => {
                s.ul_usage = AccessAssignUlUsage::AssignedOnly;
                s.f1_af1 = Some(AccessField {
                    access_code: (field1 >> 4) & 0x3,
                    base_frame_len: field1 & 0xF,
                });
                s.f2_af2 = Some(AccessField {
                    access_code: (field2 >> 4) & 0x3,
                    base_frame_len: field2 & 0xF,
                });
            }
            3 => {
                // Frame 18, header 112 (Table 21.82): UL access rights "common
                // and assigned", Field 1 is a Traffic usage marker (UMt) and
                // Field 2 is an access field. Per Table 21.81 the pre-set markers
                // UMr/UMc/UMa/UMx (values 0..=3) are EXCLUDED as traffic usage
                // markers, so any Field 1 < 4 here is a malformed / reserved
                // ACCESS-ASSIGN. Reject it as a parse error so the caller can
                // discard the AACH and treat it as not received (cl. 21.4.7.1) —
                // a receive-path parser must never panic on received bits.
                s.ul_usage = AccessAssignUlUsage::from_usage_marker(field1)
                    .filter(AccessAssignUlUsage::is_traffic)
                    .ok_or(PduParseErr::InvalidValue {
                        field: "ul_usage",
                        value: field1 as u64,
                    })?;

                s.f2_af = Some(AccessField {
                    access_code: (field2 >> 4) & 0x3,
                    base_frame_len: field2 & 0xF,
                });
            }
            _ => {
                panic!()
            }
        }

        Ok(s)
    }

    pub fn to_bitbuf(&self, buf: &mut BitBuffer) {
        if self.ul_usage == AccessAssignUlUsage::CommonOnly {
            let header = 0;
            buf.write_bits(header as u64, 2);
            buf.write_bits(self.f1_af1.as_ref().unwrap().access_code as u64, 2);
            buf.write_bits(self.f1_af1.as_ref().unwrap().base_frame_len as u64, 4);
            buf.write_bits(self.f2_af2.as_ref().unwrap().access_code as u64, 2);
            buf.write_bits(self.f2_af2.as_ref().unwrap().base_frame_len as u64, 4);
            assert!(self.f2_af.is_none());
        } else if self.ul_usage == AccessAssignUlUsage::CommonAndAssigned {
            let header = 1;
            buf.write_bits(header as u64, 2);
            buf.write_bits(self.f1_af1.as_ref().unwrap().access_code as u64, 2);
            buf.write_bits(self.f1_af1.as_ref().unwrap().base_frame_len as u64, 4);
            buf.write_bits(self.f2_af2.as_ref().unwrap().access_code as u64, 2);
            buf.write_bits(self.f2_af2.as_ref().unwrap().base_frame_len as u64, 4);
            assert!(self.f2_af.is_none());
        } else if self.ul_usage == AccessAssignUlUsage::AssignedOnly {
            let header = 2;
            buf.write_bits(header as u64, 2);
            buf.write_bits(self.f1_af1.as_ref().unwrap().access_code as u64, 2);
            buf.write_bits(self.f1_af1.as_ref().unwrap().base_frame_len as u64, 4);
            buf.write_bits(self.f2_af2.as_ref().unwrap().access_code as u64, 2);
            buf.write_bits(self.f2_af2.as_ref().unwrap().base_frame_len as u64, 4);
            assert!(self.f2_af.is_none());
        } else if self.ul_usage.is_traffic() {
            // UL usage counts as common and assigned, but with traffic marker
            let header = 3;
            buf.write_bits(header as u64, 2);
            let ul_usage = self.ul_usage.to_usage_marker().unwrap();
            buf.write_bits(ul_usage as u64, 6);
            buf.write_bits(self.f2_af.as_ref().unwrap().access_code as u64, 2);
            buf.write_bits(self.f2_af.as_ref().unwrap().base_frame_len as u64, 4);
            assert!(self.f1_af1.is_none());
            assert!(self.f2_af2.is_none());
        } else {
            unimplemented!("AccessAssign::to_bitbuf_fr18 for other cases");
        }
    }
}

impl fmt::Display for AccessAssignFr18 {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "access_assign {{ ul_usage: {}", self.ul_usage)?;
        if let Some(af) = &self.f2_af {
            write!(f, "  AF {}/{}", af.access_code, af.base_frame_len)?;
        };
        if let Some(af) = &self.f1_af1 {
            write!(f, "  AF1 {}/{}", af.access_code, af.base_frame_len)?;
        };
        if let Some(af) = &self.f2_af2 {
            write!(f, "  AF2 {}/{}", af.access_code, af.base_frame_len)?;
        };
        write!(f, " }}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame-18 ACCESS-ASSIGN with header 112 but Field 1 = 0 (UMx) is a
    /// malformed / reserved PDU (Table 21.82 requires a Traffic usage marker
    /// UMt, and Table 21.81 excludes values 0..=3). The parser must reject it
    /// with a parse error, NOT panic — this is exactly the on-air `11 000000
    /// 000000` AACH that previously crashed the whole MS stack.
    #[test]
    fn header3_non_traffic_marker_is_rejected_not_panicked() {
        for field1 in 0u8..=3 {
            let mut buf = BitBuffer::new_autoexpand(2);
            buf.write_bits(0b11, 2); // header
            buf.write_bits(field1 as u64, 6); // field 1 (usage marker)
            buf.write_bits(0, 6); // field 2
            buf.seek(0);
            let res = AccessAssignFr18::from_bitbuf(&mut buf);
            assert!(
                matches!(res, Err(PduParseErr::InvalidValue { field: "ul_usage", .. })),
                "field1={field1} must be rejected as an invalid UL usage marker, got {res:?}"
            );
        }
    }

    /// A frame-18 ACCESS-ASSIGN with header 112 and a valid Traffic usage marker
    /// (UMt, value >= 4) parses to a Traffic ul_usage plus the access field.
    #[test]
    fn header3_traffic_marker_parses() {
        let mut buf = BitBuffer::new_autoexpand(2);
        buf.write_bits(0b11, 2); // header
        buf.write_bits(4, 6); // field 1 = UMt(4)
        buf.write_bits(0b010011, 6); // field 2 = access field (code 1, len 3)
        buf.seek(0);
        let pdu = AccessAssignFr18::from_bitbuf(&mut buf).expect("valid traffic marker parses");
        assert_eq!(pdu.ul_usage, AccessAssignUlUsage::Traffic(4));
        let af = pdu.f2_af.expect("access field present");
        assert_eq!(af.access_code, 1);
        assert_eq!(af.base_frame_len, 3);
        assert!(pdu.f1_af1.is_none());
    }
}

