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

use tetra_core::TdmaTime;
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

/// Which of the two subslots of an uplink slot to transmit in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subslot {
    One,
    Two,
}

/// Source of the random choices required by the random access algorithm
/// (ETSI TS 100 392-2 cl. 23.5.1.4.5 and .4.6). Injected so the state machine is
/// deterministically testable.
pub trait RaRng {
    /// Choose a subslot index uniformly in `1..=frame_length`
    /// (cl. 23.5.1.4.6). `frame_length` is always ≥ 1.
    fn choose_subslot_index(&mut self, frame_length: u16) -> u16;
    /// Choose one of the two subslots when both are valid access opportunities
    /// in the first-try slot (cl. 23.5.1.4.5).
    fn choose_one_of_two(&mut self) -> Subslot;
}

/// Default [`RaRng`] backed by the `rand` crate (matches the repo's existing
/// `rand::random_range` usage).
pub struct ThreadRaRng;

impl RaRng for ThreadRaRng {
    fn choose_subslot_index(&mut self, frame_length: u16) -> u16 {
        rand::random_range(1..=frame_length)
    }
    fn choose_one_of_two(&mut self) -> Subslot {
        if rand::random_range(0..2) == 0 {
            Subslot::One
        } else {
            Subslot::Two
        }
    }
}

/// Reason a random access attempt was abandoned (ETSI TS 100 392-2
/// cl. 23.5.1.4.9), reported back to the requesting layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaFailure {
    /// The access code has no valid parameters or is not available (Nu == 0)
    /// (cl. 23.5.1.4.1).
    NoValidAccessCode,
    /// The PDU is not permitted to use this access code (cl. 23.5.1.4.4).
    AccessCodeNotPermitted,
    /// The maximum number of transmissions (Nu, or 2·Nu for emergency) was
    /// reached without a response (cl. 23.5.1.4.9).
    MaxTransmissions,
}

/// An action the random access state machine asks its owner (UMAC) to perform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RaAction {
    /// Transmit the pending MAC-ACCESS in the given uplink slot/subslot. The
    /// uplink slot is two timeslots after the downlink slot that produced this
    /// action (cl. 9, note in 23.5.1.4.2).
    Transmit { ul_time: TdmaTime, subslot: Subslot },
    /// The attempt succeeded: a matching response was received (cl. 23.5.1.4.8).
    Succeeded,
    /// The attempt was abandoned (cl. 23.5.1.4.9).
    Failed(RaFailure),
}

/// Internal state of the random access attempt (ETSI TS 100 392-2
/// cl. 23.5.1.4.5–.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RaState {
    /// No attempt in progress.
    Idle,
    /// First-try window: transmit in the first valid access opportunity subject
    /// to the IMM rule (cl. 23.5.1.4.5). `init_frame` is the absolute TDMA frame
    /// index at which the attempt was initiated.
    FirstTry { init_frame: i32 },
    /// Waiting for the next frame marker for the access code to start a new
    /// access frame (cl. 23.5.1.4.6).
    AwaitFrameMarker,
    /// Counting access opportunities within the chosen access frame toward the
    /// randomly chosen subslot `chosen` (cl. 23.5.1.4.6/.4.7). `counted` is the
    /// number of qualifying subslots seen so far.
    Counting { chosen: u16, counted: u16 },
    /// Transmitted; waiting up to WT downlink opportunities for a response
    /// (cl. 23.5.1.4.8). `wt_seen` counts elapsed downlink opportunities and
    /// `last_frame` is the last frame index counted (one opportunity per frame).
    AwaitResponse { wt_seen: u8, last_frame: i32 },
}

