use std::panic;

use crate::common::messagerouter::MessageQueue;
use crate::saps::tla::{TlaTlDataIndBl, TlaTlUnitdataIndBl};
use crate::saps::tma::TmaUnitdataReq;
use crate::common::address::TetraAddress;
use crate::common::bitbuffer::BitBuffer;
use crate::common::tdma_time::TdmaTime;
use crate::common::tetra_common::Sap;
use crate::common::tetra_entities::TetraEntity;
use crate::entities::llc::components::fcs::check_fcs;
use crate::entities::llc::enums::llc_pdu_type::LlcPduType;
use crate::entities::llc::pdus::bl_ack::BlAck;
use crate::entities::llc::pdus::bl_adata::BlAdata;
use crate::entities::llc::pdus::bl_data::BlData;
use crate::entities::llc::pdus::bl_udata::BlUdata;
use crate::entities::TetraEntityTrait;
use crate::saps::sapmsg::{SapMsg, SapMsgInner};
use crate::config::stack_config::*;
use crate::unimplemented_log;

pub struct AckData {
    pub addr: TetraAddress,
    pub t_start: TdmaTime,
    pub n: u8
}

pub struct Llc {
    config: SharedConfig,
    scheduled_out_acks: Vec<AckData>,
    expected_in_acks: Vec<AckData>,

    /// Next sequence number to use for outgoing BL-DATA/BL-ADATA
    /// TODO FIXME make this a link-specific value
    next_bl_ack_seq: u8,
}

impl Llc {
    pub fn new(config: SharedConfig) -> Self {
        Self { 
            config,
            // bl_links: BlLinkManager::new(),
            scheduled_out_acks: Vec::new(),
            expected_in_acks: Vec::new(),
            next_bl_ack_seq: 0,
        }
    }

    /// Schedule an ACK to be sent at a later time
    pub fn schedule_outgoing_ack(&mut self, t: TdmaTime, addr: TetraAddress, n: u8) {
        self.scheduled_out_acks.push(AckData{t_start: t, n, addr});
    }

    /// Returns details for outstanding to-be-sent ACK, if any. Returned u8 is the sequence number
    fn get_out_ack_n_if_any(&mut self, tn: u8, addr: TetraAddress) -> Option<u8> {
        for i in 0..self.scheduled_out_acks.len() {
            if self.scheduled_out_acks[i].t_start.t == tn && self.scheduled_out_acks[i].addr.ssi == addr.ssi {
                let n = self.scheduled_out_acks[i].n;
                self.scheduled_out_acks.remove(i);
                return Some(n);
            }
        }
        None
    }

    /// Returns the next sequence number for this link
    fn get_expected_in_ack_n(&mut self, _tn: u8, _addr: TetraAddress) -> u8 {
        // unimplemented_log!("get_expected_in_ack_n not implemented on a per-link basis");
        tracing::debug!("unimplemented: get_expected_in_ack_n not implemented on a per-link basis");
        self.next_bl_ack_seq ^= 1;
        self.next_bl_ack_seq ^ 1
    }

    /// Register that we expect an ACK for this link
    fn expect_ack(&mut self, t: TdmaTime, addr: TetraAddress) -> u8 {
        let n = self.get_expected_in_ack_n(t.t, addr);
        self.expected_in_acks.push(AckData{t_start: t, n, addr});
        n
    }

    fn format_ack_list(ack_list: &Vec<AckData>) -> String{
        let mut ret = String::new();
        ret.push_str("Expected in acks:\n");
        for ack in ack_list {
            ret.push_str(&format!("  t: {}, ssi: {}, n: {}\n", ack.t_start.t, ack.addr.ssi, ack.n));
        }
        ret
    }

    /// Process incoming ACK. Remove outstanding ACK expectation. We ignore unexpected ones, might be a retransmission
    fn process_incoming_ack(&mut self, tn: u8, addr: TetraAddress, n: u8) {
        for i in 0..self.expected_in_acks.len() {
            if self.expected_in_acks[i].t_start.t == tn && self.expected_in_acks[i].addr.ssi == addr.ssi {
                if self.expected_in_acks[i].n != n {
                    tracing::warn!("Received unexpected ACK for t: {} ssi: {} got n {}, expected {}", tn, addr.ssi, n, self.expected_in_acks[i].n);
                }
                self.expected_in_acks.remove(i);
                return
            }
        }

    }

