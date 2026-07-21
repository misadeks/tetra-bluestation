mod common;

use tetra_config::bluestation::StackMode;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, PhyBlockNum, Sap, debug};
use tetra_saps::sapmsg::{SapMsg, SapMsgInner};
use tetra_saps::tmv::{TmvUnitdataInd, enums::logical_chans::LogicalChannel};

use tetra_entities::umac::umac_ms::UmacMs;
use tetra_pdus::umac::enums::reservation_requirement::ReservationRequirement;
use tetra_pdus::umac::pdus::mac_access::MacAccess;
use tetra_pdus::umac::pdus::mac_end_hu::MacEndHu;

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
    // The SYNC test vector below advertises MCC=420, MNC=555. Radio-style cell
    // selection (cl. 18.3.4) now only camps on an allowed network, so program
    // that network as the home network for this test.
    let mut config = ComponentTest::get_default_test_config(StackMode::Ms);
    config.net.mcc = 420;
    config.net.mnc = 555;
    let mut test = ComponentTest::from_config(config, None);
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

/// Regression test: a MAC-RESOURCE null PDU (length indication 00000 2,
/// cl. 21.4.3.1 / Table 21.55) must be handled as downlink filler and dropped,
/// not panic. An all-zero SCH/F block decodes as MAC-RESOURCE (mac_pdu_type 00)
/// with length_ind 0 and address type "null PDU"; before the fix this hit the
/// `Invalid length_ind 0` panic in `rx_mac_resource`.
#[test]
fn test_resource_null_pdu_does_not_panic() {
    debug::setup_logging_verbose();
    let mut test = ComponentTest::new(StackMode::Ms, None);
    let components = vec![TetraEntity::Umac, TetraEntity::Llc, TetraEntity::Mle, TetraEntity::Cmce];
    let sinks = vec![];
    test.populate_entities(components, sinks);

    // 268-bit SCH/F block, all zeros: mac_pdu_type=00 (MAC-RESOURCE),
    // fill_bits=0, pos_of_grant=0, encryption_mode=00, random_access_flag=0,
    // length_ind=000000 (null PDU), addr_type=000 (null PDU).
    let m = SapMsg {
        sap: Sap::TmvSap,
        src: TetraEntity::Lmac,
        dest: TetraEntity::Umac,
        msg: SapMsgInner::TmvUnitdataInd(TmvUnitdataInd {
            pdu: BitBuffer::from_bitstr(&"0".repeat(268)),
            block_num: PhyBlockNum::Both,
            logical_channel: LogicalChannel::SchF,
            crc_pass: true,
            scrambling_code: 0,
        }),
    };
    test.submit_message(m);
    // Must not panic; the null PDU is dropped and produces no LLC delivery.
    test.deliver_all_messages();
}
/// (ETSI TS 100 392-2 cl. 21.4.2.1). Build a block, decode it back with
/// `MacAccess::from_bitbuf`, and confirm the ISSI, the absence of a length
/// indication, and the recovered TM-SDU round-trip. A self-contained
/// random-access MAC-ACCESS carries no length indication (cl. 21.4.2.1); the
/// PDU implicitly fills the MAC block and the remaining capacity is completed
/// with fill bits (cl. 23.4.2.2).
#[test]
fn test_build_mac_access_block_roundtrip() {
    debug::setup_logging_verbose();

    let issi: u32 = 0x0012_3456;
    // 28-bit SDU: 30-bit header + 28 = 58 bits of content, filled to 92 bits.
    let sdu_bits = "0110100100011110001011010010";
    let mut sdu = BitBuffer::from_bitstr(sdu_bits);

    let mut block = UmacMs::build_mac_access_block(issi, &mut sdu).expect("SDU fits a single burst");
    assert_eq!(block.get_len(), 92, "SCH/HU type-1 block is 92 bits");

    let decoded = MacAccess::from_bitbuf(&mut block).expect("decodes");
    let addr = decoded.addr.expect("addressed");
    assert_eq!(addr.ssi, issi, "recovered ISSI matches source");
    assert_eq!(decoded.length_ind, None, "no length indication (cl. 21.4.2.1)");
    assert_eq!(decoded.frag_flag, None, "no capacity request / fragmentation flag");
    assert_eq!(decoded.reservation_req, None, "no reservation requirement");
    assert_eq!(decoded.fill_bits, true, "fill bits complete the MAC block");

    // The buffer cursor now sits at the start of the TM-SDU (after the header).
    assert_eq!(block.get_pos(), 30, "MAC-ACCESS header is 30 bits (no optional field)");
    let mut recovered = vec![0u8; sdu_bits.len()];
    block.to_bitarr(&mut recovered);
    let expected: Vec<u8> = sdu_bits.chars().map(|c| (c == '1') as u8).collect();
    assert_eq!(recovered, expected, "recovered TM-SDU matches source");
}

