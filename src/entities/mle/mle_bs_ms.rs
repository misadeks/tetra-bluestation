use crate::common::address::TetraAddress;
use crate::common::messagerouter::MessageQueue;
use crate::saps::lcmc::LcmcMleUnitdataInd;
use crate::saps::lmm::LmmMleUnitdataInd;
use crate::saps::ltpd::LtpdMleUnitdataInd;
use crate::saps::tla::TlaTlDataReqBl;
use crate::entities::mle::enums::mle_pdu_type_dl::MlePduTypeDl;
use crate::entities::mle::enums::mle_protocol_discriminator::MleProtocolDiscriminator;
use crate::entities::mle::pdus::d_mle_sync::DMleSync;
use crate::entities::mle::pdus::d_mle_sysinfo::DMleSysinfo;
use crate::common::bitbuffer::BitBuffer;
use crate::common::online;
use crate::common::tetra_common::Sap;
use crate::common::tetra_entities::TetraEntity;
use crate::entities::TetraEntityTrait;
use crate::saps::sapmsg::{SapMsg, SapMsgInner};
use crate::config::stack_config::*;
use crate::unimplemented_log;

pub struct Mle {
    // config: Option<SharedConfig>,
    self_component: TetraEntity,
    config: SharedConfig,
}

impl Mle {
    pub fn new(config: SharedConfig) -> Self {
        Self { 
            self_component: TetraEntity::Mle,
            config
        }
    }

