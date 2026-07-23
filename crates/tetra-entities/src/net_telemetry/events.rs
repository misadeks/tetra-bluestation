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
}
