//! MS-side random access support (ETSI TS 100 392-2 clause 23.5.1).
//!
//! This module is MS-local: it holds the random access parameters advertised by
//! the serving cell (via ACCESS-DEFINE PDUs, cl. 21.4.4.3, and the SYSINFO
//! "default definition for access code A", cl. 21.4.4.1) and the interpretation
//! of the ACCESS-ASSIGN "Access field" (Table 21.85). It does not touch any BS
//! code path.
//!
//! Later slices build the random access state machine (first try / new access
//! frame / re-try / abandon, cl. 23.5.1.4.5–.9) on top of this state.

use tetra_pdus::umac::enums::access_assign_ul_usage::AccessAssignUlUsage;
use tetra_pdus::umac::fields::sysinfo_default_def_for_access_code_a::SysinfoDefaultDefForAccessCodeA;
use tetra_pdus::umac::pdus::access_assign::{AccessAssign, AccessField};
use tetra_pdus::umac::pdus::access_define::AccessDefine;

/// The four random access codes (ETSI TS 100 392-2 Table 21.85, "Access code").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AccessCode {
    A = 0,
    B = 1,
    C = 2,
    D = 3,
}

impl AccessCode {
    /// Map a raw 2-bit access code value to the enum.
    pub fn from_raw(v: u8) -> Option<Self> {
        match v {
            0 => Some(AccessCode::A),
            1 => Some(AccessCode::B),
            2 => Some(AccessCode::C),
            3 => Some(AccessCode::D),
            _ => None,
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

/// Interpretation of the ACCESS-ASSIGN "Base frame-length" sub-field
/// (ETSI TS 100 392-2 Table 21.85).
///
/// The raw 4-bit value does not map linearly to a subslot count beyond 5
/// subslots, so the mapping is taken verbatim from the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseFrameLength {
    /// `0000` — reserved subslot (access code has no meaning).
    ReservedSubslot,
    /// `0001` — CLCH(-Q) subslot, i.e. subslot 1 available for linearization
    /// only (access code has no meaning).
    ClchSubslot,
    /// `0010` — ongoing frame (no new frame marker here).
    OngoingFrame,
    /// `0011`..=`1111` — a frame marker starting a new access frame whose base
    /// length is the given number of subslots.
    FrameMarker(u16),
}

impl BaseFrameLength {
    /// Decode the 4-bit base frame-length field per Table 21.85.
    pub fn from_raw(v: u8) -> Self {
        match v & 0xF {
            0b0000 => BaseFrameLength::ReservedSubslot,
            0b0001 => BaseFrameLength::ClchSubslot,
            0b0010 => BaseFrameLength::OngoingFrame,
            0b0011 => BaseFrameLength::FrameMarker(1),
            0b0100 => BaseFrameLength::FrameMarker(2),
            0b0101 => BaseFrameLength::FrameMarker(3),
            0b0110 => BaseFrameLength::FrameMarker(4),
            0b0111 => BaseFrameLength::FrameMarker(5),
            0b1000 => BaseFrameLength::FrameMarker(6),
            0b1001 => BaseFrameLength::FrameMarker(8),
            0b1010 => BaseFrameLength::FrameMarker(10),
            0b1011 => BaseFrameLength::FrameMarker(12),
            0b1100 => BaseFrameLength::FrameMarker(16),
            0b1101 => BaseFrameLength::FrameMarker(20),
            0b1110 => BaseFrameLength::FrameMarker(24),
            0b1111 => BaseFrameLength::FrameMarker(32),
            _ => unreachable!(),
        }
    }

    /// True if this field starts a new access frame (a frame marker of ≥ 1
    /// subslots), per cl. 23.5.1.4.6.
    pub fn is_frame_marker(&self) -> bool {
        matches!(self, BaseFrameLength::FrameMarker(_))
    }

    /// The base frame-length in subslots for a frame marker, or `None` for the
    /// special values (reserved / CLCH / ongoing frame).
    pub fn base_subslots(&self) -> Option<u16> {
        match self {
            BaseFrameLength::FrameMarker(n) => Some(*n),
            _ => None,
        }
    }

    /// True if the subslot is reserved (or, for CLCH, unavailable for a normal
    /// access request), so it must not be counted as an access opportunity
    /// (cl. 23.5.1.4.7 point e).
    pub fn is_reserved(&self) -> bool {
        matches!(self, BaseFrameLength::ReservedSubslot | BaseFrameLength::ClchSubslot)
    }
}

/// Random access parameters advertised for one access code (the "ALOHA
/// parameters" of ETSI TS 100 392-2 cl. 23.5.1.4.1). These come either from an
/// ACCESS-DEFINE PDU (cl. 21.4.4.3) or from the SYSINFO default definition for
/// access code A (cl. 21.4.4.1); both carry the same core fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RandomAccessParams {
    /// IMM: `0` = always randomize; `15` = immediate access allowed; otherwise
    /// randomize after up to IMM TDMA frames (cl. 23.5.1.4.5).
    pub imm: u8,
    /// WT: number of downlink signalling opportunities to wait for a response
    /// before assuming failure (cl. 23.5.1.4.8).
    pub wt: u8,
    /// Nu: maximum number of random access transmissions (cl. 23.5.1.4.9).
    /// `Nu == 0` means the access code is not available for use
    /// (cl. 23.5.1.4.1).
    pub nu: u8,
    /// Frame-length factor: if true, multiply the base frame-length by 4
    /// (cl. 23.5.1.4.6).
    pub fl_factor: bool,
    /// Timeslot pointer: which uplink slots are potential access opportunities
    /// (cl. 23.5.1.4.1). `0` means "same as downlink slot assignment".
    pub ts_ptr: u8,
    /// Minimum valid PDU priority for this access code (cl. 23.5.1.4.4 a).
    pub min_pdu_prio: u8,
    /// Optional subscriber class restriction bitmap (cl. 23.5.1.4.4 b); `None`
    /// means no subscriber class restriction.
    pub subscriber_class: Option<u16>,
    /// Optional group address (GSSI) restriction (cl. 23.5.1.4.4 c); `None`
    /// means no address restriction.
    pub gssi: Option<u32>,
}

impl RandomAccessParams {
    /// Build the parameters for access code A from the SYSINFO "default
    /// definition for access code A" (cl. 21.4.4.1 / 23.5.1.4.10). The default
    /// definition carries no subscriber class or address restriction.
    pub fn from_sysinfo_default_a(def: &SysinfoDefaultDefForAccessCodeA) -> Self {
        RandomAccessParams {
            imm: def.imm,
            wt: def.wt,
            nu: def.nu,
            fl_factor: def.fl_factor,
            ts_ptr: def.ts_ptr,
            min_pdu_prio: def.min_pdu_prio,
            subscriber_class: None,
            gssi: None,
        }
    }