    fn rx_tla_mle_pdu(&mut self, _queue: &mut MessageQueue, message: SapMsg) {
        
        tracing::trace!("rx_tla_mle_pdu: {:?}", message);

        // Extract tm_sdu from whatever primitive we have
        let tm_sdu = {
            match message.msg {
                SapMsgInner::TlaTlDataIndBl(prim) => {
            online::record_seen(prim.main_address.ssi);
prim.tl_sdu}
                _ => { panic!(); }
            }
        };
        let Some(sdu) = tm_sdu else {
            tracing::debug!("rx_tla_mle_pdu: no tm_sdu");
            return;
        };

        // Determine which type of TL-SDU we have and call handler function
        let Some(bits) = sdu.peek_bits(3) else {
            tracing::warn!("insufficient bits: {}", sdu.dump_bin());
            return;
        };
        let Ok(pdu_type) = MlePduTypeDl::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, sdu.dump_bin());
            return;
        };

        match pdu_type {
            MlePduTypeDl::DNewCell => { unimplemented_log!("DNewCell") }
            MlePduTypeDl::DPrepareFail => { unimplemented_log!("DPrepareFail") }
            MlePduTypeDl::DNwrkBroadcast => { unimplemented_log!("DNwrkBroadcast") }
            MlePduTypeDl::DNwrkBroadcastExt => { unimplemented_log!("DNwrkBroadcastExt") } // TODO FIXME CHECK this option and assocaited int
            MlePduTypeDl::DRestoreAck => { unimplemented_log!("DRestoreAck") }
            MlePduTypeDl::DRestoreFail => { unimplemented_log!("DRestoreFail") }
            MlePduTypeDl::DChannelResponse => { unimplemented_log!("DChannelResponse") }
            MlePduTypeDl::ExtPdu => { unimplemented_log!("ExtPdu") }
        }
    }

    fn rx_tla_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        
        tracing::trace!("rx_tla_prim: {:?}", message);
        match message.msg {
            SapMsgInner::TlaTlDataIndBl(_) => {
                self.rx_tla_data_ind_bl(queue, message);
            }
            SapMsgInner::TlaTlUnitdataIndBl(_) => {
                self.rx_tla_unitdata_ind_bl(queue, message);
            }
            _ => { panic!(); }
        }
    }

    fn rx_tla_data_ind_bl(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {

        // Take ownership of bitbuf and read protocol discriminator
        let SapMsgInner::TlaTlDataIndBl(prim) = &mut message.msg else { panic!() };
        let Some(mut sdu) = prim.tl_sdu.take() else { panic!("no tl_sdu") };
        assert!(sdu.get_pos() == 0); // We should be at the start of the MAC PDU
        let Some(bits) = sdu.read_bits(3) else {
            tracing::warn!("insufficient bits: {}", sdu.dump_bin());
            return;
        };
        let Ok(pdu_type) = MleProtocolDiscriminator::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, sdu.dump_bin());
            return;
        };

        // Dispatch to appropriate component (or to self if for MLE)
        match pdu_type {
            MleProtocolDiscriminator::Mm => {
                let m = LmmMleUnitdataInd{ 
                    sdu,
                    handle: 0, // TODO FIXME
                    received_address: prim.main_address,
                    // received_address_type: 0, // TODO FIXME
                };
                let msg = SapMsg {
                    sap: Sap::LmmSap,
                    src: self.self_component,
                    dest: TetraEntity::Mm,
                    dltime: message.dltime,
                    msg: SapMsgInner::LmmMleUnitdataInd(m),
                };
                queue.push_back(msg);
            }
            MleProtocolDiscriminator::Cmce => {
                let m = LcmcMleUnitdataInd{ 
                    sdu,
                    handle: 0, // TODO FIXME
                    received_tetra_address: prim.main_address,
                    endpoint_id: 0, // TODO FIXME
                    link_id: 0, // TODO FIXME,
                    chan_change_resp_req: false, // TODO FIXME
                    chan_change_handle: None, // TODO FIXME
                };
                let msg = SapMsg {
                    sap: Sap::LcmcSap,
                    src: self.self_component,
                    dest: TetraEntity::Cmce,
                    dltime: message.dltime,
                    msg: SapMsgInner::LcmcMleUnitdataInd(m),
                };
                queue.push_back(msg);
            }
            MleProtocolDiscriminator::Sndcp => {
                let m = LtpdMleUnitdataInd{ 
                    sdu,
                    endpoint_id: 0, // TODO FIXME
                    link_id: 0, // TODO FIXME,
                    received_tetra_address: prim.main_address,
                    // received_address_type: 0,
                    chan_change_resp_req: false, // TODO FIXME
                    chan_change_handle: None, // TODO FIXME
                };
                let msg = SapMsg {
                    sap: Sap::TlpdSap,
                    src: self.self_component,
                    dest: TetraEntity::Sndcp,
                    dltime: message.dltime,
                    msg: SapMsgInner::LtpdMleUnitdataInd(m),
                };
                queue.push_back(msg);
            }
            MleProtocolDiscriminator::Mle => {
                self.rx_tla_mle_pdu(queue, message);
            }
            MleProtocolDiscriminator::TetraManagementEntity => {
                unimplemented_log!("MleProtocolDiscriminator::TetraManagementEntity");
            }
        }
    }


    fn rx_tla_unitdata_ind_bl(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {

        // TODO FIXME NOTE: This function is the same as the rx_tla_data_ind_bl. 
        // A cursory glance at the spec does not make clear the difference, except for the relation with
        // either udata or data at the llc. 
        // We should investigate the exact differences and account for them
        
        // Take ownership of bitbuf and read protocol discriminator
        let SapMsgInner::TlaTlUnitdataIndBl(prim) = &mut message.msg else { panic!() };
        let Some(mut sdu) = prim.tl_sdu.take() else { panic!("no tl_sdu") };
        assert!(sdu.get_pos() == 0); // We should be at the start of the MAC PDU
        
        let Some(bits) = sdu.read_bits(3) else {
            tracing::warn!("insufficient bits: {}", sdu.dump_bin());
            return;
        };
        let Ok(pdu_type) = MleProtocolDiscriminator::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, sdu.dump_bin());
            return;
        };

        // Dispatch to appropriate component (or to self if for MLE)
        match pdu_type {
            MleProtocolDiscriminator::Mm => {
                let m = LmmMleUnitdataInd{ 
                    sdu,
                    handle: 0, // TODO FIXME
                    received_address: prim.main_address,
                    // received_address_type: 0, // TODO FIXME
                };
                let msg = SapMsg {
                    sap: Sap::LmmSap,
                    src: self.self_component,
                    dest: TetraEntity::Mm,
                    dltime: message.dltime,
                    msg: SapMsgInner::LmmMleUnitdataInd(m),
                };
                queue.push_back(msg);
            }
            MleProtocolDiscriminator::Cmce => {
                let m = LcmcMleUnitdataInd{ 
                    sdu,
                    handle: 0, // TODO FIXME
                    endpoint_id: 0, // TODO FIXME
                    link_id: 0, // TODO FIXME,
                    received_tetra_address: TetraAddress::default(), // TODO FIXME
                    // received_address_type: 0,
                    chan_change_resp_req: false, // TODO FIXME
                    chan_change_handle: None, // TODO FIXME
                };
                let msg = SapMsg {
                    sap: Sap::LcmcSap,
                    src: self.self_component,
                    dest: TetraEntity::Cmce,
                    dltime: message.dltime,
                    msg: SapMsgInner::LcmcMleUnitdataInd(m),
                };
                queue.push_back(msg);
            }
            MleProtocolDiscriminator::Sndcp => {
                let m = LtpdMleUnitdataInd{ 
                    sdu,
                    endpoint_id: 0, // TODO FIXME
                    link_id: 0, // TODO FIXME,
                    received_tetra_address: prim.main_address,
                    // received_address_type: 0,
                    chan_change_resp_req: false, // TODO FIXME
                    chan_change_handle: None, // TODO FIXME
                };
                let msg = SapMsg {
                    sap: Sap::TlpdSap,
                    src: self.self_component,
                    dest: TetraEntity::Sndcp,
                    dltime: message.dltime,
                    msg: SapMsgInner::LtpdMleUnitdataInd(m),
                };
                queue.push_back(msg);
            }
            MleProtocolDiscriminator::Mle => {
                self.rx_tla_mle_pdu(queue, message);
            }
            MleProtocolDiscriminator::TetraManagementEntity => {
                unimplemented_log!("MleProtocolDiscriminator::TetraManagementEntity");
            }
        }
    }

    fn rx_tlmb_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tlmb_prim: {:?}", message);
        match message.msg {
            SapMsgInner::TlmbSysinfoInd(_) => {
                self.rx_tlmb_tl_sysinfo_ind(queue, message);
            }
            SapMsgInner::TlmbSyncInd(_) => {
                self.rx_tlmb_tl_sync_ind(queue, message);
            }
            _ => { panic!(); }
        }
    }

    pub fn rx_tlmb_tl_sysinfo_ind(&self, _queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tlmb_tl_sysinfo_ind: {:?}", message);
        
        let SapMsgInner::TlmbSysinfoInd(inner) = &mut message.msg else {panic!()};

        // Parse the TL-SDU
        let _pdu = match DMleSysinfo::from_bitbuf(&mut inner.tl_sdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing DMleSysinfo: {:?} {}", e, inner.tl_sdu.dump_bin());
                return;
            }
        };

        unimplemented_log!("rx_tlmb_tl_sysinfo_ind");
        // let need_global_state_update = {
        //     let cfg = self.config.read();

        //     pdu.location_area != cfg.la_info.location_area
        //     || pdu.subscriber_class != cfg.la_info.subscriber_class
        //     || pdu.bs_service_details.registration != cfg.la_info.registration
        //     || pdu.bs_service_details.deregistration != cfg.la_info.deregistration
        //     || pdu.bs_service_details.priority_cell != cfg.la_info.priority_cell
        //     || pdu.bs_service_details.no_minimum_mode != cfg.la_info.no_minimum_mode
        //     || pdu.bs_service_details.migration != cfg.la_info.migration
        //     || pdu.bs_service_details.system_wide_services != cfg.la_info.system_wide_services
        //     || pdu.bs_service_details.voice_service != cfg.la_info.voice_service
        //     || pdu.bs_service_details.circuit_mode_data_service != cfg.la_info.circuit_mode_data_service
        //     || pdu.bs_service_details.sndcp_service != cfg.la_info.sndcp_service
        //     || pdu.bs_service_details.aie_service != cfg.la_info.aie_service
        //     || pdu.bs_service_details.advanced_link != cfg.la_info.advanced_link                
        // };

        // if need_global_state_update {
        //     let mut cfg = self.config.write();
        //     cfg.la_info.location_area = pdu.location_area;
        //     cfg.la_info.subscriber_class = pdu.subscriber_class;
        //     cfg.la_info.registration = pdu.bs_service_details.registration;
        //     cfg.la_info.deregistration = pdu.bs_service_details.deregistration;
        //     cfg.la_info.priority_cell = pdu.bs_service_details.priority_cell;
        //     cfg.la_info.no_minimum_mode = pdu.bs_service_details.no_minimum_mode;
        //     cfg.la_info.migration = pdu.bs_service_details.migration;
        //     cfg.la_info.system_wide_services = pdu.bs_service_details.system_wide_services;
        //     cfg.la_info.voice_service = pdu.bs_service_details.voice_service;
        //     cfg.la_info.circuit_mode_data_service = pdu.bs_service_details.circuit_mode_data_service;
        //     cfg.la_info.sndcp_service = pdu.bs_service_details.sndcp_service;
        //     cfg.la_info.aie_service = pdu.bs_service_details.aie_service;
        //     cfg.la_info.advanced_link = pdu.bs_service_details.advanced_link;
        //     tracing::info!("Updated TetraGlobalState: {:?}", pdu);
        // } else {
        //     tracing::trace!("rx_tlmb_tl_sysinfo_ind: TetraGlobalState update not required");
        // }
    }

    pub fn rx_tlmb_tl_sync_ind(&self, _queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tlmb_tl_sync_ind: {:?}", message);
        
        let SapMsgInner::TlmbSyncInd(inner) = &mut message.msg else {panic!()};

        // Parse the TL-SDU
        let _pdu = match DMleSync::from_bitbuf(&mut inner.tl_sdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing DMleSync: {:?} {}", e, inner.tl_sdu.dump_bin());
                return;
            }
        };

        unimplemented_log!("rx_tlmb_tl_sync_ind");

        // let need_global_state_update = {
        //     let cfg = self.config.read();
        //         pdu.mcc  != cfg.la_info.mcc
        //         || pdu.mnc  != cfg.la_info.mnc
        //         || pdu.neighbor_cell_broadcast != cfg.la_info.neighbor_cell_broadcast
        //         || pdu.cell_load_ca            != cfg.la_info.cell_load_ca
        //         || pdu.late_entry_supported    != cfg.la_info.late_entry_supported
        // };

        // // Update global state if needed
        // if need_global_state_update {
        //     let mut cfg = self.config.write();
        //     cfg.la_info.mcc                    = pdu.mcc;
        //     cfg.la_info.mnc                    = pdu.mnc;
        //     cfg.la_info.neighbor_cell_broadcast = pdu.neighbor_cell_broadcast;
        //     cfg.la_info.cell_load_ca           = pdu.cell_load_ca;
        //     cfg.la_info.late_entry_supported   = pdu.late_entry_supported;
        //     tracing::info!("Updated TetraGlobalState: {:?}", pdu);

        //     // TODO FIXME: This is ugly. We should pass the message through all the intermediate layers
        //     let m = SapMsg {
        //         sap: Sap::TlmcSap,
        //         src: self.self_component,
        //         dest: TetraComponent::Umac,
        //         t_submit: message.t_submit,
        //         msg: SapMsgInner::TlmcConfigureReq(
        //             TlmcConfigureReq{
        //                 valid_addresses: Some(TlmcValidAddress {
        //                     mcc: cfg.la_info.mcc,
        //                     mnc: cfg.la_info.mnc,
        //                 }),
        //                 ..Default::default()                        
        //             }
        //         )
        //     };
        //     queue.push_back(m);
        // } else {
        //     tracing::trace!("rx_tlmb_tl_sysinfo_ind: TetraGlobalState update not required");
        // }
    }

    
    fn rx_tlmc_prim(&mut self, _queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tlmc_prim: {:?}", message);
        unimplemented!("rx_tlmc_prim");
        // match &message.msg {
        //     _ => {
        //         panic!();
        //     }
        // }
    }

    fn rx_lmm_mle_unitdata_req(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_lmm_mle_unitdata_req: {:?}", message);
        let SapMsgInner::LmmMleUnitdataReq(prim) = &mut message.msg else {panic!()};

        let mle_prot_discriminator = MleProtocolDiscriminator::Mm;
        let sdu_len = prim.sdu.get_len();
        let mut pdu = BitBuffer::new(3 + sdu_len);
        pdu.write_bits(mle_prot_discriminator.into_raw(), 3);
        pdu.copy_bits(&mut prim.sdu, sdu_len);
        pdu.seek(0);

        let sapmsg = SapMsg {
            sap: Sap::TlaSap,
            src: self.self_component,
            dest: TetraEntity::Llc,
            dltime: message.dltime,
            msg: SapMsgInner::TlaTlDataReqBl(TlaTlDataReqBl {
                main_address: prim.address,
                link_id: 0,
                endpoint_id: 0, 
                tl_sdu: pdu,
                scrambling_code: self.config.config().scrambling_code(), // TODO cache
                // pdu_prio: todo!(),
                stealing_permission: false,
                subscriber_class: 0, // TODO fixme
                fcs_flag: false,
                air_interface_encryption: None,
                stealing_repeats_flag: None,
                data_class_info: None,
                req_handle: 0, // TODO FIXME
                graceful_degradation: None,
            }),
        };
        queue.push_back(sapmsg);
    }

    fn rx_lmm_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_lmm_prim: {:?}", message);
        match &message.msg {
            SapMsgInner::LmmMleUnitdataReq(_prim) => {
                self.rx_lmm_mle_unitdata_req(queue, message);
            }
            _ => panic!()
        }
    }

    fn rx_tlpd_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tlpd_prim: {:?}", message);
        if matches!(message.msg, SapMsgInner::LtpdMleUnitdataReq(_)) {
            self.rx_ltpd_mle_unitdata_req(queue, message);
        } else {
            unimplemented_log!("rx_tlpd_prim: unsupported {:?}", message.msg);
        }
    }

    fn rx_ltpd_mle_unitdata_req(
        &mut self,
        queue: &mut MessageQueue,
        mut message: SapMsg,
    ) {
        tracing::trace!("rx_ltpd_mle_unitdata_req: {:?}", message);

        let SapMsgInner::LtpdMleUnitdataReq(prim) = &mut message.msg else { panic!() };

        let mle_prot_discriminator = MleProtocolDiscriminator::Sndcp;
        let sdu_len = prim.sdu.get_len();
        let mut pdu = BitBuffer::new(3 + sdu_len);
        pdu.write_bits(mle_prot_discriminator.into_raw(), 3);
        pdu.copy_bits(&mut prim.sdu, sdu_len);
        pdu.seek(0);

        let sapmsg = SapMsg {
            sap: Sap::TlaSap,
            src: self.self_component,
            dest: TetraEntity::Llc,
            dltime: message.dltime,
            msg: SapMsgInner::TlaTlDataReqBl(TlaTlDataReqBl {
                main_address: prim.address,
                link_id: prim.link_id,
                endpoint_id: prim.endpoint_id,
                tl_sdu: pdu,
                scrambling_code: self.config.config().scrambling_code(),
                stealing_permission: prim.stealing_permission,
                subscriber_class: 0,
                fcs_flag: prim.fcs_flag,
                air_interface_encryption: None,
                stealing_repeats_flag: Some(prim.stealing_repeats_flag),
                data_class_info: Some(prim.data_class_info),
                req_handle: prim.handle as i32,
                graceful_degradation: None,
            }),
        };
        queue.push_back(sapmsg);
    }

        fn rx_lcmc_mle_unitdata_req(
        &mut self,
        queue: &mut MessageQueue,
        mut message: SapMsg,
    ) {
        tracing::trace!("rx_lcmc_mle_unitdata_req: {:?}", message);

        let SapMsgInner::LcmcMleUnitdataReq(prim) = &mut message.msg else { panic!() };

        let mle_prot_discriminator = MleProtocolDiscriminator::Cmce;
        let sdu_len = prim.sdu.get_len();
        let mut pdu = BitBuffer::new(3 + sdu_len);
        pdu.write_bits(mle_prot_discriminator.into_raw(), 3);
        pdu.copy_bits(&mut prim.sdu, sdu_len);
        pdu.seek(0);

        let sapmsg = SapMsg {
            sap: Sap::TlaSap,
            src: self.self_component,
            dest: TetraEntity::Llc,
            dltime: message.dltime,
            msg: SapMsgInner::TlaTlDataReqBl(TlaTlDataReqBl {
                main_address: prim.address,
                link_id: prim.link_id,
                endpoint_id: prim.endpoint_id,
                tl_sdu: pdu,
                scrambling_code: self.config.config().scrambling_code(),
                stealing_permission: prim.stealing_permission,
                subscriber_class: 0,
                fcs_flag: false,
                air_interface_encryption: None,
                stealing_repeats_flag: None,
                data_class_info: None,
                req_handle: prim.handle as i32,
                graceful_degradation: None,
            }),
        };
        queue.push_back(sapmsg);
    }

fn rx_lcmc_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
    tracing::trace!("rx_lcmc_prim: {:?}", message);
    match message.msg {
        SapMsgInner::LcmcMleUnitdataReq(_) => self.rx_lcmc_mle_unitdata_req(queue, message),
        _ => panic!(),
    }
}



}

impl TetraEntityTrait for Mle {
    
    fn entity(&self) -> TetraEntity {
        TetraEntity::Mle
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        
        tracing::debug!("rx_prim: {:?}", message);
        match message.sap {
            Sap::TlaSap => {
                self.rx_tla_prim(queue, message);
            }
            Sap::TlmbSap => {
                self.rx_tlmb_prim(queue, message);
            }
            Sap::TlmcSap => {
                self.rx_tlmc_prim(queue, message);
            }
            Sap::LmmSap => {
                self.rx_lmm_prim(queue, message);
            }
            Sap::TlpdSap => {
                self.rx_tlpd_prim(queue, message);
            }
            Sap::LcmcSap => {
                self.rx_lcmc_prim(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }
}