/// MS random access state machine (ETSI TS 100 392-2 cl. 23.5.1.4).
///
/// The state machine is MS-local and pure: it is driven per received downlink
/// slot via [`MsRandomAccess::poll_downlink_slot`] with the interpreted
/// ACCESS-ASSIGN for that slot, and informed of responses via
/// [`MsRandomAccess::on_mac_resource`]. It never transmits itself; it returns
/// [`RaAction`]s for the owner (UMAC) to carry out.
///
/// One attempt is handled at a time (cl. 23.5.1.4.3): a new attempt is only
/// started via [`MsRandomAccess::initiate`] while [`RaState::Idle`].
#[derive(Debug, Clone)]
pub struct MsRandomAccess {
    state: RaState,
    /// The access code chosen for the current attempt.
    code: AccessCode,
    /// Whether the pending PDU is an emergency (priority 7), which relaxes the
    /// first-try and doubles the transmission limit (cl. 23.5.1.4.4/.4.5/.4.9).
    is_emergency: bool,
    /// Number of random access transmissions performed in this attempt
    /// (cl. 23.5.1.4.9).
    tx_count: u8,
}

impl Default for MsRandomAccess {
    fn default() -> Self {
        MsRandomAccess {
            state: RaState::Idle,
            code: AccessCode::A,
            is_emergency: false,
            tx_count: 0,
        }
    }
}

/// The absolute TDMA frame index of a time (monotonic, 4 timeslots per frame).
fn frame_index(t: TdmaTime) -> i32 {
    t.to_int().div_euclid(4)
}

/// The access frame length in subslots for a frame marker, applying the
/// frame-length factor (cl. 23.5.1.4.6): base × 4 if the factor is set, else
/// base × 1.
fn frame_length(base_subslots: u16, fl_factor: bool) -> u16 {
    base_subslots.saturating_mul(if fl_factor { 4 } else { 1 }).max(1)
}

impl MsRandomAccess {
    pub fn new() -> Self {
        Self::default()
    }

    /// True while a random access attempt is in progress (cl. 23.5.1.4.3: only
    /// one attempt at a time).
    pub fn is_active(&self) -> bool {
        self.state != RaState::Idle
    }

    /// The access code of the in-progress attempt (for response matching).
    pub fn active_code(&self) -> Option<AccessCode> {
        if self.is_active() {
            Some(self.code)
        } else {
            None
        }
    }

    /// Initiate a random access attempt for a pending MAC-ACCESS PDU
    /// (cl. 23.5.1.4.3). `now` is the current downlink time, `params` the access
    /// parameters in force for `code` (cl. 23.5.1.4.10), `pdu_prio` the PDU
    /// priority (0..=7), and `is_emergency` whether it is an emergency PDU
    /// (priority 7, cl. 23.5.1.4.4/.4.5).
    ///
    /// Performs the access-code permission checks of cl. 23.5.1.4.4 (PDU
    /// priority ≥ minimum, subject to the emergency exception) and returns
    /// `Err(RaFailure)` if the code is unavailable or not permitted. On success
    /// the attempt is armed and subsequent `poll_downlink_slot` calls drive it.
    pub fn initiate(
        &mut self,
        now: TdmaTime,
        code: AccessCode,
        params: &RandomAccessParams,
        pdu_prio: u8,
        is_emergency: bool,
    ) -> Result<(), RaFailure> {
        // cl. 23.5.1.4.1: Nu == 0 → access code not available.
        if !params.is_available() {
            return Err(RaFailure::NoValidAccessCode);
        }
        // cl. 23.5.1.4.4: an emergency PDU may use access code A without the
        // priority check; otherwise the PDU priority must meet the minimum.
        let emergency_code_a = is_emergency && code == AccessCode::A;
        if !emergency_code_a && pdu_prio < params.min_pdu_prio {
            return Err(RaFailure::AccessCodeNotPermitted);
        }

        self.code = code;
        self.is_emergency = is_emergency;
        self.tx_count = 0;
        // cl. 23.5.1.4.5: IMM == 0 always randomizes (go straight to a new
        // access frame); otherwise start the first-try window.
        self.state = if params.imm == 0 && !is_emergency {
            RaState::AwaitFrameMarker
        } else {
            RaState::FirstTry {
                init_frame: frame_index(now),
            }
        };
        Ok(())
    }

