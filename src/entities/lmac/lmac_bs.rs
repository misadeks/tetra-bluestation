use crate::common::messagerouter::MessageQueue;
use crate::entities::phy::enums::burst::{BurstType, PhyBlockNum, TrainingSequence};
use crate::saps::sapmsg::{SapMsg, SapMsgInner};
use crate::config::stack_config::*;
use crate::saps::tmv::enums::logical_chans::LogicalChannel;
use crate::saps::tmv::TmvUnitdataInd;
use crate::saps::tp::{TpUnitdataInd, TpUnitdataReqSlot};
use crate::common::tdma_time::TdmaTime;
use crate::common::tetra_common::Sap;
use crate::common::tetra_entities::TetraEntity;
use crate::common::bitbuffer::BitBuffer;
use crate::common::etsi_codec;
use crate::common::voice_audio;
use crate::entities::lmac::components::errorcontrol;
use crate::entities::lmac::components::scramble::scrambler;
use crate::entities::TetraEntityTrait;
use crate::unimplemented_log;


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

pub struct LmacBs {
    config: SharedConfig,

    /// Cached from global config
    stack_mode: StackMode,
    scrambling_code: u32,

    /// Traffic channels and associated state
    tchans: [LmacTrafficChan; 64],

    /// Timeslot time, provided by upper layer and then maintained in sync here
    time: TdmaTime,
    // mcc: Option<u16>,
    // mnc: Option<u16>,
    // cc: Option<u8>,

    // Details about current burst, parsed from BBK broadcast block
    // cur_burst: CurBurst,
}

impl LmacBs {
    fn normalize_tch_encoded(&self, mut buf: BitBuffer) -> BitBuffer {
        let target = etsi_codec::TCH_ENCODED_BITS;
        let len = buf.get_len();
        if len == target {
            return buf;
        }
        buf.seek(0);
        let mut out = BitBuffer::new(target);
        if len * 2 == target {
            out.copy_bits(&mut buf, len);
            buf.seek(0);
            out.copy_bits(&mut buf, len);
        } else {
            let copy_len = usize::min(len, target);
            out.copy_bits(&mut buf, copy_len);
        }
        out.seek(0);
        out
    }
    pub fn new(config: SharedConfig) -> Self {

        // Retrieve initial basic network params from config
        let (stack_mode, sc) = {
            let c = config.config();
            tracing::info!("LmacBs: initialized with stack mode {:?}, mcc {} mnc {} cc {}", c.stack_mode, c.net.mcc, c.net.mnc, c.cell.colour_code);
            (
                c.stack_mode, 
                scrambler::tetra_scramb_get_init(c.net.mcc, c.net.mnc, c.cell.colour_code)
            )
        };
        
        Self { 
            config,
            stack_mode,
            scrambling_code: sc,
            tchans: [LmacTrafficChan::default(); 64],
            time: TdmaTime::default(),
            // cur_burst: CurBurst::default(),
        }
        
    }

    /// Yields logical channel for given block. Based on Clause 9.5.1
    fn determine_logical_channel_ul(&self, blk: &TpUnitdataInd, _t: &TdmaTime, burst_is_traffic: bool, block2_stolen: bool) -> LogicalChannel {
        
        match blk.burst_type {
            BurstType::CUB => {
                // CUB is always SCH/HU
                assert!(blk.train_type == TrainingSequence::ExtendedTrainSeq, "CUB must have extended training sequence");
                LogicalChannel::SchHu
            }
            BurstType::NUB => {
                match blk.train_type {
                    TrainingSequence::NormalTrainSeq1 => {
                        // TCH or SCH/F
                        assert!(blk.block_num == PhyBlockNum::Both, "NUB with NormalTrainSeq1 must have one large block, got {:?}", blk.block_num);
                        if burst_is_traffic {
                            // Only support TCH/S speech channel for now
                            LogicalChannel::TchS
                        } else {
                            // Full slot signalling
                            LogicalChannel::SchF
                        }
                    }
                    TrainingSequence::NormalTrainSeq2 => {
                        // Clause 9.4.4.3.2: 
                        // STCH+TCH
                        // STCH+STCH (if blk1 has resource stating 2nd block stolen)
                        if !burst_is_traffic {
                            tracing::debug!("NUB with NormalTrainSeq2 but non-traffic burst");
                            // tracing::warn!("NUB with NormalTrainSeq2 but non-traffic burst, unexpected");
                        }

                        if blk.block_num == PhyBlockNum::Block1 {
                            LogicalChannel::Stch
                        } else if blk.block_num == PhyBlockNum::Block2 {
                            
                            if !burst_is_traffic || block2_stolen {
                                // TODO FIXME remove !burst_is_traffic guard, temporary fix only
                                tracing::debug!("NUB blk2 in STCH?");
                                LogicalChannel::Stch
                            } else {
                                LogicalChannel::TchS
                            }
                        } else {
                            panic!("NUB with NormalTrainSeq2 must have two blocks, got {:?}", blk.block_num);
                        }
                    }
                    _ => panic!()
                }
            }
            _ => panic!()
        }
    }

