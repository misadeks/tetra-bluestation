use super::*;

/// CMCE CC-MS call state (ETSI TS 100 392-2 cl. 14.5.2 and 14.5.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsCcState {
    Idle,
    MoCallSetup,
    MtCallSetup,
    CallActive,
    Wait,
    Restore,
    Disconnect,
    Release,
}

/// Call kind from Basic service information / communication type (cl. 14.8.2,
/// 14.8.17c).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsCallKind {
    Individual,
    Group,
    AcknowledgedGroup,
    Broadcast,
}

/// Local interpretation of Transmission grant (cl. 14.8.42) in the MS call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsTxGrantState {
    None,
    GrantedSelf,
    RequestQueued,
    GrantedOther,
    Waiting,
    Interrupted,
}

/// Last U-plane CONFIGURE request state (cl. 14.5.1.4; LCMC-SAP cl. 17.3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsUPlaneState {
    pub switch_u_plane: bool,
    pub tx_grant: bool,
    pub simplex_duplex: bool,
}

#[derive(Clone, Debug)]
pub struct MsCallTimers {
    /// T301/T302/T303 setup-phase deadline from cl. 14.5.2.1 / 14.8.17.
    pub setup_phase_deadline: Option<TdmaTime>,
    /// T310 call-timeout deadline from cl. 14.8.16.
    pub call_deadline: Option<TdmaTime>,
    pub setup_timeout: Option<CallTimeoutSetupPhase>,
    pub call_timeout: CallTimeout,
}

impl Default for MsCallTimers {
    fn default() -> Self {
        Self {
            setup_phase_deadline: None,
            call_deadline: None,
            setup_timeout: None,
            call_timeout: CallTimeout::Infinite,
        }
    }
}

/// Per-call CC-MS state keyed by the 14-bit Call identifier IE (cl. 14.8.4).
#[derive(Clone, Debug)]
pub struct MsCall {
    pub call_identifier: u16,
    pub state: MsCcState,
    pub kind: MsCallKind,
    pub basic_service: BasicServiceInformation,
    pub current_speaker_ssi: Option<u32>,
    pub tx_grant_state: MsTxGrantState,
    pub transmission_request_allowed: bool,
    pub timers: MsCallTimers,
    pub disconnect_cause: Option<DisconnectCause>,
    pub last_uplane: Option<MsUPlaneState>,
    /// Count of downlink TCH/S speech frames accepted into the U-plane for this
    /// call (i.e. received while the U-plane was switched on, cl. 14.5.1.4). A
    /// minimal received-audio egress point until a real vocoder/audio sink is
    /// wired; also the observable signal that "the MS heard the call".
    pub rx_speech_frames: u32,
    pub(super) route: CallRoute,
    pub(super) simplex_duplex_selection: bool,
    /// Signalling mode dictated by the D-SETUP Hook method selection IE
    /// (cl. 14.8.23, applied per cl. 14.5.1.1.1): `true` = on/off-hook
    /// signalling (U-ALERT then U-CONNECT), `false` = direct set-up (immediate
    /// U-CONNECT). Only meaningful for MT individual calls.
    pub(super) hook_on_off: bool,
    pub(super) pending_tx_request: bool,
    pub(super) uplane_before_wait: Option<MsUPlaneState>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CallRoute {
    pub(super) main_address: TetraAddress,
    pub(super) handle: MleHandle,
    pub(super) endpoint_id: EndpointId,
    pub(super) link_id: LinkId,
}

#[derive(Clone, Debug)]
pub(super) struct PendingOrigination {
    pub(super) called_party: TetraAddress,
    pub(super) basic_service: BasicServiceInformation,
    pub(super) simplex_duplex_selection: bool,
}

impl MsCall {
    pub(super) fn new(
        call_identifier: u16,
        state: MsCcState,
        kind: MsCallKind,
        basic_service: BasicServiceInformation,
        simplex_duplex_selection: bool,
        route: CallRoute,
        transmission_request_allowed: bool,
    ) -> Self {
        Self {
            call_identifier,
            state,
            kind,
            basic_service,
            current_speaker_ssi: None,
            tx_grant_state: MsTxGrantState::None,
            transmission_request_allowed,
            timers: MsCallTimers::default(),
            disconnect_cause: None,
            last_uplane: None,
            rx_speech_frames: 0,
            route,
            simplex_duplex_selection,
            hook_on_off: false,
            pending_tx_request: false,
            uplane_before_wait: None,
        }
    }
}
