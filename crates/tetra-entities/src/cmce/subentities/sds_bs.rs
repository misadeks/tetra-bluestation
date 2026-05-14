use crate::MessageQueue;
use crate::net_brew as brew;
use crate::net_control::ControlCommand;
use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, Layer2Service, Sap, SsiType, TdmaTime, TetraAddress, unimplemented_log};
use tetra_pdus::cmce::enums::party_type_identifier::PartyTypeIdentifier;
use tetra_pdus::cmce::enums::pre_coded_status::PreCodedStatus;
use tetra_pdus::cmce::enums::short_report_type::ShortReportType;
use tetra_pdus::cmce::pdus::d_sds_data::DSdsData;
use tetra_pdus::cmce::pdus::d_status::DStatus;
use tetra_pdus::cmce::pdus::u_sds_data::USdsData;
use tetra_pdus::cmce::pdus::u_status::UStatus;
use tetra_saps::control::enums::sds_user_data::SdsUserData;
use tetra_saps::control::sds::CmceSdsData;
use tetra_saps::lcmc::LcmcMleUnitdataReq;
use tetra_saps::{SapMsg, SapMsgInner};

/// Clause 13 Short Data Service CMCE sub-entity.
pub struct SdsBsSubentity {
    config: SharedConfig,
    dltime: TdmaTime,
}