    /// The maximum number of random access transmissions permitted for the
    /// current attempt: Nu, doubled to 2·Nu for an emergency PDU
    /// (cl. 23.5.1.4.9).
    fn max_transmissions(&self, params: &RandomAccessParams) -> u32 {
        let nu = params.nu as u32;
        if self.is_emergency {
            nu.saturating_mul(2)
        } else {
            nu
        }
    }

    /// Arm the wait-for-response state after transmitting (cl. 23.5.1.4.8) and
    /// return the transmit action for the given downlink slot.
    fn transmit(&mut self, dltime: TdmaTime, subslot: Subslot) -> RaAction {
        self.tx_count += 1;
        self.state = RaState::AwaitResponse {
            wt_seen: 0,
            last_frame: frame_index(dltime),
        };
        RaAction::Transmit {
            // The uplink slot is two timeslots after the downlink slot that
            // conveyed the ACCESS-ASSIGN (cl. 9, note in 23.5.1.4.2).
            ul_time: dltime.add_timeslots(2),
            subslot,
        }
    }

    /// Begin a fresh transmission (re-try, cl. 23.5.1.4.8) if the maximum
    /// transmission count has not been reached, otherwise abandon
    /// (cl. 23.5.1.4.9). Returns `Some(Failed)` when abandoning.
    fn retry_or_abandon(&mut self, params: &RandomAccessParams) -> Option<RaAction> {
        if self.tx_count as u32 >= self.max_transmissions(params) {
            self.state = RaState::Idle;
            Some(RaAction::Failed(RaFailure::MaxTransmissions))
        } else {
            // cl. 23.5.1.4.8: with no response, choose a new access frame and
            // retransmit.
            self.state = RaState::AwaitFrameMarker;
            None
        }
    }

    /// Drive the state machine with one received downlink slot
    /// (cl. 23.5.1.4.5–.8).
    ///
    /// - `dltime` is the downlink slot time.
    /// - `assign` is the interpreted ACCESS-ASSIGN for this slot (rights to the
    ///   uplink slot two timeslots later).
    /// - `ul_slot_valid` is true when this slot's corresponding uplink slot is a
    ///   valid access-opportunity timeslot per the timeslot pointer
    ///   (cl. 23.5.1.4.7 point a); the caller derives it from `params.ts_ptr`.
    /// - `params` are the access parameters in force for the code
    ///   (cl. 23.5.1.4.10); they may change between calls.
    /// - `rng` supplies the required random choices.
    ///
    /// Returns an [`RaAction`] to perform (transmit / abandon), or `None`.
    pub fn poll_downlink_slot(
        &mut self,
        dltime: TdmaTime,
        assign: &SlotAccessAssign,
        ul_slot_valid: bool,
        params: &RandomAccessParams,
        rng: &mut dyn RaRng,
    ) -> Option<RaAction> {
        match self.state {
            RaState::Idle => None,

            RaState::AwaitResponse { wt_seen, last_frame } => {
                // cl. 23.5.1.4.8: wait WT downlink opportunities for a response,
                // counting at most one opportunity per TDMA frame.
                let frame = frame_index(dltime);
                if frame != last_frame {
                    let wt_seen = wt_seen + 1;
                    if wt_seen >= params.wt {
                        // No response within WT → retransmit or abandon.
                        return self.retry_or_abandon(params);
                    }
                    self.state = RaState::AwaitResponse { wt_seen, last_frame: frame };
                }
                None
            }

            RaState::FirstTry { init_frame } => {
                // cl. 23.5.1.4.5: transmit in the first valid access opportunity
                // provided the IMM condition holds; otherwise fall through to a
                // new access frame.
                let frames_elapsed = frame_index(dltime) - init_frame;
                let within_imm =
                    self.is_emergency || params.imm == 15 || (frames_elapsed as i64) < (params.imm as i64);

                if ul_slot_valid && within_imm {
                    let s1 = assign.subslot1.is_opportunity_for(self.code);
                    let s2 = assign.subslot2.is_opportunity_for(self.code);
                    let subslot = match (s1, s2) {
                        (true, true) => rng.choose_one_of_two(),
                        (true, false) => Subslot::One,
                        (false, true) => Subslot::Two,
                        (false, false) => return None,
                    };
                    return Some(self.transmit(dltime, subslot));
                }

                // IMM frames have elapsed without transmitting (non-emergency,
                // IMM != 15): switch to choosing a new access frame and process
                // this slot in that mode (cl. 23.5.1.4.5 → .4.6).
                if !within_imm {
                    self.state = RaState::AwaitFrameMarker;
                    return self.poll_downlink_slot(dltime, assign, ul_slot_valid, params, rng);
                }
                None
            }

            RaState::AwaitFrameMarker => {
                // cl. 23.5.1.4.6: wait for a frame marker for the access code,
                // then choose a subslot uniformly in [1, Frame-length].
                if !ul_slot_valid {
                    return None;
                }
                if let Some(base) = self.marker_base(&assign.subslot1) {
                    let len = frame_length(base, params.fl_factor);
                    let chosen = rng.choose_subslot_index(len).clamp(1, len);
                    self.state = RaState::Counting { chosen, counted: 0 };
                    return self.count_slot(dltime, assign);
                }
                None
            }

            RaState::Counting { .. } => {
                // cl. 23.5.1.4.7: continue counting access opportunities toward
                // the chosen subslot.
                if !ul_slot_valid {
                    return None;
                }
                self.count_slot(dltime, assign)
            }
        }
    }

