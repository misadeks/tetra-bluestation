use crate::{
    common::{
        address::{SsiType, TetraAddress},
        bitbuffer::BitBuffer,
        messagerouter::MessageQueue,
        ms_route_registry,
        tetra_common::Sap,
        tetra_entities::TetraEntity,
    },
    entities::cmce::{
        enums::{cmce_pdu_type_ul::CmcePduTypeUl},
        pdus::{d_status::DStatus, u_status::UStatus, d_sds_data::DSdsData, u_sds_data::USdsData},
        subentities::sds_tl::SdsTlPdu
    },
    saps::{lcmc::LcmcMleUnitdataReq, sapmsg::{SapMsg, SapMsgInner}},
};
use crate::common::tdma_time::TdmaTime;
use crate::entities::cmce::subentities::sds_tl::SdsReport;
use crate::gateway::send::dl_time_on_ts1;

const SDS_TL_DELIVERY_DEST_NOT_REACHABLE_FAILED: u8 = 0x5A; // 01011010b

fn bitbuf_from_bytes(bytes: &[u8]) -> BitBuffer {
    let mut b = BitBuffer::new_autoexpand((bytes.len() * 8) as i32 as usize);
    for &oct in bytes {
        b.write_bits(oct as u64, 8);
    }
    b.seek(0);
    b
}

pub struct SdsSubentity {}

impl SdsSubentity {
    pub fn new() -> Self { Self {} }

    fn send_cmce_dl(queue: &mut MessageQueue, dl_time: crate::common::tdma_time::TdmaTime, dst_ssi: u32, sdu: BitBuffer) -> bool {
        let Some(route) = ms_route_registry::get(dst_ssi) else {
            tracing::warn!("SDS(BS): no route for SSI {}, cannot send DL", dst_ssi);
            return false;
        };

        let dst_addr = TetraAddress { encrypted: false, ssi_type: SsiType::Ssi, ssi: dst_ssi };
        // let dl_time = dl_time_on_ts1(dl_time, 16);

        queue.push_back(SapMsg::new(
            Sap::LcmcSap,
            TetraEntity::Cmce,
            TetraEntity::Mle,
            dl_time,
            SapMsgInner::LcmcMleUnitdataReq(LcmcMleUnitdataReq {
                sdu,
                handle: 0,
                endpoint_id: route.endpoint_id,
                link_id: route.link_id,
                dst_addr,                    // ✅ explicit destination
                layer2service: 0,
                pdu_prio: 0,
                layer2_qos: 0,
                stealing_permission: false,
                stealing_repeats_flag: false,
                eligible_for_graceful_degradation: false,
            }),
        ));
        true
    }

    /// Forward U-STATUS to called party AND send a confirmation back to originator.
    pub fn rx_u_status_forward(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else { panic!(); };

        let pdu = match UStatus::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => pdu,
            Err(e) => {
                tracing::warn!("SDS(BS): Failed parsing UStatus: {:?} {}", e, prim.sdu.dump_bin());
                return;
            }
        };

        let src_ssi = prim.received_tetra_address.ssi;
        let dst_ssi = match (pdu.called_party_type_identifier, pdu.called_party_ssi) {
            (1 | 2, Some(ssi)) => ssi as u32,
            _ => {
                tracing::warn!("SDS(BS): UStatus without called_party_ssi (CPTI={})", pdu.called_party_type_identifier);
                return;
            }
        };

        // 1) forward to destination
        let d_to_dst = DStatus {
            calling_party_type_identifier: 1,
            calling_party_address_ssi: Some(src_ssi as u64),
            calling_party_extension: None,
            pre_coded_status: pdu.pre_coded_status,
            external_subscriber_number: None,
            dm_ms_address: None,
        };

        let mut sdu_to_dst = BitBuffer::new_autoexpand(96);
        if let Err(e) = d_to_dst.to_bitbuf(&mut sdu_to_dst) {
            tracing::warn!("SDS(BS): encode DStatus->dst failed: {:?}", e);
            return;
        }
        sdu_to_dst.seek(0);