    fn rx_blk_traffic(&mut self, queue: &mut MessageQueue, mut blk: TpUnitdataInd, lchan: LogicalChannel, ul_time: TdmaTime) {
        let raw_block = blk.block;
        let (payload, crc_ok) = match etsi_codec::channel_decode_tch(&raw_block, self.scrambling_code) {
            Ok((decoded, crc_ok)) => (decoded, crc_ok),
            Err(_) => (raw_block, true),
        };
        let len_bits = payload.get_len();
        let len_bytes = (len_bits + 7) / 8;
        tracing::info!(
            "UL TCH rx time={} lchan={:?} burst={:?} train={:?} blknum={:?} len_bits={} len_bytes={} crc_fec={} frame_type=unknown",
            ul_time,
            lchan,
            blk.burst_type,
            blk.train_type,
            blk.block_num,
            len_bits,
            len_bytes,
            if crc_ok { "ok" } else { "bad" }
        );

        let mut payload = payload;
        payload.seek(0);

        let m = SapMsg {
            sap: Sap::TmvSap,
            src: TetraEntity::Lmac,
            dest: TetraEntity::Umac,
            dltime: ul_time,
            msg: SapMsgInner::TmvUnitdataInd(
                TmvUnitdataInd {
                    pdu: payload,
                    logical_channel: lchan,
                    crc_pass: crc_ok,
                    scrambling_code: self.scrambling_code,
                }
            )
        };
        queue.push_back(m);
    }

    fn rx_blk_cp(&mut self, queue: &mut MessageQueue, blk: TpUnitdataInd, lchan: LogicalChannel) {

        assert!(lchan.is_control_channel(), "rx_blk_cp: lchan {:?} is not a signalling channel", lchan);

        let (type1bits, crc_pass) = 
                errorcontrol::decode_cp(lchan, blk, Some(self.scrambling_code));
        let type1bits = type1bits.unwrap(); // Guaranteed since scramb code set

        if tracing::enabled!(tracing::Level::DEBUG) {
            tracing::debug!("rx_blk_cp {:?} CRC: {} type1 {:?}", lchan, if crc_pass { "ok" } else { "WRONG" }, type1bits);
        } else {
            tracing::info!("rx_blk_cp {:?} CRC: {}", lchan, if crc_pass { "ok" } else { "WRONG" });
        }

        // TODO FIXME, for now, we're not passing broken CRC msgs up to Lmac
        // If we see purpose, we may pass it up in the future
        if !crc_pass {
            return;
        }

        // Pass block to the upper mac
        let m = SapMsg {
            sap: Sap::TmvSap,
            src: TetraEntity::Lmac,
            dest: TetraEntity::Umac,
            dltime: self.time,
            msg: SapMsgInner::TmvUnitdataInd(
                TmvUnitdataInd {
                    pdu: type1bits,
                    logical_channel: lchan,
                    crc_pass,
                    scrambling_code: self.scrambling_code
                }
            )
        };
        queue.push_back(m);
    }

