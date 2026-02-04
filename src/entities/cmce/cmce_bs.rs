use crate::config::stack_config::SharedConfig;
use crate::common::address::{SsiType, TetraAddress};
use crate::common::bitbuffer::BitBuffer;
use crate::common::messagerouter::MessageQueue;
use crate::common::tetra_common::Sap;
use crate::common::tetra_entities::TetraEntity;
use crate::common::online;
use crate::common::group_registry;
use crate::common::groupbook;
use crate::common::sds_job;
use crate::common::voice_audio;
use crate::entities::TetraEntityTrait;
use crate::saps::sapmsg::{SapMsg, SapMsgInner};
use crate::unimplemented_log;

use std::collections::HashMap;

use super::enums::cmce_pdu_type_ul::CmcePduTypeUl;
use super::pdus::d_call_proceeding::DCallProceeding;
use super::pdus::d_connect::DConnect;
use super::pdus::d_connect_acknowledge::DConnectAcknowledge;
use super::pdus::d_info::DInfo;
use super::pdus::d_setup::DSetup;
use super::pdus::d_release::DRelease;
use super::pdus::d_tx_ceased::DTxCeased;
use super::pdus::d_tx_granted::DTxGranted;
use super::pdus::d_tx_wait::DTxWait;
use super::pdus::d_sds_data::DSdsData;
use super::pdus::u_connect::UConnect;
use super::pdus::u_disconnect::UDisconnect;
use super::pdus::u_info::UInfo;
use super::pdus::u_release::URelease;
use super::pdus::u_setup::USetup;
use super::pdus::u_sds_data::USdsData;
use super::pdus::u_tx_ceased::UTxCeased;
use super::pdus::u_tx_demand::UTxDemand;
use super::subentities::cc::CcSubentity;
use super::subentities::sds::SdsSubentity;
use super::subentities::ss::SsSubentity;

/// Minimal CMCE call-control state (voice MVP).
///
/// This is intentionally tiny: enough to accept a U-SETUP and respond with the
/// key downlink PDUs so we can iterate on traffic-channel / speech later.
#[derive(Debug, Clone)]
struct VoiceCallCtx {
    call_id: u16,
    caller_issi: u32,
    called_gssi: Option<u32>,
    called_issi: Option<u32>,
    basic_service_information: u8,
    call_priority: u8,
    tx_owner_issi: Option<u32>,
}

pub struct CmceBs {
    config: SharedConfig,

    sds: SdsSubentity,
    cc: CcSubentity,
    ss: SsSubentity,

    /// Per-call context (keyed by call_id).
    voice_calls: HashMap<u16, VoiceCallCtx>,
    next_call_id: u16,
}

const TX_GRANT_GRANTED: u8 = 1;
const TX_GRANT_NOT_GRANTED: u8 = 0;
const TX_GRANT_QUEUED: u8 = 2;
const TX_GRANT_GRANTED_TO_ANOTHER: u8 = 3;


impl CmceBs {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            config,
            sds: SdsSubentity::new(),
            cc: CcSubentity::new(),
            ss: SsSubentity::new(),

