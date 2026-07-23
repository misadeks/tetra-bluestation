use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, Sap, SsiType, TdmaTime, TetraAddress};
use tetra_entities::MessageQueue;
use tetra_entities::TetraEntityTrait;
use tetra_entities::cmce::cmce_ms::CmceMs;
use tetra_entities::cmce::subentities::cc_ms::{CcMsSubentity, MsCcState, MsTxGrantState};
use tetra_entities::net_control::ControlCommand;
use tetra_entities::net_control::channel::make_control_link;
use tetra_entities::net_telemetry::channel::telemetry_channel;
use tetra_entities::net_telemetry::events::TelemetryEvent;
use tetra_pdus::cmce::{
    enums::{
        call_timeout::CallTimeout, disconnect_cause::DisconnectCause, party_type_identifier::PartyTypeIdentifier,
        transmission_grant::TransmissionGrant,
    },
    fields::basic_service_information::BasicServiceInformation,
    pdus::{
        d_disconnect::DDisconnect, d_release::DRelease, d_setup::DSetup, d_tx_continue::DTxContinue, d_tx_granted::DTxGranted,
        d_tx_wait::DTxWait, u_alert::UAlert, u_connect::UConnect, u_disconnect::UDisconnect, u_release::URelease, u_setup::USetup,
        u_tx_ceased::UTxCeased, u_tx_demand::UTxDemand,
    },
};
use tetra_saps::control::enums::{circuit_mode_type::CircuitModeType, communication_type::CommunicationType};
use tetra_saps::lcmc::{LcmcMleUnitdataInd, LcmcMleUnitdataReq};
use tetra_saps::tncc;
use tetra_saps::{SapMsg, SapMsgInner};

#[path = "common/default_stack.rs"]
mod default_stack;

const CALL_ID: u16 = 77;
const GSSI: u32 = 91;
const ISSI: u32 = 1_000_001;

fn speech_service(communication_type: CommunicationType) -> BasicServiceInformation {
    BasicServiceInformation {
        circuit_mode_type: CircuitModeType::TchS,
        encryption_flag: false,
        communication_type,
        slots_per_frame: None,
        speech_service: Some(0),
    }
}

fn dl_msg<P>(pdu: &P, address: TetraAddress) -> SapMsg
where
    P: DlWrite,
{
    let mut sdu = BitBuffer::new_autoexpand(128);
    pdu.write_dl(&mut sdu).unwrap();
    sdu.seek(0);
    SapMsg {
        sap: Sap::LcmcSap,
        src: TetraEntity::Mle,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::LcmcMleUnitdataInd(LcmcMleUnitdataInd {
            sdu,
            handle: 11,
            endpoint_id: 22,
            link_id: 33,
            received_tetra_address: address,
            chan_change_resp_req: false,
            chan_change_handle: None,
        }),
    }
}

trait DlWrite {
    fn write_dl(&self, buffer: &mut BitBuffer) -> Result<(), tetra_core::PduParseErr>;
}

macro_rules! dl_write {
    ($ty:ty) => {
        impl DlWrite for $ty {
            fn write_dl(&self, buffer: &mut BitBuffer) -> Result<(), tetra_core::PduParseErr> {
                self.to_bitbuf(buffer)
            }
        }
    };
}

dl_write!(DSetup);
dl_write!(DTxGranted);
dl_write!(DTxWait);
dl_write!(DTxContinue);
dl_write!(DDisconnect);
dl_write!(DRelease);

fn pop_unitdata(queue: &mut MessageQueue) -> LcmcMleUnitdataReq {
    loop {
        let msg = queue.pop_front().expect("expected queued SAP message");
        if let SapMsgInner::LcmcMleUnitdataReq(prim) = msg.msg {
            return prim;
        }
    }
}

fn pop_config(queue: &mut MessageQueue) -> tetra_saps::lcmc::LcmcMleConfigureReq {
    loop {
        let msg = queue.pop_front().expect("expected queued SAP message");
        if let SapMsgInner::LcmcMleConfigureReq(prim) = msg.msg {
            return prim;
        }
    }
}

