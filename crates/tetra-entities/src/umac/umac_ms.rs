use std::panic;

use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, PhyBlockNum, Sap, SsiType, TdmaTime, TetraAddress, Todo, unimplemented_log};
use tetra_saps::tlmb::{TlmbSyncInd, TlmbSysinfoInd};
use tetra_saps::tma::TmaUnitdataInd;
use tetra_saps::tmv::TmvConfigureReq;
use tetra_saps::tmv::enums::logical_chans::LogicalChannel;
use tetra_saps::{SapMsg, SapMsgInner};

use tetra_pdus::umac::enums::broadcast_type::BroadcastType;
use tetra_pdus::umac::enums::mac_pdu_type::MacPduType;
use tetra_pdus::umac::pdus::access_assign::AccessAssign;
use tetra_pdus::umac::pdus::access_assign_fr18::AccessAssignFr18;
use tetra_pdus::umac::pdus::access_define::AccessDefine;
use tetra_pdus::umac::pdus::mac_access::MacAccess;
use tetra_pdus::umac::pdus::mac_end_dl::MacEndDl;
use tetra_pdus::umac::pdus::mac_frag_dl::MacFragDl;
use tetra_pdus::umac::pdus::mac_resource::MacResource;
use tetra_pdus::umac::pdus::mac_sync::MacSync;
use tetra_pdus::umac::pdus::mac_sysinfo::MacSysinfo;

use crate::umac::subcomp::fillbits;
use crate::umac::subcomp::ms_defrag::MsDefrag;
use crate::umac::subcomp::ms_random_access::AccessParamStore;
use crate::{MessagePrio, MessageQueue, TetraEntityTrait};

/// SCH/HU (Control Uplink Burst) type-1 MAC block length in bits (ETSI TS 100
/// 392-2, SCH/HU coding parameters).
const SCH_HU_TYPE1_BITS: usize = 92;

/// A MAC block built and queued for uplink transmission, awaiting a valid
/// access opportunity. ETSI TS 100 392-2 cl. 23.5 (MAC access). The random
/// access slot-selection algorithm (cl. 23.5.1.4) that decides *when* to send
/// this, and the actual transmission, are driven by the real-time uplink PHY
/// (Phase 3d).
#[derive(Debug, Clone)]
pub struct PendingUplink {
    /// The full type-1 MAC block (e.g. 92-bit SCH/HU), ready for LMAC encode.
    pub mac_block: BitBuffer,
    pub logical_channel: LogicalChannel,
    pub scrambling_code: u32,
}

pub struct UmacMs {
    // config: Option<SharedConfig>,
    dltime: TdmaTime,
    self_component: TetraEntity,
    config: SharedConfig,
    defrag: MsDefrag,

    /// Provided by MLE over TlmbSap, to compute scrambling code, which is passed to lmac
    mcc: Option<u16>,
    /// Provided by MLE over TlmbSap, to compute scrambling code, which is passed to lmac
    mnc: Option<u16>,
    /// Provided by MLE over TlmbSap, to compute scrambling code, which is passed to lmac
    cc: Option<u8>,
    /// Derived from mcc/mnc, and passed to lmac
    scrambling_code: Option<u32>,

    /// MAC block queued for uplink transmission, awaiting an access opportunity
    /// (cl. 23.5). Consumed by the uplink PHY driver (Phase 3d).
    pending_uplink: Option<PendingUplink>,

    /// Random access parameters advertised by the serving cell, per access code
    /// (ACCESS-DEFINE cl. 21.4.4.3 + SYSINFO default-A cl. 21.4.4.1).
    access_params: AccessParamStore,
}

impl UmacMs {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            dltime: TdmaTime::default(),
            self_component: TetraEntity::Umac,
            config,
            defrag: MsDefrag::new(),

