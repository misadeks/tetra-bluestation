use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, BurstType, PhyBlockNum, PhyBlockType, Sap, TdmaTime, TrainingSequence, unimplemented_log};
use tetra_pdus::phy::traits::rxtx_dev::{RxBurstBits, RxTxDev};
use tetra_saps::tp::TpUnitdataInd;
use tetra_saps::{SapMsg, SapMsgInner};

use crate::phy::components::{burst_consts::*, train_consts::TIMESLOT_TYPE4_BITS};
use crate::{MessageQueue, TetraEntityTrait};

/// MS-mode physical layer.
///
/// Unlike [`super::phy_bs::PhyBs`], which is timing master (its blocking
/// `rxtx_timeslot` call is the clock for the whole stack), the MS PHY is
/// **receive-driven**: it continuously demodulates the downlink, recovers
/// TDMA slot timing from the SYNC burst (ref. ETSI TS 100 392-2 clause 7),
/// and drives the stack clock from RX. Uplink transmission is added in a later
/// phase.
///
/// On every demodulated downlink slot the burst is split into its constituent
/// blocks (inverse of [`super::components::slotter::build_sdb`] /
/// [`super::components::slotter::build_ndb`], Clause 9.4.4.2.5/9.4.4.2.6) and
/// forwarded to [`crate::lmac::lmac_ms::LmacMs`] as `TpUnitdataInd`.
pub struct PhyMs<D: RxTxDev> {
    #[allow(dead_code)]
    config: SharedConfig,

    /// Downlink TDMA time, recovered from the received burst timing (a relative
    /// slot count until the BSCH content is decoded in a later phase).
    dltime: TdmaTime,

    /// Whether downlink frame synchronization has been achieved.
    synced: bool,

    /// RX/TX device.
    rxtxdev: D,
}

impl<D: RxTxDev> PhyMs<D> {
    pub fn new(config: SharedConfig, rxtxdev: D) -> Self {
        Self {
            config,
            dltime: TdmaTime::default(),
            synced: false,
            rxtxdev,
        }
    }

    /// Forward a single decoded downlink block to the LMAC over the TP SAP.
    fn send_rxblock_to_lmac(
        queue: &mut MessageQueue,
        train_type: TrainingSequence,
        burst_type: BurstType,
        block_type: PhyBlockType,
        block_num: PhyBlockNum,
        bits: BitBuffer,
    ) {
        let sapmsg = SapMsg {
            sap: Sap::TpSap,
            src: TetraEntity::Phy,
            dest: TetraEntity::Lmac,
            msg: SapMsgInner::TpUnitdataInd(TpUnitdataInd {
                train_type,
                burst_type,
                block_type,
                block_num,
                block: bits,
            }),
        };
        queue.push_back(sapmsg);
    }

    /// Reassemble the 30-bit broadcast block (BBK, carrying the AACH) that a
    /// normal downlink burst carries in two segments either side of the
    /// training sequence (Clause 9.4.4.2.5).
    fn extract_ndb_bbk(bits: &[u8]) -> BitBuffer {
        let mut bbk = BitBuffer::new(NDB_BBK_BITS);
        bbk.copy_bits_from_bitarr(&bits[NDB_BBK1_OFFSET..NDB_BBK1_OFFSET + NDB_BBK1_BITS]);
        bbk.copy_bits_from_bitarr(&bits[NDB_BBK2_OFFSET..NDB_BBK2_OFFSET + NDB_BBK2_BITS]);
        bbk.seek(0);
        bbk
    }

