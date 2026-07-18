mod common;

use tetra_config::bluestation::StackMode;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, PhyBlockNum, Sap, debug};
use tetra_saps::sapmsg::{SapMsg, SapMsgInner};
use tetra_saps::tmv::{TmvUnitdataInd, enums::logical_chans::LogicalChannel};

use tetra_entities::umac::umac_ms::UmacMs;
use tetra_pdus::umac::pdus::mac_access::MacAccess;

use crate::common::ComponentTest;

#[test]
/// A test containing a single Lmac frame, containing a MAC-RESOURCE with no SDU, and a NULL pdu
fn test_umac_ms() {
    debug::setup_logging_verbose();
    let mut test = ComponentTest::new(StackMode::Ms, None);
    let components = vec![TetraEntity::Umac];
    let sinks: Vec<TetraEntity> = vec![];
    test.populate_entities(components, sinks);

    let m = SapMsg {
        sap: Sap::TmvSap,
        src: TetraEntity::Lmac,
        dest: TetraEntity::Umac,
        msg: SapMsgInner::TmvUnitdataInd(TmvUnitdataInd {
            pdu: BitBuffer::from_bitstr(
                "0010001000110001011010110000101010001010000100000000110000010000100000000000000000000000000000000000000000000000000000000000",
            ),
            block_num: PhyBlockNum::Block1,
            logical_channel: LogicalChannel::SchHd,
            crc_pass: true,
            scrambling_code: 0,
        }),
    };

    // Submit and process message
    test.submit_message(m);
    test.deliver_all_messages();
    let sink_msgs = test.dump_sinks();

    // Evaluate results
    assert_eq!(sink_msgs.len(), 0);
    tracing::warn!("Validation of result not implemented");
}

#[test]
/// A test containing a 3-fragment message, which is reassembled by the UMAC
/// The message ultimately contains an SDS message, which is reconstructed in the CMCE.
/// Also tests the in-between LLC and MLE.  
fn test_umac_frag() {
    debug::setup_logging_verbose();
    let mut test = ComponentTest::new(StackMode::Ms, None);
    let components = vec![TetraEntity::Umac, TetraEntity::Llc, TetraEntity::Mle, TetraEntity::Cmce];
    let sinks = vec![];
    test.populate_entities(components, sinks);

    // NDB 56/18/1/000 type1: 0000000111111001011010110000101001100011000000110100111101011010111110000100110000110000100100011000000000001100010101000000
    // NDB 57/01/1/000 type1: 0111000100110000000000010011001000110000001101000010110000110001010000000000110000010000100000000000000000000000000000000000
    let m = SapMsg {
        sap: Sap::TmvSap,
        src: TetraEntity::Lmac,
        dest: TetraEntity::Umac,
        msg: SapMsgInner::TmvUnitdataInd(TmvUnitdataInd {
            pdu: BitBuffer::from_bitstr(
                "0000000111111001011010110000101001100011000000110100111101011010111110000100110000110000100100011000000000001100010101000000",
            ),
            block_num: PhyBlockNum::Block1,
            logical_channel: LogicalChannel::SchHd,
            crc_pass: true,
            scrambling_code: 0,
        }),
    };
    test.submit_message(m);
    test.deliver_all_messages();

    let m = SapMsg {
        sap: Sap::TmvSap,
        src: TetraEntity::Lmac,
        dest: TetraEntity::Umac,
        msg: SapMsgInner::TmvUnitdataInd(TmvUnitdataInd {
            pdu: BitBuffer::from_bitstr(
                "0111000100110000000000010011001000110000001101000010110000110001010000000000110000010000100000000000000000000000000000000000",
            ),
            block_num: PhyBlockNum::Block1,
            logical_channel: LogicalChannel::SchHd,
            crc_pass: true,
            scrambling_code: 0,
        }),
    };

    test.submit_message(m);
    test.deliver_all_messages();
    let msgs = test.dump_sinks();
    for msg in msgs.iter() {
        tracing::info!("\nSink message: {:?}", msg);
    }

    tracing::warn!("Validation of result not implemented");
}