/// Slice 3C: verify the fill-bit completion rule (ETSI TS 100 392-2
/// cl. 23.4.2.2). With no length indication the MAC-ACCESS implicitly spans the
/// whole MAC block; the fill bits are a single "1" immediately after the TM-SDU
/// followed by "0"s to the end of the block. A 17-bit SDU gives 30 + 17 = 47
/// bits of content, so the fill marker sits at bit 47 and bits 48..92 are zero.
#[test]
fn test_build_mac_access_block_fillbits() {
    debug::setup_logging_verbose();

    let issi: u32 = 0x00AB_CDEF;
    let sdu_bits = "01101001000111100"; // 17 bits
    let mut sdu = BitBuffer::from_bitstr(sdu_bits);

    let mut block = UmacMs::build_mac_access_block(issi, &mut sdu).expect("SDU fits a single burst");
    assert_eq!(block.get_len(), 92);

    // Fill-bit structure (cl. 23.4.2.2): "1" right after the TM-SDU, then zeros.
    let marker_pos = 30 + sdu_bits.len(); // header + TM-SDU
    assert_eq!(
        block.peek_bits_startoffset(marker_pos, 1).unwrap(),
        1,
        "fill marker '1' immediately follows the TM-SDU"
    );
    for i in (marker_pos + 1)..92 {
        assert_eq!(block.peek_bits_startoffset(i, 1).unwrap(), 0, "fill bits are zero to the block end");
    }

    let decoded = MacAccess::from_bitbuf(&mut block).expect("decodes");
    assert_eq!(decoded.addr.expect("addressed").ssi, issi);
    assert_eq!(decoded.length_ind, None, "no length indication (cl. 21.4.2.1)");
    assert_eq!(decoded.fill_bits, true, "fill bits present");

    // SDU sits immediately after the 30-bit header, before the fill bits.
    assert_eq!(block.get_pos(), 30);
    let mut recovered = vec![0u8; sdu_bits.len()];
    block.to_bitarr(&mut recovered);
    let expected: Vec<u8> = sdu_bits.chars().map(|c| (c == '1') as u8).collect();
    assert_eq!(recovered, expected, "recovered TM-SDU matches source");
}

/// Slice 3C: an SDU too large for a single self-contained SCH/HU access burst
/// must be rejected by `build_mac_access_block` (63 bits of SDU plus the 30-bit
/// header exceeds the 92-bit type-1 block). Such SDUs are instead fragmented via
/// `build_mac_access_frag_start` + `build_mac_end_hu_block` (cl. 23.4.2.1.2).
#[test]
fn test_build_mac_access_block_oversized() {
    let issi: u32 = 0x0000_0001;
    let sdu_bits = "0".repeat(63); // 30 + 63 = 93 > 92
    let mut sdu = BitBuffer::from_bitstr(&sdu_bits);
    assert!(
        UmacMs::build_mac_access_block(issi, &mut sdu).is_none(),
        "oversized SDU must be rejected by the self-contained path (needs fragmentation)"
    );
}

/// A pseudo-random bit string of the given length (deterministic).
fn bitstr(len: usize) -> String {
    let mut s = String::with_capacity(len);
    let mut x: u32 = 0x9E37_79B9;
    for _ in 0..len {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        s.push(if (x >> 31) & 1 == 1 { '1' } else { '0' });
    }
    s
}

