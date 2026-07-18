use tetra_core::{BitBuffer, BurstType, PhyBlockNum, PhyBlockType, TdmaTime, TrainingSequence};

#[derive(Debug, Clone)]
pub struct TpUnitdataInd {
    pub train_type: TrainingSequence,
    pub burst_type: BurstType,
    pub block_type: PhyBlockType,
    /// Undefined for BBK. For all others: [ Block1 | Block2 | Both ]
    pub block_num: PhyBlockNum,
    pub block: BitBuffer,
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
}
