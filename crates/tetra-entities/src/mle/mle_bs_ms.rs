use crate::mle::components::mle_router::MleRouter;
use crate::{MessageQueue, TetraEntityTrait};
use time::{OffsetDateTime, UtcOffset};
use tetra_config::{SharedConfig, StackMode};
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, Sap, SsiType, TdmaTime, TetraAddress, unimplemented_log};
use tetra_saps::lcmc::LcmcMleUnitdataInd;
use tetra_saps::lmm::LmmMleUnitdataInd;
use tetra_saps::ltpd::LtpdMleUnitdataInd;
use tetra_saps::tla::{TlaTlDataReqBl, TlaTlUnitdataReqBl};
use tetra_saps::{SapMsg, SapMsgInner};

use tetra_pdus::mle::enums::mle_pdu_type_dl::MlePduTypeDl;
use tetra_pdus::mle::enums::mle_protocol_discriminator::MleProtocolDiscriminator;
use tetra_pdus::mle::pdus::d_mle_sync::DMleSync;
use tetra_pdus::mle::pdus::d_mle_sysinfo::DMleSysinfo;
use tetra_pdus::mle::pdus::d_nwrk_broadcast::DNwrkBroadcast;

pub struct Mle {
    // config: Option<SharedConfig>,
    self_component: TetraEntity,
    config: SharedConfig,

    router: MleRouter,
}

impl Mle {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            self_component: TetraEntity::Mle,
            config,