        tracing::info!("SDS(BS): forward STATUS {} -> {} (status={})", src_ssi, dst_ssi, pdu.pre_coded_status);
        if !Self::send_cmce_dl(queue, message.dltime, dst_ssi, sdu_to_dst) {
            return;
        }

        // 2) confirmation back to originator (pragmatic, vendor-friendly)
        // We present it as if it came from the called party.
        let d_to_src = DStatus {
            calling_party_type_identifier: 1,
            calling_party_address_ssi: Some(dst_ssi as u64),
            calling_party_extension: None,
            pre_coded_status: pdu.pre_coded_status,
            external_subscriber_number: None,
            dm_ms_address: None,
        };

        let mut sdu_to_src = BitBuffer::new_autoexpand(96);
        if let Err(e) = d_to_src.to_bitbuf(&mut sdu_to_src) {
            tracing::warn!("SDS(BS): encode DStatus->src failed: {:?}", e);
            return;
        }
        sdu_to_src.seek(0);

        tracing::info!("SDS(BS): confirm STATUS accepted to {} (as-from={})", src_ssi, dst_ssi);
        Self::send_cmce_dl(queue, message.dltime.add_timeslots(4), src_ssi, sdu_to_src);
    }

    fn send_sds_tl_report(
        queue: &mut MessageQueue,
        dl_time: TdmaTime,
        dst_ssi: u32,          // who receives the REPORT (originator)
        as_from_ssi: u32,      // calling_party shown in D-SDS-DATA (often the intended destination SSI)
        protocol_id: u8,
        message_reference: u8,
        delivery_status: u8,
    ) {
        // Build SDS-TL REPORT (UD4 payload)
        let tl = SdsTlPdu::Report(SdsReport {
            protocol_id,
            acknowledgement_required: false,
            storage: false,
            delivery_status,
            message_reference,
            validity_period: None,
            forward_address: None,
            user_data: vec![],
        });

        let ud4_bytes = match tl.to_ud4_bytes() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("SDS(BS): build SDS-TL REPORT failed: {:?}", e);
                return;
            }
        };

        let ud4 = bitbuf_from_bytes(&ud4_bytes);
        let ud4_len_bits = ud4.get_len() as u64;

        // Wrap it into D-SDS-DATA (SDTI=3)
        let d = DSdsData {
            calling_party_type_identifier: 1,
            calling_party_address_ssi: Some(as_from_ssi as u64),
            calling_party_extension: None,

            short_data_type_identifier: 3,
            user_defined_data_1: None,
            user_defined_data_2: None,
            user_defined_data_3: None,

            length_indicator: Some(ud4_len_bits),
            user_defined_data_4: Some(ud4),

            external_subscriber_number: None,
            dm_ms_address: None,
        };

        let mut sdu = BitBuffer::new_autoexpand(256);
        if let Err(e) = d.to_bitbuf(&mut sdu) {
            tracing::warn!("SDS(BS): encode DSdsData (REPORT) failed: {:?}", e);
            return;
        }
        sdu.seek(0);

        tracing::info!(
            "SDS(BS): sending SDS-TL REPORT to {} (as-from={}) status=0x{:02X} msg_ref={}",
            dst_ssi, as_from_ssi, delivery_status, message_reference
        );

        Self::send_cmce_dl(queue, dl_time, dst_ssi, sdu);
    }

    pub fn rx_u_sds_data_forward(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else { panic!(); };

        let mut pdu = match USdsData::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => pdu,
            Err(e) => {
                tracing::warn!("SDS(BS): Failed parsing USdsData: {:?} {}", e, prim.sdu.dump_bin());
                return;
            }
        };


        let src_ssi = prim.received_tetra_address.ssi;
        let dst_ssi = match (pdu.called_party_type_identifier, pdu.called_party_ssi) {
            (1 | 2, Some(ssi)) => ssi as u32,
            _ => {
                tracing::warn!("SDS(BS): USdsData without called_party_ssi (CPTI={})", pdu.called_party_type_identifier);
                return;
            }
        };

        if pdu.short_data_type_identifier == 3 {
            if let Some(mut ud4) = pdu.user_defined_data_4.take() {
                ud4.seek(0);

                match SdsTlPdu::from_ud4(&mut ud4) {
                    Ok(tl) => {
                        if let Some(txt) = tl.decoded_text_latin1() {
                            tracing::info!("SDS(BS): Text message (Latin-1): {:?}", txt);
                        }
                    }
                    Err(e) => tracing::warn!("SDS-TL parse failed: {:?} {}", e, ud4.dump_bin()),
                }

                // put it back so forwarding can encode DSdsData SDTI=3
                ud4.seek(0);
                pdu.user_defined_data_4.replace(ud4);
            }
        }

        // If destination is offline/unreachable, send REPORT back to originator and stop.
        if ms_route_registry::get(dst_ssi).is_none() || src_ssi == dst_ssi {
            // Extract protocol_id + message_reference from SDS-TL TRANSFER (SDTI=3).
            // If parsing fails, fall back to protocol_id=130 (text messaging) and msg_ref=0.
            let (protocol_id, msg_ref) = if pdu.short_data_type_identifier == 3 {
                if let Some(mut ud4) = pdu.user_defined_data_4.take() {
                    ud4.seek(0);
                    let parsed = SdsTlPdu::from_ud4(&mut ud4).ok();
                    ud4.seek(0);
                    pdu.user_defined_data_4.replace(ud4);

                    match parsed {
                        Some(SdsTlPdu::Transfer(t)) => (t.protocol_id, t.message_reference),
                        Some(SdsTlPdu::Report(r)) => (r.protocol_id, r.message_reference),
                        Some(SdsTlPdu::Ack(a)) => (a.protocol_id, a.message_reference),
                        _ => (130u8, 0u8),
                    }
                } else {
                    (130u8, 0u8)
                }
            } else {
                (130u8, 0u8)
            };

            // Pace it by +4 timeslots (you found this is stable)
            let dl_time = message.dltime.add_timeslots(4);

            // Send REPORT to originator, "as-from" the intended destination SSI.
            Self::send_sds_tl_report(
                queue,
                dl_time,
                src_ssi,
                dst_ssi,
                protocol_id,
                msg_ref,
                SDS_TL_DELIVERY_DEST_NOT_REACHABLE_FAILED,
            );

            tracing::info!(
        "SDS(BS): dst {} unreachable, reported failure to {}",
        dst_ssi, src_ssi
    );
            return;
        }


        let d = DSdsData {
            calling_party_type_identifier: 1,
            calling_party_address_ssi: Some(src_ssi as u64),
            calling_party_extension: None,
            short_data_type_identifier: pdu.short_data_type_identifier,
            user_defined_data_1: pdu.user_defined_data_1,
            user_defined_data_2: pdu.user_defined_data_2,
            user_defined_data_3: pdu.user_defined_data_3,
            length_indicator: pdu.length_indicator,
            user_defined_data_4: pdu.user_defined_data_4,
            external_subscriber_number: None,
            dm_ms_address: None,
        };

        tracing::info!("SDS sent to {}, content: {}", dst_ssi, d);

        let mut sdu = BitBuffer::new_autoexpand(128);
        if let Err(e) = d.to_bitbuf(&mut sdu) {
            tracing::warn!("SDS(BS): encode DSdsData failed: {:?}", e);
            return;
        }
        sdu.seek(0);

        tracing::info!("SDS(BS): forward SDS {} -> {} (SDTI={})", src_ssi, dst_ssi, pdu.short_data_type_identifier);
        Self::send_cmce_dl(queue, message.dltime, dst_ssi, sdu);
    }

    pub fn route_rf_deliver(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &message.msg else { panic!(); };
        let Some(bits) = prim.sdu.peek_bits(5) else { return; };
        let Ok(pdu_type) = CmcePduTypeUl::try_from(bits) else { return; };

        match pdu_type {
            CmcePduTypeUl::UStatus => self.rx_u_status_forward(queue, message),
            CmcePduTypeUl::USdsData => self.rx_u_sds_data_forward(queue, message),
            _ => {}
        }
    }
    }