            voice_calls: HashMap::new(),
            next_call_id: 1,
        }
    }

    fn alloc_call_id(&mut self) -> u16 {
        // 14-bit space, avoid 0.
        let id = self.next_call_id & 0x3FFF;
        self.next_call_id = (self.next_call_id.wrapping_add(1)) & 0x3FFF;
        if id == 0 { 1 } else { id }
    }

    fn is_known_group(&self, gssi: u32) -> bool {
        if !group_registry::members(gssi).is_empty() {
            return true;
        }
        groupbook::get().groups.iter().any(|g| g.gssi == gssi)
    }

    fn build_dl_pdu(
        &self,
        f: impl FnOnce(&mut BitBuffer) -> Result<(), crate::common::pdu_parse_error::PduParseErr>,
    ) -> Option<BitBuffer> {
        let mut pdu = BitBuffer::new_autoexpand(1024);
        if let Err(e) = f(&mut pdu) {
            tracing::warn!("CMCE DL encode failed: {:?}", e);
            return None;
        }
        pdu.seek(0);
        Some(pdu)
    }

    fn tx_group_listeners(
        &mut self,
        queue: &mut MessageQueue,
        dltime: crate::common::tdma_time::TdmaTime,
        gssi: u32,
        link_id: u16,
        endpoint_id: u16,
        exclude_issi: Option<u32>,
        pdu: BitBuffer,
    ) {
        let members = group_registry::members(gssi);
        let mut sent_any = false;
        for ssi in members {
            if Some(ssi) == exclude_issi {
                continue;
            }
            let dst = TetraAddress { encrypted: false, ssi_type: SsiType::Ssi, ssi };
            self.tx_lcmc_dl_bits(queue, dltime, dst, link_id, endpoint_id, pdu.clone());
            sent_any = true;
        }
        if !sent_any {
            let dst_group = TetraAddress { encrypted: false, ssi_type: SsiType::Gssi, ssi: gssi };
            self.tx_lcmc_dl_bits(queue, dltime, dst_group, link_id, endpoint_id, pdu);
        }
    }

    fn tx_lcmc_dl_bits(
        &mut self,
        queue: &mut MessageQueue,
        dltime: crate::common::tdma_time::TdmaTime,
        dst: TetraAddress,
        link_id: u16,
        endpoint_id: u16,
        mut pdu: BitBuffer,
    ) {
        pdu.seek(0);
        let sapmsg = SapMsg {
            sap: Sap::LcmcSap,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Mle,
            dltime,
            msg: SapMsgInner::LcmcMleUnitdataReq(crate::saps::lcmc::LcmcMleUnitdataReq {
                sdu: pdu,
                address: dst,
                handle: 0,
                endpoint_id: endpoint_id as i32,
                link_id: link_id as i32,
                layer2service: 0,
                pdu_prio: 0,
                layer2_qos: 0,
                stealing_permission: false,
                stealing_repeats_flag: false,
                eligible_for_graceful_degradation: true,
            }),
        };
        queue.push_back(sapmsg);
    }

    fn tx_lcmc_dl_pdu(
        &mut self,
        queue: &mut MessageQueue,
        dltime: crate::common::tdma_time::TdmaTime,
        dst: TetraAddress,
        link_id: u16,
        endpoint_id: u16,
        f: impl FnOnce(&mut BitBuffer) -> Result<(), crate::common::pdu_parse_error::PduParseErr>,
    ) {
        let mut pdu = BitBuffer::new_autoexpand(1024);
        if let Err(e) = f(&mut pdu) {
            tracing::warn!("CMCE DL encode failed: {:?}", e);
            return;
        }
        self.tx_lcmc_dl_bits(queue, dltime, dst, link_id, endpoint_id, pdu);
    }

    fn tx_dsds(
        &mut self,
        queue: &mut MessageQueue,
        dltime: crate::common::tdma_time::TdmaTime,
        dst: TetraAddress,
        link_id: u16,
        endpoint_id: u16,
        dsds: DSdsData,
    ) {
        let mut pdu = BitBuffer::new_autoexpand(4096);
        if let Err(e) = dsds.to_bitbuf(&mut pdu) {
            tracing::error!("SDS TX encode failed: {:?}", e);
            return;
        }
        pdu.seek(0);

        let sapmsg = SapMsg {
            sap: Sap::LcmcSap,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Mle,
            dltime,
            msg: SapMsgInner::LcmcMleUnitdataReq(crate::saps::lcmc::LcmcMleUnitdataReq {
                sdu: pdu,
                address: dst,
                handle: 0,
                endpoint_id: endpoint_id as i32,
                link_id: link_id as i32,
                layer2service: 0,
                pdu_prio: 0,
                layer2_qos: 0,
                stealing_permission: false,
                stealing_repeats_flag: false,
                eligible_for_graceful_degradation: true,
            }),
        };
        queue.push_back(sapmsg);
    }

    fn rx_tnsds_unitdata_req(
        &mut self,
        queue: &mut MessageQueue,
        dltime: crate::common::tdma_time::TdmaTime,
        req: crate::saps::tnsds::TnsdsUnitdataReq,
    ) {
        let trace = req.trace.clone();
        if let Some(tr) = trace.clone() {
            sds_job::update_part_status(tr.job_id, tr.part_index, sds_job::SdsPartStatus::SubmittedToStack);
        }
        // Encode as SDS Type-4 in D-SDS-DATA
        let mut payload = req.type4_payload;
        payload.seek(0);

        if payload.get_len() > crate::common::sds_codec::MAX_SDS_TYPE4_BITS {
            tracing::error!("SDS TX payload too large: {} bits (max {} bits). Split the message or shorten it.", payload.get_len(), crate::common::sds_codec::MAX_SDS_TYPE4_BITS);
            if let Some(tr) = trace.clone() {
                sds_job::set_part_error(tr.job_id, tr.part_index, format!("payload too large: {} bits", payload.get_len()));
            }
            return;
        }

        tracing::info!("SDS TX req: dst={:?} calling={} link_id={} endpoint_id={} mr={} drr={} payload_bits={} payload_hex={}",
            req.dst, req.calling_ssi, req.link_id, req.endpoint_id, req.message_reference, req.delivery_report_request,
            payload.get_len(), payload.dump_hex());

        let dsds = DSdsData {
            calling_party_type_identifier: 1, // SSI
            calling_party_address_ssi: Some(req.calling_ssi as u64),
            calling_party_extension: None,
            short_data_type_identifier: 3,
            user_defined_data_1: None,
            user_defined_data_2: None,
            user_defined_data_3: None,
            length_indicator: Some(payload.get_len() as u64),
            user_defined_data_4: Some(payload),
            external_subscriber_number: None,
            dm_ms_address: None,
        };

        self.tx_dsds(queue, dltime, req.dst, req.link_id, req.endpoint_id, dsds);
        if let Some(tr) = trace {
            sds_job::update_part_status(tr.job_id, tr.part_index, sds_job::SdsPartStatus::LcmcSubmitted);
        }
    }

    pub fn rx_lcmc_mle_unitdata_ind(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_lcmc_mle_unitdata_ind");

        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else { panic!() };

        let Some(bits) = prim.sdu.peek_bits(5) else {
            tracing::warn!("insufficient bits: {}", prim.sdu.dump_bin());
            return;
        };
        let Ok(pdu_type) = CmcePduTypeUl::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, prim.sdu.dump_bin());
            return;
        };

        match pdu_type {
            CmcePduTypeUl::USdsData => {
                let mut sdu = BitBuffer::from_bitbuffer_pos(&prim.sdu);
                match USdsData::from_bitbuf(&mut sdu) {
                    Ok(usds) => {
                        if let Some(ref p) = usds.user_defined_data_4 {
                            tracing::info!("SDS RX: from_ssi={} dst_type={} dst_ssi={:?} sdti={} payload_bits={} payload_hex={}",
                                prim.received_tetra_address.ssi,
                                usds.called_party_type_identifier,
                                usds.called_party_ssi,
                                usds.short_data_type_identifier,
                                p.get_len(),
                                p.dump_hex());
                        } else {
                            tracing::info!("SDS RX: from_ssi={} dst_type={} dst_ssi={:?} sdti={} payload_bits=0 payload_hex=", 
                                prim.received_tetra_address.ssi,
                                usds.called_party_type_identifier,
                                usds.called_party_ssi,
                                usds.short_data_type_identifier);
                        }

                        // Minimal relay: forward Type-4 to called_party_ssi as downlink D-SDS-DATA
                        if let Some(dst_ssi) = usds.called_party_ssi {
                            let dst = TetraAddress {
                                encrypted: false,
                                ssi_type: match usds.called_party_type_identifier {
                                    1 => SsiType::Issi,
                                    2 => SsiType::Gssi,
                                    _ => SsiType::Ssi,
                                },
                                ssi: dst_ssi as u32,
                            };

                            let calling = prim.received_tetra_address.ssi as u64;
                            online::record_seen(calling as u32);
                            if usds.called_party_type_identifier == 2 { online::record_group_seen(dst_ssi as u32, calling as u32); }
                            let dsds = DSdsData {
                                calling_party_type_identifier: 1,
                                calling_party_address_ssi: Some(calling),
                                calling_party_extension: None,
                                short_data_type_identifier: usds.short_data_type_identifier,
                                user_defined_data_1: usds.user_defined_data_1,
                                user_defined_data_2: usds.user_defined_data_2,
                                user_defined_data_3: usds.user_defined_data_3,
                                length_indicator: usds.length_indicator,
                                user_defined_data_4: usds.user_defined_data_4,
                                external_subscriber_number: usds.external_subscriber_number,
                                dm_ms_address: usds.dm_ms_address,
                            };

                            self.tx_dsds(queue, message.dltime, dst, prim.link_id as u16, prim.endpoint_id as u16, dsds);
                        }
                    }
                    Err(e) => tracing::warn!("USdsData parse failed: {:?}", e),
                }
            }
            CmcePduTypeUl::UAlert => unimplemented_log!("UAlert"),
            CmcePduTypeUl::UDisconnect => {
                let mut sdu = BitBuffer::from_bitbuffer_pos(&prim.sdu);
                match UDisconnect::from_bitbuf(&mut sdu) {
                    Ok(ud) => {
                        let issi = prim.received_tetra_address.ssi;
                        tracing::info!(
                            "VOICE UDisconnect: from={} call_id={} cause={}",
                            issi,
                            ud.call_identifier,
                            ud.disconnect_cause
                        );

                        if let Some(ctx) = self.voice_calls.remove(&ud.call_identifier) {
                            voice_audio::set_talker(ud.call_identifier, None);
                            voice_audio::end_call(ud.call_identifier, "u_disconnect");

                            let release = DRelease {
                                call_identifier: ud.call_identifier,
                                disconnect_cause: ud.disconnect_cause,
                                notification_indicator: None,
                                facility: None,
                                proprietary: None,
                            };

                            // Notify caller.
                            let dst_caller = TetraAddress {
                                encrypted: false,
                                ssi_type: SsiType::Ssi,
                                ssi: ctx.caller_issi,
                            };
                            self.tx_lcmc_dl_pdu(queue, message.dltime, dst_caller, prim.link_id as u16, prim.endpoint_id as u16, |b| {
                                release.to_bitbuf(b)
                            });

                            // Notify called party / listeners, if any.
                            if let Some(gssi) = ctx.called_gssi {
                                if let Some(pdu) = self.build_dl_pdu(|b| release.to_bitbuf(b)) {
                                    self.tx_group_listeners(
                                        queue,
                                        message.dltime,
                                        gssi,
                                        prim.link_id as u16,
                                        prim.endpoint_id as u16,
                                        Some(ctx.caller_issi),
                                        pdu,
                                    );
                                }
                            } else if let Some(issi) = ctx.called_issi {
                                if issi != ctx.caller_issi {
                                    let dst = TetraAddress { encrypted: false, ssi_type: SsiType::Ssi, ssi: issi };
                                    self.tx_lcmc_dl_pdu(queue, message.dltime, dst, prim.link_id as u16, prim.endpoint_id as u16, |b| {
                                        release.to_bitbuf(b)
                                    });
                                }
                            }
                        } else {
                            tracing::warn!("VOICE UDisconnect: unknown call_id={}", ud.call_identifier);
                        }
                    }
                    Err(e) => tracing::warn!("UDisconnect parse failed: {:?}", e),
                }
            }
            CmcePduTypeUl::UInfo => {
                let mut sdu = BitBuffer::from_bitbuffer_pos(&prim.sdu);
                match UInfo::from_bitbuf(&mut sdu) {
                    Ok(ui) => {
                        let issi = prim.received_tetra_address.ssi;
                        tracing::info!(
                            "VOICE UInfo: from={} call_id={} poll_resp={} modify={:?} dtmf={:?}",
                            issi,
                            ui.call_identifier,
                            ui.poll_response,
                            ui.modify,
                            ui.dtmf
                        );

                        if self.voice_calls.get(&ui.call_identifier).is_none() {
                            tracing::warn!("UInfo for unknown call_id={}", ui.call_identifier);
                            return;
                        }
                        voice_audio::mark_activity_now(ui.call_identifier, message.dltime);

                        let has_payload = ui.modify.is_some() || ui.dtmf.is_some() || ui.facility.is_some() || ui.proprietary.is_some();
                        if !has_payload {
                            return;
                        }

                        let info = DInfo {
                            call_identifier: ui.call_identifier,
                            reset_call_time_out_timer_t310_: true,
                            poll_request: false,
                            new_call_identifier: None,
                            call_time_out: None,
                            call_time_out_set_up_phase_t301_t302_: None,
                            call_ownership: None,
                            modify: ui.modify,
                            call_status: None,
                            temporary_address: None,
                            notification_indicator: None,
                            poll_response_percentage: None,
                            poll_response_number: None,
                            dtmf: ui.dtmf,
                            facility: ui.facility,
                            poll_response_addresses: None,
                            proprietary: ui.proprietary,
                        };

                        if let Some(ctx) = self.voice_calls.get(&ui.call_identifier) {
                            if let Some(gssi) = ctx.called_gssi {
                                if let Some(pdu) = self.build_dl_pdu(|b| info.to_bitbuf(b)) {
                                    self.tx_group_listeners(
                                        queue,
                                        message.dltime,
                                        gssi,
                                        prim.link_id as u16,
                                        prim.endpoint_id as u16,
                                        Some(issi),
                                        pdu,
                                    );
                                }
                            } else if let Some(issi_dst) = ctx.called_issi {
                                if issi_dst != issi {
                                    let dst = TetraAddress { encrypted: false, ssi_type: SsiType::Ssi, ssi: issi_dst };
                                    self.tx_lcmc_dl_pdu(queue, message.dltime, dst, prim.link_id as u16, prim.endpoint_id as u16, |b| {
                                        info.to_bitbuf(b)
                                    });
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!("UInfo parse failed: {:?}", e),
                }
            }
            CmcePduTypeUl::URelease => {
                let mut sdu = BitBuffer::from_bitbuffer_pos(&prim.sdu);
                match URelease::from_bitbuf(&mut sdu) {
                    Ok(ur) => {
                        let issi = prim.received_tetra_address.ssi;
                        tracing::info!(
                            "VOICE URelease: from={} call_id={} cause={}",
                            issi,
                            ur.call_identifier,
                            ur.disconnect_cause
                        );
                        if self.voice_calls.remove(&ur.call_identifier).is_some() {
                            voice_audio::set_talker(ur.call_identifier, None);
                            voice_audio::end_call(ur.call_identifier, "u_release");
                        } else {
                            tracing::warn!("VOICE URelease: unknown call_id={}", ur.call_identifier);
                        }
                    }
                    Err(e) => tracing::warn!("URelease parse failed: {:?}", e),
                }
            }
            CmcePduTypeUl::USetup => {
                let mut sdu = BitBuffer::from_bitbuffer_pos(&prim.sdu);
                match USetup::from_bitbuf(&mut sdu) {
                    Ok(us) => {
                        let caller_issi = prim.received_tetra_address.ssi;
                        online::record_seen(caller_issi);

                        let called_ssi = us.called_party_ssi.map(|v| v as u32);
                        let (called_gssi, called_issi) = match called_ssi {
                            Some(ssi) if self.is_known_group(ssi) => {
                                online::record_group_seen(ssi, caller_issi);
                                (Some(ssi), None)
                            }
                            Some(ssi) => (None, Some(ssi)),
                            None => (None, None),
                        };

                        let call_id = self.alloc_call_id();
                        let ctx = VoiceCallCtx {
                            call_id,
                            caller_issi,
                            called_gssi,
                            called_issi,
                            basic_service_information: us.basic_service_information,
                            call_priority: us.call_priority,
                            tx_owner_issi: None,
                        };
                        self.voice_calls.insert(call_id, ctx.clone());

                        let mode = voice_audio::CallDuplexMode::from_simplex_duplex_flag(us.simplex_duplex_selection);
                        let audio_call = voice_audio::start_call(
                            call_id,
                            caller_issi,
                            called_gssi,
                            called_issi,
                            message.dltime,
                            mode,
                        );
                        tracing::info!(
                            "VOICE audio: call_id={} mode={:?} tch_ts={} tchan={}",
                            audio_call.call_id,
                            audio_call.mode,
                            audio_call.tch_ts,
                            audio_call.tchan
                        );

                        tracing::info!(
                            "VOICE USetup: caller={} called_ssi={:?} cpti={:?} bsi=0x{:02X} prio={} -> call_id={}",
                            caller_issi,
                            us.called_party_ssi,
                            us.called_party_type_identifier,
                            us.basic_service_information,
                            us.call_priority,
                            call_id
                        );

                        // Reply to caller: D-CALL-PROCEEDING and D-CONNECT.
                        let dst_caller = prim.received_tetra_address;

                        self.tx_lcmc_dl_pdu(queue, message.dltime, dst_caller, prim.link_id as u16, prim.endpoint_id as u16, |b| {
                            DCallProceeding {
                                call_identifier: call_id,
                                call_time_out_set_up_phase: 7,
                                hook_method_selection: us.hook_method_selection,
                                simplex_duplex_selection: us.simplex_duplex_selection,
                                // Optional fields are only needed if different from the request.
                                // For the MVP, keep them absent.
                                basic_service_information: None,
                                call_status: Some(0),
                                notification_indicator: None,
                                facility: None,
                                proprietary: None,
                            }.to_bitbuf(b)
                        });

                        self.tx_lcmc_dl_pdu(queue, message.dltime, dst_caller, prim.link_id as u16, prim.endpoint_id as u16, |b| {
                                DConnect {
                                    call_identifier: call_id,
                                    call_time_out: 15,
                                    hook_method_selection: us.hook_method_selection,
                                    simplex_duplex_selection: us.simplex_duplex_selection,
                                    transmission_grant: 0,
                                    transmission_request_permission: true,
                                    call_ownership: true,
                                // Type2 optional fields. For MVP we omit them (assume same as requested).
                                call_priority: None,
                                basic_service_information: None,
                                temporary_address: None,
                                notification_indicator: None,
                                facility: None,
                                proprietary: None,
                            }.to_bitbuf(b)
                        });

                        // Notify called party/listeners.
                        if let Some(gssi) = called_gssi {
                            if let Some(pdu) = self.build_dl_pdu(|b| {
                                DSetup {
                                    call_identifier: call_id,
                                    call_time_out: 15,
                                    hook_method_selection: us.hook_method_selection,
                                    simplex_duplex_selection: us.simplex_duplex_selection,
                                    basic_service_information: us.basic_service_information,
                                    transmission_grant: TX_GRANT_NOT_GRANTED,
                                    transmission_request_permission: true,
                                    call_priority: us.call_priority,
                                    notification_indicator: None,
                                    temporary_address: None,
                                    calling_party_type_identifier: Some(1u64),
                                    calling_party_address_ssi: Some(caller_issi as u64),
                                    calling_party_extension: None,
                                    external_subscriber_number: None,
                                    facility: None,
                                    dm_ms_address: None,
                                    proprietary: None,
                                }.to_bitbuf(b)
                            }) {
                                self.tx_group_listeners(
                                    queue,
                                    message.dltime,
                                    gssi,
                                    prim.link_id as u16,
                                    prim.endpoint_id as u16,
                                    Some(caller_issi),
                                    pdu,
                                );
                            }
                        } else if let Some(issi) = called_issi {
                            let dst = TetraAddress { encrypted: false, ssi_type: SsiType::Ssi, ssi: issi };
                            self.tx_lcmc_dl_pdu(queue, message.dltime, dst, prim.link_id as u16, prim.endpoint_id as u16, |b| {
                                DSetup {
                                    call_identifier: call_id,
                                    call_time_out: 15,
                                    hook_method_selection: us.hook_method_selection,
                                    simplex_duplex_selection: us.simplex_duplex_selection,
                                    basic_service_information: us.basic_service_information,
                                    transmission_grant: TX_GRANT_NOT_GRANTED,
                                    transmission_request_permission: true,
                                    call_priority: us.call_priority,
                                    notification_indicator: None,
                                    temporary_address: None,
                                    calling_party_type_identifier: Some(1u64),
                                    calling_party_address_ssi: Some(caller_issi as u64),
                                    calling_party_extension: None,
                                    external_subscriber_number: None,
                                    facility: None,
                                    dm_ms_address: None,
                                    proprietary: None,
                                }.to_bitbuf(b)
                            });
                        }
                    }
                    Err(e) => tracing::warn!("USetup parse failed: {:?}", e),
                }
            }
            CmcePduTypeUl::UConnect => {
                let mut sdu = BitBuffer::from_bitbuffer_pos(&prim.sdu);
                match UConnect::from_bitbuf(&mut sdu) {
                    Ok(uc) => {
                        let issi = prim.received_tetra_address.ssi;
                        tracing::info!(
                            "VOICE UConnect: from={} call_id={} hook={} duplex={} bsi={:?}",
                            issi,
                            uc.call_identifier,
                            uc.hook_method_selection,
                            uc.simplex_duplex_selection,
                            uc.basic_service_information
                        );

                        let Some(ctx) = self.voice_calls.get(&uc.call_identifier).cloned() else {
                            tracing::warn!("VOICE UConnect: unknown call_id={}", uc.call_identifier);
                            return;
                        };

                        // Refresh activity so setup timeouts don't trigger while waiting for traffic.
                        voice_audio::mark_activity_now(uc.call_identifier, message.dltime);

                        // Acknowledge connect to the called MS.
                        self.tx_lcmc_dl_pdu(queue, message.dltime, prim.received_tetra_address, prim.link_id as u16, prim.endpoint_id as u16, |b| {
                            DConnectAcknowledge {
                                call_identifier: uc.call_identifier,
                                call_time_out: 15,
                                transmission_grant: TX_GRANT_NOT_GRANTED,
                                transmission_request_permission: true,
                                notification_indicator: None,
                                facility: None,
                                proprietary: None,
                            }.to_bitbuf(b)
                        });

                        let dst_caller = TetraAddress {
                            encrypted: false,
                            ssi_type: SsiType::Ssi,
                            ssi: ctx.caller_issi,
                        };
                        self.tx_lcmc_dl_pdu(
                            queue,
                            message.dltime,
                            dst_caller,
                            prim.link_id as u16,
                            prim.endpoint_id as u16,
                            |b| {
                                DConnectAcknowledge {
                                    call_identifier: uc.call_identifier,
                                    call_time_out: 15,
                                    transmission_grant: TX_GRANT_NOT_GRANTED,
                                    transmission_request_permission: true,
                                    notification_indicator: None,
                                    facility: None,
                                    proprietary: None,
                                }
                                    .to_bitbuf(b)
                            },
                        );
                        tracing::info!(
                "VOICE: sent D-Connect-Ack to called={} and caller={} for call_id={}",
                issi,
                ctx.caller_issi,
                uc.call_identifier
            );

                    }
                    Err(e) => tracing::warn!("UConnect parse failed: {:?}", e),
                }
            }
            CmcePduTypeUl::UStatus => unimplemented_log!("UStatus"),
            CmcePduTypeUl::UTxCeased => {
                let mut sdu = BitBuffer::from_bitbuffer_pos(&prim.sdu);
                match UTxCeased::from_bitbuf(&mut sdu) {
                    Ok(utx) => {
                        let issi = prim.received_tetra_address.ssi;
                        if let Some(mut ctx) = self.voice_calls.get(&utx.call_identifier).cloned() {
                            ctx.tx_owner_issi = None;
                            self.voice_calls.insert(utx.call_identifier, ctx.clone());
                        }
                        voice_audio::set_talker(utx.call_identifier, None);
                        voice_audio::mark_activity_now(utx.call_identifier, message.dltime);
                        tracing::info!("VOICE UTxCeased: from={} call_id={}", issi, utx.call_identifier);

                        // Broadcast D-TX-CEASED to group and to caller.

                        // To caller
                        self.tx_lcmc_dl_pdu(queue, message.dltime, prim.received_tetra_address, prim.link_id as u16, prim.endpoint_id as u16, |b| {
                            DTxCeased {
                                call_identifier: utx.call_identifier,
                                transmission_request_permission: true,
                                notification_indicator: None,
                                facility: None,
                                dm_ms_address: None,
                                proprietary: None,
                            }.to_bitbuf(b)
                        });

                        // To called party / group listeners (if known)
                        if let Some(ctx) = self.voice_calls.get(&utx.call_identifier) {
                            if let Some(gssi) = ctx.called_gssi {
                                if let Some(pdu) = self.build_dl_pdu(|b| {
                                    DTxCeased {
                                        call_identifier: utx.call_identifier,
                                        transmission_request_permission: true,
                                        notification_indicator: None,
                                        facility: None,
                                        dm_ms_address: None,
                                        proprietary: None,
                                    }.to_bitbuf(b)
                                }) {
                                    self.tx_group_listeners(
                                        queue,
                                        message.dltime,
                                        gssi,
                                        prim.link_id as u16,
                                        prim.endpoint_id as u16,
                                        Some(ctx.caller_issi),
                                        pdu,
                                    );
                                }
                            } else if let Some(issi) = ctx.called_issi {
                                if issi != ctx.caller_issi {
                                    let dst = TetraAddress { encrypted: false, ssi_type: SsiType::Ssi, ssi: issi };
                                    self.tx_lcmc_dl_pdu(queue, message.dltime, dst, prim.link_id as u16, prim.endpoint_id as u16, |b| {
                                        DTxCeased {
                                            call_identifier: utx.call_identifier,
                                            transmission_request_permission: true,
                                            notification_indicator: None,
                                            facility: None,
                                            dm_ms_address: None,
                                            proprietary: None,
                                        }.to_bitbuf(b)
                                    });
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!("UTxCeased parse failed: {:?}", e),
                }
            }
            CmcePduTypeUl::UTxDemand => {
                let mut sdu = BitBuffer::from_bitbuffer_pos(&prim.sdu);
                match UTxDemand::from_bitbuf(&mut sdu) {
                    Ok(utx) => {
                        let issi = prim.received_tetra_address.ssi;
                        tracing::info!(
                            "VOICE UTxDemand: from={} call_id={} prio={} enc={} ",
                            issi,
                            utx.call_identifier,
                            utx.tx_demand_priority,
                            utx.encryption_control
                        );

                        let Some(mut ctx) = self.voice_calls.get(&utx.call_identifier).cloned() else {
                            tracing::warn!("UTxDemand without call context: from={} call_id={}", issi, utx.call_identifier);
                            return;
                        };
                        if voice_audio::get_call(ctx.call_id).is_none() {
                            tracing::warn!("UTxDemand for inactive call_id={} (dropping)", ctx.call_id);
                            self.voice_calls.remove(&ctx.call_id);
                            return;
                        }

                        // Simple floor control: single talker at a time.
                        if let Some(owner) = ctx.tx_owner_issi {
                            if owner != issi {
                                let wait = DTxWait {
                                    call_identifier: ctx.call_id,
                                    transmission_request_permission: true,
                                    notification_indicator: None,
                                    facility: None,
                                    dm_ms_address: None,
                                    proprietary: None,
                                };
                                self.tx_lcmc_dl_pdu(queue, message.dltime, prim.received_tetra_address, prim.link_id as u16, prim.endpoint_id as u16, |b| wait.to_bitbuf(b));
                                return;
                            }
                        }

                        ctx.tx_owner_issi = Some(issi);
                        self.voice_calls.insert(ctx.call_id, ctx.clone());
                        voice_audio::set_talker(ctx.call_id, Some(issi));
                        voice_audio::mark_activity_now(ctx.call_id, message.dltime);

                        let granted = DTxGranted {
                            call_identifier: ctx.call_id,
                            transmission_grant: TX_GRANT_GRANTED,
                            transmission_request_permission: true,
                            encryption_control: utx.encryption_control,
                            reserved: false,
                            notification_indicator: None,
                            transmitting_party_type_identifier: Some(1u64),
                            transmitting_party_address_ssi: Some(issi as u64),
                            transmitting_party_extension: None,
                            external_subscriber_number: None,
                            facility: None,
                            dm_ms_address: None,
                            proprietary: None,
                        };
                        self.tx_lcmc_dl_pdu(queue, message.dltime, prim.received_tetra_address, prim.link_id as u16, prim.endpoint_id as u16, |b| granted.to_bitbuf(b));

                        // Inform called party / group listeners about the granted talker.
                        if let Some(gssi) = ctx.called_gssi {
                            if let Some(pdu) = self.build_dl_pdu(|b| {
                                DTxGranted {
                                    call_identifier: ctx.call_id,
                                    transmission_grant: TX_GRANT_GRANTED,
                                    transmission_request_permission: true,
                                    encryption_control: utx.encryption_control,
                                    reserved: false,
                                    notification_indicator: None,
                                    transmitting_party_type_identifier: Some(1u64),
                                    transmitting_party_address_ssi: Some(issi as u64),
                                    transmitting_party_extension: None,
                                    external_subscriber_number: None,
                                    facility: None,
                                    dm_ms_address: None,
                                    proprietary: None,
                                }.to_bitbuf(b)
                            }) {
                                self.tx_group_listeners(
                                    queue,
                                    message.dltime,
                                    gssi,
                                    prim.link_id as u16,
                                    prim.endpoint_id as u16,
                                    Some(ctx.caller_issi),
                                    pdu,
                                );
                            }
                        } else if let Some(issi_dst) = ctx.called_issi {
                            if issi_dst != ctx.caller_issi {
                                let dst = TetraAddress { encrypted: false, ssi_type: SsiType::Ssi, ssi: issi_dst };
                                self.tx_lcmc_dl_pdu(queue, message.dltime, dst, prim.link_id as u16, prim.endpoint_id as u16, |b| {
                                    DTxGranted {
                                        call_identifier: ctx.call_id,
                                        transmission_grant: TX_GRANT_GRANTED,
                                        transmission_request_permission: true,
                                        encryption_control: utx.encryption_control,
                                        reserved: false,
                                        notification_indicator: None,
                                        transmitting_party_type_identifier: Some(1u64),
                                        transmitting_party_address_ssi: Some(issi as u64),
                                        transmitting_party_extension: None,
                                        external_subscriber_number: None,
                                        facility: None,
                                        dm_ms_address: None,
                                        proprietary: None,
                                    }.to_bitbuf(b)
                                });
                            }
                        }
                    }
                    Err(e) => tracing::warn!("UTxDemand parse failed: {:?}", e),
                }
            }
            CmcePduTypeUl::UCallRestore => unimplemented_log!("UCallRestore"),
            CmcePduTypeUl::UFacility => unimplemented_log!("UFacility"),
            CmcePduTypeUl::CmceFunctionNotSupported => unimplemented_log!("CmceFunctionNotSupported"),
        }
    }
}

impl TetraEntityTrait for CmceBs {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Cmce
    }

    fn set_config(&mut self, config: SharedConfig) {
        self.config = config;
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);

        match message.sap {
            Sap::LcmcSap => match message.msg {
                SapMsgInner::LcmcMleUnitdataInd(_) => self.rx_lcmc_mle_unitdata_ind(queue, message),
                _ => panic!(),
            },
            Sap::TnsdsSap => match message.msg {
                SapMsgInner::TnsdsUnitdataReq(req) => self.rx_tnsds_unitdata_req(queue, message.dltime, req),
                _ => panic!(),
            },
            _ => panic!(),
        }
    }
}