    fn rx_tma_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tma_prim");
        match message.msg {
            SapMsgInner::TmaUnitdataInd(_) => {
                self.rx_tma_unitdata_ind(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }

    fn rx_tla_tlunitdata_req_bl(&mut self, _queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tla_tlunitdata_req_bl");
        let SapMsgInner::TlaTlUnitdataReqBl(_prim) = message.msg else { panic!() };

        unimplemented_log!("rx_tla_tlunitdata_req_bl");
    }

    /// See Clause 22.3.2.3 for Acknowledged data transmission in basic link
    fn rx_tla_tldata_req_bl(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tla_tldata_req_bl");
        let SapMsgInner::TlaTlDataReqBl(mut prim) = message.msg else { panic!() };

        // If an ack still needs to be sent, get the relevant expected sequence number
        let out_ack_n = self.get_out_ack_n_if_any(message.dltime.t, prim.main_address);

        // Get an ack sequence number for outgoing message
        let expected_in_ack_n = self.expect_ack(message.dltime, prim.main_address);

        // Construct PDU, write header. Also, register we expect an ACK
        let mut pdu_buf = BitBuffer::new_autoexpand(32);

        // Determine message type and build
        if let Some(out_ack_n) = out_ack_n {
            // BL-ADATA (with or without FCS)
            let pdu = BlAdata {
                has_fcs: prim.fcs_flag,
                nr: out_ack_n,
                ns: expected_in_ack_n, 
            };
            pdu.to_bitbuf(&mut pdu_buf);
            // Append SDU
            let sdu_len = prim.tl_sdu.get_len_remaining();
            pdu_buf.copy_bits(&mut prim.tl_sdu, sdu_len);
            pdu_buf.seek(0);
            tracing::debug!("-> {:?} sdu {}", pdu, pdu_buf.dump_bin());
        } else {
            // BL-DATA (with or without FCS)
            let pdu = BlData {
                has_fcs: prim.fcs_flag,
                ns: expected_in_ack_n,
            };
            pdu.to_bitbuf(&mut pdu_buf);
            // Append SDU
            let sdu_len = prim.tl_sdu.get_len_remaining();
            pdu_buf.copy_bits(&mut prim.tl_sdu, sdu_len);
            pdu_buf.seek(0);
            tracing::debug!("-> {:?} sdu {}", pdu, pdu_buf.dump_bin());
        }
        

        // TODO FIXME:
        // According to the spec we should issue a TL-REPORT to the upper layer
        // self.issue_tla_report_ind(queue, TlaReport::ConfirmHandle);

        let sapmsg = SapMsg {
            sap: Sap::TmaSap,
            src: self.entity(),
            dest: TetraEntity::Umac,
            dltime: message.dltime,
            msg: SapMsgInner::TmaUnitdataReq(TmaUnitdataReq {
                req_handle: prim.req_handle,
                pdu: pdu_buf,
                main_address: prim.main_address,
                scrambling_code: prim.scrambling_code,
                endpoint_id: prim.endpoint_id,
                stealing_permission: prim.stealing_permission,
                subscriber_class: prim.subscriber_class,
                air_interface_encryption: prim.air_interface_encryption,
                stealing_repeats_flag: prim.stealing_repeats_flag,
                data_category: prim.data_class_info
            })
        };        
        queue.push_back(sapmsg);
    }

    fn rx_tla_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tla_prim");
        match &message.msg {
            SapMsgInner::TlaTlDataReqBl(_) => {
                self.rx_tla_tldata_req_bl(queue, message);
            }
            SapMsgInner::TlaTlUnitdataReqBl(_) => {
                self.rx_tla_tlunitdata_req_bl(queue, message);

            }
            _ => panic!()
        }
    }

    /// Clause 20.4.1.1.4 TMA-UNITDATA primitive
    /// TMA-UNITDATA indication: this primitive shall be used by the MAC to deliver a received TM-SDU. This primitive
    /// may also be used with no TM-SDU if the MAC needs to inform the higher layers of a channel allocation received
    /// without an associated TM-SDU.
    fn rx_tma_unitdata_ind(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tma_unitdata_ind");
        
        // Determine which type of TL-SDU we have
        let pdu_type = if let SapMsgInner::TmaUnitdataInd(prim) = &mut message.msg {
            let Some(pdu) = prim.pdu.as_ref() else { 
                panic!("no pdu");
            };
            let Some(bits) = pdu.peek_bits(4) else {
                tracing::warn!("insufficient bits: {}", pdu.dump_bin());
                return;
            };
            let Ok(pdu_type) = LlcPduType::try_from(bits) else {
                tracing::warn!("invalid pdu type: {} in {}", bits, pdu.dump_bin());
                return;
            };

            pdu_type
        } else { panic!(); };

        // Call handler function
        match pdu_type {
            // All Basic Link types can be handled by the same function
            LlcPduType::BlAdata | LlcPduType::BlAdataFcs |
            LlcPduType::BlData | LlcPduType ::BlDataFcs |
            LlcPduType::BlUdata | LlcPduType::BlUdataFcs |
            LlcPduType::BlAck | LlcPduType::BlAckFcs => {
                self.rx_tma_unitdata_ind_bl(queue, message);
            }

            LlcPduType::AlSetup | 
            LlcPduType::AlDataAlFinal |
            LlcPduType::AlAlUdataAlUfinal |
            LlcPduType::AlAckAlRnr |
            LlcPduType::AlReconnect |
            LlcPduType::AlDisc => {
                unimplemented_log!("LlcPduType Advanced Link: {}", pdu_type);
            }

            _ => { panic!(); }
        }
    }

    fn rx_tma_unitdata_ind_bl(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tma_unitdata_ind_bl");

        // Get header bits (again) and prepare MLE message
        let SapMsgInner::TmaUnitdataInd(prim) = &mut message.msg else { panic!(); };
        let Some(mut pdu) = prim.pdu.take() else { panic!("no pdu"); };
        let Some(bits) = pdu.peek_bits(4) else {
            tracing::warn!("insufficient bits: {}", pdu.dump_bin());
            return;
        };        
        let Ok(pdu_type) = LlcPduType::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, pdu.dump_bin());
            return;
        };

