
use std::panic;
use crate::config::config::SharedConfig;
use crate::common::messagerouter::MessageQueue;
use crate::entities::phy::components::train_consts::TIMESLOT_TYPE4_BITS;
use crate::saps::sapmsg::{SapMsg, SapMsgInner};
use crate::saps::tp::TpUnitdataInd;
use crate::common::bitbuffer::BitBuffer;
use crate::common::tdma_time::TdmaTime;
use crate::common::tetra_common::Sap;
use crate::common::tetra_entities::TetraEntity;
use crate::entities::phy::enums::burst::*;
use crate::entities::phy::components::slotter::{build_ndb, build_sdb};
use crate::entities::phy::traits::rxtx_dev::RxBurstBits;
use crate::entities::TetraEntityTrait;
use crate::entities::umac::subcomp::bs_channel_scheduler::MACSCHED_TX_AHEAD;

use super::traits::rxtx_dev::{RxTxDev, TxSlotBits};
use super::components::phy_io_file::PhyIoFile;

const RX_BUF_SIZE: usize = 2048*4;
const MAX_RXBLOCK_SIZE: usize = 2048;
const MAX_RXBURSTSYNC_BLOCK: usize = MAX_RXBLOCK_SIZE*4;

pub struct PhyBs<D: RxTxDev> {

    self_component: TetraEntity,
    config: SharedConfig,

    dl_input_file: Option<PhyIoFile>,
    dl_output_file: Option<PhyIoFile>,
    ul_input_file: Option<PhyIoFile>,
    ul_output_file: Option<PhyIoFile>,

    /// RX/TX device
    rxtxdev: D,
}

impl <D: RxTxDev>PhyBs<D> {
    pub fn new(config: SharedConfig, rxtxdev: D) -> Self {
        Self {
            self_component: TetraEntity::Phy,
            config,
            dl_input_file: None,
            dl_output_file: None,
            ul_input_file: None,
            ul_output_file: None,
            rxtxdev,
        }
    }

    pub fn set_io_files(&mut self, dl_input_file: Option<PhyIoFile>, dl_output_file: Option<PhyIoFile>, ul_input_file: Option<PhyIoFile>, ul_output_file: Option<PhyIoFile>) {
        self.dl_input_file = dl_input_file;
        self.dl_output_file = dl_output_file;
        self.ul_input_file = ul_input_file;
        self.ul_output_file = ul_output_file;
    }

    fn send_rxblock_to_lmac(queue: &mut MessageQueue, train_type: TrainingSequence, burst_type: BurstType, block_type: PhyBlockType, block_num: PhyBlockNum, bits: BitBuffer) {
        let sapmsg = SapMsg { 
            sap: Sap::TpSap, 
            src: TetraEntity::Phy, 
            dest: TetraEntity::Lmac,
            dltime: TdmaTime::default(),
            msg: SapMsgInner::TpUnitdataInd(TpUnitdataInd { 
                train_type,
                burst_type,
                block_type,
                block_num,
                block: bits
            }),
        };
        queue.push_back(sapmsg);
    }

