use tetra_config::bluestation::SharedConfig;
use tetra_core::{BitBuffer, Layer2Service, Sap, SsiType, TetraAddress, tetra_entities::TetraEntity};
use tetra_pdus::cmce::{
    enums::cmce_pdu_type_dl::CmcePduTypeDl,
    enums::party_type_identifier::PartyTypeIdentifier,
    enums::pre_coded_status::PreCodedStatus,
    pdus::{d_sds_data::DSdsData, d_status::DStatus, u_sds_data::USdsData, u_status::UStatus},
};
use tetra_saps::control::enums::sds_user_data::SdsUserData;
use tetra_saps::lcmc::LcmcMleUnitdataReq;
use tetra_saps::tnsds::{TnsdsStatusIndication, TnsdsUnitdataIndication};
use tetra_saps::{SapMsg, SapMsgInner};

use crate::MessageQueue;
use crate::net_telemetry::{TelemetryEvent, channel::TelemetrySink};

/// Clause 13 Short Data Service CMCE sub-entity (MS side).
///
/// Receive: decodes D-SDS-DATA (cl. 14.7.1.10) / D-STATUS (cl. 14.7.1.11) and
/// surfaces them to the user application as TNSDS-UNITDATA / TNSDS-STATUS
/// indications (cl. 13.3.2) over the telemetry SAP.
///
/// Transmit: builds U-SDS-DATA (cl. 14.7.2.8) / U-STATUS (cl. 14.7.2.7) from
/// TNSDS-UNITDATA / TNSDS-STATUS requests and sends them uplink via the
/// LCMC-SAP MLE-UNITDATA request (mirrors the BS `sds_bs.rs`). The calling
/// party (own ISSI) is carried by the MAC source address, so it is not encoded
/// in the uplink PDU. SDS-TL (cl. 29) end-to-end delivery reporting is deferred.
pub struct SdsMsSubentity {
    #[allow(dead_code)]
    config: SharedConfig,
    telemetry: Option<TelemetrySink>,
}

impl SdsMsSubentity {
    /// Create a new instance of the SDS sub-entity.
    pub fn new(config: SharedConfig, telemetry: Option<TelemetrySink>) -> Self {
        SdsMsSubentity { config, telemetry }
    }

    fn emit(&self, event: TelemetryEvent) {
        if let Some(sink) = &self.telemetry {
            sink.send(event);
        }
    }

    pub fn rx_sds_data(&mut self, _queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_sds_data");

        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            panic!();
        };
        // The address the downlink was delivered to (our ISSI, or a GSSI for a
        // group SDS): distinguishes an individual from a group message per the
        // TNSDS-UNITDATA "called party address type" (cl. 13.3.3).
        let called_is_group = prim.received_tetra_address.ssi_type == SsiType::Gssi;