        let (has_fcs, ns, nr) = match pdu_type {
            LlcPduType::BlAdata | LlcPduType::BlAdataFcs => {
                match BlAdata::from_bitbuf(&mut pdu) {
                    Ok(pdu) => {
                        tracing::debug!("<- {:?}", pdu);
                        (pdu.has_fcs, Some(pdu.ns), Some(pdu.nr))
                    }
                    Err(e) => {
                        tracing::warn!("Failed parsing BlAdata: {:?} {}", e, pdu.dump_bin());
                        return;
                    }
                }         
            }

            LlcPduType::BlData | LlcPduType ::BlDataFcs => {
                match BlData::from_bitbuf(&mut pdu) {
                    Ok(pdu) => {
                        tracing::debug!("<- {:?}", pdu);
                        (pdu.has_fcs, Some(pdu.ns), None)
                    }
                    Err(e) => {
                        tracing::warn!("Failed parsing BlData: {:?} {}", e, pdu.dump_bin());
                        return;
                    }
                }
            }
            LlcPduType::BlAck | LlcPduType::BlAckFcs => {
                match BlAck::from_bitbuf(&mut pdu) {
                    Ok(pdu) => {
                        tracing::debug!("<- {:?}", pdu);
                        (pdu.has_fcs, None, Some(pdu.nr))
                    }
                    Err(e) => {
                        tracing::warn!("Failed parsing BlAck: {:?} {}", e, pdu.dump_bin());
                        return;
                    }
                }
            }
            LlcPduType::BlUdata | LlcPduType::BlUdataFcs => {
                match BlUdata::from_bitbuf(&mut pdu) {
                    Ok(pdu) => {
                        tracing::debug!("<- {:?}", pdu);
                        (pdu.has_fcs, None, None)
                    }
                    Err(e) => {
                        tracing::warn!("Failed parsing BlUdata: {:?} {}", e, pdu.dump_bin());
                        return;
                    }

                }
            }
            _ => { panic!(); }
        };
        
        // If FCS is present, check it. If wrong, we bail here
        if has_fcs && !check_fcs(&pdu) {
            tracing::warn!("FCS check failed");
            return;
        }

        // If ns is present, we need to send an ACK
        if let Some(ns) = ns {
            // Send ACK
            let ul_time = message.dltime.add_timeslots(-2);
            self.schedule_outgoing_ack(ul_time, prim.main_address, ns);
        }

        // if nr is present, we have received an ACK on a previous message
        if let Some(nr) = nr {
            let ul_time = message.dltime.add_timeslots(-2);
            self.process_incoming_ack(ul_time.t, prim.main_address, nr);
        }

        if pdu_type == LlcPduType::BlAck || pdu_type == LlcPduType::BlAckFcs {
            // No need to do anything further
            // TODO FIXME: flag sent sdu as acked
            return;
        }

        // If unacknowledged data transfer service, we send a TL-UNITDATA indication
        // to MLE. If acknowledged data transfer service, we send a TL-DATA indication
        
