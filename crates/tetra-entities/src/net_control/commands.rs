// ---------------------------------------------------------------------------
// Command / CommandResponse — concrete enums sent through the channel
//
// The command server sends a Command; the stack processes it and returns
// a CommandResponse.  Placeholder variants are provided for now.
// ---------------------------------------------------------------------------

use bitcode::{Decode, Encode};
use serde::{Deserialize, Serialize};

use crate::management::{ManagementCommand, ManagementResponse};
use crate::tnmm::{
    TnmmAttachDetachGroupIdentityRequest, TnmmDeregistrationRequest, TnmmEnergySavingRequest, TnmmRegistrationRequest, TnmmStatusRequest,
};
use tetra_saps::tncc::{TnccCompleteRequest, TnccDtmfRequest, TnccReleaseRequest, TnccSetupRequest, TnccSetupResponse, TnccTxRequest};
use tetra_saps::tnsds::{TnsdsStatusRequest, TnsdsUnitdataRequest};

/// Command received from the remote command server.
#[derive(Debug, Clone, Encode, Decode, Serialize, Deserialize)]
pub enum ControlCommand {
    /// Command to send an SDS for local delivery
    SendSds {
        handle: u32,
        source_ssi: u32,
        dest_ssi: u32,
        dest_is_group: bool,
        len_bits: u16,
        payload: Vec<u8>,
    },

    /// Placeholder command A.
    CommandA { handle: u32, parameter: u32 },
    /// Placeholder command B.
    TestCmdB {
        handle: u32,
        source_ssi: u32,
        is_group: bool,
        payload: Vec<u8>,
    },

    // -----------------------------------------------------------------------
    // TNMM-SAP requests (Plane A, INBOUND) — ETSI TS 100 392-2 v3.10.1
    // cl. 15.3.3. These are the MS-side TNMM request primitives sent from the
    // user application (external UI) to Mobility Management. Payloads carry the
    // exact Request-column parameter sets of Tables 15.1/15.2/15.3/15.5/15.7;
    // see the `crate::tnmm` module. `handle` is a transport-level correlation
    // id (not a TNMM parameter) so a UI can match asynchronous outcomes.
    // -----------------------------------------------------------------------
    /// TNMM-REGISTRATION request (Table 15.5, cl. 15.3.3.7). Larger payload is
    /// boxed to keep the enum small.
    TnmmRegistration {
        handle: u32,
        request: Box<TnmmRegistrationRequest>,
    },
    /// TNMM-DEREGISTRATION request (Table 15.2, cl. 15.3.3.2).
    TnmmDeregistration { handle: u32, request: TnmmDeregistrationRequest },
    /// TNMM-ATTACH DETACH GROUP IDENTITY request (Table 15.1, cl. 15.3.3.1).
    TnmmAttachDetachGroupIdentity {
        handle: u32,
        request: TnmmAttachDetachGroupIdentityRequest,
    },
    /// TNMM-STATUS request (Table 15.7, cl. 15.3.3.9).
    TnmmStatus { handle: u32, request: TnmmStatusRequest },
    /// TNMM-ENERGY SAVING request (Table 15.3, cl. 15.3.3.5) — dormant.
    TnmmEnergySaving { handle: u32, request: TnmmEnergySavingRequest },

    // -----------------------------------------------------------------------
    // TNCC-SAP requests (Plane A, INBOUND) — ETSI TS 100 392-2 v3.10.1
    // cl. 11.3.3. Payloads carry the Request/Response-column parameter sets of
    // Tables 11.2/11.7/11.8/11.9. `handle` and `call_identifier` are local
    // transport fields (correlation id and TNCC-SAP instance selector), not TNCC
    // primitive parameters.
    // -----------------------------------------------------------------------
    /// TNCC-SETUP request (Table 11.8, cl. 11.3.3.8).
    TnccSetup { handle: u32, request: Box<TnccSetupRequest> },
    /// TNCC-SETUP response (Table 11.8, cl. 11.3.3.8).
    TnccSetupResponse {
        handle: u32,
        call_identifier: u16,
        response: TnccSetupResponse,
    },
    /// TNCC-COMPLETE request (Table 11.2, cl. 11.3.3.2).
    TnccComplete {
        handle: u32,
        call_identifier: u16,
        request: TnccCompleteRequest,
    },
    /// TNCC-TX request (Table 11.9, cl. 11.3.3.9).
    TnccTx {
        handle: u32,
        call_identifier: u16,
        request: TnccTxRequest,
    },
    /// TNCC-DTMF request (Table 11.3, cl. 11.3.3.3): send in-call DTMF digits
    /// (→ U-INFO with the DTMF IE, cl. 14.7.2.6 / 14.8.19).
    TnccDtmf {
        handle: u32,
        call_identifier: u16,
        request: TnccDtmfRequest,
    },
    /// TNCC-RELEASE request (Table 11.7, cl. 11.3.3.7).
    TnccRelease {
        handle: u32,
        call_identifier: u16,
        request: TnccReleaseRequest,
    },