    /// Build the parameters from an ACCESS-DEFINE PDU (cl. 21.4.4.3).
    pub fn from_access_define(def: &AccessDefine) -> Self {
        RandomAccessParams {
            imm: def.imm,
            wt: def.wt,
            nu: def.nu,
            fl_factor: def.frame_len_factor,
            ts_ptr: def.ts_pointer,
            min_pdu_prio: def.min_pdu_prio,
            subscriber_class: def.subscriber_class,
            gssi: def.gssi,
        }
    }

    /// Whether this access code is currently available for use. `Nu == 0`
    /// indicates the code is not available (cl. 23.5.1.4.1).
    pub fn is_available(&self) -> bool {
        self.nu != 0
    }
}

/// Store of the serving cell's random access parameters, per access code, with
/// the SYSINFO-vs-ACCESS-DEFINE precedence of cl. 23.5.1.4.10.
///
/// For access code A: the SYSINFO default definition is used until a "common"
/// ACCESS-DEFINE PDU for access code A is received; thereafter ACCESS-DEFINE
/// takes over and SYSINFO defaults are ignored. Access codes B, C, D are only
/// available once their ACCESS-DEFINE PDU has been received.
#[derive(Debug, Default, Clone)]
pub struct AccessParamStore {
    /// ACCESS-DEFINE parameters per access code (index 0..=3 = A..=D).
    defined: [Option<RandomAccessParams>; 4],
    /// SYSINFO default definition for access code A (cl. 21.4.4.1).
    sysinfo_default_a: Option<RandomAccessParams>,
    /// Whether a "common" ACCESS-DEFINE for access code A has been received; it
    /// permanently overrides the SYSINFO default (cl. 23.5.1.4.10).
    access_define_a_received: bool,
}

impl AccessParamStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adopt the SYSINFO default definition for access code A (cl. 21.4.4.1).
    /// Ignored once an ACCESS-DEFINE for code A has been received
    /// (cl. 23.5.1.4.10).
    pub fn update_sysinfo_default_a(&mut self, def: &SysinfoDefaultDefForAccessCodeA) {
        self.sysinfo_default_a = Some(RandomAccessParams::from_sysinfo_default_a(def));
    }