        let pdu = match DSdsData::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("Received DSdsData: {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing DSdsData: {:?} {}", e, prim.sdu.dump_bin());
                return;
            }
        };

        // ETSI TS 100 392-2 cl. 14.7.1.10 delivers CMCE SDS user data to the MS.
        // Surface it to the user application as a TNSDS-UNITDATA indication
        // (Table 13.3, cl. 13.3.2.3). SDS-TL interpretation of Type-4 user data
        // (cl. 29) remains above this CMCE receive decode and is out of scope.
        let calling_party_ssi = pdu.calling_party_address_ssi.unwrap_or(0) as u32;
        tracing::info!(
            calling_party = calling_party_ssi,
            group = called_is_group,
            data = ?pdu.user_defined_data,
            "CMCE-MS: received D-SDS-DATA"
        );
        self.emit(TelemetryEvent::TnsdsUnitdataIndication(TnsdsUnitdataIndication {
            calling_party_ssi,
            called_party_is_group: called_is_group,
            user_data: pdu.user_defined_data,
        }));
    }

    pub fn rx_status(&mut self, _queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_status");

        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            panic!();
        };
        let pdu = match DStatus::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => pdu,
            Err(e) => {
                tracing::warn!("Failed parsing DStatus: {:?} {}", e, prim.sdu.dump_bin());
                return;
            }
        };

        // ETSI TS 100 392-2 cl. 14.7.1.11 / cl. 14.8.34. Surface to the user
        // application as a TNSDS-STATUS indication (Table 13.1, cl. 13.3.2.1).
        let calling_party_ssi = pdu.calling_party_address_ssi.unwrap_or(0) as u32;
        let status_number = pdu.pre_coded_status.into_raw();
        tracing::info!(
            calling_party = calling_party_ssi,
            status = %pdu.pre_coded_status,
            "CMCE-MS: received D-STATUS"
        );
        self.emit(TelemetryEvent::TnsdsStatusIndication(TnsdsStatusIndication {
            calling_party_ssi,
            status_number,
        }));
    }

    /// Send a U-SDS-DATA PDU uplink (cl. 14.7.2.8) — TNSDS-UNITDATA request
    /// (Table 13.3, cl. 13.3.2.3). Mirrors `sds_bs::send_d_sds_data`.
    pub fn send_u_sds_data(&mut self, queue: &mut MessageQueue, dest_ssi: u32, dest_is_group: bool, user_defined_data: SdsUserData) {
        let pdu = USdsData {
            area_selection: 0,
            called_party_type_identifier: PartyTypeIdentifier::Ssi,
            called_party_short_number_address: None,
            called_party_ssi: Some(dest_ssi as u64),
            called_party_extension: None,
            user_defined_data,
            external_subscriber_number: None,
            dm_ms_address: None,
        };

        tracing::info!("CMCE-MS: -> U-SDS-DATA to {} {}", if dest_is_group { "GSSI" } else { "ISSI" }, dest_ssi);

        let mut sdu = BitBuffer::new_autoexpand(128);
        if let Err(e) = pdu.to_bitbuf(&mut sdu) {
            tracing::error!("Failed to serialize U-SDS-DATA: {:?}", e);
            return;
        }
        sdu.seek(0);

        self.push_uplink(queue, sdu, dest_ssi, dest_is_group);
    }

    /// Send a U-STATUS PDU uplink (cl. 14.7.2.7) — TNSDS-STATUS request
    /// (Table 13.1, cl. 13.3.2.1). Mirrors `sds_bs::send_d_status`.
    pub fn send_u_status(&mut self, queue: &mut MessageQueue, dest_ssi: u32, dest_is_group: bool, status_number: u16) {
        let pdu = UStatus {
            area_selection: 0,
            called_party_type_identifier: PartyTypeIdentifier::Ssi,
            called_party_short_number_address: None,
            called_party_ssi: Some(dest_ssi as u64),
            called_party_extension: None,
            pre_coded_status: PreCodedStatus::from(status_number),
            external_subscriber_number: None,
            dm_ms_address: None,
        };

        tracing::info!(
            "CMCE-MS: -> U-STATUS to {} {} status={}",
            if dest_is_group { "GSSI" } else { "ISSI" },
            dest_ssi,
            status_number
        );

        let mut sdu = BitBuffer::new_autoexpand(64);
        if let Err(e) = pdu.to_bitbuf(&mut sdu) {
            tracing::error!("Failed to serialize U-STATUS: {:?}", e);
            return;
        }
        sdu.seek(0);

        self.push_uplink(queue, sdu, dest_ssi, dest_is_group);
    }

    /// Push a serialized CMCE uplink SDU to MLE over the LCMC-SAP. Layer 2
    /// service follows the addressing (cl. 14.7.2): acknowledged basic link for
    /// an individual (ISSI) destination, unacknowledged for a group (GSSI).
    fn push_uplink(&self, queue: &mut MessageQueue, sdu: BitBuffer, dest_ssi: u32, dest_is_group: bool) {
        let (dest_ssi_type, layer2service) = if dest_is_group {
            (SsiType::Gssi, Layer2Service::Unacknowledged)
        } else {
            (SsiType::Issi, Layer2Service::Acknowledged)
        };
        let dest_addr = TetraAddress::new(dest_ssi, dest_ssi_type);
        let msg = SapMsg {
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
                main_address: dest_addr,
                tx_reporter: None,
            }),
        };
        queue.push_back(msg);
    }

    /// Poor man's rx_prim, as this is a subcomponent and not governed by the MessageRouter
    /// If need be, we can deviate from the standard's subentity ranking and make this a full-fledged component
    /// See Figure 14.2: Block view of CMCE-MS
    pub fn route_rf_deliver(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("route_rf_deliver");

        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            panic!();
        };
        let Some(bits) = prim.sdu.peek_bits(5) else {
            tracing::warn!("insufficient bits: {}", prim.sdu.dump_bin());
            return;
        };

        let Ok(pdu_type) = CmcePduTypeDl::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, prim.sdu.dump_bin());
            return;
        };

        // TODO FIXME: Besides these PDUs, we can also receive several signals (BUSY ind, CLOSE ind, etc)
        match pdu_type {
            CmcePduTypeDl::DSdsData => {
                self.rx_sds_data(queue, message);
            }
            CmcePduTypeDl::DStatus => {
                self.rx_status(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetra_config::bluestation::{from_toml_str, SharedConfig};
    use tetra_pdus::cmce::pdus::{d_sds_data::DSdsData, d_status::DStatus};
    use tetra_saps::lcmc::LcmcMleUnitdataInd;
    use crate::net_telemetry::channel::telemetry_channel;

    const MS_TOML: &str = r#"
config_version = "0.6"
stack_mode = "Ms"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
tx_freq = 439825000
rx_freq = 430425000
ppm_err = 0
device = "driver=sx"
sample_rate = 600000
rx_antenna = "RX"
tx_antenna = "TX"
rx_gain_lna = 48.0
rx_gain_pga = 8.0
tx_gain_dac = 0.0
tx_gain_mixer = 0.0

[net_info]
mcc = 901
mnc = 9999

[cell_info]
freq_band = 4
main_carrier = 1593
duplex_spacing = 7
custom_duplex_spacing = 9400000
freq_offset = 0
reverse_operation = false
location_area = 1
colour_code = 1

[ms]
issi = 1000001
subscriber_class = 1
attach_groups = []
"#;

    fn make_sds(sink: Option<TelemetrySink>) -> SdsMsSubentity {
        let cfg = from_toml_str(MS_TOML).expect("valid MS test config");
        SdsMsSubentity::new(SharedConfig::from_parts(cfg, None), sink)
    }

    fn deliver(sdu: BitBuffer, received: TetraAddress) -> SapMsg {
        SapMsg {
            sap: Sap::LcmcSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Cmce,
            msg: SapMsgInner::LcmcMleUnitdataInd(LcmcMleUnitdataInd {
                sdu,
                handle: 0,
                endpoint_id: 0,
                link_id: 0,
                received_tetra_address: received,
                chan_change_resp_req: false,
                chan_change_handle: None,
            }),
        }
    }

    // --- Transmit (U-SDS-DATA / U-STATUS) decoded by the BS's own parsers ---

    #[test]
    fn u_sds_data_roundtrips_through_bs_parser() {
        let mut sds = make_sds(None);
        let mut q = MessageQueue::new();
        let payload = SdsUserData::Type4(16, vec![0xAB, 0xCD]);
        sds.send_u_sds_data(&mut q, 1234, false, payload.clone());

        let msg = q.pop_front().expect("U-SDS-DATA queued");
        let SapMsgInner::LcmcMleUnitdataReq(mut prim) = msg.msg else {
            panic!("expected LcmcMleUnitdataReq");
        };
        // Individual (ISSI) destination => acknowledged basic link (cl. 14.7.2).
        assert_eq!(prim.main_address.ssi, 1234);
        assert!(matches!(prim.main_address.ssi_type, SsiType::Issi));
        assert!(matches!(prim.layer2service, Layer2Service::Acknowledged));

        let pdu = USdsData::from_bitbuf(&mut prim.sdu).expect("BS decodes U-SDS-DATA");
        assert_eq!(pdu.called_party_ssi, Some(1234));
        assert!(matches!(pdu.called_party_type_identifier, PartyTypeIdentifier::Ssi));
        assert_eq!(pdu.user_defined_data, payload);
    }

    #[test]
    fn u_status_roundtrips_through_bs_parser_group() {
        let mut sds = make_sds(None);
        let mut q = MessageQueue::new();
        let status = PreCodedStatus::from(0x8002).into_raw();
        sds.send_u_status(&mut q, 91, true, status);

        let msg = q.pop_front().expect("U-STATUS queued");
        let SapMsgInner::LcmcMleUnitdataReq(mut prim) = msg.msg else {
            panic!("expected LcmcMleUnitdataReq");
        };
        // Group (GSSI) destination => unacknowledged (cl. 14.7.2).
        assert_eq!(prim.main_address.ssi, 91);
        assert!(matches!(prim.main_address.ssi_type, SsiType::Gssi));
        assert!(matches!(prim.layer2service, Layer2Service::Unacknowledged));

        let pdu = UStatus::from_bitbuf(&mut prim.sdu).expect("BS decodes U-STATUS");
        assert_eq!(pdu.called_party_ssi, Some(91));
        assert_eq!(pdu.pre_coded_status.into_raw(), status);
    }

    // --- Receive emits exactly one TNSDS indication from a BS-built PDU ---

    fn build_d_sds_data(source: u32, data: SdsUserData) -> BitBuffer {
        let pdu = DSdsData {
            calling_party_type_identifier: PartyTypeIdentifier::Ssi,
            calling_party_address_ssi: Some(source as u64),
            calling_party_extension: None,
            user_defined_data: data,
            external_subscriber_number: None,
            dm_ms_address: None,
        };
        let mut sdu = BitBuffer::new_autoexpand(128);
        pdu.to_bitbuf(&mut sdu).unwrap();
        sdu.seek(0);
        sdu
    }

    fn build_d_status(source: u32, status: PreCodedStatus) -> BitBuffer {
        let pdu = DStatus {
            calling_party_type_identifier: PartyTypeIdentifier::Ssi,
            calling_party_address_ssi: Some(source as u64),
            calling_party_extension: None,
            pre_coded_status: status,
            external_subscriber_number: None,
            dm_ms_address: None,
        };
        let mut sdu = BitBuffer::new_autoexpand(64);
        pdu.to_bitbuf(&mut sdu).unwrap();
        sdu.seek(0);
        sdu
    }

    #[test]
    fn rx_d_sds_data_emits_unitdata_indication() {
        let (sink, source_rx) = telemetry_channel();
        let mut sds = make_sds(Some(sink));
        let mut q = MessageQueue::new();

        let payload = SdsUserData::Type4(16, vec![0x12, 0x34]);
        let sdu = build_d_sds_data(555, payload.clone());
        // Delivered to our own ISSI => individual (not group).
        let msg = deliver(sdu, TetraAddress::new(1000001, SsiType::Issi));
        sds.route_rf_deliver(&mut q, msg);

        let ev = source_rx.try_recv().expect("one telemetry event");
        match ev {
            TelemetryEvent::TnsdsUnitdataIndication(ind) => {
                assert_eq!(ind.calling_party_ssi, 555);
                assert!(!ind.called_party_is_group);
                assert_eq!(ind.user_data, payload);
            }
            other => panic!("unexpected event: {:?}", other),
        }
        assert!(source_rx.try_recv().is_none(), "exactly one indication");
    }

    #[test]
    fn rx_d_sds_data_group_addressed_sets_group_flag() {
        let (sink, source_rx) = telemetry_channel();
        let mut sds = make_sds(Some(sink));
        let mut q = MessageQueue::new();

        let sdu = build_d_sds_data(777, SdsUserData::Type1(5));
        let msg = deliver(sdu, TetraAddress::new(91, SsiType::Gssi));
        sds.route_rf_deliver(&mut q, msg);

        let ev = source_rx.try_recv().expect("one telemetry event");
        match ev {
            TelemetryEvent::TnsdsUnitdataIndication(ind) => {
                assert_eq!(ind.calling_party_ssi, 777);
                assert!(ind.called_party_is_group);
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }

    #[test]
    fn rx_d_status_emits_status_indication() {
        let (sink, source_rx) = telemetry_channel();
        let mut sds = make_sds(Some(sink));
        let mut q = MessageQueue::new();

        let status = PreCodedStatus::from(0x8003);
        let sdu = build_d_status(2200699, status);
        let msg = deliver(sdu, TetraAddress::new(1000001, SsiType::Issi));
        sds.route_rf_deliver(&mut q, msg);

        let ev = source_rx.try_recv().expect("one telemetry event");
        match ev {
            TelemetryEvent::TnsdsStatusIndication(ind) => {
                assert_eq!(ind.calling_party_ssi, 2200699);
                assert_eq!(ind.status_number, status.into_raw());
            }
            other => panic!("unexpected event: {:?}", other),
        }
        assert!(source_rx.try_recv().is_none(), "exactly one indication");
    }

    #[test]
    fn rx_without_sink_does_not_panic() {
        let mut sds = make_sds(None);
        let mut q = MessageQueue::new();
        let sdu = build_d_sds_data(555, SdsUserData::Type1(1));
        let msg = deliver(sdu, TetraAddress::new(1000001, SsiType::Issi));
        sds.route_rf_deliver(&mut q, msg);
        assert!(q.pop_front().is_none());
    }
}