fn group_setup(grant: TransmissionGrant) -> DSetup {
    DSetup {
        call_identifier: CALL_ID,
        call_time_out: CallTimeout::T5m,
        hook_method_selection: false,
        simplex_duplex_selection: false,
        basic_service_information: speech_service(CommunicationType::P2Mp),
        transmission_grant: grant,
        transmission_request_permission: true,
        call_priority: 0,
        notification_indicator: None,
        temporary_address: None,
        calling_party_address_ssi: Some(ISSI),
        calling_party_extension: None,
        external_subscriber_number: None,
        facility: None,
        dm_ms_address: None,
        proprietary: None,
    }
}

fn tncc_setup_response() -> tncc::TnccSetupResponse {
    tncc::TnccSetupResponse {
        access_priority: None,
        basic_service_information: None,
        clir_control: None,
        hook_method_selection: tncc::HookMethodSelection::HookOnHookOffSignallingOrCallAcceptanceSignalling,
        simplex_duplex_selection: tncc::SimplexDuplexSelection::SimplexOperation,
        traffic_stealing: None,
    }
}

fn tncc_complete_request() -> tncc::TnccCompleteRequest {
    tncc::TnccCompleteRequest {
        access_priority: None,
        basic_service_information_offered: None,
        hook_method: tncc::HookMethodSelection::HookOnHookOffSignallingOrCallAcceptanceSignalling,
        simplex_duplex: tncc::SimplexDuplexSelection::SimplexOperation,
        traffic_stealing: None,
    }
}
fn shared_ms_config() -> SharedConfig {
    SharedConfig::from_parts(default_stack::default_test_config_ms(), None)
}

fn tncc_basic() -> tetra_saps::tncc::TnccBasicServiceInformation {
    use tetra_saps::tncc as t;
    t::TnccBasicServiceInformation {
        circuit_mode_service: t::CircuitModeService::SpeechService,
        communication_type: t::CommunicationType::PointToMultipoint,
        data_service: None,
        data_call_capacity: None,
        encryption_flag: t::EncryptionFlag::ClearEndToEndTransmission,
        speech_service: Some(t::SpeechService::TetraEncodedOneTimeslotSpeech),
    }
}

fn tncc_setup_request() -> tetra_saps::tncc::TnccSetupRequest {
    use tetra_saps::tncc as t;
    t::TnccSetupRequest {
        access_priority: None,
        area_selection: None,
        basic_service_information: tncc_basic(),
        call_priority: t::CallPriority::PriorityNotDefined,
        called_party_type_identifier: t::CalledPartyTypeIdentifier::Ssi,
        called_party_sna: None,
        called_party_ssi: Some(GSSI),
        called_party_extension: None,
        external_subscriber_number_called: None,
        clir_control: None,
        hook_method_selection: t::HookMethodSelection::NoHookSignallingDirectThroughConnect,
        request_to_transmit_send_data: t::RequestToTransmitSendData::RequestToTransmitSendData,
        simplex_duplex_selection: t::SimplexDuplexSelection::SimplexOperation,
        traffic_stealing: None,
    }
}

#[test]
fn cmce_ms_tncc_setup_command_emits_decodable_u_setup() {
    let (dispatcher, endpoint) = make_control_link();
    let mut cmce = CmceMs::new(shared_ms_config(), None, Some(endpoint));
    let mut q = MessageQueue::new();

    dispatcher.send(ControlCommand::TnccSetup {
        handle: 44,
        request: Box::new(tncc_setup_request()),
    });
    cmce.tick_start(&mut q, TdmaTime::default());

    let ack = dispatcher.try_recv_response().expect("TNCC ack");
    assert!(matches!(
        ack,
        tetra_entities::net_control::ControlResponse::TnccAck {
            handle: 44,
            accepted: true,
            ..
        }
    ));
    let mut prim = pop_unitdata(&mut q);
    let pdu = USetup::from_bitbuf(&mut prim.sdu).unwrap();
    assert_eq!(pdu.called_party_ssi, Some(GSSI as u64));
    assert!(pdu.request_to_transmit_send_data);
    assert_eq!(pdu.basic_service_information.communication_type, CommunicationType::P2Mp);
}