    fn split_rxslot_and_send_to_lmac(queue: &mut MessageQueue, burst: &RxBurstBits<'_>) {

        const NUB_BITS: usize =              4+216+22+216+4;
        const NUB_BLK_BITS: usize =          216;
        const NUB_BLK1_OFFSET: usize =       4;
        const NUB_BLK2_OFFSET: usize =       4+216+22;
        const CUB_BITS: usize =              4+84+30+84+4;
        const CUB_BLK_BITS: usize =          84;
        const CUB_BLK1_OFFSET: usize =       4;
        const CUB_BLK2_OFFSET: usize =       4+84+30;

        const TAIL_BITS: [u8; 4] = [1u8, 1u8, 0u8, 0u8];

        let train_seq = burst.train_type;
        match train_seq {
            TrainingSequence::NormalTrainSeq1 => { 

                assert!(burst.bits.len() == NUB_BITS);

                let mut blk = BitBuffer::new(NUB_BLK_BITS * 2);
                blk.copy_bits_from_bitarr(&burst.bits[NUB_BLK1_OFFSET..NUB_BLK1_OFFSET + NUB_BLK_BITS]);
                blk.copy_bits_from_bitarr(&burst.bits[NUB_BLK2_OFFSET..NUB_BLK2_OFFSET + NUB_BLK_BITS]);
                blk.seek(0);

                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::NUB, PhyBlockType::NUB, PhyBlockNum::Both, blk);
            }

            TrainingSequence::NormalTrainSeq2 => { 

                assert!(burst.bits.len() == NUB_BITS);
                
                let blk1 = BitBuffer::from_bitarr(&burst.bits[NUB_BLK1_OFFSET..NUB_BLK1_OFFSET + NUB_BLK_BITS]);
                let blk2 = BitBuffer::from_bitarr(&burst.bits[NUB_BLK2_OFFSET..NUB_BLK2_OFFSET + NUB_BLK_BITS]);

                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::NUB, PhyBlockType::NUB, PhyBlockNum::Block1, blk1);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::NUB, PhyBlockType::NUB, PhyBlockNum::Block2, blk2);
            }
            TrainingSequence::ExtendedTrainSeq => { 

                assert!(burst.bits.len() == CUB_BITS);

                let mut blk = BitBuffer::new(CUB_BLK_BITS * 2);
                blk.copy_bits_from_bitarr(&burst.bits[CUB_BLK1_OFFSET..CUB_BLK1_OFFSET + CUB_BLK_BITS]);
                blk.copy_bits_from_bitarr(&burst.bits[CUB_BLK2_OFFSET..CUB_BLK2_OFFSET + CUB_BLK_BITS]);
                blk.seek(0);

                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::CUB, PhyBlockType::SSN1, PhyBlockNum::Block1, blk);
            }

            _ => panic!()
        }
    }

    fn rx_tpsap_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        
        // Handle TpUnitdataReq with a TX slot
        // Prepare TxSlotBits for transmission
        // TODO FIXME: optimize
        
        let SapMsgInner::TpUnitdataReq(prim) = message.msg else {panic!()};

        // Generate block (from file or from LMAC data)
        let mut burst = [0u8; TIMESLOT_TYPE4_BITS];
        if let Some(dl_input_file) = &mut self.dl_input_file {
            
            // Code for testing mode, when replaying from DL input file
            if let Err(e) = dl_input_file.read_block(&mut burst) {
                panic!("DL input file error: {:?}", e);
            }
            
        } else {

            // We received data from LMAC, convert BBK block to bitarr
            assert!(prim.bbk.is_some());
            let mut bbk = [0u8; 30];
            prim.bbk.unwrap().to_bitarr(&mut bbk);

            // Build NDB or SDB burst
            burst = match prim.burst_type {
                BurstType::SDB => {
                    // SDB burst
                    assert!(prim.train_type == TrainingSequence::SyncTrainSeq);
                    assert!(prim.blk1.is_some() && prim.blk2.is_some());
                    
                    let mut blk1 = [0u8; 120];
                    let mut blk2 = [0u8; 216];
                    prim.blk1.unwrap().to_bitarr(&mut blk1); // Guaranteed for SDB
                    prim.blk2.unwrap().to_bitarr(&mut blk2); // Guaranteed for SDB

                    build_sdb(&blk1, &bbk, &blk2)
                }
                BurstType::NDB => {

                    let mut blk1 = [0u8; 216];
                    let mut blk2 = [0u8; 216];

                    match prim.train_type{
                        TrainingSequence::NormalTrainSeq1 => {
                            // Single large block
                            assert!(prim.blk1.is_some() && prim.blk2.is_none());
                            let mut blk1_src = prim.blk1.unwrap(); // Guaranteed for NDB
                            blk1_src.to_bitarr(&mut blk1);
                            blk1_src.to_bitarr(&mut blk2);
                        }
                        TrainingSequence::NormalTrainSeq2 => {
                            // Two half slots
                            assert!(prim.blk1.is_some() && prim.blk2.is_some());
                            prim.blk1.unwrap().to_bitarr(&mut blk1); // Guaranteed for NDB
                            prim.blk2.unwrap().to_bitarr(&mut blk2); // Guaranteed for NDB trainseq 2
                        }
                        _ => panic!("Unsupported training sequence for NDB burst")
                    }

                    build_ndb(prim.train_type, &blk1, &bbk, &blk2)
                }
                _ => panic!()
            };
        }

        // Code for testing mode, when capturing all DL output to file
        if let Some(dl_output_file) = &mut self.dl_output_file {
            if let Err(e) = dl_output_file.write_block(&mut burst) {
                panic!("DL output file error: {:?}", e);
            }
        } 

        // Prepare the TX slot for the tx device
        let tx_slot: [TxSlotBits; 1] = [TxSlotBits {
            time: message.dltime.add_timeslots(MACSCHED_TX_AHEAD as i32),
            slot: Some(&burst),
            ..Default::default()
        }];

        // Transmit slot and receive rx data (if any trainseq was found)
        // This function is blocking and the source of timing sync in the whole stack
        let rx = self.rxtxdev.rxtx_timeslot(&tx_slot).expect("Got error from rxtx_timeslot");
        
        // Process received slot (either full, subslot1 or subslot2)
        // In exceptional cases, we might receive multiple slots (multiple possible detected bursts in one timeslot)
        // This may be due to two subslots, or due to false psoitives in training seq detection
        // The Lmac error correction will eliminate the false positives
        for rx_slot in rx {
            if let Some(rx_slot) = rx_slot {
                
                let mut slot_sent = false;
                if rx_slot.slot.train_type != TrainingSequence::NotFound {
                    tracing::debug!("Received full slot at time: {:?}: {:?}", rx_slot.time, rx_slot.slot.train_type);
                    Self::split_rxslot_and_send_to_lmac(queue, &rx_slot.slot);
                    slot_sent = true;
                }
                if rx_slot.subslot1.train_type != TrainingSequence::NotFound {
                    tracing::debug!("Received subslot1 at time: {:?}: {:?}", rx_slot.time, rx_slot.subslot1.train_type);
                    if slot_sent {
                        tracing::warn!("Sending same burst twice to LMAC");
                    } 
                    Self::split_rxslot_and_send_to_lmac(queue, &rx_slot.subslot1);
                    slot_sent = true;
                }
                if rx_slot.subslot2.train_type != TrainingSequence::NotFound {
                    tracing::debug!("Received subslot2 at time: {:?}: {:?}", rx_slot.time, rx_slot.subslot2.train_type);
                    if slot_sent {
                        tracing::warn!("Sending same burst twice to LMAC");
                    } 
                    Self::split_rxslot_and_send_to_lmac(queue, &rx_slot.subslot2);
                }
            }
        }
    }


    fn rx_tpc_prim(&mut self, _queue: &mut MessageQueue, _message: SapMsg) {
        unimplemented!();
    }
}


impl<D: RxTxDev + Send + 'static> TetraEntityTrait for PhyBs<D> {

    fn entity(&self) -> TetraEntity {
        TetraEntity::Phy
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        
        tracing::debug!("rx_prim: {:?}", message);
        let sap = message.sap;

        match sap {
            Sap::TpSap => {
                self.rx_tpsap_prim(queue, message);
            }
            Sap::TpcSap => {
                self.rx_tpc_prim(queue, message);
            }
            _ => { panic!(); }
        }
    }
}