impl SdsBsSubentity {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            config,
            dltime: TdmaTime::default(),
        }
    }

    pub fn tick_start(&mut self, ts: TdmaTime) {
        self.dltime = ts;
    }

    pub fn route_rf_deliver(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("SDS route_rf_deliver");
        let dltime = self.dltime.forward_to_timeslot(1);

        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            panic!();
        };
        let calling_party = prim.received_tetra_address;

        let pdu = match USdsData::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing U-SDS-DATA: {:?} {}", e, prim.sdu.dump_bin());
                return;
            }
        };

        if !Self::feature_check_u_sds_data(&pdu) {
            tracing::warn!("Unsupported features in U-SDS-DATA, dropping");
            return;
        }

        // Extract destination SSI (guaranteed present after feature check)
        let dest_ssi = pdu.called_party_ssi.unwrap() as u32;
        let source_ssi = calling_party.ssi;

        tracing::info!(
            "SDS: U-SDS-DATA from ISSI {} to ISSI {}, type={}",
            source_ssi,
            dest_ssi,
            pdu.user_defined_data.type_identifier()
        );

        // Route: local delivery (ISSI or GSSI), Brew forward, or drop
        let is_local_issi = self.config.state_read().subscribers.is_registered(dest_ssi);
        let is_local_group = !is_local_issi && self.config.state_read().subscribers.has_group_members(dest_ssi);

        if is_local_issi {
            tracing::info!("SDS: local delivery: {} -> {}", source_ssi, dest_ssi);
            self.send_d_sds_data(queue, dltime, source_ssi, dest_ssi, SsiType::Issi, pdu.user_defined_data);
        } else if is_local_group {
            tracing::info!("SDS: group delivery: {} -> GSSI {}", source_ssi, dest_ssi);
            self.send_d_sds_data(queue, dltime, source_ssi, dest_ssi, SsiType::Gssi, pdu.user_defined_data);
        } else {
            tracing::warn!("SDS: dest SSI {} not local, dropping", dest_ssi);
        }
    }

    /// Handle incoming SDS data from Brew entity (network-originated SDS)
    pub fn rx_sds_from_brew(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        let dltime = self.dltime.forward_to_timeslot(1);
        let SapMsgInner::CmceSdsData(sds) = message.msg else {
            panic!("Expected CmceSdsData message");
        };

        if !self.config.state_read().subscribers.is_registered(sds.dest_issi) {
            tracing::warn!("SDS: dest ISSI {} from Brew is not locally registered, dropping", sds.dest_issi);
            return;
        }

        self.send_d_sds_data(queue, dltime, sds.source_issi, sds.dest_issi, SsiType::Issi, sds.user_defined_data);
    }

    pub fn rx_sds_from_control(&mut self, queue: &mut MessageQueue, message: ControlCommand) -> bool {
        let ControlCommand::SendSds {
            source_ssi,
            dest_ssi,
            dest_is_group,
            len_bits,
            payload,
            ..
        } = message
        else {
            panic!("Expected SendSds command");
        };

        if !dest_is_group && !self.config.state_read().subscribers.is_registered(dest_ssi) {
            tracing::warn!("SDS: dest ISSI {} from Control is not locally registered, dropping", dest_ssi);
            return false;
        }

        let dest_type = if dest_is_group { SsiType::Gssi } else { SsiType::Issi };
        self.send_d_sds_data(
            queue,
            self.dltime.forward_to_timeslot(1),
            source_ssi,
            dest_ssi,
            dest_type,
            SdsUserData::Type4(len_bits, payload),
        );
        true
    }

    pub fn route_status_deliver(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("SDS route_status_deliver");
        let dltime = self.dltime.forward_to_timeslot(1);

        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            panic!();
        };
        let calling_party = prim.received_tetra_address;

        let pdu = match UStatus::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing U-STATUS: {:?} {}", e, prim.sdu.dump_bin());
                return;
            }
        };

        if !Self::feature_check_u_status(&pdu) {
            tracing::warn!("Unsupported features in U-STATUS, dropping");
            return;
        }

        let dest_ssi = pdu.called_party_ssi.unwrap() as u32;
        let source_ssi = calling_party.ssi;

        if self.config.state_read().subscribers.is_registered(dest_ssi) {
            self.send_d_status(queue, dltime, source_ssi, dest_ssi, pdu.pre_coded_status);
        } else if brew::is_active(&self.config) {
            let user_defined_data = if let PreCodedStatus::SdsTl(report) = &pdu.pre_coded_status {
                let delivery_status = match report.short_report_type() {
                    ShortReportType::MessageReceived | ShortReportType::MessageConsumed => 0x00,
                    ShortReportType::ProtOrEncodingNotSupported => 0x01,
                    ShortReportType::DestMemFull => 0x02,
                };
                SdsUserData::Type4(32, vec![0x82, 0x10, delivery_status, report.message_reference()])
            } else {
                SdsUserData::Type1(pdu.pre_coded_status.into_raw())
            };

            queue.push_back(SapMsg {
                sap: Sap::Control,
                src: TetraEntity::Cmce,
                dest: TetraEntity::Brew,
                msg: SapMsgInner::CmceSdsData(CmceSdsData {
                    source_issi: source_ssi,
                    dest_issi: dest_ssi,
                    user_defined_data,
                }),
            });
        } else {
            tracing::warn!("SDS-STATUS: dest ISSI {} not local and Brew is inactive, dropping", dest_ssi);
        }
    }

    fn send_d_status(
        &self,
        queue: &mut MessageQueue,
        _dltime: TdmaTime,
        source_issi: u32,
        dest_issi: u32,
        pre_coded_status: PreCodedStatus,
    ) {
        let pdu = DStatus {
            calling_party_type_identifier: PartyTypeIdentifier::Ssi,
            calling_party_address_ssi: Some(source_issi as u64),
            calling_party_extension: None,
            pre_coded_status,
            external_subscriber_number: None,
            dm_ms_address: None,
        };

        let mut sdu = BitBuffer::new_autoexpand(64);
        if let Err(e) = pdu.to_bitbuf(&mut sdu) {
            tracing::error!("Failed to serialize D-STATUS: {:?}", e);
            return;
        }
        sdu.seek(0);

        queue.push_back(SapMsg {
            sap: Sap::LcmcSap,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LcmcMleUnitdataReq(LcmcMleUnitdataReq {
                sdu,
                handle: 0,
                endpoint_id: 0,
                link_id: 0,
                layer2service: Layer2Service::Todo,
                pdu_prio: 0,
                layer2_qos: 0,
                stealing_permission: false,
                stealing_repeats_flag: false,
                chan_alloc: None,
                main_address: TetraAddress::new(dest_issi, SsiType::Issi),
                tx_reporter: None,
            }),
        });
    }

    fn send_d_sds_data(
        &self,
        queue: &mut MessageQueue,
        _dltime: TdmaTime,
        source_issi: u32,
        dest_ssi: u32,
        dest_ssi_type: SsiType,
        user_defined_data: SdsUserData,
    ) {
        let pdu = DSdsData {
            calling_party_type_identifier: PartyTypeIdentifier::Ssi,
            calling_party_address_ssi: Some(source_issi as u64),
            calling_party_extension: None,
            user_defined_data,
            external_subscriber_number: None,
            dm_ms_address: None,
        };

        let mut sdu = BitBuffer::new_autoexpand(128);
        if let Err(e) = pdu.to_bitbuf(&mut sdu) {
            tracing::error!("Failed to serialize D-SDS-DATA: {:?}", e);
            return;
        }
        sdu.seek(0);

        let layer2service = match dest_ssi_type {
            SsiType::Issi => Layer2Service::Acknowledged,
            SsiType::Gssi => Layer2Service::Unacknowledged,
            _ => Layer2Service::Todo,
        };
        queue.push_back(SapMsg {
            sap: Sap::LcmcSap,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LcmcMleUnitdataReq(LcmcMleUnitdataReq {
                sdu,
                handle: 0,
                endpoint_id: 0,
                link_id: 0,
                layer2service,
                pdu_prio: 0,
                layer2_qos: 0,
                stealing_permission: false,
                stealing_repeats_flag: false,
                chan_alloc: None,
                main_address: TetraAddress::new(dest_ssi, dest_ssi_type),
                tx_reporter: None,
            }),
        });
    }

    fn feature_check_u_sds_data(pdu: &USdsData) -> bool {
        Self::has_supported_called_party(
            pdu.called_party_type_identifier,
            pdu.called_party_ssi,
            pdu.called_party_short_number_address,
            pdu.called_party_extension,
            pdu.external_subscriber_number.as_ref(),
            pdu.dm_ms_address.as_ref(),
            "SDS",
        )
    }

    fn feature_check_u_status(pdu: &UStatus) -> bool {
        Self::has_supported_called_party(
            pdu.called_party_type_identifier,
            pdu.called_party_ssi,
            pdu.called_party_short_number_address,
            pdu.called_party_extension,
            pdu.external_subscriber_number.as_ref(),
            pdu.dm_ms_address.as_ref(),
            "SDS-STATUS",
        )
    }

    fn has_supported_called_party<T, U>(
        called_party_type_identifier: PartyTypeIdentifier,
        called_party_ssi: Option<u64>,
        called_party_short_number_address: Option<u64>,
        called_party_extension: Option<u64>,
        external_subscriber_number: Option<&T>,
        dm_ms_address: Option<&U>,
        label: &str,
    ) -> bool {
        let mut supported = true;
        if called_party_ssi.is_none() {
            if called_party_short_number_address.is_some() {
                unimplemented_log!("{}: short number addressing not supported", label);
            } else {
                tracing::warn!("{}: no destination address", label);
            }
            supported = false;
        }
        if called_party_type_identifier != PartyTypeIdentifier::Ssi {
            unimplemented_log!("{}: called party type {:?} not supported", label, called_party_type_identifier);
            supported = false;
        }
        if called_party_extension.is_some() {
            unimplemented_log!("{}: TSI extension addressing not supported", label);
        }
        if external_subscriber_number.is_some() {
            unimplemented_log!("{}: external_subscriber_number not supported", label);
        }
        if dm_ms_address.is_some() {
            unimplemented_log!("{}: dm_ms_address not supported", label);
        }
        supported
    }
}