#[test]
fn cmce_ms_downlink_setup_emits_tncc_setup_indication() {
    let (sink, source) = telemetry_channel();
    let mut cmce = CmceMs::new(shared_ms_config(), Some(sink), None);
    let mut q = MessageQueue::new();

    cmce.rx_prim(
        &mut q,
        dl_msg(
            &group_setup(TransmissionGrant::GrantedToOtherUser),
            TetraAddress::new(GSSI, SsiType::Gssi),
        ),
    );

    let event = source.try_recv().expect("TNCC telemetry");
    let TelemetryEvent::TnccSetupIndication {
        call_identifier,
        indication,
    } = event
    else {
        panic!("expected TNCC setup indication");
    };
    assert_eq!(call_identifier, CALL_ID);
    assert_eq!(indication.called_party_ssi, GSSI);
    assert_eq!(indication.calling_party_ssi, Some(ISSI));
}
#[test]
fn ms_originated_setup_pdus_decode_with_tetra_pdus() {
    let mut cc = CcMsSubentity::new(None);
    let mut q = MessageQueue::new();

    cc.originate_group_call(&mut q, GSSI, speech_service(CommunicationType::P2Mp), false);
    let mut prim = pop_unitdata(&mut q);
    let pdu = USetup::from_bitbuf(&mut prim.sdu).unwrap();
    assert_eq!(pdu.called_party_type_identifier, PartyTypeIdentifier::Ssi);
    assert_eq!(pdu.called_party_ssi, Some(GSSI as u64));
    assert_eq!(pdu.basic_service_information.communication_type, CommunicationType::P2Mp);

    cc.originate_individual_call(&mut q, ISSI, speech_service(CommunicationType::P2p), true, true);
    let mut prim = pop_unitdata(&mut q);
    let pdu = USetup::from_bitbuf(&mut prim.sdu).unwrap();
    assert_eq!(pdu.called_party_ssi, Some(ISSI as u64));
    assert!(pdu.hook_method_selection);
    assert!(pdu.simplex_duplex_selection);
    assert!(pdu.request_to_transmit_send_data);
    assert_eq!(pdu.basic_service_information.communication_type, CommunicationType::P2p);
}

#[test]
fn active_group_tx_demand_ceased_and_disconnect_pdus_decode() {
    let mut cc = CcMsSubentity::new(None);
    let mut q = MessageQueue::new();
    cc.route_rd_deliver(
        &mut q,
        dl_msg(&group_setup(TransmissionGrant::NotGranted), TetraAddress::new(GSSI, SsiType::Gssi)),
    );
    assert_eq!(cc.call(CALL_ID).unwrap().state, MsCcState::CallActive);

    assert!(cc.request_tx(&mut q, CALL_ID, 2));
    let mut prim = pop_unitdata(&mut q);
    let pdu = UTxDemand::from_bitbuf(&mut prim.sdu).unwrap();
    assert_eq!(pdu.call_identifier, CALL_ID);
    assert_eq!(pdu.tx_demand_priority, 2);

    assert!(cc.cease_tx(&mut q, CALL_ID));
    let mut prim = pop_unitdata(&mut q);
    assert!(prim.stealing_permission);
    assert!(prim.stealing_repeats_flag);
    let pdu = UTxCeased::from_bitbuf(&mut prim.sdu).unwrap();
    assert_eq!(pdu.call_identifier, CALL_ID);
    let cfg = pop_config(&mut q);
    assert!(!cfg.switch_u_plane);

    assert!(cc.disconnect_call(&mut q, CALL_ID, DisconnectCause::UserRequestedDisconnection));
    let mut prim = pop_unitdata(&mut q);
    let pdu = UDisconnect::from_bitbuf(&mut prim.sdu).unwrap();
    assert_eq!(pdu.call_identifier, CALL_ID);
    assert_eq!(pdu.disconnect_cause, DisconnectCause::UserRequestedDisconnection);
}