    /// Adopt an ACCESS-DEFINE PDU (cl. 21.4.4.3) for the access code it defines.
    /// A subsequent ACCESS-DEFINE for the same code overwrites the previous
    /// definition (cl. 23.5.1.4.1 NOTE 1).
    pub fn update_access_define(&mut self, def: &AccessDefine) {
        let Some(code) = AccessCode::from_raw(def.access_code) else {
            return;
        };
        if code == AccessCode::A {
            self.access_define_a_received = true;
        }
        self.defined[code.index()] = Some(RandomAccessParams::from_access_define(def));
    }

    /// The currently valid parameters for an access code, applying the
    /// SYSINFO/ACCESS-DEFINE precedence (cl. 23.5.1.4.10). Returns `None` if the
    /// code has no valid definition yet.
    pub fn params_for(&self, code: AccessCode) -> Option<&RandomAccessParams> {
        match code {
            AccessCode::A => {
                if self.access_define_a_received {
                    self.defined[AccessCode::A.index()].as_ref()
                } else {
                    self.sysinfo_default_a.as_ref()
                }
            }
            other => self.defined[other.index()].as_ref(),
        }
    }
}

/// The access opportunity state of one uplink subslot, derived from an
/// ACCESS-ASSIGN Access field (ETSI TS 100 392-2 cl. 21.5.1 / 23.5.1.4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubslotAccess {
    /// The designated access code, if any. Meaningful only for "ongoing frame"
    /// and frame-marker subslots; for reserved/CLCH subslots the access code has
    /// no meaning (Table 21.85 note 1) and this is `None`.
    pub access_code: Option<AccessCode>,
    /// The base frame-length interpretation of this subslot.
    pub frame_len: BaseFrameLength,
}

impl SubslotAccess {
    /// A reserved subslot (not usable for access).
    pub fn reserved() -> Self {
        SubslotAccess {
            access_code: None,
            frame_len: BaseFrameLength::ReservedSubslot,
        }
    }

    /// Whether this subslot is a usable access opportunity for `code`: it must
    /// not be reserved or a CLCH (linearization) subslot, and its designated
    /// access code must match (cl. 23.5.1.4.7 points d and e).
    pub fn is_opportunity_for(&self, code: AccessCode) -> bool {
        !self.frame_len.is_reserved() && self.access_code == Some(code)
    }

    /// Whether this subslot starts a new access frame for `code`, i.e. carries a
    /// frame marker for that access code (cl. 23.5.1.4.6).
    pub fn is_frame_marker_for(&self, code: AccessCode) -> bool {
        self.frame_len.is_frame_marker() && self.access_code == Some(code)
    }
}

/// The access rights conveyed by one ACCESS-ASSIGN PDU to the two subslots of
/// the corresponding uplink slot (ETSI TS 100 392-2 cl. 23.5.1.4.2). The uplink
/// slot is two timeslots after the downlink slot that carried the ACCESS-ASSIGN
/// (cl. 9, note in 23.5.1.4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotAccessAssign {
    pub subslot1: SubslotAccess,
    pub subslot2: SubslotAccess,
}

impl SlotAccessAssign {
    fn both_reserved() -> Self {
        SlotAccessAssign {
            subslot1: SubslotAccess::reserved(),
            subslot2: SubslotAccess::reserved(),
        }
    }
}