    /// If subslot 1 of `assign` carries a frame marker for the access code,
    /// return its base frame-length (cl. 23.5.1.4.6). The frame marker is always
    /// in subslot 1 of the marker slot (cl. 23.5.1.4.2 point e).
    fn marker_base(&self, subslot1: &SubslotAccess) -> Option<u16> {
        if subslot1.is_frame_marker_for(self.code) {
            subslot1.frame_len.base_subslots()
        } else {
            None
        }
    }

    /// Count the (up to two) access opportunities in this slot toward the chosen
    /// subslot, transmitting when the chosen count is reached (cl. 23.5.1.4.7).
    /// The first counted subslot is the frame-marker subslot (subslot 1 of the
    /// marker slot).
    fn count_slot(&mut self, dltime: TdmaTime, assign: &SlotAccessAssign) -> Option<RaAction> {
        for (idx, sub) in [(Subslot::One, assign.subslot1), (Subslot::Two, assign.subslot2)] {
            if !sub.is_opportunity_for(self.code) {
                continue;
            }
            let RaState::Counting { chosen, counted } = self.state else {
                return None;
            };
            let counted = counted + 1;
            if counted >= chosen {
                return Some(self.transmit(dltime, idx));
            }
            self.state = RaState::Counting { chosen, counted };
        }
        None
    }

    /// Inform the state machine that a MAC-RESOURCE was received on the downlink
    /// (cl. 23.5.1.4.8). `addr_matches` is true when the PDU is addressed to this
    /// MS, and `random_access_success` is the MAC-RESOURCE "random access flag"
    /// acknowledging a successful random access (cl. 21.4.3.1). Returns
    /// `Some(Succeeded)` when the attempt is completed.
    pub fn on_mac_resource(&mut self, addr_matches: bool, random_access_success: bool) -> Option<RaAction> {
        if self.is_active() && addr_matches && random_access_success {
            self.state = RaState::Idle;
            Some(RaAction::Succeeded)
        } else {
            None
        }
    }

    /// Abandon any in-progress attempt without a response (cl. 23.5.1.4.9, e.g.
    /// on TMA-CANCEL). Resets to idle.
    pub fn cancel(&mut self) {
        self.state = RaState::Idle;
        self.tx_count = 0;
    }
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