#[test]
fn incoming_individual_answer_and_network_disconnect_pdus_decode() {
    let mut cc = CcMsSubentity::new(None);
    let mut q = MessageQueue::new();
    let mut setup = group_setup(TransmissionGrant::NotGranted);
    setup.basic_service_information = speech_service(CommunicationType::P2p);
    setup.hook_method_selection = true; // on/off-hook signalling (cl. 14.8.23)
    cc.route_rd_deliver(&mut q, dl_msg(&setup, TetraAddress::new(ISSI, SsiType::Issi)));
    assert_eq!(cc.call(CALL_ID).unwrap().state, MsCcState::MtCallSetup);

    // On/off-hook (cl. 14.5.1.1.1): the TNCC-SETUP response only rings the far
    // end with U-ALERT; the call stays in MT-CALL-SETUP, no U-CONNECT yet.
    assert!(cc.handle_tncc_setup_response(&mut q, CALL_ID, &tncc_setup_response()));
    let mut prim = pop_unitdata(&mut q);
    let alert = UAlert::from_bitbuf(&mut prim.sdu).unwrap();
    assert_eq!(alert.call_identifier, CALL_ID);
    assert_eq!(cc.call(CALL_ID).unwrap().state, MsCcState::MtCallSetup);

    // Local pickup: TNCC-COMPLETE sends U-CONNECT to connect the call.
    assert!(cc.handle_tncc_complete(&mut q, CALL_ID, &tncc_complete_request()));
    let mut prim = pop_unitdata(&mut q);
    let connect = UConnect::from_bitbuf(&mut prim.sdu).unwrap();
    assert_eq!(connect.call_identifier, CALL_ID);

    let disconnect = DDisconnect {
        call_identifier: CALL_ID,
        disconnect_cause: DisconnectCause::SwmiRequestedDisconnection,
        notification_indicator: None,
        facility: None,
        proprietary: None,
    };
    cc.route_rd_deliver(&mut q, dl_msg(&disconnect, TetraAddress::new(ISSI, SsiType::Issi)));
    let mut prim = pop_unitdata(&mut q);
    let release = URelease::from_bitbuf(&mut prim.sdu).unwrap();
    assert_eq!(release.call_identifier, CALL_ID);
    assert_eq!(release.disconnect_cause, DisconnectCause::SwmiRequestedDisconnection);
    assert!(!pop_config(&mut q).switch_u_plane);
}

#[test]
fn downlink_grant_wait_continue_and_release_update_ms_state() {
    let mut cc = CcMsSubentity::new(None);
    let mut q = MessageQueue::new();
    cc.route_rd_deliver(
        &mut q,
        dl_msg(
            &group_setup(TransmissionGrant::GrantedToOtherUser),
            TetraAddress::new(GSSI, SsiType::Gssi),
        ),
    );
    let call = cc.call(CALL_ID).unwrap();
    assert_eq!(call.tx_grant_state, MsTxGrantState::GrantedOther);
    let cfg = pop_config(&mut q);
    assert!(cfg.switch_u_plane);
    assert!(!cfg.tx_grant);

    let granted = DTxGranted {
        call_identifier: CALL_ID,
        transmission_grant: TransmissionGrant::Granted.into_raw() as u8,
        transmission_request_permission: true,
        encryption_control: false,
        reserved: false,
        notification_indicator: None,
        transmitting_party_type_identifier: None,
        transmitting_party_address_ssi: None,
        transmitting_party_extension: None,
        external_subscriber_number: None,
        facility: None,
        dm_ms_address: None,
        proprietary: None,
    };
    cc.route_rd_deliver(&mut q, dl_msg(&granted, TetraAddress::new(GSSI, SsiType::Gssi)));
    assert_eq!(cc.call(CALL_ID).unwrap().tx_grant_state, MsTxGrantState::GrantedSelf);
    assert!(pop_config(&mut q).tx_grant);

    let wait = DTxWait {
        call_identifier: CALL_ID,
        transmission_request_permission: true,
        notification_indicator: None,
        facility: None,
        dm_ms_address: None,
        proprietary: None,
    };
    cc.route_rd_deliver(&mut q, dl_msg(&wait, TetraAddress::new(GSSI, SsiType::Gssi)));
    assert_eq!(cc.call(CALL_ID).unwrap().state, MsCcState::Wait);
    assert!(!pop_config(&mut q).switch_u_plane);

    let cont = DTxContinue {
        call_identifier: CALL_ID,
        do_continue: true,
        transmission_request_permission: true,
        notification_indicator: None,
        facility: None,
        dm_ms_address: None,
        proprietary: None,
    };
    cc.route_rd_deliver(&mut q, dl_msg(&cont, TetraAddress::new(GSSI, SsiType::Gssi)));
    assert_eq!(cc.call(CALL_ID).unwrap().state, MsCcState::CallActive);
    assert!(pop_config(&mut q).switch_u_plane);

    let release = DRelease {
        call_identifier: CALL_ID,
        disconnect_cause: DisconnectCause::SwmiRequestedDisconnection,
        notification_indicator: None,
        facility: None,
        proprietary: None,
    };
    cc.route_rd_deliver(&mut q, dl_msg(&release, TetraAddress::new(GSSI, SsiType::Gssi)));
    assert_eq!(cc.call_count(), 0);
    assert!(!pop_config(&mut q).switch_u_plane);
}