fn subslot_from_field(af: &AccessField) -> SubslotAccess {
    SubslotAccess {
        access_code: AccessCode::from_raw(af.access_code),
        frame_len: BaseFrameLength::from_raw(af.base_frame_len),
    }
}

/// Interpret an ACCESS-ASSIGN PDU into the access rights for the two uplink
/// subslots, per ETSI TS 100 392-2 cl. 23.5.1.4.2. `on_common_channel` is true
/// when the MS is camped on its common control channel (currently always true
/// for this MS implementation).
///
/// Rules implemented (cl. 23.5.1.4.2):
/// - An MS on the CCCH treats an "Assigned only" uplink designation as both
///   subslots reserved.
/// - Two access fields: field 1 → subslot 1, field 2 → subslot 2, independently.
/// - A single access field applies to both subslots per points a)–e):
///   reserved → both reserved; CLCH → subslot 1 linearization, subslot 2
///   reserved; ongoing frame → both ongoing; frame marker → subslot 1 marker,
///   subslot 2 ongoing frame.
/// - No access field (e.g. uplink traffic) → both subslots reserved.
pub fn interpret_access_assign(aa: &AccessAssign, on_common_channel: bool) -> SlotAccessAssign {
    // An MS on its common control channel regards an "Assigned only" uplink slot
    // as reserved (cl. 23.5.1.4.2).
    if on_common_channel && aa.ul_usage == AccessAssignUlUsage::AssignedOnly {
        return SlotAccessAssign::both_reserved();
    }

    // Two access fields present: independent rights per subslot (header == 0).
    if let (Some(af1), Some(af2)) = (aa.f1_af1, aa.f2_af2) {
        return SlotAccessAssign {
            subslot1: subslot_from_field(&af1),
            subslot2: subslot_from_field(&af2),
        };
    }

    // A single access field applies to both subslots per points a)–e).
    if let Some(af) = aa.f2_af {
        let code = AccessCode::from_raw(af.access_code);
        let fl = BaseFrameLength::from_raw(af.base_frame_len);
        let (s1, s2) = match fl {
            BaseFrameLength::ReservedSubslot => (SubslotAccess::reserved(), SubslotAccess::reserved()),
            BaseFrameLength::ClchSubslot => (
                SubslotAccess {
                    access_code: None,
                    frame_len: BaseFrameLength::ClchSubslot,
                },
                SubslotAccess::reserved(),
            ),
            BaseFrameLength::OngoingFrame => {
                let s = SubslotAccess {
                    access_code: code,
                    frame_len: BaseFrameLength::OngoingFrame,
                };
                (s, s)
            }
            BaseFrameLength::FrameMarker(_) => (
                SubslotAccess { access_code: code, frame_len: fl },
                SubslotAccess {
                    access_code: code,
                    frame_len: BaseFrameLength::OngoingFrame,
                },
            ),
        };
        return SlotAccessAssign { subslot1: s1, subslot2: s2 };
    }

    // No access field (uplink traffic) → both subslots reserved.
    SlotAccessAssign::both_reserved()
}

#[cfg(test)]
mod tests {
    use tetra_pdus::umac::enums::access_assign_dl_usage::AccessAssignDlUsage;

    use super::*;

    fn af(code: u8, base: u8) -> AccessField {
        AccessField {
            access_code: code,
            base_frame_len: base,
        }
    }

    fn aa_two_fields(af1: AccessField, af2: AccessField) -> AccessAssign {
        AccessAssign {
            _header: 0,
            dl_usage: AccessAssignDlUsage::CommonControl,
            ul_usage: AccessAssignUlUsage::CommonOnly,
            f1_af1: Some(af1),
            f2_af2: Some(af2),
            f2_af: None,
        }
    }

    fn aa_single_field(field: AccessField, ul_usage: AccessAssignUlUsage) -> AccessAssign {
        AccessAssign {
            _header: 1,
            dl_usage: AccessAssignDlUsage::CommonControl,
            ul_usage,
            f1_af1: None,
            f2_af2: None,
            f2_af: Some(field),
        }
    }
    fn sysinfo_def_a() -> SysinfoDefaultDefForAccessCodeA {
        SysinfoDefaultDefForAccessCodeA {
            imm: 3,
            wt: 5,
            nu: 4,
            fl_factor: false,
            ts_ptr: 0,
            min_pdu_prio: 0,
        }
    }