            router: MleRouter::new(),
        }
    }

    fn rx_tla_mle_pdu(&mut self, _queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tla_mle_pdu");

        // Extract tm_sdu from whatever primitive we have
        let tm_sdu = {
            match message.msg {
                SapMsgInner::TlaTlDataIndBl(prim) => prim.tl_sdu,
                _ => {
                    panic!();
                }
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
            MlePduTypeDl::DNewCell => {
                unimplemented_log!("DNewCell")
            }
            MlePduTypeDl::DPrepareFail => {
                unimplemented_log!("DPrepareFail")
            }
            MlePduTypeDl::DNwrkBroadcast => {
                unimplemented_log!("DNwrkBroadcast")
            }
            MlePduTypeDl::DNwrkBroadcastExt => {
                unimplemented_log!("DNwrkBroadcastExt")
            } // TODO FIXME CHECK this option and assocaited int
            MlePduTypeDl::DRestoreAck => {
                unimplemented_log!("DRestoreAck")
            }
            MlePduTypeDl::DRestoreFail => {
                unimplemented_log!("DRestoreFail")
            }
            MlePduTypeDl::DChannelResponse => {
                unimplemented_log!("DChannelResponse")
            }
            MlePduTypeDl::ExtPdu => {
                unimplemented_log!("ExtPdu")
            }
        }
    }

    fn rx_tla_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tla_prim");
        match message.msg {
            SapMsgInner::TlaTlDataIndBl(_) => {
                self.rx_tla_data_ind_bl(queue, message);
            }
            SapMsgInner::TlaTlUnitdataIndBl(_) => {
                self.rx_tla_unitdata_ind_bl(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }

    fn rx_tla_data_ind_bl(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        // Take ownership of bitbuf and read protocol discriminator
        let SapMsgInner::TlaTlDataIndBl(prim) = &mut message.msg else {
            panic!()
        };
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
                let handle = self
                    .router
                    .create_handle(prim.main_address, prim.link_id, prim.endpoint_id, message.dltime);
                let m = LmmMleUnitdataInd {
                    sdu,
                    handle,
                    received_address: prim.main_address,
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
                let handle = self
                    .router
                    .create_handle(prim.main_address, prim.link_id, prim.endpoint_id, message.dltime);
                let m = LcmcMleUnitdataInd {
                    sdu,
                    handle,
                    received_tetra_address: prim.main_address,
                    endpoint_id: prim.endpoint_id,
                    link_id: prim.link_id,
                    chan_change_resp_req: false, // TODO FIXME
                    chan_change_handle: None,    // TODO FIXME
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
                let m = LtpdMleUnitdataInd {
                    sdu,
                    endpoint_id: prim.endpoint_id,
                    link_id: prim.link_id,
                    received_tetra_address: prim.main_address,
                    chan_change_resp_req: false, // TODO FIXME
                    chan_change_handle: None,    // TODO FIXME
                };
                let msg = SapMsg {
                    sap: Sap::LcmcSap,
                    src: self.self_component,
                    dest: TetraEntity::Cmce,
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
        // It seems only the SNDCP uses unacknowledged TL-UNITDATA.
        // We should investigate the exact differences and account for them

        // Take ownership of bitbuf and read protocol discriminator
        let SapMsgInner::TlaTlUnitdataIndBl(prim) = &mut message.msg else {
            panic!()
        };
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
                tracing::warn!("TM-UNITDATA for MM?"); // todo fixme find if ever used
                let handle = self
                    .router
                    .create_handle(prim.main_address, prim.link_id, prim.endpoint_id, message.dltime);
                let m = LmmMleUnitdataInd {
                    sdu,
                    handle,
                    received_address: prim.main_address,
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
                tracing::warn!("TM-UNITDATA for MM?"); // todo fixme find if ever used
                let handle = self
                    .router
                    .create_handle(prim.main_address, prim.link_id, prim.endpoint_id, message.dltime);
                let m = LcmcMleUnitdataInd {
                    sdu,
                    handle,
                    endpoint_id: prim.endpoint_id,
                    link_id: prim.link_id,
                    received_tetra_address: prim.main_address,
                    chan_change_resp_req: false, // TODO FIXME
                    chan_change_handle: None,    // TODO FIXME
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
                let m = LtpdMleUnitdataInd {
                    sdu,
                    endpoint_id: prim.endpoint_id,
                    link_id: prim.link_id,
                    received_tetra_address: prim.main_address,
                    chan_change_resp_req: false, // TODO FIXME
                    chan_change_handle: None,    // TODO FIXME
                };
                let msg = SapMsg {
                    sap: Sap::LcmcSap,
                    src: self.self_component,
                    dest: TetraEntity::Cmce,
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
        tracing::trace!("rx_tlmb_prim");
        match message.msg {
            SapMsgInner::TlmbSysinfoInd(_) => {
                self.rx_tlmb_tl_sysinfo_ind(queue, message);
            }
            SapMsgInner::TlmbSyncInd(_) => {
                self.rx_tlmb_tl_sync_ind(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }

    pub fn rx_tlmb_tl_sysinfo_ind(&self, _queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tlmb_tl_sysinfo_ind");

        let SapMsgInner::TlmbSysinfoInd(inner) = &mut message.msg else {
            panic!()
        };

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
        tracing::trace!("rx_tlmb_tl_sync_ind");

        let SapMsgInner::TlmbSyncInd(inner) = &mut message.msg else {
            panic!()
        };

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

    fn rx_tlmc_prim(&mut self, _queue: &mut MessageQueue, _message: SapMsg) {
        tracing::trace!("rx_tlmc_prim");
        unimplemented!("rx_tlmc_prim");
        // match &message.msg {
        //     _ => {
        //         panic!();
        //     }
        // }
    }

    fn rx_lmm_mle_unitdata_req(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_lmm_mle_unitdata_req");
        let SapMsgInner::LmmMleUnitdataReq(prim) = &mut message.msg else {
            panic!()
        };

        let mle_prot_discriminator = MleProtocolDiscriminator::Mm;
        let sdu_len = prim.sdu.get_len();
        let mut pdu = BitBuffer::new(3 + sdu_len);
        pdu.write_bits(mle_prot_discriminator.into_raw(), 3);
        pdu.copy_bits(&mut prim.sdu, sdu_len);
        pdu.seek(0);

        let (addr, link, endpoint) = self.router.use_handle(prim.handle, message.dltime);
        assert_eq!(addr.ssi, prim.address.ssi);
        let sapmsg = SapMsg {
            sap: Sap::TlaSap,
            src: self.self_component,
            dest: TetraEntity::Llc,
            dltime: message.dltime,
            msg: SapMsgInner::TlaTlDataReqBl(TlaTlDataReqBl {
                main_address: prim.address,
                link_id: link,
                endpoint_id: endpoint,
                tl_sdu: pdu,
                stealing_permission: false,
                subscriber_class: 0, // TODO fixme
                fcs_flag: false,
                air_interface_encryption: None,
                stealing_repeats_flag: None,
                data_class_info: None,
                req_handle: 0, // TODO FIXME; should we pass the same handle here?
                graceful_degradation: None,
                chan_alloc: None,
                // redundant_transmission: 1,
            }),
        };
        queue.push_back(sapmsg);
    }

    fn rx_lmm_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_lmm_prim");
        match &message.msg {
            SapMsgInner::LmmMleUnitdataReq(_prim) => {
                self.rx_lmm_mle_unitdata_req(queue, message);
            }
            _ => panic!(),
        }
    }

    fn rx_tlpd_prim(&mut self, _queue: &mut MessageQueue, _message: SapMsg) {
        tracing::trace!("rx_tlpd_prim");
        unimplemented!("rx_tlpd_prim");
        // match &message.msg {
        //     _ => {
        //         panic!();
        //     }
        // }
    }

    fn rx_lcmc_mle_unitdata_req(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_lcmc_mle_unitdata_req");
        let SapMsgInner::LcmcMleUnitdataReq(prim) = &mut message.msg else {
            panic!()
        };

        let mle_prot_discriminator = MleProtocolDiscriminator::Cmce;
        let sdu_len = prim.sdu.get_len();
        let mut pdu = BitBuffer::new(3 + sdu_len);
        pdu.write_bits(mle_prot_discriminator.into_raw(), 3);
        pdu.copy_bits(&mut prim.sdu, sdu_len);
        pdu.seek(0);

        let (_addr, link, endpoint) = self.router.use_handle(prim.handle, message.dltime);
        assert_eq!(link, prim.link_id);
        assert_eq!(endpoint, prim.endpoint_id);
        // Take Channel Allocation Request if any
        let chan_alloc = prim.chan_alloc.take();

        let sapmsg = SapMsg {
            sap: Sap::TlaSap,
            src: self.self_component,
            dest: TetraEntity::Llc,
            dltime: message.dltime,
            msg: SapMsgInner::TlaTlDataReqBl(TlaTlDataReqBl {
                main_address: prim.main_address,
                link_id: prim.link_id,
                endpoint_id: prim.endpoint_id,
                tl_sdu: pdu,
                stealing_permission: prim.stealing_permission,
                subscriber_class: 0, // TODO fixme
                fcs_flag: false,
                air_interface_encryption: None,
                stealing_repeats_flag: None,
                data_class_info: None,
                req_handle: 0, // TODO FIXME
                graceful_degradation: None,
                chan_alloc,
                // redundant_transmission: prim.redundant_transmission
            }),
        };
        queue.push_back(sapmsg);
    }

    fn rx_lcmc_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_lcmc_prim");
        match &message.msg {
            SapMsgInner::LcmcMleUnitdataReq(_) => {
                self.rx_lcmc_mle_unitdata_req(queue, message);
            }
            _ => panic!(),
        }
    }

    /// Encodes TETRA Network Time IE (Table 18.100) into the low 48 bits of a u64:
    /// [47..24] UTC time (24) in 2-second units since Jan 1 00:00 UTC
    /// [23]     Local time offset sign (1): 0=+, 1=-
    /// [22..17] Local time offset (6) in 15-minute units (0..=0x38 valid; 0x3F invalid)
    /// [16..11] Year since 2000 (6) (0..=0x3E valid; 0x3F invalid)
    /// [10..0]  Reserved (11) = all ones (0x7FF)
    fn encode_tetra_network_time(now: OffsetDateTime) -> u64 {
        // UTC time field (24 bits): 2-second ticks since Jan 1 00:00 UTC
        let utc = now.to_offset(UtcOffset::UTC);
        let doy = utc.ordinal() as u64; // 1..=366
        let secs_of_day = (utc.hour() as u64) * 3600
            + (utc.minute() as u64) * 60
            + (utc.second() as u64);

        let secs_since_year_start = (doy - 1) * 86_400 + secs_of_day;
        let mut utc_units = secs_since_year_start / 2; // 2-second steps

        // Enforce 24-bit + reserved-range rules (NOTE 1)
        // Reserved: 0xF142FF..=0xFFFFFE, Invalid: 0xFFFFFF
        if utc_units > 0x00FF_FFFF || (utc_units >= 0x00F1_42FF && utc_units <= 0x00FF_FFFE) {
            utc_units = 0x00FF_FFFF;
        }

        // Local time offset sign + magnitude (NOTE 2,3)
        // Use the offset embedded in `now` (includes DST if caller provided local time properly)
        let offset_secs = now.offset().whole_seconds();
        let sign: u64 = if offset_secs < 0 { 1 } else { 0 };

        // Step size is 15 minutes = 900 seconds, max ±14h => 56 steps.
        // If it's not a multiple of 15 minutes or out of range -> invalid (0x3F).
        let mut offset_units_15m: u64 = {
            let abs = offset_secs.unsigned_abs() as u64;
            if abs % 900 != 0 {
                0x3F
            } else {
                let steps = abs / 900;
                if steps > 56 { 0x3F } else { steps }
            }
        };

        // Avoid reserved encodings 0x39..0x3E (shouldn’t happen if <=56 anyway)
        if (0x39..=0x3E).contains(&offset_units_15m) {
            offset_units_15m = 0x3F;
        }

        // Year since 2000 (NOTE 4)
        let year_since_2000_i32 = now.year() - 2000;
        let year_field: u64 = if (0..=62).contains(&year_since_2000_i32) {
            year_since_2000_i32 as u64
        } else {
            0x3F // invalid year
        };

        // Reserved (NOTE 5): all ones
        let reserved_11: u64 = 0x7FF;

        // Pack into 48 bits:
        // UTC(24) | sign(1) | offset(6) | year(6) | reserved(11)
        ((utc_units & 0x00FF_FFFF) << 24)
            | ((sign & 0x1) << 23)
            | ((offset_units_15m & 0x3F) << 17)
            | ((year_field & 0x3F) << 11)
            | reserved_11
    }

    fn send_network_time_broadcast(&self, queue: &mut MessageQueue, ts: TdmaTime) {
        if self.config.config().stack_mode != StackMode::Bs {
            return;
        }

        // Broadcast once per multiframe on MCCH (avoid control frame 18).
        if ts.f != 1 || ts.t != 1 {
            return;
        }

        let utc_now = OffsetDateTime::now_utc();

        // Derive local offset at "now" and convert (includes DST if OS reports it)
        let local_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
        let local_now = utc_now.to_offset(local_offset);

        let tetra_network_time = Self::encode_tetra_network_time(local_now);

        let cell_load_ca = self.config.state_read().cell_load_ca & 0x3;
        let pdu = DNwrkBroadcast {
            cell_re_select_parameters: 0,
            cell_load_ca,
            tetra_network_time: Some(tetra_network_time),
            // Explicitly signal "no neighbour cell information" (P-bit set, value 0).
            number_of_ca_neighbour_cells: Some(0),
            neighbour_cell_information_for_ca: None,
        };

        tracing::debug!("DNwrkBroadcast {:?}", &pdu);

        let mut sdu = BitBuffer::new_autoexpand(96);
        sdu.write_bits(MleProtocolDiscriminator::Mle.into_raw(), 3);
        if let Err(err) = pdu.to_bitbuf(&mut sdu) {
            tracing::warn!("Failed building D-NWRK-BROADCAST: {:?}", err);
            return;
        }
        sdu.seek(0);

        // Broadcast on MCCH using the "all ones" address (cell-wide broadcast).
        let main_address = TetraAddress {
            ssi: 0x00FF_FFFF,
            ssi_type: SsiType::Gssi,
            encrypted: false,
        };

        let msg = SapMsg {
            sap: Sap::TlaSap,
            src: self.self_component,
            dest: TetraEntity::Llc,
            dltime: ts,
            msg: SapMsgInner::TlaTlUnitdataReqBl(TlaTlUnitdataReqBl {
                main_address,
                link_id: 0,
                endpoint_id: 0,
                tl_sdu: sdu,
                scrambling_code: 0,
                pdu_prio: 0,
                stealing_permission: false,
                subscriber_class: 0,
                fcs_flag: false,
                air_interface_encryption: 0,
                data_prio: 0,
                packet_data_flag: false,
                n_tlsdu_repeats: None,
                scheduled_data_status: 0,
                max_schedule_interval: None,
                data_class_info: None,
                req_handle: 0,
            }),
        };

        queue.push_back(msg);
    }
}

impl TetraEntityTrait for Mle {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Mle
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);
        // tracing::debug!(ts=%message.dltime, "rx_prim: {:?}", message);

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

    fn tick_start(&mut self, queue: &mut MessageQueue, ts: TdmaTime) {
        self.send_network_time_broadcast(queue, ts);
    }
}