    // ---- Random access state machine (cl. 23.5.1.4.5–.9) --------------------

    /// Deterministic RNG for tests: returns queued subslot indices / choices.
    struct FakeRng {
        indices: std::collections::VecDeque<u16>,
        choices: std::collections::VecDeque<Subslot>,
    }
    impl FakeRng {
        fn new() -> Self {
            FakeRng {
                indices: Default::default(),
                choices: Default::default(),
            }
        }
        fn with_index(mut self, i: u16) -> Self {
            self.indices.push_back(i);
            self
        }
        fn with_choice(mut self, c: Subslot) -> Self {
            self.choices.push_back(c);
            self
        }
    }
    impl RaRng for FakeRng {
        fn choose_subslot_index(&mut self, frame_length: u16) -> u16 {
            self.indices.pop_front().unwrap_or(1).min(frame_length)
        }
        fn choose_one_of_two(&mut self) -> Subslot {
            self.choices.pop_front().unwrap_or(Subslot::One)
        }
    }

    fn params(imm: u8, wt: u8, nu: u8) -> RandomAccessParams {
        RandomAccessParams {
            imm,
            wt,
            nu,
            fl_factor: false,
            ts_ptr: 0,
            min_pdu_prio: 0,
            subscriber_class: None,
            gssi: None,
        }
    }

    fn t(ts: u8, f: u8) -> TdmaTime {
        TdmaTime { t: ts, f, m: 1, h: 0 }
    }

    /// A slot whose subslot 1 is an ongoing-frame opportunity for code A.
    fn ongoing_a() -> SlotAccessAssign {
        let s = SubslotAccess {
            access_code: Some(AccessCode::A),
            frame_len: BaseFrameLength::OngoingFrame,
        };
        SlotAccessAssign { subslot1: s, subslot2: s }
    }

    /// A slot whose subslot 1 is a frame marker for code A with the given base
    /// length and subslot 2 is an ongoing frame (cl. 23.5.1.4.2 point e).
    fn marker_a(base: u16) -> SlotAccessAssign {
        let raw = match base {
            1 => 0b0011,
            2 => 0b0100,
            3 => 0b0101,
            4 => 0b0110,
            _ => panic!("unsupported base"),
        };
        SlotAccessAssign {
            subslot1: SubslotAccess {
                access_code: Some(AccessCode::A),
                frame_len: BaseFrameLength::from_raw(raw),
            },
            subslot2: SubslotAccess {
                access_code: Some(AccessCode::A),
                frame_len: BaseFrameLength::OngoingFrame,
            },
        }
    }

    #[test]
    fn test_ra_imm15_first_valid_opportunity() {
        // IMM == 15: transmit in the first valid opportunity (cl. 23.5.1.4.5).
        let mut ra = MsRandomAccess::new();
        let p = params(15, 4, 3);
        ra.initiate(t(1, 1), AccessCode::A, &p, 0, false).unwrap();
        assert!(ra.is_active());
        let mut rng = FakeRng::new();
        // Subslot 1 only is an opportunity → transmit in subslot 1 at DL+2.
        let a = ra.poll_downlink_slot(t(1, 1), &ongoing_a(), true, &p, &mut rng);
        assert_eq!(
            a,
            Some(RaAction::Transmit {
                ul_time: t(1, 1).add_timeslots(2),
                subslot: Subslot::One,
            })
        );
    }

    #[test]
    fn test_ra_imm15_both_subslots_uses_rng() {
        let mut ra = MsRandomAccess::new();
        let p = params(15, 4, 3);
        ra.initiate(t(1, 1), AccessCode::A, &p, 0, false).unwrap();
        let both = ongoing_a(); // both subslots are opportunities for A
        let mut rng = FakeRng::new().with_choice(Subslot::Two);
        let a = ra.poll_downlink_slot(t(1, 1), &both, true, &p, &mut rng);
        match a {
            Some(RaAction::Transmit { subslot, .. }) => assert_eq!(subslot, Subslot::Two),
            other => panic!("expected transmit, got {other:?}"),
        }
    }

