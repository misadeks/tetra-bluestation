use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, Sap, SsiType, TetraAddress};
use tetra_entities::MessageQueue;
use tetra_entities::cmce::subentities::cc_ms::{CcMsSubentity, MsCcState, MsTxGrantState};
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
use tetra_saps::{SapMsg, SapMsgInner};

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

#[test]
fn ms_originated_setup_pdus_decode_with_tetra_pdus() {
    let mut cc = CcMsSubentity::new();
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
    let mut cc = CcMsSubentity::new();
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
    let mut cc = CcMsSubentity::new();
    let mut q = MessageQueue::new();
    let mut setup = group_setup(TransmissionGrant::NotGranted);
    setup.basic_service_information = speech_service(CommunicationType::P2p);
    cc.route_rd_deliver(&mut q, dl_msg(&setup, TetraAddress::new(ISSI, SsiType::Issi)));
    assert_eq!(cc.call(CALL_ID).unwrap().state, MsCcState::MtCallSetup);

    assert!(cc.answer_call(&mut q, CALL_ID, true));
    let mut prim = pop_unitdata(&mut q);
    let alert = UAlert::from_bitbuf(&mut prim.sdu).unwrap();
    assert_eq!(alert.call_identifier, CALL_ID);
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
    let mut cc = CcMsSubentity::new();
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