        pdu.set_raw_start(pdu.get_raw_pos());
        // tracing::info!("got sdu: {:}", sdu.dump_bin());
        let s = if pdu_type == LlcPduType::BlUdata || pdu_type == LlcPduType::BlUdataFcs {
            // Unacknowledged data transfer service
            let m = TlaTlUnitdataIndBl {
                address_type: 0, // TODO FIXME
                main_address: prim.main_address,
                link_id: 0, // TODO FIXME
                endpoint_id: prim.endpoint_id,
                new_endpoint_id: prim.new_endpoint_id,
                css_endpoint_id: prim.css_endpoint_id,
                tl_sdu: if pdu.get_len_remaining() > 0 { Some(pdu) } else { None },
                scrambling_code: prim.scrambling_code,
                fcs_flag: has_fcs,
                air_interface_encryption: prim.air_interface_encryption,
                chan_change_resp_req: prim.chan_change_response_req,
                chan_change_handle: prim.chan_change_handle,
                chan_info: prim.chan_info,
                report: None, // TODO FIXME
            };
            SapMsg{ 
                sap: Sap::TlaSap,
                src: TetraEntity::Llc,
                dest: TetraEntity::Mle,
                dltime: message.dltime,
                msg: SapMsgInner::TlaTlUnitdataIndBl(m) 
            }
        } else {
            // Acknowledged data transfer service
            let m = TlaTlDataIndBl {
                address_type: 0, // TODO FIXME
                main_address: prim.main_address,
                link_id: 0, // TODO FIXME
                endpoint_id: prim.endpoint_id,
                new_endpoint_id: prim.new_endpoint_id,
                css_endpoint_id: prim.css_endpoint_id,
                tl_sdu: if pdu.get_len_remaining() > 0 { Some(pdu) } else { None },
                scrambling_code: prim.scrambling_code,
                fcs_flag: has_fcs,
                air_interface_encryption: prim.air_interface_encryption,
                chan_change_resp_req: prim.chan_change_response_req,
                chan_change_handle: prim.chan_change_handle,
                chan_info: prim.chan_info,
                req_handle: 0, // TODO FIXME
            };
            SapMsg{ 
                sap: Sap::TlaSap,
                src: TetraEntity::Llc,
                dest: TetraEntity::Mle,
                dltime: message.dltime,
                msg: SapMsgInner::TlaTlDataIndBl(m) 
            }
        };

        queue.push_back(s);
    }
}

impl TetraEntityTrait for Llc {

    fn entity(&self) -> TetraEntity {
        TetraEntity::Llc
    }

    fn set_config(&mut self, config: SharedConfig) {
        self.config = config;
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        
        tracing::debug!("rx_prim: {:?}", message);
        match message.sap {
            Sap::TmaSap => {
                self.rx_tma_prim(queue, message);
            }

            // TMB-SAP and TMC-SAP are skipped and passed straight between MAC and MLE
            
            Sap::TlaSap => {
                self.rx_tla_prim(queue, message);
            }
            _ => panic!()
        }
    }

    fn tick_end(&mut self, queue: &mut MessageQueue, ts: Option<TdmaTime>) -> bool{
        
        // Check if any unsent ACKs are still here
        // Take oldest element from scheduled_out_acks, and remove it from the list
        let ret = !self.scheduled_out_acks.is_empty();
        while let Some(ack) = self.scheduled_out_acks.first() {

            tracing::debug!("tick_end: auto-ack for ssi: {}, n: {}", ack.addr.ssi, ack.n);
            
            let mut pdu_buf = BitBuffer::new_autoexpand(5);
            let pdu = BlAck {
                has_fcs: false,
                nr: ack.n
            };
            pdu.to_bitbuf(&mut pdu_buf);
            pdu_buf.seek(0);
            tracing::debug!("-> {:?} {}", pdu, pdu_buf.dump_bin());

            let sapmsg = SapMsg {
                sap: Sap::TmaSap,
                src: self.entity(),
                dest: TetraEntity::Umac,
                dltime: ts.unwrap_or_default(),
                msg: SapMsgInner::TmaUnitdataReq(TmaUnitdataReq {
                    req_handle: 0, // TODO FIXME
                    pdu: pdu_buf,
                    main_address: ack.addr,
                    scrambling_code: self.config.config().scrambling_code(),
                    endpoint_id: ack.t_start.t as i32, // TODO FIXME
                    stealing_permission: false, // TODO FIXME
                    subscriber_class: 0, // TODO FIXME
                    air_interface_encryption: None, // TODO FIXME
                    stealing_repeats_flag: None, // TODO FIXME
                    data_category: None // TODO FIXME
                })
            };        
            queue.push_back(sapmsg);
            self.scheduled_out_acks.remove(0);
        }

        ret
    }
}
