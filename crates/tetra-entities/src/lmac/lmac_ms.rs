use crate::{MessagePrio, MessageQueue, TetraEntityTrait};
use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{PhyBlockNum, PhyBlockType, Sap, TdmaTime, unimplemented_log};
use tetra_core::{BurstType, TrainingSequence};
use tetra_saps::tmv::TmvUnitdataInd;
use tetra_saps::tmv::enums::logical_chans::LogicalChannel;
use tetra_saps::tp::TpUnitdataInd;
use tetra_saps::tp::TpUnitdataReqSlot;
use tetra_saps::{SapMsg, SapMsgInner};

use crate::lmac::components::{errorcontrol, scrambler};

#[derive(Debug, Clone, Copy)]
pub struct LmacTrafficChan {
    pub is_active: bool,
    pub logical_channel: LogicalChannel,
    // TODO FIXME: extend with all required fields
}

impl Default for LmacTrafficChan {
    fn default() -> Self {
        Self {
            is_active: false,
            logical_channel: LogicalChannel::TchS,
        }
    }
}

#[derive(Default)]
pub struct CurBurst {
    pub is_traffic: bool,
    pub usage: Option<u8>,
    pub blk1_stolen: bool,
    pub blk2_stolen: bool,
}

pub struct LmacMs {
    config: SharedConfig,

    /// Retrieved from SYNC frame
    scrambling_code: Option<u32>,

    /// Traffic channels and associated state
    tchans: [LmacTrafficChan; 64],

    /// Timeslot time, provided by upper layer and then maintained in sync here
    ts: Option<TdmaTime>,
    // mcc: Option<u16>,
    // mnc: Option<u16>,
    // cc: Option<u8>,
    /// Details about current burst, parsed from BBK broadcast block
    cur_burst: CurBurst,
}

impl LmacMs {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            config,
            scrambling_code: None,
            tchans: [LmacTrafficChan::default(); 64],
            cur_burst: CurBurst::default(),

