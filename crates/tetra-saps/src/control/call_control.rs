use tetra_core::Direction;

use crate::control::enums::circuit_mode_type::CircuitModeType;

#[derive(Debug, Clone)]
pub struct Circuit {
    /// Direction
    pub direction: Direction,

    /// Timeslot in which this circuit exists
    pub ts: u8,

    /// Usage number, between 4 and 63
    pub usage: u8,

    /// Traffic channel type
    pub circuit_mode: CircuitModeType,

    // pub comm_type: CommunicationType,

    // pub simplex_duplex: bool,

    // pub slots_per_frame: Option<u8>, // only relevant for circuit data
    /// 2 opt, 00 = TETRA encoded speech, 1|2 = reserved, 3 = proprietary
    pub speech_service: Option<u8>,
    /// Whether end-to-end encryption is enabled on this circuit
    pub etee_encrypted: bool,
}

#[derive(Debug)]
pub enum CallControl {
    /// Signals to set up a circuit
    /// Created by CMCE, sent to Umac
    /// Umac forwards to Lmac
    Open(Circuit),
    /// Signals to release a circuit
    /// Created by CMCE, sent to Umac
    /// Umac forwards to Lmac
    /// Contains (Direction, timeslot) of associated circuit
    Close(Direction, u8),
    /// Notifies Brew entity about a locally-initiated UL group call.
    /// Brew checks if the GSSI is subscribed and starts forwarding voice to TetraPack.
    LocalCallStart {
        call_id: u16,
        source_issi: u32,
        dest_gssi: u32,
        ts: u8,
    },
    /// Notifies Brew that transmission stopped on a local call (entering hangtime).
    /// Brew should immediately stop forwarding audio to TetraPack.
    LocalCallTxStopped { call_id: u16, ts: u8 },
    /// Notifies Brew entity that a local UL call has ended
    LocalCallEnd { call_id: u16, ts: u8 },
    /// Request CMCE to start a network-initiated group call
    /// Sent by Brew when TetraPack sends GROUP_TX
    NetworkCallStart {
        brew_uuid: uuid::Uuid, // Brew session UUID for tracking
        source_issi: u32,      // Current speaker
        dest_gssi: u32,        // Target group
        priority: u8,          // Call priority
    },
    /// Notify Brew that network call is ready with allocated resources
    /// Response from CMCE after circuit allocation
    NetworkCallReady {
        brew_uuid: uuid::Uuid, // Matches request
        call_id: u16,          // CMCE-allocated call identifier
        ts: u8,                // Allocated timeslot
        usage: u8,             // Usage number
    },
    /// Request CMCE to end a network call
    /// Sent by Brew when TetraPack sends GROUP_IDLE
    NetworkCallEnd {
        brew_uuid: uuid::Uuid, // Identifies the call to end
    },
}
