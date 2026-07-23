mod common;

use tetra_config::bluestation::StackMode;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, BurstType, PhyBlockNum, PhyBlockType, PhysicalChannel, Sap, TdmaTime, TrainingSequence, debug};
use tetra_saps::sapmsg::{SapMsg, SapMsgInner};
use tetra_saps::tmv::{TmvConfigureReq, TmvUnitdataReq, TmvUnitdataReqSlot, enums::logical_chans::LogicalChannel};
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
            reserved_access: false,
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
        time: TdmaTime::default(),
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

#[test]
/// D-2 (tune plumbing): a TMV-TUNE from UMAC is forwarded down to the PHY as a
/// TPC-TUNE carrying the same carrier (LMAC holds no radio-tuning state).
fn test_lmac_ms_forwards_tune_to_phy() {
    let mut test = ComponentTest::new(StackMode::Ms, None);
    test.populate_entities(vec![TetraEntity::Lmac], vec![TetraEntity::Phy]);

    let m = SapMsg {
        sap: Sap::TmvSap,
        src: TetraEntity::Umac,
        dest: TetraEntity::Lmac,
        msg: SapMsgInner::TmvTuneReq(tetra_saps::tmv::TmvTuneReq { carrier_hz: 396_000_000 }),
    };
    test.submit_message(m);
    test.deliver_all_messages();
    let mut sink_msgs = test.dump_sinks();

    assert_eq!(sink_msgs.len(), 1, "LmacMs should emit exactly one TPC request");
    let msg = sink_msgs.remove(0);
    assert_eq!(msg.sap, Sap::TpcSap);
    assert_eq!(msg.dest, TetraEntity::Phy);
    let SapMsgInner::TpcTuneReq(req) = msg.msg else {
        panic!("expected TpcTuneReq, got {:?}", msg.msg);
    };
    assert_eq!(req.carrier_hz, 396_000_000);
}

// ─────────────────────────────────────────────────────────────────────────
// M1 — MS downlink TCH/S (traffic) reception
// ─────────────────────────────────────────────────────────────────────────

/// Deterministic 274-bit ACELP type-1 speech frame (TCH/S, cl. 8.2.1).
fn make_speech_frame() -> BitBuffer {
    let bits: Vec<u8> = (0..274).map(|i| (((i * 7 + 3) % 5) & 1) as u8).collect();
    BitBuffer::from_bitarr(&bits)
}

/// Encode a type-1 block into an on-air type-5 block for the given control
/// channel (SCH/F, STCH, …) using the LMAC error-control path.
fn encode_cp_block(lchan: LogicalChannel, mac_block: BitBuffer, scrambling_code: u32) -> BitBuffer {
    errorcontrol::encode_cp(TmvUnitdataReq {
        mac_block,
        logical_channel: lchan,
        scrambling_code,
    })
}

/// Build a downlink TP-UNITDATA indication for a received full/half slot.
fn dl_tp_ind(block: BitBuffer, block_num: PhyBlockNum, time: TdmaTime) -> TpUnitdataInd {
    TpUnitdataInd {
        train_type: TrainingSequence::NotFound,
        burst_type: BurstType::NDB,
        block_type: PhyBlockType::NDB,
        block_num,
        block,
        time,
    }
}

/// Drive LmacMs (MS) with a TMV-CONFIGURE (scrambling code / traffic / stealing
/// state) followed by a received downlink burst, and return everything it
/// forwards up to the UMAC.
fn rx_downlink(configure: TmvConfigureReq, ind: TpUnitdataInd) -> Vec<SapMsg> {
    let mut test = ComponentTest::new(StackMode::Ms, None);
    test.populate_entities(vec![TetraEntity::Lmac], vec![TetraEntity::Umac]);

    test.submit_message(SapMsg {
        sap: Sap::TmvSap,
        src: TetraEntity::Umac,
        dest: TetraEntity::Lmac,
        msg: SapMsgInner::TmvConfigureReq(configure),
    });
    test.deliver_all_messages();

    test.submit_message(SapMsg {
        sap: Sap::TpSap,
        src: TetraEntity::Phy,
        dest: TetraEntity::Lmac,
        msg: SapMsgInner::TpUnitdataInd(ind),
    });
    test.deliver_all_messages();

    test.dump_sinks()
}