    fn access_define(code: u8, nu: u8, min_prio: u8) -> AccessDefine {
        AccessDefine {
            common_or_assigned_control: false,
            access_code: code,
            imm: 15,
            wt: 6,
            nu,
            frame_len_factor: true,
            ts_pointer: 1,
            min_pdu_prio: min_prio,
            opt_field_flag: 0,
            subscriber_class: None,
            gssi: None,
        }
    }

    #[test]
    fn test_base_frame_length_table() {
        // Table 21.85 verbatim.
        assert_eq!(BaseFrameLength::from_raw(0b0000), BaseFrameLength::ReservedSubslot);
        assert_eq!(BaseFrameLength::from_raw(0b0001), BaseFrameLength::ClchSubslot);
        assert_eq!(BaseFrameLength::from_raw(0b0010), BaseFrameLength::OngoingFrame);
        assert_eq!(BaseFrameLength::from_raw(0b0011), BaseFrameLength::FrameMarker(1));
        assert_eq!(BaseFrameLength::from_raw(0b0111), BaseFrameLength::FrameMarker(5));
        assert_eq!(BaseFrameLength::from_raw(0b1000), BaseFrameLength::FrameMarker(6));
        assert_eq!(BaseFrameLength::from_raw(0b1001), BaseFrameLength::FrameMarker(8));
        assert_eq!(BaseFrameLength::from_raw(0b1111), BaseFrameLength::FrameMarker(32));

        assert!(BaseFrameLength::from_raw(0b0011).is_frame_marker());
        assert!(!BaseFrameLength::from_raw(0b0010).is_frame_marker());
        assert_eq!(BaseFrameLength::from_raw(0b0110).base_subslots(), Some(4));
        assert_eq!(BaseFrameLength::from_raw(0b0010).base_subslots(), None);
        assert!(BaseFrameLength::from_raw(0b0000).is_reserved());
        assert!(BaseFrameLength::from_raw(0b0001).is_reserved());
        assert!(!BaseFrameLength::from_raw(0b0010).is_reserved());
    }

    #[test]
    fn test_sysinfo_default_a_until_access_define() {
        let mut store = AccessParamStore::new();
        // No definition yet.
        assert!(store.params_for(AccessCode::A).is_none());

        // SYSINFO default definition applies to code A (cl. 23.5.1.4.10).
        store.update_sysinfo_default_a(&sysinfo_def_a());
        let p = store.params_for(AccessCode::A).expect("code A available from SYSINFO");
        assert_eq!(p.imm, 3);
        assert_eq!(p.nu, 4);
        assert_eq!(p.subscriber_class, None);

        // A "common" ACCESS-DEFINE for code A overrides the SYSINFO default.
        store.update_access_define(&access_define(0, 7, 2));
        let p = store.params_for(AccessCode::A).expect("code A from ACCESS-DEFINE");
        assert_eq!(p.imm, 15, "ACCESS-DEFINE params now in force");
        assert_eq!(p.nu, 7);
        assert_eq!(p.min_pdu_prio, 2);
        assert!(p.fl_factor);
    }

    #[test]
    fn test_codes_bcd_require_access_define() {
        let mut store = AccessParamStore::new();
        store.update_sysinfo_default_a(&sysinfo_def_a());

        // SYSINFO default only covers code A; B/C/D need ACCESS-DEFINE.
        assert!(store.params_for(AccessCode::B).is_none());
        assert!(store.params_for(AccessCode::C).is_none());
        assert!(store.params_for(AccessCode::D).is_none());

        store.update_access_define(&access_define(1, 3, 0));
        assert!(store.params_for(AccessCode::B).is_some());
        assert!(store.params_for(AccessCode::C).is_none());
    }

    #[test]
    fn test_nu_zero_is_unavailable() {
        let p = RandomAccessParams::from_access_define(&access_define(0, 0, 0));
        assert!(!p.is_available(), "Nu == 0 means access code not available (cl. 23.5.1.4.1)");
        let p = RandomAccessParams::from_access_define(&access_define(0, 1, 0));
        assert!(p.is_available());
    }