    #[test]
    fn test_ra_invalid_ul_slot_no_transmit() {
        // ts_pointer mismatch (ul_slot_valid == false) must not transmit
        // (cl. 23.5.1.4.7 point a).
        let mut ra = MsRandomAccess::new();
        let p = params(15, 4, 3);
        ra.initiate(t(1, 1), AccessCode::A, &p, 0, false).unwrap();
        let mut rng = FakeRng::new();
        assert_eq!(ra.poll_downlink_slot(t(1, 1), &ongoing_a(), false, &p, &mut rng), None);
    }

    #[test]
    fn test_ra_imm0_uses_new_access_frame() {
        // IMM == 0 always randomizes: no first-try, wait for a frame marker then
        // count to the chosen subslot (cl. 23.5.1.4.5 → .4.6).
        let mut ra = MsRandomAccess::new();
        let p = params(0, 4, 3);
        ra.initiate(t(1, 1), AccessCode::A, &p, 0, false).unwrap();
        // Choose subslot 2 within a 2-subslot frame.
        let mut rng = FakeRng::new().with_index(2);

        // An ongoing-frame slot before any marker is ignored.
        assert_eq!(ra.poll_downlink_slot(t(1, 1), &ongoing_a(), true, &p, &mut rng), None);

        // Marker slot: subslot 1 (marker) is count #1; subslot 2 (ongoing) is
        // count #2 == chosen → transmit in subslot 2.
        let a = ra.poll_downlink_slot(t(1, 2), &marker_a(2), true, &p, &mut rng);
        assert_eq!(
            a,
            Some(RaAction::Transmit {
                ul_time: t(1, 2).add_timeslots(2),
                subslot: Subslot::Two,
            })
        );
    }

    #[test]
    fn test_ra_new_frame_counts_across_slots() {
        // chosen == 3 within a base-4 frame: marker subslot (#1), marker subslot2
        // ongoing (#2), then the next slot's subslot1 (#3) → transmit there
        // (cl. 23.5.1.4.7).
        let mut ra = MsRandomAccess::new();
        let p = params(0, 4, 3);
        ra.initiate(t(1, 1), AccessCode::A, &p, 0, false).unwrap();
        let mut rng = FakeRng::new().with_index(3);

        assert_eq!(ra.poll_downlink_slot(t(1, 1), &marker_a(4), true, &p, &mut rng), None);
        let a = ra.poll_downlink_slot(t(1, 2), &ongoing_a(), true, &p, &mut rng);
        assert_eq!(
            a,
            Some(RaAction::Transmit {
                ul_time: t(1, 2).add_timeslots(2),
                subslot: Subslot::One,
            })
        );
    }

    #[test]
    fn test_ra_success_on_mac_resource() {
        let mut ra = MsRandomAccess::new();
        let p = params(15, 4, 3);
        ra.initiate(t(1, 1), AccessCode::A, &p, 0, false).unwrap();
        let mut rng = FakeRng::new();
        ra.poll_downlink_slot(t(1, 1), &ongoing_a(), true, &p, &mut rng);
        // A matching MAC-RESOURCE with the random access flag set → success.
        assert_eq!(ra.on_mac_resource(true, true), Some(RaAction::Succeeded));
        assert!(!ra.is_active());
        // Non-matching or non-ack MAC-RESOURCE is ignored.
        ra.initiate(t(1, 1), AccessCode::A, &p, 0, false).unwrap();
        assert_eq!(ra.on_mac_resource(false, true), None);
        assert_eq!(ra.on_mac_resource(true, false), None);
        assert!(ra.is_active());
    }