/// Uplink MAC fragmentation (ETSI TS 100 392-2 cl. 23.4.2.1.2, form i): an
/// oversized TM-SDU is split into a MAC-ACCESS "start of fragmentation"
/// (carrying a capacity request) transmitted by random access, and a MAC-END-HU
/// remainder transmitted on a granted subslot. Build both blocks, decode them
/// back through the base station's own PDU parsers, and confirm the two
/// fragments reassemble to exactly the original TM-SDU.
#[test]
fn test_uplink_fragmentation_roundtrip() {
    debug::setup_logging_verbose();

    let issi: u32 = 0x0012_D687; // 1234567
    // 96-bit SDU (e.g. an ITSI-attach demand with a group attachment). The
    // first fragment carries 56 bits (fills the 92-bit block after the 36-bit
    // frag-start header); the 40-bit remainder goes in the MAC-END-HU.
    let sdu_bits = bitstr(96);
    let expected: Vec<u8> = sdu_bits.chars().map(|c| (c == '1') as u8).collect();
    let mut sdu = BitBuffer::from_bitstr(&sdu_bits);

    let (mut frag_block, mut remainder) =
        UmacMs::build_mac_access_frag_start(issi, &mut sdu).expect("oversized SDU fragments");
    assert_eq!(frag_block.get_len(), 92, "frag-start is a 92-bit type-1 block");
    assert_eq!(remainder.get_len(), 40, "remainder = 96 - 56 first-fragment bits");

    // Decode the frag-start MAC-ACCESS with the BS's own parser.
    let frag_pdu = MacAccess::from_bitbuf(&mut frag_block).expect("frag-start decodes");
    assert_eq!(frag_pdu.addr.expect("addressed").ssi, issi, "frag-start ISSI matches");
    assert_eq!(frag_pdu.frag_flag, Some(true), "start-of-fragmentation flag set");
    assert_eq!(
        frag_pdu.reservation_req,
        Some(ReservationRequirement::Req1Subslot),
        "capacity request asks for one subslot"
    );
    assert_eq!(frag_pdu.length_ind, None, "frag-start carries no length indication");
    assert_eq!(frag_pdu.fill_bits, false, "first fragment fills the block, no fill bits");
    // Header is 36 bits (ISSI + optional field flag + capacity request); the
    // first 56 TM-SDU bits follow.
    assert_eq!(frag_block.get_pos(), 36, "frag-start header is 36 bits");
    let mut first = vec![0u8; 56];
    frag_block.to_bitarr(&mut first);
    assert_eq!(first, expected[..56], "first fragment carries the first 56 SDU bits");

    // Build and decode the MAC-END-HU remainder.
    let mut end_block = UmacMs::build_mac_end_hu_block(&mut remainder).expect("remainder fits one MAC-END-HU");
    assert_eq!(end_block.get_len(), 92, "MAC-END-HU is a 92-bit type-1 block");
    let end_pdu = MacEndHu::from_bitbuf(&mut end_block).expect("MAC-END-HU decodes");
    // 7-bit header + 40-bit SDU = 47 content bits; padded to 48 (6 octets) with
    // a single fill bit (cl. 23.4.2.2).
    assert_eq!(end_pdu.length_ind, Some(6), "length indication = 6 octets");
    assert_eq!(end_pdu.fill_bits, true, "one fill bit pads to the octet boundary");
    assert_eq!(end_pdu.reservation_req, None, "final fragment carries no reservation requirement");
    assert_eq!(end_block.get_pos(), 7, "MAC-END-HU header is 7 bits");
    let mut rest = vec![0u8; 40];
    end_block.to_bitarr(&mut rest);
    assert_eq!(rest, expected[56..], "MAC-END-HU carries the remaining 40 SDU bits");

    // The two fragments reassemble to exactly the original TM-SDU.
    let mut reassembled = first;
    reassembled.extend_from_slice(&rest);
    assert_eq!(reassembled, expected, "reassembled SDU equals the original");
}

/// A byte-aligned MAC-END-HU remainder needs no fill bits: 7-bit header + a
/// 49-bit remainder = 56 bits (7 octets), so `length_ind` = 7 and
/// `fill_bits` is false (cl. 21.4.2.2 / 23.4.2.2).
#[test]
fn test_mac_end_hu_byte_aligned_no_fill() {
    let rem_bits = bitstr(49);
    let expected: Vec<u8> = rem_bits.chars().map(|c| (c == '1') as u8).collect();
    let mut remainder = BitBuffer::from_bitstr(&rem_bits);

    let mut block = UmacMs::build_mac_end_hu_block(&mut remainder).expect("fits one MAC-END-HU");
    let pdu = MacEndHu::from_bitbuf(&mut block).expect("decodes");
    assert_eq!(pdu.length_ind, Some(7), "56 content bits = 7 octets");
    assert_eq!(pdu.fill_bits, false, "byte-aligned, no fill bits");
    let mut rest = vec![0u8; 49];
    block.to_bitarr(&mut rest);
    assert_eq!(rest, expected, "recovered remainder matches");
}

/// `build_mac_access_frag_start` only fragments genuinely oversized SDUs: a
/// short SDU that fits a single fragment returns `None` (the caller uses the
/// self-contained `build_mac_access_block` path instead).
#[test]
fn test_frag_start_rejects_small_sdu() {
    let issi: u32 = 0x0000_0001;
    let mut sdu = BitBuffer::from_bitstr(&"1".repeat(40)); // <= 56-bit first fragment
    assert!(
        UmacMs::build_mac_access_frag_start(issi, &mut sdu).is_none(),
        "an SDU that fits one fragment must not be fragmented"
    );
}
