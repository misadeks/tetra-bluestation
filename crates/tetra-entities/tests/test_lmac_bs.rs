mod common;

use tetra_config::bluestation::StackMode;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, BurstType, PhyBlockNum, PhyBlockType, Sap, TdmaTime, TrainingSequence, debug};
use tetra_entities::lmac::components::errorcontrol;
use tetra_saps::sapmsg::{SapMsg, SapMsgInner};
use tetra_saps::tmv::{TmvConfigureReq, TmvUnitdataReq, enums::logical_chans::LogicalChannel};
use tetra_saps::tp::TpUnitdataInd;

use crate::common::ComponentTest;

#[test]
fn test_non_traffic_blk2_stolen_does_not_panic() {
    debug::setup_logging_verbose();

    let dltime = TdmaTime::default();
    let mut test = ComponentTest::new(StackMode::Bs, Some(dltime));
    test.populate_entities(vec![TetraEntity::Lmac], vec![TetraEntity::Umac]);

    let encoded_stch = errorcontrol::encode_cp(TmvUnitdataReq {
        mac_block: BitBuffer::new(124),
        logical_channel: LogicalChannel::Stch,
        scrambling_code: 3, // decode_cp uses SCRAMB_INIT when block_type is SB1
    });

    let configure = SapMsg {
        sap: Sap::TmvSap,
        src: TetraEntity::Umac,
        dest: TetraEntity::Lmac,
        msg: SapMsgInner::TmvConfigureReq(TmvConfigureReq {
            scrambling_code: None,
            is_traffic: None,
            blk2_stolen: Some(true),
            tch_type_and_interleaving_depth: None,
            time: None,
        }),
    };

    let ul_block2 = SapMsg {
        sap: Sap::TpSap,
        src: TetraEntity::Phy,
        dest: TetraEntity::Lmac,
        msg: SapMsgInner::TpUnitdataInd(TpUnitdataInd {
            train_type: TrainingSequence::NormalTrainSeq2,
            burst_type: BurstType::NUB,
            // SB1 forces decode_cp() to use default scrambling init (3),
            // so this test can generate a deterministic valid block.
            block_type: PhyBlockType::SB1,
            block_num: PhyBlockNum::Block2,
            block: encoded_stch,
        }),
    };

    test.submit_message(configure);
    test.submit_message(ul_block2);
    test.run_stack(Some(1));

    let sink_msgs = test.dump_sinks();
    assert_eq!(sink_msgs.len(), 1, "LMAC should forward decoded control block to UMAC sink");
}