    #[test]
    fn test_interpret_two_access_fields() {
        // Header 0: field 1 → subslot 1, field 2 → subslot 2, independently
        // (cl. 23.5.1.4.2). Code B (01), 2-subslot marker in subslot 1; code A
        // (00) ongoing frame in subslot 2.
        let aa = aa_two_fields(af(0b01, 0b0100), af(0b00, 0b0010));
        let s = interpret_access_assign(&aa, true);
        assert_eq!(s.subslot1.access_code, Some(AccessCode::B));
        assert_eq!(s.subslot1.frame_len, BaseFrameLength::FrameMarker(2));
        assert!(s.subslot1.is_frame_marker_for(AccessCode::B));
        assert!(s.subslot1.is_opportunity_for(AccessCode::B));
        assert!(!s.subslot1.is_opportunity_for(AccessCode::A));

        assert_eq!(s.subslot2.access_code, Some(AccessCode::A));
        assert_eq!(s.subslot2.frame_len, BaseFrameLength::OngoingFrame);
        assert!(s.subslot2.is_opportunity_for(AccessCode::A));
        assert!(!s.subslot2.is_frame_marker_for(AccessCode::A));
    }

    #[test]
    fn test_interpret_single_field_reserved_and_clch() {
        // Point b) reserved → both subslots reserved.
        let s = interpret_access_assign(&aa_single_field(af(0, 0b0000), AccessAssignUlUsage::CommonAndAssigned), true);
        assert!(s.subslot1.frame_len.is_reserved());
        assert!(s.subslot2.frame_len.is_reserved());
        assert!(!s.subslot1.is_opportunity_for(AccessCode::A));

        // Point c) CLCH → subslot 1 linearization (not an opportunity), subslot
        // 2 reserved.
        let s = interpret_access_assign(&aa_single_field(af(0, 0b0001), AccessAssignUlUsage::CommonAndAssigned), true);
        assert_eq!(s.subslot1.frame_len, BaseFrameLength::ClchSubslot);
        assert!(!s.subslot1.is_opportunity_for(AccessCode::A));
        assert_eq!(s.subslot2.frame_len, BaseFrameLength::ReservedSubslot);
    }

    #[test]
    fn test_interpret_single_field_marker_and_ongoing() {
        // Point e) frame marker → subslot 1 marker, subslot 2 ongoing frame,
        // both for the same access code (code C = 10, 3-subslot marker = 0101).
        let s = interpret_access_assign(&aa_single_field(af(0b10, 0b0101), AccessAssignUlUsage::CommonAndAssigned), true);
        assert_eq!(s.subslot1.access_code, Some(AccessCode::C));
        assert_eq!(s.subslot1.frame_len, BaseFrameLength::FrameMarker(3));
        assert!(s.subslot1.is_frame_marker_for(AccessCode::C));
        assert_eq!(s.subslot2.access_code, Some(AccessCode::C));
        assert_eq!(s.subslot2.frame_len, BaseFrameLength::OngoingFrame);
        assert!(s.subslot2.is_opportunity_for(AccessCode::C));
        assert!(!s.subslot2.is_frame_marker_for(AccessCode::C));

        // Point d) ongoing frame → both subslots ongoing for the same code.
        let s = interpret_access_assign(&aa_single_field(af(0b00, 0b0010), AccessAssignUlUsage::CommonAndAssigned), true);
        assert!(s.subslot1.is_opportunity_for(AccessCode::A));
        assert!(s.subslot2.is_opportunity_for(AccessCode::A));
    }

    #[test]
    fn test_interpret_assigned_only_on_common_is_reserved() {
        // An MS on the CCCH treats "Assigned only" as both subslots reserved
        // (cl. 23.5.1.4.2), even though the field would otherwise be usable.
        let s = interpret_access_assign(&aa_single_field(af(0b00, 0b0100), AccessAssignUlUsage::AssignedOnly), true);
        assert!(s.subslot1.frame_len.is_reserved());
        assert!(s.subslot2.frame_len.is_reserved());
    }
}
