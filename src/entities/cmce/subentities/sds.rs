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
    },
    saps::{lcmc::LcmcMleUnitdataReq, sapmsg::{SapMsg, SapMsgInner}},
};

pub struct SdsSubentity {}

impl SdsSubentity {
    pub fn new() -> Self { Self {} }

    fn send_cmce_dl(queue: &mut MessageQueue, dl_time: crate::common::tdma_time::TdmaTime, dst_ssi: u32, sdu: BitBuffer) -> bool {
        let Some(route) = ms_route_registry::get(dst_ssi) else {
            tracing::warn!("SDS(BS): no route for SSI {}, cannot send DL", dst_ssi);
            return false;
        };

        let dst_addr = TetraAddress { encrypted: false, ssi_type: SsiType::Ssi, ssi: dst_ssi };

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

    pub fn rx_u_sds_data_forward(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else { panic!(); };

        let pdu = match USdsData::from_bitbuf(&mut prim.sdu) {
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
