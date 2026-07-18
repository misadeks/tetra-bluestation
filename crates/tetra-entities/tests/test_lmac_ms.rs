mod common;

use tetra_config::bluestation::StackMode;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, BurstType, PhyBlockNum, PhyBlockType, PhysicalChannel, Sap, TdmaTime, TrainingSequence, debug};
use tetra_saps::sapmsg::{SapMsg, SapMsgInner};
use tetra_saps::tmv::{TmvUnitdataReq, TmvUnitdataReqSlot, enums::logical_chans::LogicalChannel};
use tetra_saps::tp::TpUnitdataInd;
use tetra_entities::lmac::components::errorcontrol;

use crate::common::ComponentTest;

/// Build a deterministic type-1 MAC block of `nbits` bits.
fn make_block(nbits: usize) -> BitBuffer {
    let bits: Vec<u8> = (0..nbits).map(|i| (((i * 5 + 1) % 3) & 1) as u8).collect();
    BitBuffer::from_bitarr(&bits)
}

/// Extract the raw bits of a BitBuffer into a Vec for comparison.
fn bits_of(buf: &BitBuffer, nbits: usize) -> Vec<u8> {
    let mut buf = buf.clone();
    buf.seek(0);
    let mut out = vec![0u8; nbits];
    buf.to_bitarr(&mut out);
    out
}

/// Drive a single TMV-UNITDATA request carrying one uplink MAC block through
/// LmacMs and return the encoded TP-UNITDATA request slot it emits to PHY.
fn encode_uplink(lchan: LogicalChannel, mac_block: BitBuffer, scrambling_code: u32) -> tetra_saps::tp::TpUnitdataReqSlot {
    let mut test = ComponentTest::new(StackMode::Ms, None);
    test.populate_entities(vec![TetraEntity::Lmac], vec![TetraEntity::Phy]);

    let m = SapMsg {
        sap: Sap::TmvSap,
        src: TetraEntity::Umac,
        dest: TetraEntity::Lmac,
        msg: SapMsgInner::TmvUnitdataReq(TmvUnitdataReqSlot {
            ts: TdmaTime::default(),
            ul_phy_chan: PhysicalChannel::Cp,
            blk1: Some(TmvUnitdataReq {
                mac_block,
                logical_channel: lchan,
                scrambling_code,
            }),
            blk2: None,
            bbk: None,
        }),
    };

    test.submit_message(m);
    test.deliver_all_messages();
    let mut sink_msgs = test.dump_sinks();

    assert_eq!(sink_msgs.len(), 1, "LmacMs should emit exactly one TP request");
    let msg = sink_msgs.remove(0);
    assert_eq!(msg.sap, Sap::TpSap);
    assert_eq!(msg.dest, TetraEntity::Phy);
    let SapMsgInner::TpUnitdataReq(slot) = msg.msg else {
        panic!("expected TpUnitdataReq, got {:?}", msg.msg);
    };
    slot
}

/// Wrap an encoded type-5 block in a TP-UNITDATA indication for decode_cp.
fn tp_ind(type5: BitBuffer, block_type: PhyBlockType) -> TpUnitdataInd {
    TpUnitdataInd {
        train_type: TrainingSequence::NotFound,
        burst_type: BurstType::CUB,
        block_type,
        block_num: PhyBlockNum::Block1,
        block: type5,
    }
}

#[test]
/// SCH/HU uplink (MAC-ACCESS carrier): LmacMs must encode the 92-bit type-1
/// block to a 168-bit type-5 CUB block and select the Control Uplink Burst with
/// the extended training sequence. Round-trip through decode_cp recovers it.
fn test_lmac_ms_tx_schhu_cub() {
    debug::setup_logging_verbose();

    let scrambling_code = 1761749767;
    let mac_block = make_block(92);
    let orig_bits = bits_of(&mac_block, 92);

    let slot = encode_uplink(LogicalChannel::SchHu, mac_block, scrambling_code);

    assert_eq!(slot.burst_type, BurstType::CUB);
    assert_eq!(slot.train_type, TrainingSequence::ExtendedTrainSeq);
    assert!(slot.bbk.is_none());
    assert!(slot.blk2.is_none());
    let type5 = slot.blk1.expect("blk1 present");
    assert_eq!(type5.get_len(), 168, "SCH/HU type-5 block is 168 bits");

    let (decoded, crc_ok) = errorcontrol::decode_cp(LogicalChannel::SchHu, tp_ind(type5, PhyBlockType::SSN1), Some(scrambling_code));
    assert!(crc_ok, "CRC must pass on round-trip");
    assert_eq!(bits_of(&decoded.unwrap(), 92), orig_bits, "recovered type-1 block must match original");
}

#[test]
/// SCH/F uplink (full-slot signalling): LmacMs must encode the 268-bit type-1
/// block to a 432-bit type-5 NUB block and select the Normal Uplink Burst with
/// normal training sequence 1. Round-trip through decode_cp recovers it.
fn test_lmac_ms_tx_schf_nub() {
    debug::setup_logging_verbose();

    let scrambling_code = 1761749767;
    let mac_block = make_block(268);
    let orig_bits = bits_of(&mac_block, 268);

    let slot = encode_uplink(LogicalChannel::SchF, mac_block, scrambling_code);

    assert_eq!(slot.burst_type, BurstType::NUB);
    assert_eq!(slot.train_type, TrainingSequence::NormalTrainSeq1);
    assert!(slot.bbk.is_none());
    assert!(slot.blk2.is_none());
    let type5 = slot.blk1.expect("blk1 present");
    assert_eq!(type5.get_len(), 432, "SCH/F type-5 block is 432 bits");

    let (decoded, crc_ok) = errorcontrol::decode_cp(LogicalChannel::SchF, tp_ind(type5, PhyBlockType::NUB), Some(scrambling_code));
    assert!(crc_ok, "CRC must pass on round-trip");
    assert_eq!(bits_of(&decoded.unwrap(), 268), orig_bits, "recovered type-1 block must match original");
}