#[test]
/// A test containing a SYSINFO frame, parsed by UMAC and MLE
fn test_sysinfo() {
    debug::setup_logging_verbose();
    let mut test = ComponentTest::new(StackMode::Ms, None);
    let components = vec![TetraEntity::Umac, TetraEntity::Llc, TetraEntity::Mle];
    let sinks = vec![
        // TetraComponent::Mle
    ];
    test.populate_entities(components, sinks);

    // Sysinfo test
    let m = SapMsg {
        sap: Sap::TmvSap,
        src: TetraEntity::Lmac,
        dest: TetraEntity::Umac,
        msg: SapMsgInner::TmvUnitdataInd(TmvUnitdataInd {
            // mac_block: BitBuffer::from_bitstr("1000001100101010010000000000110001101001011100000000001110001111100000100000000000010111100001100000111111000000110101100111"),
            pdu: BitBuffer::from_bitstr(
                "1000010000111111010001000000100001101001111100000000000000011101000011100000000000000000000000101111111111100101110101110111",
            ),
            block_num: PhyBlockNum::Block2,
            logical_channel: LogicalChannel::Bnch,
            crc_pass: true,
            scrambling_code: 0,
        }),
    };
    test.submit_message(m);
    test.deliver_all_messages();
    let msgs = test.dump_sinks();
    for msg in msgs.iter() {
        tracing::info!("\nSink message: {:?}", msg);
    }

    tracing::warn!("Validation of result not implemented");
}

#[test]
/// A test containing a SYNC frame, parsed by UMAC and MLE
fn test_sync() {
    debug::setup_logging_verbose();
    let mut test = ComponentTest::new(StackMode::Ms, None);
    let components = vec![TetraEntity::Umac, TetraEntity::Llc, TetraEntity::Mle];
    let sinks = vec![TetraEntity::Lmac];
    test.populate_entities(components, sinks);

    // SB1 09/11/4/000 type1: 000100000111010110010010000000001101001000000100010101110011
    // TMB-SAP SYNC CC 000001(0x01) TN 11(4) FN 01011(11) MN 001001( 9) MCC 0110100100(420) MNC 00001000101011(555)
    let m = SapMsg {
        sap: Sap::TmvSap,
        src: TetraEntity::Lmac,
        dest: TetraEntity::Umac,
        msg: SapMsgInner::TmvUnitdataInd(TmvUnitdataInd {
            pdu: BitBuffer::from_bitstr("000100000111010110010010000000001101001000000100010101110011"),
            // pdu: BitBuffer::from_bitstr("000100000111100100111110000000000110011000000000000101111001"),
            block_num: PhyBlockNum::Block1,
            logical_channel: LogicalChannel::Bsch,
            crc_pass: true,
            scrambling_code: 0,
        }),
    };
    test.submit_message(m);
    test.deliver_all_messages();
    let msgs = test.dump_sinks();
    for msg in msgs.iter() {
        tracing::info!("\nSink message: {:?}", msg);
    }

    // The full camp-on-cell loop must have fired end to end:
    //   UMAC parses MAC-SYNC (cl. 21.4.4.2) -> forwards the D-MLE-SYNC SDU to MLE
    //   -> MLE performs initial cell selection (cl. 18.3.4.6) and returns the
    //   valid MCC/MNC over TL-CONFIGURE -> UMAC derives the scrambling code
    //   (cl. 8.2.5 / 23.2.2) and pushes it, plus the recovered time, down to LMAC.
    let mut got_time = false;
    let mut scrambling_code = None;
    for msg in msgs.iter() {
        if let SapMsgInner::TmvConfigureReq(cfg) = &msg.msg {
            if cfg.time.is_some() {
                got_time = true;
            }
            if let Some(sc) = cfg.scrambling_code {
                scrambling_code = Some(sc);
            }
        }
    }

    assert!(got_time, "UMAC should seed LMAC time from the BSCH");
    // Test vector: CC=1, MCC=420, MNC=555
    // scrambling = ((cc | (mnc << 6) | (mcc << 20)) << 2) | 3
    assert_eq!(
        scrambling_code,
        Some(1_761_749_767),
        "UMAC should derive and install the cell scrambling code"
    );
}