    #[test]
    fn test_ra_retry_then_abandon_after_nu() {
        // Nu == 2: transmit, WT expires (retry), transmit again, WT expires →
        // abandon with MaxTransmissions (cl. 23.5.1.4.8/.4.9).
        let mut ra = MsRandomAccess::new();
        let p = params(15, 2, 2);
        ra.initiate(t(1, 1), AccessCode::A, &p, 0, false).unwrap();
        let mut rng = FakeRng::new().with_index(1).with_index(1);

        // TX #1.
        assert!(matches!(
            ra.poll_downlink_slot(t(1, 1), &ongoing_a(), true, &p, &mut rng),
            Some(RaAction::Transmit { .. })
        ));
        // WT counts one opportunity per frame; wt == 2 → retry (no action yet).
        assert_eq!(ra.poll_downlink_slot(t(1, 2), &ongoing_a(), true, &p, &mut rng), None);
        assert_eq!(ra.poll_downlink_slot(t(1, 3), &ongoing_a(), true, &p, &mut rng), None);
        // Now awaiting a new frame marker → TX #2 on the marker slot.
        assert!(matches!(
            ra.poll_downlink_slot(t(1, 4), &marker_a(1), true, &p, &mut rng),
            Some(RaAction::Transmit { .. })
        ));
        // WT expires again; tx_count == Nu == 2 → abandon.
        assert_eq!(ra.poll_downlink_slot(t(1, 5), &ongoing_a(), true, &p, &mut rng), None);
        assert_eq!(
            ra.poll_downlink_slot(t(1, 6), &ongoing_a(), true, &p, &mut rng),
            Some(RaAction::Failed(RaFailure::MaxTransmissions))
        );
        assert!(!ra.is_active());
    }

    #[test]
    fn test_ra_emergency_doubles_transmissions() {
        // Emergency (priority 7) doubles the transmission limit to 2·Nu
        // (cl. 23.5.1.4.9) and bypasses the min-priority check (cl. 23.5.1.4.4).
        let mut ra = MsRandomAccess::new();
        let mut p = params(15, 1, 1);
        p.min_pdu_prio = 5; // would normally reject a low-priority PDU
        // Emergency code A: permitted despite min_pdu_prio.
        ra.initiate(t(1, 1), AccessCode::A, &p, 0, true).unwrap();
        let mut rng = FakeRng::new().with_index(1).with_index(1);
        // 2·Nu == 2 transmissions allowed. TX #1 in the first-try slot.
        assert!(matches!(
            ra.poll_downlink_slot(t(1, 1), &ongoing_a(), true, &p, &mut rng),
            Some(RaAction::Transmit { .. })
        ));
        // WT == 1 → the next frame expires the wait and triggers a retry (no
        // action; now awaiting a new frame marker).
        assert_eq!(ra.poll_downlink_slot(t(1, 2), &ongoing_a(), true, &p, &mut rng), None);
        // A later marker slot → TX #2.
        assert!(matches!(
            ra.poll_downlink_slot(t(1, 3), &marker_a(1), true, &p, &mut rng),
            Some(RaAction::Transmit { .. })
        ));
        // Second WT expiry → abandon (2 transmissions done).
        assert_eq!(
            ra.poll_downlink_slot(t(1, 4), &ongoing_a(), true, &p, &mut rng),
            Some(RaAction::Failed(RaFailure::MaxTransmissions))
        );
    }

    #[test]
    fn test_ra_initiate_rejects_unavailable_and_low_priority() {
        let mut ra = MsRandomAccess::new();
        // Nu == 0 → unavailable (cl. 23.5.1.4.1).
        assert_eq!(
            ra.initiate(t(1, 1), AccessCode::A, &params(15, 4, 0), 0, false),
            Err(RaFailure::NoValidAccessCode)
        );
        // PDU priority below the minimum, non-emergency → not permitted
        // (cl. 23.5.1.4.4).
        let mut p = params(15, 4, 3);
        p.min_pdu_prio = 4;
        assert_eq!(
            ra.initiate(t(1, 1), AccessCode::A, &p, 2, false),
            Err(RaFailure::AccessCodeNotPermitted)
        );
        assert!(!ra.is_active());
    }
}