            ts: None,
        }
    }

    fn rx_bbk(&mut self, queue: &mut MessageQueue, bbk: TpUnitdataInd) {
        // tracing::trace!("rx_bbk: {:?}", bbk.block.dump_bin());

        let type5 = bbk.block;
        tracing::trace!("rx_bbk type5: {:?}", type5.dump_bin_full(true));

        // Unscrambling, type5 -> type2
        let Some(scrambling_code) = self.scrambling_code else {
            tracing::warn!("rx_bbk: no scrambling code set, need to receive SYNC first");
            return;
        };

        let type1 = errorcontrol::decode_aach(type5, scrambling_code);

        // Pass block to the upper mac
        let m = SapMsg {
            sap: Sap::TmvSap,
            src: TetraEntity::Lmac,
            dest: TetraEntity::Umac,
            msg: SapMsgInner::TmvUnitdataInd(TmvUnitdataInd {
                pdu: type1,
                block_num: PhyBlockNum::Undefined,
                logical_channel: LogicalChannel::Aach,
                crc_pass: true,
                scrambling_code,
            }),
        };

        // This message needs to be processed immediately, as the BBK block contains the ACCESS-ASSIGN,
        // determining how to interpret the two half slots of the burst.
        queue.push_prio(m, MessagePrio::Immediate);
    }

    fn determine_logical_channel_dl(&self, blk: &TpUnitdataInd, t: &TdmaTime) -> LogicalChannel {
        if blk.block_type == PhyBlockType::BBK {
            // BBK is always AACH
            return LogicalChannel::Aach;
        }

        // SB1 is always SYNC
        if blk.block_type == PhyBlockType::SB1 {
            return LogicalChannel::Bsch;
        }

        // Sanity check: this should not be a mandatory BSCH block
        assert!(
            !(t.is_mandatory_bsch() && blk.block_num == PhyBlockNum::Block1),
            "Mandatory BSCH block should be be SB1, not {:?}",
            blk.block_type
        );

        // SB2 is broadcast if scheduled according to time
        if blk.block_type == PhyBlockType::SB2 && t.is_mandatory_bnch() {
            return LogicalChannel::Bnch;
        }

        // is_traffic was previously extracted from the BBK block
        // If traffic, but block was stolen, we're still signalling (e.g. SCH_HD)
        if self.cur_burst.is_traffic {
            if (blk.block_num == PhyBlockNum::Block1 && self.cur_burst.blk1_stolen)
                || (blk.block_num == PhyBlockNum::Block2 && self.cur_burst.blk2_stolen)
            {
                // This block is stolen traffic
                return LogicalChannel::Stch;
            } else {
                // Traffic
                // TODO FIXME determine which KIND of traffic
                return LogicalChannel::TchS;
            }
        }

        // By default, we're on the signalling channel
        if blk.block_num == PhyBlockNum::Both {
            LogicalChannel::SchF
        } else {
            LogicalChannel::SchHd
        }
    }

    fn rx_blk_traffic(&mut self, _queue: &mut MessageQueue, _blk: TpUnitdataInd, _lchan: LogicalChannel) {
        unimplemented_log!("rx_blk_traffic: Traffic channel reception not implemented yet");
    }

    fn rx_blk_cp(&mut self, queue: &mut MessageQueue, blk: TpUnitdataInd, lchan: LogicalChannel) {
        let block_num = blk.block_num;
        let (type1bits, crc_pass) = errorcontrol::decode_cp(lchan, blk, self.scrambling_code);

        // Check if we indeed decoded a block, if so, continue
        if let Some(type1bits) = type1bits {
            tracing::debug!(
                "rx_blk_cp {:?} {} type1 {:?}",
                lchan,
                if lchan != LogicalChannel::Aach {
                    if crc_pass { "CRC: OK" } else { "CRC: WRONG" }
                } else {
                    ""
                },
                type1bits
            );

            // TODO FIXME, for now, we're not passing broken CRC msgs up
            // If we see purpose, we may pass it up in the future
            if !crc_pass {
                return;
            }

            // TODO FIXME maybe consider returning scramb_code from decode_cp
            let scramb_code = if lchan == LogicalChannel::Bsch {
                scrambler::SCRAMB_INIT
            } else {
                self.scrambling_code.unwrap() // Guaranteed since we were able to decode
            };

            // Pass block to the upper mac
            let m = SapMsg {
                sap: Sap::TmvSap,
                src: TetraEntity::Lmac,
                dest: TetraEntity::Umac,
                msg: SapMsgInner::TmvUnitdataInd(TmvUnitdataInd {
                    pdu: type1bits,
                    block_num,
                    logical_channel: lchan,
                    crc_pass,
                    scrambling_code: scramb_code,
                }),
            };
            queue.push_back(m);
        }
    }

    fn rx_tp_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_tp_prim: time: {:?} msg {:?}", self.ts, message);

        let SapMsgInner::TpUnitdataInd(prim) = message.msg else { panic!() };
        let lchan = self.determine_logical_channel_dl(&prim, self.ts.as_ref().unwrap_or(&TdmaTime::default()));

        match lchan {
            LogicalChannel::Aach => {
                self.rx_bbk(queue, prim);
            }
            LogicalChannel::TchS | LogicalChannel::Tch24 | LogicalChannel::Tch48 | LogicalChannel::Tch72 => {
                self.rx_blk_traffic(queue, prim, lchan)
            }
            _ => {
                self.rx_blk_cp(queue, prim, lchan);
            }
        }
    }

    fn rx_tmv_configure_req(&mut self, _queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tmv_configure_req");
        let SapMsgInner::TmvConfigureReq(prim) = &mut message.msg else {
            panic!()
        };

        if let Some(time) = prim.time {
            self.ts = Some(time);
            tracing::debug!("rx_tmv_configure_req: set tdma_time {}", time);
        }

        if let Some(scrambling_code) = prim.scrambling_code {
            self.scrambling_code = Some(scrambling_code);
            tracing::debug!("rx_tmv_configure_req: set scrambling_code {}", scrambling_code);
        }

        if let Some(is_traffic) = prim.is_traffic {
            self.cur_burst.is_traffic = is_traffic;
            tracing::debug!("rx_tmv_configure_req: set cur_burst.is_traffic {}", is_traffic);
        }
    }

    fn rx_tmv_unitdata_req_slot(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::debug!("rx_tmv_unitdata_req_slot");
        let SapMsgInner::TmvUnitdataReq(prim) = &mut message.msg else {
            panic!()
        };

        // The MS transmits a single uplink burst per granted (sub)slot. Unlike the
        // BS downlink slot there is no BBK/AACH on the uplink, and a single access
        // burst carries exactly one MAC block: SCH/HU on a Control Uplink Burst or
        // SCH/F on a Normal Uplink Burst. ETSI TS 100 392-2 cl. 9.4.4.2 (uplink
        // bursts), cl. 23.5 (MAC random/reserved access).
        assert!(prim.bbk.is_none(), "rx_tmv_unitdata_req_slot: MS uplink has no BBK/AACH");
        assert!(prim.blk2.is_none(), "rx_tmv_unitdata_req_slot: MS uplink burst carries a single block");
        let blk1 = prim
            .blk1
            .take()
            .expect("rx_tmv_unitdata_req_slot: blk1 must be present");

        // The upper MAC schedules the uplink burst in a specific granted slot
        // (the random/reserved-access opportunity, cl. 23.5). Carry that absolute
        // TDMA time down to the PHY so it can time the transmission; PhyMs cannot
        // recover it otherwise (the BS downlink, by contrast, is clock-driven).
        let ul_time = prim.ts;

        // Select uplink burst type + training sequence from the logical channel
        // (cl. 9.4.4.2). SCH/F uses normal training sequence 1 (n), SCH/HU uses the
        // extended training sequence (x).
        let (burst_type, train_type) = match blk1.logical_channel {
            LogicalChannel::SchF => (BurstType::NUB, TrainingSequence::NormalTrainSeq1),
            LogicalChannel::SchHu => (BurstType::CUB, TrainingSequence::ExtendedTrainSeq),
            other => panic!("rx_tmv_unitdata_req_slot: unsupported uplink logical channel {:?}", other),
        };

        // Channel-encode type1 -> type5: CRC16, convolutional encode, puncture,
        // interleave and scramble (cl. 8.2 channel coding, cl. 8.2.5 scrambling).
        // The uplink burst scrambling code is the serving cell's, already set from
        // the received SYNC (same code used for downlink decode).
        let type5 = errorcontrol::encode_cp(blk1);

        let prim_phy = TpUnitdataReqSlot {
            train_type,
            burst_type,
            bbk: None,
            blk1: Some(type5),
            blk2: None,
            time: Some(ul_time),
        };

        let m = SapMsg {
            sap: Sap::TpSap,
            src: TetraEntity::Lmac,
            dest: TetraEntity::Phy,
            msg: SapMsgInner::TpUnitdataReq(prim_phy),
        };
        queue.push_back(m);
    }

    fn rx_tmv_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tmv_prim");

        match message.msg {
            SapMsgInner::TmvConfigureReq(_) => {
                self.rx_tmv_configure_req(queue, message);
            }
            SapMsgInner::TmvUnitdataReq(_) => {
                self.rx_tmv_unitdata_req_slot(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }
}

impl TetraEntityTrait for LmacMs {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Lmac
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);
        // tracing::debug!(ts=%message.dltime, "rx_prim: {:?}", message);

        match message.sap {
            Sap::TpSap => {
                self.rx_tp_prim(queue, message);
            }
            Sap::TmvSap => {
                self.rx_tmv_prim(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }

    fn tick_start(&mut self, _queue: &mut MessageQueue, _ts: TdmaTime) {
        // Reset current burst state
        self.cur_burst = CurBurst::default();

        // The MS maintains absolute downlink time locally: it is seeded from each
        // SYNC burst (via TMV-CONFIGURE from UMAC, ETSI TS 100 392-2 cl. 7 /
        // 21.4.4.2) and advanced one timeslot per received slot. The router's
        // `ts` is a relative pacing clock in MS mode, so we self-advance rather
        // than assert against it.
        if let Some(mod_time) = self.ts {
            self.ts = Some(mod_time.add_timeslots(1));
            tracing::debug!("tick: new TdmaTime: {:?}", self.ts.unwrap());
        }
    }
}