#[test]
fn test_resource() {
    debug::setup_logging_verbose();
    let mut test = ComponentTest::new(StackMode::Ms, None);
    let components = vec![TetraEntity::Umac, TetraEntity::Llc, TetraEntity::Mle, TetraEntity::Cmce];
    let sinks = vec![];
    test.populate_entities(components, sinks);

    let m = SapMsg {
        sap: Sap::TmvSap,
        src: TetraEntity::Lmac,
        dest: TetraEntity::Umac,
        msg: SapMsgInner::TmvUnitdataInd(TmvUnitdataInd {
            pdu: BitBuffer::from_bitstr(
                "0010000010001110000000000000000001100101110110001000100110001001010001101100100100011110001110010011000000000001001100111110000000001000000000000001000010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            ),
            block_num: PhyBlockNum::Both,
            logical_channel: LogicalChannel::SchF,
            crc_pass: true,
            scrambling_code: 0,
        }),
    };
    test.submit_message(m);
    test.deliver_all_messages();
    let msgs = test.dump_sinks();
    for msg in msgs.iter() {
        tracing::info!("\nSink message: {:?}", msg);
    }

    tracing::warn!("Validation of result not implemented");
}

/// Slice 3C: verify the MS uplink MAC-ACCESS block builder
/// (ETSI TS 100 392-2 cl. 21.4.2.1). Build a byte-aligned block (no fill bits),
/// decode it back with `MacAccess::from_bitbuf`, and confirm the ISSI, length
/// indication and recovered TM-SDU round-trip.
#[test]
fn test_build_mac_access_block_roundtrip() {
    debug::setup_logging_verbose();

    let issi: u32 = 0x0012_3456;
    // 28-bit SDU: 36-bit header + 28 = 64 bits = 8 octets, exactly byte-aligned.
    let sdu_bits = "0110100100011110001011010010";
    let mut sdu = BitBuffer::from_bitstr(sdu_bits);

    let mut block = UmacMs::build_mac_access_block(issi, &mut sdu).expect("SDU fits a single burst");
    assert_eq!(block.get_len(), 92, "SCH/HU type-1 block is 92 bits");

    let decoded = MacAccess::from_bitbuf(&mut block).expect("decodes");
    let addr = decoded.addr.expect("addressed");
    assert_eq!(addr.ssi, issi, "recovered ISSI matches source");
    assert_eq!(decoded.length_ind, Some(8), "length indication = 8 octets");
    assert_eq!(decoded.fill_bits, false, "no fill bits for byte-aligned content");

    // The buffer cursor now sits at the start of the TM-SDU (after the header).
    assert_eq!(block.get_pos(), 36, "MAC-ACCESS header is 36 bits");
    let mut recovered = vec![0u8; sdu_bits.len()];
    block.to_bitarr(&mut recovered);
    let expected: Vec<u8> = sdu_bits.chars().map(|c| (c == '1') as u8).collect();
    assert_eq!(recovered, expected, "recovered TM-SDU matches source");
}

/// Slice 3C: verify fill-bit insertion when the content is not byte-aligned
/// (ETSI TS 100 392-2 cl. 21.4.2.1 length indication). A 17-bit SDU gives
/// 36 + 17 = 53 bits of content, padded with 3 fill bits to 56 bits (7 octets).
#[test]
fn test_build_mac_access_block_fillbits() {
    debug::setup_logging_verbose();

    let issi: u32 = 0x00AB_CDEF;
    let sdu_bits = "01101001000111100"; // 17 bits
    let mut sdu = BitBuffer::from_bitstr(sdu_bits);

    let mut block = UmacMs::build_mac_access_block(issi, &mut sdu).expect("SDU fits a single burst");
    assert_eq!(block.get_len(), 92);

    let decoded = MacAccess::from_bitbuf(&mut block).expect("decodes");
    assert_eq!(decoded.addr.expect("addressed").ssi, issi);
    assert_eq!(decoded.length_ind, Some(7), "36 + 17 + 3 fill = 56 bits = 7 octets");
    assert_eq!(decoded.fill_bits, true, "fill bits present");

    // SDU sits immediately after the 36-bit header, before the fill bits.
    assert_eq!(block.get_pos(), 36);
    let mut recovered = vec![0u8; sdu_bits.len()];
    block.to_bitarr(&mut recovered);
    let expected: Vec<u8> = sdu_bits.chars().map(|c| (c == '1') as u8).collect();
    assert_eq!(recovered, expected, "recovered TM-SDU matches source");
}

/// Slice 3C: an SDU too large for a single SCH/HU access burst must be rejected
/// (uplink fragmentation, cl. 21.4.3, is not yet implemented). 60 bits of SDU
/// plus the 36-bit header exceeds the 92-bit type-1 block.
#[test]
fn test_build_mac_access_block_oversized() {
    let issi: u32 = 0x0000_0001;
    let sdu_bits = "0".repeat(60); // 36 + 60 = 96 > 92
    let mut sdu = BitBuffer::from_bitstr(&sdu_bits);
    assert!(
        UmacMs::build_mac_access_block(issi, &mut sdu).is_none(),
        "oversized SDU must be rejected (needs fragmentation)"
    );
}