#[test]
/// M1: a full-slot TCH/S burst received on a *non-control* timeslot is decoded
/// to speech and delivered up the TMD-SAP tagged with the timeslot it arrived
/// on (cl. 8.2 channel coding / cl. 23.4.2 traffic channels). The assigned
/// traffic channel generally lives on a different timeslot than the MCCH, so the
/// per-burst slot — not the maintained control-channel clock — is the tag.
fn test_lmac_ms_rx_tchs_delivers_speech_on_traffic_timeslot() {
    debug::setup_logging_verbose();

    let scrambling_code = 1761749767;
    let frame = make_speech_frame();
    let frame_bits = bits_of(&frame, 274);
    let type5 = errorcontrol::encode_tp(
        TmvUnitdataReq {
            mac_block: frame,
            logical_channel: LogicalChannel::TchS,
            scrambling_code,
        },
        1,
    );
    assert_eq!(type5.get_len(), 432, "TCH/S type-5 block is 432 bits");

    // Traffic on timeslot 3 (the control channel is camped on timeslot 1).
    let traffic_ts = TdmaTime { t: 3, f: 1, m: 1, h: 0 };

    let configure = TmvConfigureReq {
        scrambling_code: Some(scrambling_code),
        is_traffic: Some(true),
        ..Default::default()
    };
    let mut out = rx_downlink(configure, dl_tp_ind(type5, PhyBlockNum::Both, traffic_ts));

    assert_eq!(out.len(), 1, "exactly one speech frame forwarded to UMAC");
    let msg = out.remove(0);
    assert_eq!(msg.sap, Sap::TmdSap);
    assert_eq!(msg.dest, TetraEntity::Umac);
    let SapMsgInner::TmdCircuitDataInd(ind) = msg.msg else {
        panic!("expected TmdCircuitDataInd, got {:?}", msg.msg);
    };
    assert_eq!(ind.ts, traffic_ts.t, "speech tagged with the traffic timeslot");
    assert_eq!(ind.data, frame_bits, "decoded ACELP frame must match the transmitted one");
}

#[test]
/// M1: a stolen half-slot (STCH) on a traffic burst is routed to the signalling
/// decode path, NOT to the vocoder (cl. 23 channel stealing). It must surface as
/// a control-plane TMV-UNITDATA (SCH/HD coding) and never as circuit speech.
fn test_lmac_ms_rx_stch_routes_to_signalling_not_speech() {
    debug::setup_logging_verbose();

    let scrambling_code = 1761749767;
    // 124-bit SCH/HD type-1 block carried by the stolen half-slot.
    let stolen: Vec<u8> = (0..124).map(|i| (i % 2) as u8).collect();
    let type5 = encode_cp_block(LogicalChannel::Stch, BitBuffer::from_bitarr(&stolen), scrambling_code);
    assert_eq!(type5.get_len(), 216, "STCH/SCH-HD type-5 block is 216 bits");

    let traffic_ts = TdmaTime { t: 3, f: 1, m: 1, h: 0 };

    // Traffic burst whose second half-slot has been stolen for signalling.
    let configure = TmvConfigureReq {
        scrambling_code: Some(scrambling_code),
        is_traffic: Some(true),
        blk2_stolen: Some(true),
        ..Default::default()
    };
    let out = rx_downlink(configure, dl_tp_ind(type5, PhyBlockNum::Block2, traffic_ts));

    assert!(
        !out.iter().any(|m| matches!(m.msg, SapMsgInner::TmdCircuitDataInd(_))),
        "stolen block must not be decoded as speech"
    );
    assert!(
        out.iter().any(|m| matches!(
            &m.msg,
            SapMsgInner::TmvUnitdataInd(u) if u.logical_channel == LogicalChannel::Stch
        )),
        "stolen block must be delivered on the signalling (STCH) path, got {:?}",
        out
    );
}

#[test]
/// M1 regression: with no traffic active (control timeslot), a full-slot SCH/F
/// burst still decodes as signalling and is delivered up the TMV-SAP — the new
/// traffic branch must not hijack the control-plane receive path.
fn test_lmac_ms_control_slot_decode_unaffected() {
    debug::setup_logging_verbose();

    let scrambling_code = 1761749767;
    let mac_block = make_block(268);
    let orig = bits_of(&mac_block, 268);
    let type5 = encode_cp_block(LogicalChannel::SchF, mac_block, scrambling_code);

    let control_ts = TdmaTime { t: 1, f: 3, m: 1, h: 0 };

    // is_traffic defaults to false (control channel).
    let configure = TmvConfigureReq {
        scrambling_code: Some(scrambling_code),
        ..Default::default()
    };
    let mut out = rx_downlink(configure, dl_tp_ind(type5, PhyBlockNum::Both, control_ts));

    assert!(
        !out.iter().any(|m| matches!(m.msg, SapMsgInner::TmdCircuitDataInd(_))),
        "control-plane burst must not be decoded as speech"
    );
    assert_eq!(out.len(), 1, "exactly one signalling block forwarded to UMAC");
    let msg = out.remove(0);
    assert_eq!(msg.sap, Sap::TmvSap);
    let SapMsgInner::TmvUnitdataInd(u) = msg.msg else {
        panic!("expected TmvUnitdataInd, got {:?}", msg.msg);
    };
    assert_eq!(u.logical_channel, LogicalChannel::SchF);
    assert!(u.crc_pass, "SCH/F round-trip CRC must pass");
    assert_eq!(bits_of(&u.pdu, 268), orig, "recovered SCH/F block must match original");
}