    /// Split a demodulated downlink slot into its type-5 blocks and forward
    /// them to the LMAC. The broadcast block (AACH) is sent first so the upper
    /// MAC can interpret the rest of the slot.
    fn split_dl_slot_and_send_to_lmac(queue: &mut MessageQueue, burst: &RxBurstBits<'_>) {
        let train_seq = burst.train_type;
        let bits = burst.bits;

        match train_seq {
            TrainingSequence::SyncTrainSeq => {
                // Synchronization downlink burst (SDB), Clause 9.4.4.2.6:
                // SB1 = BSCH (SYNC), BBK = AACH, SB2 = BNCH (broadcast).
                assert!(bits.len() == TIMESLOT_TYPE4_BITS);

                let bbk = BitBuffer::from_bitarr(&bits[SB_BBK_OFFSET..SB_BBK_OFFSET + SB_BBK_BITS]);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::SDB, PhyBlockType::BBK, PhyBlockNum::Undefined, bbk);

                let sb1 = BitBuffer::from_bitarr(&bits[SB_BLK1_OFFSET..SB_BLK1_OFFSET + SB_BLK1_BITS]);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::SDB, PhyBlockType::SB1, PhyBlockNum::Block1, sb1);

                let sb2 = BitBuffer::from_bitarr(&bits[SB_BLK2_OFFSET..SB_BLK2_OFFSET + SB_BLK2_BITS]);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::SDB, PhyBlockType::SB2, PhyBlockNum::Block2, sb2);
            }

            TrainingSequence::NormalTrainSeq1 => {
                // Normal downlink burst (NDB), Clause 9.4.4.2.5: the full slot
                // is a single logical block spanning both half-slots (SCH_F).
                assert!(bits.len() == TIMESLOT_TYPE4_BITS);

                let bbk = Self::extract_ndb_bbk(bits);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::NDB, PhyBlockType::BBK, PhyBlockNum::Undefined, bbk);

                let mut blk = BitBuffer::new(NDB_BLK_BITS * 2);
                blk.copy_bits_from_bitarr(&bits[NDB_BLK1_OFFSET..NDB_BLK1_OFFSET + NDB_BLK_BITS]);
                blk.copy_bits_from_bitarr(&bits[NDB_BLK2_OFFSET..NDB_BLK2_OFFSET + NDB_BLK_BITS]);
                blk.seek(0);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::NDB, PhyBlockType::NDB, PhyBlockNum::Both, blk);
            }

            TrainingSequence::NormalTrainSeq2 => {
                // Normal downlink burst (NDB): two independent half-slot blocks
                // (SCH_HD).
                assert!(bits.len() == TIMESLOT_TYPE4_BITS);

                let bbk = Self::extract_ndb_bbk(bits);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::NDB, PhyBlockType::BBK, PhyBlockNum::Undefined, bbk);

                let blk1 = BitBuffer::from_bitarr(&bits[NDB_BLK1_OFFSET..NDB_BLK1_OFFSET + NDB_BLK_BITS]);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::NDB, PhyBlockType::NDB, PhyBlockNum::Block1, blk1);

                let blk2 = BitBuffer::from_bitarr(&bits[NDB_BLK2_OFFSET..NDB_BLK2_OFFSET + NDB_BLK_BITS]);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::NDB, PhyBlockType::NDB, PhyBlockNum::Block2, blk2);
            }

            other => {
                tracing::warn!("PhyMs: unexpected downlink training sequence {:?}, dropping slot", other);
            }
        }
    }
}

impl<D: RxTxDev + Send + 'static> TetraEntityTrait for PhyMs<D> {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Phy
    }

    fn rx_prim(&mut self, _queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);

        match message.sap {
            // Uplink transmit requests (later phase).
            Sap::TpSap => {
                unimplemented_log!("PhyMs TpSap (uplink transmit) not implemented yet");
            }
            Sap::TpcSap => {
                unimplemented_log!("PhyMs TpcSap not implemented yet");
            }
            _ => {
                panic!("PhyMs received unexpected SAP: {:?}", message.sap);
            }
        }
    }

    /// Receive-driven clock source for the MS run loop.
    ///
    /// Blocks on the downlink until the device demodulates a slot, forwards any
    /// decoded bursts to the LMAC, and returns the recovered TDMA time so the
    /// [`crate::MessageRouter`] can drive the stack clock (ETSI TS 100 392-2
    /// clause 7). While unsynchronized, `rxtx_timeslot` keeps consuming RX
    /// samples internally until the synchronization training sequence is found,
    /// so this naturally paces the loop from the air interface.
    fn drive_rx(&mut self, queue: &mut MessageQueue) -> Option<TdmaTime> {
        let mut recovered: Option<TdmaTime> = None;
        let mut has_burst = false;

        {
            let rx = self.rxtxdev.rxtx_timeslot(&[]).expect("Got error from rxtx_timeslot");

            // The MS configures a single downlink demodulator, but the device
            // may return several entries (e.g. an unused uplink slot); process
            // whichever slots were actually demodulated.
            for rx_slot in rx.into_iter().flatten() {
                recovered = Some(rx_slot.time);
                if rx_slot.slot.train_type != TrainingSequence::NotFound {
                    has_burst = true;
                    Self::split_dl_slot_and_send_to_lmac(queue, &rx_slot.slot);
                }
            }
        }

        if let Some(time) = recovered {
            self.dltime = time;
            if has_burst && !self.synced {
                self.synced = true;
                tracing::info!(ts = %self.dltime, "PhyMs: downlink synchronized");
            }
        }

        recovered
    }
}
