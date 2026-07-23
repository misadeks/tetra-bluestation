use tetra_core::{BitBuffer, BurstType, PhyBlockNum, PhyBlockType, TdmaTime, TrainingSequence};

#[derive(Debug, Clone)]
pub struct TpUnitdataInd {
    pub train_type: TrainingSequence,
    pub burst_type: BurstType,
    pub block_type: PhyBlockType,
    /// Undefined for BBK. For all others: [ Block1 | Block2 | Both ]
    pub block_num: PhyBlockNum,
    pub block: BitBuffer,

    /// Absolute TDMA time of the slot this burst was demodulated in.
    ///
    /// The downlink demodulator walks every timeslot of the frame (ETSI TS 100
    /// 392-2 cl. 9.3), so a single received frame yields bursts on several
    /// timeslots. The receiving MAC needs the slot number to tell them apart —
    /// in particular to distinguish the control-channel timeslot from an
    /// assigned traffic channel (TCH), which generally lives on a *different*
    /// timeslot of the same carrier. This mirrors the uplink
    /// [`TpUnitdataReqSlot::time`] on the transmit path.
    pub time: TdmaTime,
}

#[derive(Debug, Clone)]
pub struct TpUnitdataReqSlot {
    pub train_type: TrainingSequence,
    pub burst_type: BurstType,
    pub bbk: Option<BitBuffer>,
    pub blk1: Option<BitBuffer>,
    pub blk2: Option<BitBuffer>,
    /// Absolute TDMA time of the slot the burst must be transmitted in.
    ///
    /// The MS uplink is scheduled at an explicit slot recovered from the
    /// downlink (the granted opportunity, ETSI TS 100 392-2 cl. 23.5), so the
    /// upper MAC/LMAC set this to `Some(ul_time)` and the PHY schedules TX at
    /// that hardware time. The BS downlink derives its own TX time from the
    /// stack clock, so it leaves this `None`.
    pub time: Option<TdmaTime>,

    /// `true` when this uplink burst is a **reserved-access** transmission that
    /// must land in exactly the granted slot (see
    /// [`crate::tmv::TmvUnitdataReqSlot::reserved_access`]). The MS PHY
    /// transmits a reserved burst only if the granted slot is still reachable
    /// ahead of the TX generation frontier; if it would have to be moved to a
    /// later slot it is dropped instead (a later slot is not reserved and the
    /// BS would reject it). `false` for contention bursts and BS downlink.
    pub reserved_access: bool,
}