    fn rx_tp_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        
        tracing::debug!("rx_tp_prim: msg {:?}", message);

        let ul_time = message.dltime;
        let SapMsgInner::TpUnitdataInd(prim) = message.msg else { panic!() };
        let burst_is_traffic = voice_audio::is_traffic_ts(ul_time.t);
        let lchan = self.determine_logical_channel_ul(&prim, &ul_time, burst_is_traffic, false);

        // Heuristic: for uplink NormalTrainSeq2 blocks, treat CRC-failed STCH as TCH
        // when there is any active call. This helps when timeslot mapping is off and
        // STCH decoding fails for traffic blocks.
        if lchan == LogicalChannel::Stch
            && voice_audio::get_any_call().is_some()
            && prim.burst_type == BurstType::NUB
            && prim.train_type == TrainingSequence::NormalTrainSeq2
        {
            let prim_for_crc = TpUnitdataInd {
                train_type: prim.train_type,
                burst_type: prim.burst_type,
                block_type: prim.block_type,
                block_num: prim.block_num,
                block: prim.block.clone(),
            };
            let (_type1, crc_pass) = errorcontrol::decode_cp(lchan, prim_for_crc, Some(self.scrambling_code));
            if !crc_pass {
                tracing::info!(
                    "UL TCH heuristic: STCH CRC failed, treating as TCH time={} blknum={:?}",
                    ul_time,
                    prim.block_num
                );
                self.rx_blk_traffic(queue, prim, LogicalChannel::TchS, ul_time);
                return;
            }
        }

