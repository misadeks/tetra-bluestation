// ---------------------------------------------------------------------------
// TelemetryEvent — concrete enum sent through the channel
//
// Small, hot-path variants are inline (no heap allocation).
// Rare / large variants use heap-allocated payload so the enum stays small.
// ---------------------------------------------------------------------------

use bitcode::{Decode, Encode};
use serde::{Deserialize, Serialize};

use crate::tnmm::{
    TnmmAttachDetachGroupIdentityConfirm, TnmmAttachDetachGroupIdentityIndication, TnmmEnergySavingConfirm, TnmmEnergySavingIndication,
    TnmmRegistrationConfirm, TnmmRegistrationIndication, TnmmReportIndication, TnmmServiceIndication, TnmmStatusConfirm,
    TnmmStatusIndication,
};
use tetra_saps::tncc::{
    TnccAlertIndication, TnccCompleteConfirm, TnccCompleteIndication, TnccNotifyIndication, TnccProceedIndication, TnccReleaseConfirm,
    TnccReleaseIndication, TnccSetupConfirm, TnccSetupIndication, TnccTxConfirm, TnccTxIndication,
};
use tetra_saps::tnsds::{TnsdsMessageIndication, TnsdsReportIndication, TnsdsStatusIndication, TnsdsUnitdataIndication};

/// TelemetryEvent enum sent by a TetraEntity through the TelemetrySink
/// then, serializable by any codec for transmission over the network,
/// using any Transport.
#[derive(Debug, Clone, Encode, Decode, Serialize, Deserialize)]
pub enum TelemetryEvent {
    /// Registration event
    MsRegistration {
        issi: u32,
    },
    /// Deregistration event. Also counts as a deregistration for all groups the ISSI was attached to.
    MsDeregistration {
        issi: u32,
    },
    MsGroupAttach {
        issi: u32,
        gssis: Vec<u32>,
    },
    MsGroupDetach {
        issi: u32,
        gssis: Vec<u32>,
    },

    // -----------------------------------------------------------------------
    // TNMM-SAP indications / confirms (Plane A, OUTBOUND) — ETSI TS 100 392-2
    // v3.10.1 cl. 15.3.3. These are the MS-side TNMM primitives sent from MM to
    // the user application (here, the external UI). Payloads carry the exact
    // parameter sets of Tables 15.1–15.7; see the `crate::tnmm` module. The
    // larger registration payloads are boxed to keep the enum small.
    // -----------------------------------------------------------------------
    /// TNMM-REGISTRATION indication (Table 15.5, cl. 15.3.3.7).
    TnmmRegistrationIndication(Box<TnmmRegistrationIndication>),
    /// TNMM-REGISTRATION confirm (Table 15.5, cl. 15.3.3.7).
    TnmmRegistrationConfirm(Box<TnmmRegistrationConfirm>),
    /// TNMM-ATTACH DETACH GROUP IDENTITY indication (Table 15.1, cl. 15.3.3.1).
    TnmmAttachDetachGroupIdentityIndication(TnmmAttachDetachGroupIdentityIndication),
    /// TNMM-ATTACH DETACH GROUP IDENTITY confirm (Table 15.1, cl. 15.3.3.1).
    TnmmAttachDetachGroupIdentityConfirm(TnmmAttachDetachGroupIdentityConfirm),
    /// TNMM-REPORT indication (Table 15.4, cl. 15.3.3.6).
    TnmmReportIndication(TnmmReportIndication),
    /// TNMM-SERVICE indication (Table 15.6, cl. 15.3.3.8).
    TnmmServiceIndication(TnmmServiceIndication),
    /// TNMM-STATUS indication (Table 15.7, cl. 15.3.3.9).
    TnmmStatusIndication(TnmmStatusIndication),
    /// TNMM-STATUS confirm (Table 15.7, cl. 15.3.3.9).
    TnmmStatusConfirm(TnmmStatusConfirm),
    /// TNMM-ENERGY SAVING indication (Table 15.3, cl. 15.3.3.5) — dormant.
    TnmmEnergySavingIndication(TnmmEnergySavingIndication),
    /// TNMM-ENERGY SAVING confirm (Table 15.3, cl. 15.3.3.5) — dormant.
    TnmmEnergySavingConfirm(TnmmEnergySavingConfirm),