            mcc: None,
            mnc: None,
            cc: None,
            scrambling_code: None,
            pending_uplink: None,
            access_params: AccessParamStore::new(),
        }
    }

    fn rx_tmv_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tmv_prim");
        match message.msg {
            SapMsgInner::TmvUnitdataInd(_) => {
                self.rx_tmv_unitdata_ind(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }

    pub fn rx_tmv_unitdata_ind(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        tracing::trace!("rx_tmv_unitdata_ind: {:?}", prim.logical_channel);

        match prim.logical_channel {
            LogicalChannel::Aach => {
                self.rx_tmv_aach(queue, message);
            }

            LogicalChannel::Bsch => {
                self.rx_tmv_bsch(queue, message);
            }

            LogicalChannel::SchF => {
                // Full slot signalling
                assert!(
                    prim.block_num == PhyBlockNum::Both,
                    "{:?} can't have block_num {:?}",
                    prim.logical_channel,
                    prim.block_num
                );
                self.rx_tmv_sch(queue, message);
            }

            LogicalChannel::Bnch | LogicalChannel::Stch | LogicalChannel::SchHd => {
                // Half slot signalling
                assert!(
                    matches!(prim.block_num, PhyBlockNum::Block1 | PhyBlockNum::Block2),
                    "{:?} can't have block_num {:?}",
                    prim.logical_channel,
                    prim.block_num
                );
                self.rx_tmv_sch(queue, message);
            }
            _ => unreachable!("invalid channel: {:?}", prim.logical_channel),
        }
    }

    /// Receive signalling (SCH, or STCH / BNCH)
    pub fn rx_tmv_sch(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tmv_sch");

        // Iterate until no more messages left in mac block
        loop {
            // Extract info from inner block
            let SapMsgInner::TmvUnitdataInd(prim) = &message.msg else {
                panic!()
            };
            let Some(bits) = prim.pdu.peek_bits(3) else {
                tracing::warn!("insufficient bits: {}", prim.pdu.dump_bin());
                return;
            };
            let Ok(pdu_type) = MacPduType::try_from(bits >> 1) else {
                tracing::warn!("invalid pdu type: {}", bits >> 1);
                return;
            };
            let orig_start = prim.pdu.get_raw_start();
            let lchan = prim.logical_channel;

            match pdu_type {
                MacPduType::MacResourceMacData => {
                    self.rx_mac_resource(queue, &mut message);
                }
                MacPduType::MacFragMacEnd => {
                    // Also need third bit; designates mac-frag versus mac-end
                    if bits & 1 == 0 {
                        self.rx_mac_frag(queue, &mut message);
                    } else {
                        self.rx_mac_end(queue, &mut message);
                    }
                }
                MacPduType::Broadcast => {
                    self.rx_broadcast(queue, &mut message);
                }
                MacPduType::SuppMacUSignal => {
                    if lchan == LogicalChannel::Stch {
                        // U-SIGNAL since we're on the stealing channel
                        self.rx_usignal(queue, &mut message);
                    } else {
                        self.rx_supp(queue, &mut message);
                    }
                }
            }

            // Check if end of message reached by re-borrowing inner
            // If start was not updated, we also consider it end of message
            // If 16 or more bits remain (len of null pdu), we continue parsing
            if let SapMsgInner::TmvUnitdataInd(prim) = &message.msg {
                if prim.pdu.get_raw_start() != orig_start && prim.pdu.get_len() >= 16 {
                    tracing::trace!(
                        "rx_tmv_unitdata_ind_sch: Remaining {} bits: {:?}",
                        prim.pdu.get_len_remaining(),
                        prim.pdu.dump_bin_full(true)
                    );
                } else {
                    tracing::trace!("rx_tmv_unitdata_ind_sch: End of message reached");
                    break;
                }
            }
        }
    }

    // message pos: start of broadcast frame
    // Will NOT advance pos but pass to underlying function
    fn rx_broadcast(&mut self, queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_broadcast");

        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        assert!(prim.pdu.peek_bits(2).unwrap() == MacPduType::Broadcast.into_raw()); // MAC PDU type

        let bits = prim.pdu.peek_bits_posoffset(2, 2).unwrap();
        let bcast_type = BroadcastType::try_from(bits).expect("invalid broadcast type");

        match bcast_type {
            BroadcastType::Sysinfo => {
                self.rx_broadcast_sysinfo(queue, message);
            }
            BroadcastType::AccessDefine => {
                self.rx_broadcast_access_define(message);
            }
            _ => {
                panic!();
            }
        }
    }

    // Parses an ACCESS-DEFINE PDU (ETSI TS 100 392-2 cl. 21.4.4.3) and adopts
    // the random access parameters for the access code it defines
    // (cl. 23.5.1.4.1).
    fn rx_broadcast_access_define(&mut self, message: &mut SapMsg) {
        tracing::trace!("rx_broadcast_access_define");
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };

        let pdu = match AccessDefine::from_bitbuf(&mut prim.pdu) {
            Ok(pdu) => {
                tracing::debug!("<- {}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing AccessDefine: {:?} {}", e, prim.pdu.dump_bin());
                return;
            }
        };

        // An MS on its common control channel ignores ACCESS-DEFINE PDUs marked
        // for an assigned control channel (cl. 23.5.1.4.1). The MS currently
        // only camps on the common control channel.
        if pdu.common_or_assigned_control {
            tracing::trace!("rx_broadcast_access_define: ignoring assigned-control ACCESS-DEFINE");
            return;
        }

        self.access_params.update_access_define(&pdu);
    }

    // Parses the sysinfo pdu
    fn rx_broadcast_sysinfo(&mut self, queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_broadcast_sysinfo");
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };

        // Parse SYSINFO header and optional data
        let pdu = match MacSysinfo::from_bitbuf(&mut prim.pdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing MacSysinfo: {:?} {}", e, prim.pdu.dump_bin());
                return;
            }
        };

        // Adopt the "default definition for access code A" if present
        // (cl. 21.4.4.1 / 23.5.1.4.10). Ignored by the store once a "common"
        // ACCESS-DEFINE for code A has been received.
        if let Some(def) = &pdu.default_access_code {
            self.access_params.update_sysinfo_default_a(def);
        }

        // TODO FIXME adopt sysinfo info into global state
        if pdu.hyperframe_number.is_some() && pdu.hyperframe_number.unwrap() != self.dltime.h {
            // Send message to Phy about new hyperframe number
            let mut new_time = self.dltime;
            new_time.h = pdu.hyperframe_number.unwrap();
            let t = TdmaTime {
                t: self.dltime.t,
                f: self.dltime.f,
                m: self.dltime.m,
                h: pdu.hyperframe_number.unwrap(),
            };
            let m = SapMsg {
                sap: Sap::TmvSap,
                src: self.self_component,
                dest: TetraEntity::Lmac,
                msg: SapMsgInner::TmvConfigureReq(TmvConfigureReq {
                    time: Some(t),
                    ..Default::default()
                }),
            };
            tracing::info!("rx_broadcast_sysinfo: Updated TdmaTime: {:?} -> {:?}", self.dltime, new_time);
            queue.push_back(m);
        }

        let tlsdu = BitBuffer::from_bitbuffer_pos(&prim.pdu);
        let m = SapMsg {
            sap: Sap::TlmbSap,
            src: TetraEntity::Umac,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::TlmbSysinfoInd(TlmbSysinfoInd {
                endpoint_id: 0,
                tl_sdu: tlsdu,
                mac_broadcast_info: None,
            }),
        };

        queue.push_back(m);
    }

    fn rx_mac_resource(&mut self, queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_mac_resource");
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        assert!(prim.pdu.get_pos() == 0); // We should be at the start of the MAC PDU

        // Parse header and optional ChanAlloc
        let pdu = match MacResource::from_bitbuf(&mut prim.pdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing MacResource: {:?} {}", e, prim.pdu.dump_bin());
                return;
            }
        };

        if pdu.encryption_mode > 0 {
            unimplemented_log!("rx_mac_resource: Encryption mode > 0, not implemented");
        }

        // Compute len
        let mut pdu_len_bits = {
            match pdu.length_ind {
                0b000001..0b111010 => {
                    // tracing::trace!("rx_mac_resource: length_ind {}", pdu.length_ind);
                    pdu.length_ind as usize * 8
                }
                0b111110 => {
                    // Second half slot stolen in STCH
                    unimplemented_log!("rx_mac_resource: SECOND HALF SLOT STOLEN IN STCH but signal not implemented");
                    prim.pdu.get_len()
                }
                0b111111 => {
                    // Start of fragmentation
                    // tracing::trace!("rx_mac_resource: frag start length_ind {}", pdu.length_ind);
                    prim.pdu.get_len()
                }
                _ => panic!("rx_mac_resource: Invalid length_ind {}", pdu.length_ind),
            }
        };

        if pdu_len_bits > prim.pdu.get_len() {
            // TODO FIXME: I sometimes encounter len = 0b100010 = 32
            // This does not fit, since it translates to 272 bits while it comes in a 268 bit slot
            // We'll correct for that by simply cropping to the end... But this is strange
            tracing::warn!(
                "rx_mac_resource: Strange length_ind {} in MAC resource, truncating from {} to {}",
                pdu.length_ind,
                pdu_len_bits,
                prim.pdu.get_len()
            );
            pdu_len_bits = prim.pdu.get_len();
        }

        // Strip fill bits. Maintain original end to allow for later parsing of a second mac block
        tracing::trace!("rx_mac_resource: {}", prim.pdu.dump_bin_full(true));
        let num_fill_bits = {
            if pdu.fill_bits {
                fillbits::removal::get_num_fill_bits(&prim.pdu, pdu_len_bits, pdu.is_null_pdu())
            } else {
                0
            }
        };
        pdu_len_bits -= num_fill_bits;
        let orig_end = prim.pdu.get_raw_end();
        prim.pdu.set_raw_end(prim.pdu.get_raw_start() + pdu_len_bits);
        tracing::trace!(
            "rx_mac_resource: pdu: {} sdu: {} fb: {}: {}",
            pdu_len_bits,
            prim.pdu.get_len_remaining(),
            num_fill_bits,
            prim.pdu.dump_bin_full(true)
        );

        if pdu.addr.is_none() {
            // TODO not sure if there is scenarios in which we want to pass a null pdu to the LLC
            // tracing::warn!("rx_mac_resource: Null PDU not passed to LLC");
            return;
        }

        // Decrypt if needed
        if pdu.encryption_mode > 0 {
            unimplemented_log!("rx_mac_resource: Encryption mode > 0");
            return;
            // TODO:
            // Check if key available
            // generate keystream
            // apply keystream to data
            // re-decode chanalloc
            // continue
        }

        tracing::debug!("rx_mac_resource: {}", prim.pdu.dump_bin_full(true));
        if pdu.length_ind == 0b111111 {
            // Fragmentation start, add to defragmenter
            self.defrag.insert_first(&mut prim.pdu, self.dltime, pdu.addr.unwrap(), None);
        } else if pdu.length_ind == 0b111110 {
            tracing::warn!("rx_mac_resource: SECOND HALF SLOT STOLEN IN STCH but not implemented");
        } else {
            // Pass directly to LLC
            let sdu = {
                if pdu.length_ind == 0 {
                    None // Null PDU
                } else if prim.pdu.get_len_remaining() == 0 {
                    None // No more data in this block
                } else {
                    // TODO FIXME should not copy here but take ownership
                    // Copy inner part, without MAC header or fill bits
                    Some(BitBuffer::from_bitbuffer_pos(&prim.pdu))
                }
            };
            // tracing::debug!("rx_mac_resource: sdu: {:?}", sdu.as_ref().unwrap().dump_bin_full(true));

            if sdu.is_some() {
                // We have an SDU for the LLC, deliver it.
                let m = SapMsg {
                    sap: Sap::TmaSap,
                    src: TetraEntity::Umac,
                    dest: TetraEntity::Llc,
                    msg: SapMsgInner::TmaUnitdataInd(TmaUnitdataInd {
                        pdu: sdu,
                        main_address: pdu.addr.unwrap(),
                        scrambling_code: prim.scrambling_code,
                        endpoint_id: 0,        // TODO FIXME
                        new_endpoint_id: None, // TODO FIXME
                        css_endpoint_id: None, // TODO FIXME
                        air_interface_encryption: pdu.encryption_mode as Todo,
                        chan_change_response_req: false,
                        chan_change_handle: None,
                        chan_info: None,
                    }),
                };
                queue.push_back(m);
            } else {
                // Either this is a null pdu or we are at the end of the block
                // For now, we don't deliver this. However, important data may need to be signalled upwards
                tracing::info!("rx_mac_resource: empty PDU not passed to LLC");
            }
        }

        // Since this is not a null pdu, more MAC PDUs may follow
        // This allows parent function to continue parsing
        prim.pdu.set_raw_end(orig_end);
        prim.pdu.set_raw_pos(prim.pdu.get_raw_start() + pdu_len_bits + num_fill_bits);
        prim.pdu.set_raw_start(prim.pdu.get_raw_pos());
    }

    fn rx_mac_frag(&mut self, _queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_mac_frag");
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        assert!(prim.pdu.get_pos() == 0); // We should be at the start of the MAC PDU

        // Parse header and optional ChanAlloc
        let pdu = match MacFragDl::from_bitbuf(&mut prim.pdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing MacFragDl: {:?} {}", e, prim.pdu.dump_bin());
                return;
            }
        };

        // Strip fill bits. This message is known to fill the slot.
        let mut pdu_len_bits = prim.pdu.get_len();
        let num_fill_bits = {
            if pdu.fill_bits {
                fillbits::removal::get_num_fill_bits(&prim.pdu, pdu_len_bits, false)
            } else {
                0
            }
        };
        pdu_len_bits -= num_fill_bits;
        prim.pdu.set_raw_end(prim.pdu.get_raw_start() + pdu_len_bits);
        tracing::debug!("rx_mac_frag: pdu_len_bits: {} fill_bits: {}", pdu_len_bits, num_fill_bits);

        // Decrypt if needed
        if let Some(_aie_info) = self.defrag.buffers[(self.dltime.t - 1) as usize].aie_info {
            // TODO FIXME implement
            unimplemented_log!("rx_mac_frag: Encryption not supported");
            return;
        }

        // Insert into defragmenter
        self.defrag.insert_next(&mut prim.pdu, self.dltime);
    }

    fn rx_mac_end(&mut self, queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_mac_end");
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        assert!(prim.pdu.get_pos() == 0); // We should be at the start of the MAC PDU

        // Parse header and optional ChanAlloc
        let pdu = match MacEndDl::from_bitbuf(&mut prim.pdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing MacEndDl: {:?} {}", e, prim.pdu.dump_bin());
                return;
            }
        };

        // Compute len
        assert!(pdu.length_ind != 0); // Reserved
        let mut pdu_len_bits = pdu.length_ind as usize * 8;

        // Strip fill bits. Maintain original end to allow for later parsing of a second mac block
        let num_fill_bits = {
            if pdu.fill_bits {
                fillbits::removal::get_num_fill_bits(&prim.pdu, pdu_len_bits, false)
            } else {
                0
            }
        };
        pdu_len_bits -= num_fill_bits;
        let orig_end = prim.pdu.get_raw_end();
        prim.pdu.set_raw_end(prim.pdu.get_raw_start() + pdu_len_bits);
        tracing::debug!("rx_mac_end: pdu_len_bits: {} fill_bits: {}", pdu_len_bits, num_fill_bits);

        // Decrypt if needed
        if let Some(_aie_info) = self.defrag.buffers[(self.dltime.t - 1) as usize].aie_info {
            // TODO FIXME implement
            unimplemented!("rx_mac_end: Encryption not supported");
            // TODO FIXME Also re-parse chanalloc
        }

        // Insert into defragmenter
        self.defrag.insert_last(&mut prim.pdu, self.dltime);

        // Fetch finalized block
        let defragbuf = self.defrag.take_defragged_buf(self.dltime);
        let Some(defragbuf) = defragbuf else {
            tracing::warn!("rx_mac_end: could not obtain defragged buf");
            return;
        };

        // Pass block directly to LLC
        tracing::debug!("rx_mac_end: sdu: {:?}", defragbuf.buffer.dump_bin());

        let m = SapMsg {
            sap: Sap::TmaSap,
            src: TetraEntity::Umac,
            dest: TetraEntity::Llc,
            msg: SapMsgInner::TmaUnitdataInd(TmaUnitdataInd {
                pdu: Some(defragbuf.buffer),
                main_address: defragbuf.addr,
                scrambling_code: prim.scrambling_code,
                endpoint_id: 0,              // TODO FIXME
                new_endpoint_id: None,       // TODO FIXME
                css_endpoint_id: None,       // TODO FIXME
                air_interface_encryption: 0, // TODO FIXME implement
                chan_change_response_req: false,
                chan_change_handle: None,
                chan_info: None,
            }),
        };
        queue.push_back(m);

        // Since this is not a null pdu, more MAC PDUs may follow
        // This allows parent function to continue parsing
        prim.pdu.set_raw_end(orig_end);
        prim.pdu.set_raw_pos(prim.pdu.get_raw_start() + pdu_len_bits + num_fill_bits);
        prim.pdu.set_raw_start(prim.pdu.get_raw_pos());
    }

    fn rx_usignal(&self, _queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_usignal");
        let SapMsgInner::TmvUnitdataInd(_prim) = &mut message.msg else {
            panic!()
        };
        unimplemented!("rx_usignal");
    }

    fn rx_supp(&self, _queue: &mut MessageQueue, message: &mut SapMsg) {
        tracing::trace!("rx_supp");

        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        // Check we're indeed on the right channel (Clause 21.4.1 Table 21.48)
        assert!(prim.logical_channel != LogicalChannel::Stch && prim.logical_channel != LogicalChannel::SchHd);
        unimplemented!("rx_supp");
    }

    pub fn rx_tmv_aach(&self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tmv_aach");

        // TODO FIXME, more extensively store and process AACH state in both LMAC and UMAC
        // Then we send a msg down only if a change is needed, like we do for the scrambling code

        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };

        let is_traffic = if self.dltime.f != 18 {
            let pdu = match AccessAssign::from_bitbuf(&mut prim.pdu) {
                Ok(pdu) => {
                    tracing::debug!("<- {:?}", pdu);
                    pdu
                }
                Err(e) => {
                    tracing::warn!("Failed parsing AccessAssign: {:?} {}", e, prim.pdu.dump_bin());
                    return;
                }
            };

            pdu.dl_usage.is_traffic()
        } else {
            let _pdu = match AccessAssignFr18::from_bitbuf(&mut prim.pdu) {
                Ok(pdu) => {
                    tracing::debug!("<- {:?}", pdu);
                    pdu
                }
                Err(e) => {
                    tracing::warn!("Failed parsing AccessAssignFr18: {:?} {}", e, prim.pdu.dump_bin());
                    return;
                }
            };

            false
        };

        let m = SapMsg {
            sap: Sap::TmvSap,
            src: TetraEntity::Umac,
            dest: TetraEntity::Lmac,
            msg: SapMsgInner::TmvConfigureReq(TmvConfigureReq {
                is_traffic: Some(is_traffic),
                ..Default::default()
            }),
        };
        // This message needs to be processed NOW since it affects the other blocks in this timeslot
        queue.push_prio(m, MessagePrio::Immediate);
    }

    pub fn rx_tmv_bsch(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tmv_bsch");
        let SapMsgInner::TmvUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };

        // Parse the MAC-SYNC PDU carried by the BSCH (ETSI TS 100 392-2 cl. 21.4.4.2).
        let pdu = match MacSync::from_bitbuf(&mut prim.pdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing MacSync: {:?} {}", e, prim.pdu.dump_bin());
                return;
            }
        };

        // Adopt the colour code. Together with the MCC/MNC provided by MLE over
        // TLMC, it derives the scrambling code (cl. 23.2.2 / 8.2.5).
        if self.cc != Some(pdu.colour_code) {
            tracing::info!("rx_tmv_bsch: colour code {:?} -> {}", self.cc, pdu.colour_code);
            self.cc = Some(pdu.colour_code);
        }

        // Seed the absolute downlink time from the SYNC burst (cl. 7 / 21.4.4.2).
        // UMAC free-runs this between SYNC bursts (see `tick_start`) and re-seeds
        // it here each frame 18.
        self.dltime = pdu.time;

        // Push the recovered time down to LMAC so it classifies logical channels
        // (BNCH / frame 18) and interprets AACH against the correct absolute time.
        let m = SapMsg {
            sap: Sap::TmvSap,
            src: self.self_component,
            dest: TetraEntity::Lmac,
            msg: SapMsgInner::TmvConfigureReq(TmvConfigureReq {
                time: Some(pdu.time),
                ..Default::default()
            }),
        };
        queue.push_back(m);

        // Forward the remaining bits (the D-MLE-SYNC SDU, cl. 18.4.2.1) up to MLE
        // for initial cell selection (cl. 18.3.4.6). MLE replies with a
        // TL-CONFIGURE carrying the valid MCC/MNC, which lets us derive the
        // scrambling code and submit it to LMAC.
        let tlsdu = BitBuffer::from_bitbuffer_pos(&prim.pdu);
        let m = SapMsg {
            sap: Sap::TlmbSap,
            src: TetraEntity::Umac,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::TlmbSyncInd(TlmbSyncInd {
                endpoint_id: 0,
                tl_sdu: tlsdu,
            }),
        };
        queue.push_back(m);
    }

    fn rx_tma_prim(&mut self, _queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tma_prim");
        let SapMsgInner::TmaUnitdataReq(mut prim) = message.msg else {
            panic!("rx_tma_prim: unexpected primitive");
        };

        // The MS can only transmit once it has camped on a cell and derived the
        // serving-cell scrambling code from SYNC (Phase 2). Without it we cannot
        // channel-encode an uplink burst.
        let Some(scrambling_code) = self.scrambling_code else {
            tracing::warn!("rx_tma_prim: no scrambling code yet (not camped), dropping uplink SDU");
            return;
        };

        let issi = self
            .config
            .config()
            .ms
            .as_ref()
            .expect("MS config section required in MS mode")
            .issi;

        // Build a MAC-ACCESS carrying the TM-SDU for random access on SCH/HU
        // (Control Uplink Burst). ETSI TS 100 392-2 cl. 21.4.2.1, cl. 23.5.1.
        let sdu_len = prim.pdu.get_len();
        let Some(mac_block) = Self::build_mac_access_block(issi, &mut prim.pdu) else {
            tracing::warn!(
                "rx_tma_prim: SDU ({} bits) too large for a single MAC-ACCESS burst; uplink fragmentation not implemented",
                sdu_len
            );
            return;
        };

        tracing::debug!("rx_tma_prim: queued MAC-ACCESS uplink for ISSI {} ({} SDU bits)", issi, sdu_len);

        // Queue for transmission at the next valid random-access opportunity.
        // The access-frame selection / randomised access algorithm
        // (cl. 23.5.1.4) and the actual transmit are driven by the real-time
        // uplink PHY (Phase 3d).
        self.pending_uplink = Some(PendingUplink {
            mac_block,
            logical_channel: LogicalChannel::SchHu,
            scrambling_code,
        });
    }

    /// Build a MAC-ACCESS type-1 MAC block (ETSI TS 100 392-2 cl. 21.4.2.1)
    /// carrying `sdu` for transmission on SCH/HU (Control Uplink Burst). The
    /// block is addressed with the MS's own ISSI (the uplink MAC-ACCESS address
    /// is always an ISSI, cl. 21.4.2.1). Returns the full 92-bit type-1 block
    /// (MAC-ACCESS header + TM-SDU + fill bits), or `None` if the SDU does not
    /// fit a single access burst (uplink fragmentation, cl. 21.4.3, not yet
    /// implemented).
    pub fn build_mac_access_block(issi: u32, sdu: &mut BitBuffer) -> Option<BitBuffer> {
        let mut pdu = MacAccess {
            fill_bits: false,
            encrypted: false,
            addr: Some(TetraAddress {
                ssi_type: SsiType::Issi,
                ssi: issi,
            }),
            event_label: None,
            length_ind: Some(0),
            frag_flag: None,
            reservation_req: None,
        };

        // Measure the header length. The fill_bits flag and the length_ind
        // value do not change the number of header bits.
        let hdr_len = {
            let mut scratch = BitBuffer::new(64);
            pdu.to_bitbuf(&mut scratch);
            scratch.get_pos()
        };

        let sdu_len = sdu.get_len();
        let content_len = hdr_len + sdu_len;
        // The length indication counts octets, so pad the content to the next
        // byte boundary with fill bits (cl. 21.4.2.1 / 23.4.3 length indication).
        let num_fill_bits = (8 - content_len % 8) % 8;
        let total_len = content_len + num_fill_bits;
        if total_len > SCH_HU_TYPE1_BITS {
            return None; // Would require uplink fragmentation (deferred)
        }

        pdu.length_ind = Some((total_len / 8) as u8);
        pdu.fill_bits = num_fill_bits != 0;

        // Assemble the full 92-bit type-1 block: MAC-ACCESS header + TM-SDU +
        // fill bits. Bits beyond the length indication are don't-care (left
        // zeroed); the receiver ignores everything past length_ind octets.
        let mut block = BitBuffer::new(SCH_HU_TYPE1_BITS);
        pdu.to_bitbuf(&mut block);
        sdu.seek(0);
        block.copy_bits(sdu, sdu_len);
        fillbits::addition::write(&mut block, Some(num_fill_bits));
        block.seek(0);
        Some(block)
    }

    /// Take the MAC block queued for uplink transmission, if any. Consumed by
    /// the uplink PHY driver once a valid access opportunity is reached
    /// (Phase 3d).
    pub fn take_pending_uplink(&mut self) -> Option<PendingUplink> {
        self.pending_uplink.take()
    }

    fn rx_tlmb_prim(&mut self, _queue: &mut MessageQueue, _message: SapMsg) {
        tracing::trace!("rx_tlmb_prim");
        unimplemented!();
    }

    fn update_scrambing_and_submit_to_lmac(&mut self, queue: &mut MessageQueue) {
        if let (Some(mcc), Some(mnc), Some(cc)) = (self.mcc, self.mnc, self.cc) {
            self.scrambling_code = Some((((cc as u32) | ((mnc as u32) << 6) | ((mcc as u32) << 20)) << 2) | 3);

            tracing::trace!(
                "compute_scrambling_and_submit_to_lmac cc {} mcc {} mnc {} scrambling_code: {}",
                cc,
                mcc,
                mnc,
                self.scrambling_code.unwrap()
            );

            let m = SapMsg {
                sap: Sap::TmvSap,
                src: self.self_component,
                dest: TetraEntity::Lmac,
                msg: SapMsgInner::TmvConfigureReq(TmvConfigureReq {
                    scrambling_code: self.scrambling_code,
                    ..Default::default()
                }),
            };
            queue.push_back(m);
        }
    }

    fn rx_tlmc_configure_req(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tlmc_configure_req");
        let SapMsgInner::TlmcConfigureReq(prim) = &message.msg else {
            panic!()
        };

        if let Some(valid_addresses) = &prim.valid_addresses {
            tracing::debug!("rx_tlmc_configure_req: valid_addresses: {:?}", valid_addresses);

            self.mcc = Some(valid_addresses.mcc);
            self.mnc = Some(valid_addresses.mnc);

            // Attempt to update scrambling code (if cc is also known)
            self.update_scrambing_and_submit_to_lmac(queue);
        } else {
            tracing::warn!("rx_tlmc_configure_req: No valid addresses provided");
        }
    }

    fn rx_tlmc_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tlmc_prim");
        match message.msg {
            SapMsgInner::TlmcConfigureReq(_) => {
                self.rx_tlmc_configure_req(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }
}

impl TetraEntityTrait for UmacMs {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Umac
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);
        // tracing::debug!(ts=%message.dltime, "rx_prim: {:?}", message);

        match message.sap {
            Sap::TmvSap => {
                self.rx_tmv_prim(queue, message);
            }

            Sap::TmaSap => {
                self.rx_tma_prim(queue, message);
            }

            Sap::TlmbSap => {
                self.rx_tlmb_prim(queue, message);
            }

            Sap::TlmcSap => {
                self.rx_tlmc_prim(queue, message);
            }

            _ => {
                panic!()
            }
        }
    }

    fn tick_start(&mut self, _queue: &mut MessageQueue, _ts: TdmaTime) {
        // The MS free-runs the absolute downlink time between SYNC bursts,
        // advancing one timeslot per received slot. It is re-seeded from each
        // BSCH in `rx_tmv_bsch` (ETSI TS 100 392-2 cl. 7 / 21.4.4.2). The
        // router's `ts` is a relative pacing clock in MS mode, so it is
        // intentionally not used here.
        self.dltime = self.dltime.add_timeslots(1);
    }
}