        match lchan {
            LogicalChannel::Clch => {}
            LogicalChannel::TchS | LogicalChannel::Tch24 | LogicalChannel::Tch48 | LogicalChannel::Tch72 => {
                self.rx_blk_traffic(queue, prim, lchan, ul_time)
            }
            _ => {
                self.rx_blk_cp(queue, prim, lchan);
            }
        }
    }

    fn rx_tmv_configure_req(&mut self, _queue: &mut MessageQueue, mut message: SapMsg) {

        tracing::trace!("rx_tmv_configure_req: {:?}", message);
        let SapMsgInner::TmvConfigureReq(_prim) = &mut message.msg else {panic!()};
        unimplemented_log!("rx_tmv_configure_req");

        // if let Some(time) = prim.time { 
        //     self.time = Some(time); 
        //     tracing::debug!("rx_tmv_configure_req: set tdma_time {}", time);
        // }

        // if let Some(scrambling_code) = prim.scrambling_code { 
        //     self.scrambling_code = Some(scrambling_code); 
        //     tracing::debug!("rx_tmv_configure_req: set scrambling_code {}", scrambling_code);
        // }

        // if let Some(is_traffic) = prim.is_traffic {
        //     self.cur_burst.is_traffic = is_traffic;
        //     tracing::debug!("rx_tmv_configure_req: set cur_burst.is_traffic {}", is_traffic);
        // }
    }

    /// Request from Umac to transmit a message
    fn rx_tmv_unitdata_req_slot(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {

        tracing::debug!("rx_tmv_unitdata_req_slot: {:?}", message);
        let SapMsgInner::TmvUnitdataReq(prim) = &mut message.msg else {panic!()};
        
        // assert!(prim.ts == self.time, "rx_tmv_unitdata_req_slot: timeslot mismatch, lmac has {} while prim is for {}", self.time, prim.ts);
        assert!(prim.bbk.is_some(), "rx_tmv_unitdata_req_slot: bbk must be present");
        assert!(prim.blk1.is_some(), "rx_tmv_unitdata_req_slot: blk1 must be present");
        
        let bbk = prim.bbk.take().unwrap();   // Guaranteed for BS stack
        let blk1 = prim.blk1.take().unwrap(); // Guaranteed for BS stack
        let blk2 = prim.blk2.take();

        // Determine train and burst type
        let (burst_type, train_type) = 
                if blk1.logical_channel == LogicalChannel::Bsch {
            // Synchronization Donwlink Burst
            assert!(blk2.is_some());
            (BurstType::SDB, TrainingSequence::SyncTrainSeq)
        } else if blk1.logical_channel == LogicalChannel::SchF || (blk1.logical_channel.is_traffic() && blk2.is_none()) {
            // Single full block
            assert!(blk2.is_none());
            (BurstType::NDB, TrainingSequence::NormalTrainSeq1)
        } else {
            // Two half-blocks
            assert!(blk2.is_some());
            (BurstType::NDB, TrainingSequence::NormalTrainSeq2)
        };

        let mut prim_phy = TpUnitdataReqSlot {
            train_type,
            burst_type,
            bbk: None,
            blk1: None,
            blk2: None,
        };

        prim_phy.bbk = Some(errorcontrol::encode_aach(bbk.mac_block, bbk.scrambling_code));
        if blk1.logical_channel.is_traffic() {
            let len = blk1.mac_block.get_len();
            if len == etsi_codec::TCH_ENCODED_BITS {
                // Pass-through: already encoded
                prim_phy.blk1 = Some(blk1.mac_block);
            } else if etsi_codec::available() {
                let encoded = match etsi_codec::channel_encode_tch(&blk1.mac_block, blk1.scrambling_code) {
                    Ok(buf) => buf,
                    Err(_) => {
                        tracing::warn!("TCH encode failed: falling back to padded traffic");
                        self.normalize_tch_encoded(blk1.mac_block)
                    }
                };
                prim_phy.blk1 = Some(encoded);
            } else {
                tracing::warn!(
                    "TCH passthrough: expected {} bits, got {}; padding",
                    etsi_codec::TCH_ENCODED_BITS,
                    len
                );
                prim_phy.blk1 = Some(self.normalize_tch_encoded(blk1.mac_block));
            }
        } else {
            prim_phy.blk1 = Some(errorcontrol::encode_cp(blk1));
        }
        if let Some(blk2) = blk2 {
            if blk2.logical_channel.is_traffic() {
                let len = blk2.mac_block.get_len();
                if len == etsi_codec::TCH_ENCODED_BITS {
                    prim_phy.blk2 = Some(blk2.mac_block);
                } else if etsi_codec::available() {
                    let encoded = match etsi_codec::channel_encode_tch(&blk2.mac_block, blk2.scrambling_code) {
                        Ok(buf) => buf,
                        Err(_) => {
                            tracing::warn!("TCH encode failed: falling back to padded traffic");
                            self.normalize_tch_encoded(blk2.mac_block)
                        }
                    };
                    prim_phy.blk2 = Some(encoded);
                } else {
                    tracing::warn!(
                        "TCH passthrough: expected {} bits, got {}; padding",
                        etsi_codec::TCH_ENCODED_BITS,
                        len
                    );
                    prim_phy.blk2 = Some(self.normalize_tch_encoded(blk2.mac_block));
                }
            } else {
                prim_phy.blk2 = Some(errorcontrol::encode_cp(blk2));
            }
        }

        // Pass timeslot worth of blocks to Phy
        let m = SapMsg {
            sap: Sap::TpSap,
            src: TetraEntity::Lmac,
            dest: TetraEntity::Phy,
            dltime: self.time,
            msg: SapMsgInner::TpUnitdataReq(prim_phy)
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
            _ => { panic!(); }
        }
    }
}

impl TetraEntityTrait for LmacBs {
    
    fn entity(&self) -> TetraEntity {
        TetraEntity::Lmac
    }

    fn set_config(&mut self, config: SharedConfig) {
        self.config = config;
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        
        tracing::debug!("rx_prim: {:?}", message);
        match message.sap {
            Sap::TpSap => {
                self.rx_tp_prim(queue, message);
            }
            Sap::TmvSap => {
                self.rx_tmv_prim(queue, message);
            }
            _ =>  panic!()
        }
    }

    fn tick_start(&mut self, _queue: &mut MessageQueue, ts: Option<TdmaTime>) {
        self.time = ts.unwrap();
    }
}
