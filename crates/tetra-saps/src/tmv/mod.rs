pub mod enums;

use tetra_core::{BitBuffer, PhyBlockNum, PhysicalChannel, TdmaTime, Todo};

use crate::tmv::enums::logical_chans::LogicalChannel;

// The TMV-UNITDATA request primitive shall be used to request the lower MAC to transmit a MAC block
#[derive(Debug, Clone)]
pub struct TmvUnitdataReq {
    pub mac_block: BitBuffer,
    pub logical_channel: LogicalChannel,
    pub scrambling_code: u32,
}

/// MS only — runtime downlink retune request (**[impl policy]**).
///
/// Upper-MAC -> lower-MAC hop of the MLE-owned cell-selection / scan retune
/// path (MLE -> UMAC (TLMC) -> LMAC (TMV) -> PHY (TPC)). LMAC forwards it to the
/// PHY as a `TpcTuneReq`. Not an over-the-air primitive.
#[derive(Debug, Clone)]
pub struct TmvTuneReq {
    /// Absolute downlink centre frequency to tune to, in Hz.
    pub carrier_hz: u32,
}

/// MS only — runtime uplink (TX) retune request (**[impl policy]**).
///
/// Upper-MAC -> lower-MAC hop of the camp-time uplink derivation: once the MS
/// camps on a cell, UMAC derives the uplink carrier from the cell's broadcast
/// D-MLE-SYSINFO parameters (band + main carrier + duplex spacing resolved
/// through the programmed duplex table, EN 300 392-2 cl. 18.4.2.2 / cl. 21.4.4)
/// and requests the lower MAC retune the transmitter. LMAC forwards it to the
/// PHY as a `TpcTxTuneReq`. Not an over-the-air primitive.
#[derive(Debug, Clone)]
pub struct TmvTxTuneReq {
    /// Absolute uplink centre frequency to tune the transmitter to, in Hz.
    pub carrier_hz: u32,
}

#[derive(Debug, Clone)]
pub struct TmvUnitdataReqSlot {
    /// Timeslot at which this block is to be transmitted
    pub ts: TdmaTime,
    pub ul_phy_chan: PhysicalChannel,

    /// First MAC block in this timeslot. May be received from LLC
    /// If none was received, UMAC auto-generates a SYNC SB1 broadcast block
    /// Can either fill a subslot or a full slot, depending on logical channel
    pub blk1: Option<TmvUnitdataReq>,

    /// Second MAC block, if blk1 is half-slot. May be received from LLC
    /// If none was received, UMAC auto-generates a SYSINFO block
    /// Can only be present if blk1 is not a full slot
    pub blk2: Option<TmvUnitdataReq>,

    /// The BBK block. We might consider letting the LMAC generate this automatically.
    pub bbk: Option<TmvUnitdataReq>,

    /// `true` when this uplink burst is a **reserved-access** transmission (the
    /// BS reserved this exact slot in response to a capacity request, e.g. the
    /// MAC-END-HU completing an uplink fragmentation, ETSI TS 100 392-2
    /// cl. 23.5.2.2.2). A reserved burst must be transmitted in exactly the
    /// granted slot; unlike contention random access it may not be moved to a
    /// later occurrence. `false` for random-access / contention bursts and for
    /// all BS downlink transmissions.
    pub reserved_access: bool,
}

/// The TMV-UNITDATA indication primitive shall be used by the lower MAC to deliver a received MAC block;
#[derive(Debug, Clone)]
pub struct TmvUnitdataInd {
    pub pdu: BitBuffer,

    /// While not in the spec, the Umac needs to know which block this is.
    /// For instance, in order to determine the owner of a UL halfslot containing a MAC-FRAG (which doesn't contain an SSI field)
    pub block_num: PhyBlockNum,

    pub logical_channel: LogicalChannel,

    /// If no CRC is present on this message type (for example, for AACH), crc_pass is set to True
    pub crc_pass: bool,
    pub scrambling_code: u32,
}

/// Clause 23.2.1
/// The TMV-CONFIGURE primitive shall be used to provide the lower MAC with information about the configuration
/// of the channel or about the format of a received slot.

#[derive(Debug, Clone, Default)]
pub struct TmvConfigureReq {
    // pub channel_info: Option<Todo>,
    /// Received from umac upon change of network information
    pub scrambling_code: Option<u32>,
    // Energy economy or part-time reception or napping information
    // pub energy_economy_info: Option<Todo>,
    pub is_traffic: Option<bool>,
    /// Used by Umac to signal Lmac that the second half of the slot is stolen
    pub blk2_stolen: Option<bool>,
    pub tch_type_and_interleaving_depth: Option<Todo>,
    // pub monitoring_pattern_info: Option<Todo>,
    /// NOTE time not usually passed down but convenient for detecting fr18 etc.
    pub time: Option<TdmaTime>,
}

#[derive(Debug, Clone)]
pub struct TmvConfigureConf {
    pub channel_info: Todo,
}
