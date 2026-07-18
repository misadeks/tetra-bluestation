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

use tetra_pdus::umac::fields::sysinfo_default_def_for_access_code_a::SysinfoDefaultDefForAccessCodeA;
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