    // -----------------------------------------------------------------------
    // TNSDS-SAP requests (Plane A, INBOUND) — ETSI TS 100 392-2 v3.10.1
    // cl. 13.3.2. MS-side Short Data Service request primitives sent from the
    // user application (external UI) to CMCE/SDS. Payloads carry the parameter
    // subset of Tables 13.1/13.3; see the `tetra_saps::tnsds` module. `handle`
    // is a transport-level correlation id (the cl. 13.3.3 local SDU identifier).
    // -----------------------------------------------------------------------
    /// TNSDS-UNITDATA request (Table 13.3, cl. 13.3.2.3): send user-defined SDS
    /// data (→ U-SDS-DATA, cl. 14.7.2.8).
    TnsdsUnitdata { handle: u32, request: TnsdsUnitdataRequest },
    /// TNSDS-STATUS request (Table 13.1, cl. 13.3.2.1): send a pre-coded status
    /// (→ U-STATUS, cl. 14.7.2.7).
    TnsdsStatus { handle: u32, request: TnsdsStatusRequest },

    // -----------------------------------------------------------------------
    // U-plane uplink speech (traffic), INBOUND. Symmetric counterpart of the
    // outbound `TelemetryEvent::MsSpeechFrame`. Not a TNCC-SAP signalling
    // primitive: it carries circuit-mode speech, delivered into the U-plane the
    // CC has switched on (ETSI TS 100 392-2 cl. 14.5.1.4) and transmitted on the
    // granted uplink traffic slot (cl. 23). The external UI runs the ACELP
    // vocoder (microphone → speech codec, EN 300 395-2); the stack performs no
    // vocoding.
    // -----------------------------------------------------------------------
    /// One uplink TCH/S speech block for a call this MS is transmitting on.
    /// `data` is `frame_bits` bits carried one-bit-per-byte (274 for TCH/S = two
    /// 137-bit ACELP frames), matching the downlink `MsSpeechFrame` layout.
    /// Fire-and-forget: no response is produced — the frame rate makes per-frame
    /// acknowledgement impractical, and a dropped frame is covered by the MAC's
    /// silence-on-underrun (cl. 23). Frames arriving while the MS does not hold
    /// the floor for the call are discarded.
    MsUplinkSpeech {
        call_identifier: u16,
        frame_bits: u16,
        data: Vec<u8>,
    },

    // -----------------------------------------------------------------------
    // Management / provisioning (Plane B, **NON-STANDARD**). Wraps the
    // implementation-defined `crate::management` command set. Kept in its own
    // variant so Plane B never mixes with the standardized TNMM-SAP (Plane A)
    // on the wire. See `crate::management` for the standards disclaimer.
    // -----------------------------------------------------------------------
    /// Management command (non-standard stack provisioning / runtime-state read).
    Management(ManagementCommand),
}

/// Response sent back to the remote command server after processing a [`Command`].
#[derive(Debug, Clone, Encode, Decode, Serialize, Deserialize)]
pub enum ControlResponse {
    /// Response to [`Command::CommandA`].
    CommandAResponse { handle: u32, result: u32 },
    /// Response to [`Command::SendSds`].
    SendSdsResponse { handle: u32, success: bool },

    /// Transport-level acknowledgement that a TNMM request was accepted for
    /// processing by MM. The TNMM *result* is reported asynchronously through the
    /// TNMM-SAP indications/confirms on the telemetry channel (cl. 15.3.2), so
    /// this only reports whether MM acted on the request. `detail` documents a
    /// deferral for requests targeting features not implemented in this stack.
    TnmmAck {
        handle: u32,
        accepted: bool,
        detail: Option<String>,
    },

    /// Transport-level acknowledgement that a TNCC request was accepted for
    /// processing by CMCE/CC. The TNCC result is reported asynchronously through
    /// TNCC-SAP indications/confirms on telemetry (cl. 11.3.2).
    TnccAck {
        handle: u32,
        accepted: bool,
        detail: Option<String>,
    },

    /// Transport-level acknowledgement that a TNSDS request was accepted for
    /// processing by CMCE/SDS. The delivery result (transmitted / lost) is
    /// reported asynchronously via the TNSDS-REPORT indication (Table 13.2,
    /// cl. 13.3.2.2) once the SDS-TL / MLE tx-result hook is wired; today this
    /// only reports whether SDS acted on the request.
    TnsdsAck {
        handle: u32,
        accepted: bool,
        detail: Option<String>,
    },

    /// Response to a management command (Plane B, **NON-STANDARD**). Wraps the
    /// implementation-defined `crate::management` response set.
    Management(ManagementResponse),
}