    // -----------------------------------------------------------------------
    // TNCC-SAP indications / confirms (Plane A, OUTBOUND) — ETSI TS 100 392-2
    // v3.10.1 cl. 11.3.3. Payloads carry the exact primitive table parameter
    // sets from Tables 11.1/11.2/11.5/11.6/11.7/11.8/11.9. `call_identifier`
    // is a local TNCC-SAP instance selector, not a TNCC primitive parameter.
    // -----------------------------------------------------------------------
    /// TNCC-ALERT indication (Table 11.1, cl. 11.3.3.1).
    TnccAlertIndication {
        call_identifier: u16,
        indication: TnccAlertIndication,
    },
    /// TNCC-COMPLETE indication (Table 11.2, cl. 11.3.3.2).
    TnccCompleteIndication {
        call_identifier: u16,
        indication: TnccCompleteIndication,
    },
    /// TNCC-COMPLETE confirm (Table 11.2, cl. 11.3.3.2).
    TnccCompleteConfirm {
        call_identifier: u16,
        confirm: TnccCompleteConfirm,
    },
    /// TNCC-NOTIFY indication (Table 11.5, cl. 11.3.3.5).
    TnccNotifyIndication {
        call_identifier: u16,
        indication: TnccNotifyIndication,
    },
    /// TNCC-PROCEED indication (Table 11.6, cl. 11.3.3.6).
    TnccProceedIndication {
        call_identifier: u16,
        indication: TnccProceedIndication,
    },
    /// TNCC-RELEASE indication (Table 11.7, cl. 11.3.3.7).
    TnccReleaseIndication {
        call_identifier: u16,
        indication: TnccReleaseIndication,
    },
    /// TNCC-RELEASE confirm (Table 11.7, cl. 11.3.3.7).
    TnccReleaseConfirm {
        call_identifier: u16,
        confirm: TnccReleaseConfirm,
    },
    /// TNCC-SETUP indication (Table 11.8, cl. 11.3.3.8).
    TnccSetupIndication {
        call_identifier: u16,
        indication: Box<TnccSetupIndication>,
    },
    /// TNCC-SETUP confirm (Table 11.8, cl. 11.3.3.8).
    TnccSetupConfirm {
        call_identifier: u16,
        confirm: Box<TnccSetupConfirm>,
    },
    /// TNCC-TX indication (Table 11.9, cl. 11.3.3.9).
    TnccTxIndication {
        call_identifier: u16,
        indication: TnccTxIndication,
    },
    /// TNCC-TX confirm (Table 11.9, cl. 11.3.3.9).
    TnccTxConfirm {
        call_identifier: u16,
        confirm: TnccTxConfirm,
    },

    // -----------------------------------------------------------------------
    // TNSDS-SAP indications (Plane A, OUTBOUND) — ETSI TS 100 392-2 v3.10.1
    // cl. 13.3.2. MS-side SDS/status primitives sent from CMCE/SDS to the user
    // application (external UI). Payloads carry the parameter subset of
    // Tables 13.1/13.3; see the `tetra_saps::tnsds` module.
    // -----------------------------------------------------------------------
    /// TNSDS-UNITDATA indication (Table 13.3, cl. 13.3.2.3): a user-defined SDS
    /// message (D-SDS-DATA, cl. 14.7.1.10) was received.
    TnsdsUnitdataIndication(TnsdsUnitdataIndication),
    /// TNSDS-STATUS indication (Table 13.1, cl. 13.3.2.1): a pre-coded status
    /// message (D-STATUS, cl. 14.7.1.11) was received.
    TnsdsStatusIndication(TnsdsStatusIndication),
    /// TNSDS-UNITDATA indication for a received SDS-TL SDS-TRANSFER (cl. 29.4.2.4):
    /// a text/user message with a message reference + delivery-report request.
    TnsdsMessageIndication(TnsdsMessageIndication),
    /// TNSDS-REPORT indication (Table 13.2, cl. 13.3.2.2): a delivery/read report
    /// (SDS-REPORT / SDS-ACK / SDS-SHORT-REPORT) for a message this MS sent.
    TnsdsReportIndication(TnsdsReportIndication),

