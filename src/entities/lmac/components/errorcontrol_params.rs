use crate::saps::tmv::enums::logical_chans::LogicalChannel;

/// Each LogicalChannel is associated with a set of error control parameters.
#[derive(Debug)]
pub struct ErrorControlParams {
    // pub name:           &'static str,
    pub type345_bits:   usize,
    pub type2_bits:     usize,
    pub type1_bits:     usize,
    pub interleave_a:   usize,
    pub have_crc16:     bool,
}

/// Parameters for the BSCH (Broadcast Synchronization Channel)
pub const BSCH_PARAMS: ErrorControlParams = ErrorControlParams {
    type345_bits: 120,
    type2_bits:   80,
    type1_bits:   60,
    interleave_a: 11,
    have_crc16:   true,
};

/// Parameters for the SCH/HD (half slot) signalling channel, also for STCH and BNCH
pub const SCH_HD_PARAMS: ErrorControlParams = ErrorControlParams {
    type345_bits: 216,
    type2_bits:   144,
    type1_bits:   124,
    interleave_a: 101,
    have_crc16:   true,
};

/// Parameters for the BBK (Broadcast Block) channel, used for AACH
pub const AACH_PARAMS: ErrorControlParams = ErrorControlParams {
    type345_bits: 30,
    type2_bits:   30,
    type1_bits:   14,
    interleave_a: 0,        // No interleaving
    have_crc16:   false,
};

/// Parameters for the SCH/F channel
pub const SCH_F_PARAMS: ErrorControlParams = ErrorControlParams {
    type345_bits: 432,
    type2_bits:   288,
    type1_bits:   268,
    interleave_a: 103,
    have_crc16:   true,
};

/// Placeholder parameters for traffic channels (TCH).
/// TODO: Verify per-channel coding/interleaving for PDCH/traffic against spec.
pub const TCH_F_PARAMS: ErrorControlParams = ErrorControlParams {
    type345_bits: 432,
    type2_bits:   288,
    type1_bits:   268,
    interleave_a: 103,
    have_crc16:   true,
};

/// Parameters for the SCH/HU (half slot uplink, Control Uplink Burst) channel
pub const SCH_HU_PARAMS: ErrorControlParams = ErrorControlParams {
    type345_bits: 168,
    type2_bits:   112,
    type1_bits:   92,
    interleave_a: 13,
    have_crc16:   true,
};

/// Gets error control parameters for a given DL logical channel.
pub fn get_params(lchan: LogicalChannel) -> &'static ErrorControlParams {

    match lchan {
        LogicalChannel::Bsch => {
            &BSCH_PARAMS
        }
        LogicalChannel::SchHd | 
        LogicalChannel::Stch | 
        LogicalChannel::Bnch => {
            &SCH_HD_PARAMS
        }
        LogicalChannel::Aach => {
            &AACH_PARAMS
        }
        LogicalChannel::SchF => {
            &SCH_F_PARAMS
        }
        LogicalChannel::SchHu => {
            &SCH_HU_PARAMS
        }
        
        LogicalChannel::TchS => &TCH_F_PARAMS,
        LogicalChannel::Tch24 => &TCH_F_PARAMS,
        LogicalChannel::Tch48 => &TCH_F_PARAMS,
        LogicalChannel::Tch72 => &TCH_F_PARAMS,

        LogicalChannel::Blch => unimplemented!(),
        LogicalChannel::Clch => unimplemented!(),
    }
}