    // -----------------------------------------------------------------------
    // U-plane speech (Plane U, OUTBOUND) — downlink circuit-mode traffic.
    //
    // Not a TNMM/TNCC control primitive: this is the received U-plane speech
    // stream (ETSI TS 100 392-2 cl. 14.5.1.4, U-plane switching) offloaded to
    // the external UI, which runs the ACELP speech decoder. The stack performs
    // no vocoding.
    // -----------------------------------------------------------------------
    /// One decoded downlink TCH/S speech block for an active call.
    ///
    /// `data` is the channel-decoded type-1 bit block (cl. 19.4): `frame_bits`
    /// bits carried one-bit-per-byte (274 for TCH/S = two 137-bit ACELP speech
    /// frames, EN 300 395-2). `sequence` is a per-call monotonically increasing
    /// frame counter for jitter/ordering at the UI. `bad_frame` is the
    /// channel-decode CRC bad-frame indicator (BFI): when `true` the UI must
    /// apply error concealment (substitution/muting) rather than decode the
    /// block as valid speech. `transmitting_party_ssi` is the current talker
    /// when known (from the last D-TX GRANTED), for UI attribution.
    MsSpeechFrame {
        call_identifier: u16,
        timeslot: u8,
        sequence: u32,
        transmitting_party_ssi: Option<u32>,
        frame_bits: u16,
        bad_frame: bool,
        data: Vec<u8>,
    },

    // -----------------------------------------------------------------------
    // Manual cell survey (Plane B, non-standard OUTBOUND) — results of an
    // operator-driven receive-only scan of the codeplug candidate carriers.
    //
    // Not a TNMM primitive: the survey UX is implementation policy layered on
    // ETSI cl. 18.3.4 initial cell selection. Each per-cell field is parsed
    // per spec (D-MLE-SYNC cl. 18.4.2.1, D-MLE-SYSINFO cl. 18.4.2.2); the
    // survey transmits nothing.
    // -----------------------------------------------------------------------
    /// One cell found during a manual survey. Emitted once per surveyed cell.
    MsScanResult {
        /// Downlink carrier the cell was found on (Hz).
        carrier_hz: u32,
        /// Mobile Country Code (D-MLE-SYNC, cl. 18.4.2.1).
        mcc: u16,
        /// Mobile Network Code (D-MLE-SYNC, cl. 18.4.2.1).
        mnc: u16,
        /// Location Area (D-MLE-SYSINFO, cl. 18.4.2.2); `None` if the cell
        /// synced but no SYSINFO was captured within the dwell.
        location_area: Option<u16>,
        /// Colour code — not available at MLE (a MAC-layer scrambling quantity);
        /// always `None`, carried for a stable UI schema.
        colour_code: Option<u8>,
        /// Serving-cell downlink receive level in uncalibrated dBFS, if measured.
        rssi_dbfs: Option<f32>,
        /// Whether the cell advertises that registration is required
        /// (BS service details, cl. 18.4.2.2); `None` if SYSINFO not captured.
        registration_required: Option<bool>,
        /// Whether the cell supports late entry (D-MLE-SYNC, cl. 18.4.2.1).
        late_entry_supported: bool,
    },
    /// End of a manual survey: `found` cells were reported across `scanned`
    /// candidate carriers.
    MsScanComplete {
        found: u32,
        scanned: u32,
    },
}
