mod common;

use tetra_config::bluestation::StackMode;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, Direction, Sap, SsiType, TdmaTime, TetraAddress, TxState, debug};
use tetra_pdus::cmce::enums::{
    cmce_pdu_type_dl::CmcePduTypeDl, party_type_identifier::PartyTypeIdentifier, transmission_grant::TransmissionGrant,
};
use tetra_pdus::cmce::fields::basic_service_information::BasicServiceInformation;
use tetra_pdus::cmce::pdus::{
    d_connect::DConnect, d_connect_acknowledge::DConnectAcknowledge, d_setup::DSetup, d_tx_ceased::DTxCeased, d_tx_granted::DTxGranted,
    u_connect::UConnect, u_setup::USetup, u_tx_ceased::UTxCeased, u_tx_demand::UTxDemand,
};
use tetra_saps::control::brew::{BrewSubscriberAction, MmSubscriberUpdate};
use tetra_saps::control::call_control::{CallControl, NetworkCircuitCall};
use tetra_saps::control::enums::circuit_mode_type::CircuitModeType;
use tetra_saps::control::enums::communication_type::CommunicationType;
use tetra_saps::lcmc::LcmcMleUnitdataInd;
use tetra_saps::lcmc::enums::ul_dl_assignment::UlDlAssignment;
use tetra_saps::sapmsg::{SapMsg, SapMsgInner};

use crate::common::ComponentTest;

const TEST_GSSI: u32 = 91;
const TEST_ISSI: u32 = 1000001;

/// Helper: register a subscriber on a GSSI so CMCE accepts calls for that group.
fn register_subscriber(test: &mut ComponentTest, issi: u32, gssi: u32) {
    let register = SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Mm,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::MmSubscriberUpdate(MmSubscriberUpdate {
            issi,
            groups: vec![],
            action: BrewSubscriberAction::Register,
        }),
    };
    test.submit_message(register);
    test.run_stack(Some(1));

    let affiliate = SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Mm,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::MmSubscriberUpdate(MmSubscriberUpdate {
            issi,
            groups: vec![gssi],
            action: BrewSubscriberAction::Affiliate,
        }),
    };
    test.submit_message(affiliate);
    test.run_stack(Some(1));
    test.dump_sinks();
}

/// Helper: build a U-SETUP SAP message for a group call.
fn build_u_setup_msg(calling_issi: u32, dest_gssi: u32) -> SapMsg {
    let u_setup = USetup {
        area_selection: 0,
        hook_method_selection: false,
        simplex_duplex_selection: false,
        basic_service_information: BasicServiceInformation {
            circuit_mode_type: CircuitModeType::TchS,
            encryption_flag: false,
            communication_type: CommunicationType::P2Mp,
            slots_per_frame: None,
            speech_service: Some(0),
        },
        request_to_transmit_send_data: false,
        call_priority: 0,
        clir_control: 0,
        called_party_type_identifier: PartyTypeIdentifier::Ssi,
        called_party_ssi: Some(dest_gssi as u64),
        called_party_short_number_address: None,
        called_party_extension: None,
        external_subscriber_number: None,
        facility: None,
        dm_ms_address: None,
        proprietary: None,
    };

    let mut sdu = BitBuffer::new_autoexpand(80);
    u_setup.to_bitbuf(&mut sdu).expect("Failed to serialize USetup");
    sdu.seek(0);

    SapMsg {
        sap: Sap::LcmcSap,
        src: TetraEntity::Mle,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::LcmcMleUnitdataInd(LcmcMleUnitdataInd {
            sdu,
            handle: 1,
            endpoint_id: 1,
            link_id: 1,
            received_tetra_address: TetraAddress::new(calling_issi, SsiType::Issi),
            chan_change_resp_req: false,
            chan_change_handle: None,
        }),
    }
}

/// Helper: build a U-SETUP SAP message for an individual call.
fn build_individual_u_setup_msg(calling_issi: u32, called_issi: u32) -> SapMsg {
    build_individual_u_setup_msg_with_mode(calling_issi, called_issi, true)
}

fn build_individual_u_setup_msg_with_mode(calling_issi: u32, called_issi: u32, simplex_duplex_selection: bool) -> SapMsg {
    let u_setup = USetup {
        area_selection: 0,
        hook_method_selection: true,
        simplex_duplex_selection,
        basic_service_information: BasicServiceInformation {
            circuit_mode_type: CircuitModeType::TchS,
            encryption_flag: false,
            communication_type: CommunicationType::P2p,
            slots_per_frame: None,
            speech_service: Some(0),
        },
        request_to_transmit_send_data: false,
        call_priority: 0,
        clir_control: 0,
        called_party_type_identifier: PartyTypeIdentifier::Ssi,
        called_party_ssi: Some(called_issi as u64),
        called_party_short_number_address: None,
        called_party_extension: None,
        external_subscriber_number: None,
        facility: None,
        dm_ms_address: None,
        proprietary: None,
    };

    let mut sdu = BitBuffer::new_autoexpand(80);
    u_setup.to_bitbuf(&mut sdu).expect("Failed to serialize USetup");
    sdu.seek(0);

    SapMsg {
        sap: Sap::LcmcSap,
        src: TetraEntity::Mle,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::LcmcMleUnitdataInd(LcmcMleUnitdataInd {
            sdu,
            handle: 1,
            endpoint_id: 1,
            link_id: 1,
            received_tetra_address: TetraAddress::new(calling_issi, SsiType::Issi),
            chan_change_resp_req: false,
            chan_change_handle: None,
        }),
    }
}

fn lcmc_ind(sender_issi: u32, sdu: BitBuffer) -> SapMsg {
    SapMsg {
        sap: Sap::LcmcSap,
        src: TetraEntity::Mle,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::LcmcMleUnitdataInd(LcmcMleUnitdataInd {
            sdu,
            handle: 1,
            endpoint_id: 1,
            link_id: 1,
            received_tetra_address: TetraAddress::new(sender_issi, SsiType::Issi),
            chan_change_resp_req: false,
            chan_change_handle: None,
        }),
    }
}

fn build_u_connect_msg(sender_issi: u32, call_id: u16, simplex_duplex_selection: bool) -> SapMsg {
    let u_connect = UConnect {
        call_identifier: call_id,
        hook_method_selection: true,
        simplex_duplex_selection,
        basic_service_information: None,
        facility: None,
        proprietary: None,
    };

    let mut sdu = BitBuffer::new_autoexpand(80);
    u_connect.to_bitbuf(&mut sdu).expect("Failed to serialize UConnect");
    sdu.seek(0);
    lcmc_ind(sender_issi, sdu)
}

fn build_u_tx_demand_msg(sender_issi: u32, call_id: u16) -> SapMsg {
    let u_tx_demand = UTxDemand {
        call_identifier: call_id,
        tx_demand_priority: 0,
        encryption_control: false,
        reserved: false,
        facility: None,
        dm_ms_address: None,
        proprietary: None,
    };

    let mut sdu = BitBuffer::new_autoexpand(80);
    u_tx_demand.to_bitbuf(&mut sdu).expect("Failed to serialize UTxDemand");
    sdu.seek(0);
    lcmc_ind(sender_issi, sdu)
}

fn build_u_tx_ceased_msg(sender_issi: u32, call_id: u16) -> SapMsg {
    let u_tx_ceased = UTxCeased {
        call_identifier: call_id,
        facility: None,
        dm_ms_address: None,
        proprietary: None,
    };

    let mut sdu = BitBuffer::new_autoexpand(80);
    u_tx_ceased.to_bitbuf(&mut sdu).expect("Failed to serialize UTxCeased");
    sdu.seek(0);
    lcmc_ind(sender_issi, sdu)
}

fn dl_pdu_type(sdu: &BitBuffer) -> Option<CmcePduTypeDl> {
    CmcePduTypeDl::try_from(sdu.peek_bits(5)?).ok()
}

fn find_lcmc_req(msgs: &[SapMsg], address_issi: u32, pdu_type: CmcePduTypeDl) -> Option<(BitBuffer, Option<UlDlAssignment>)> {
    msgs.iter().find_map(|msg| {
        if msg.dest != TetraEntity::Mle {
            return None;
        }

        let SapMsgInner::LcmcMleUnitdataReq(prim) = &msg.msg else {
            return None;
        };

        if prim.main_address.ssi != address_issi || dl_pdu_type(&prim.sdu) != Some(pdu_type) {
            return None;
        }

        Some((
            prim.sdu.clone(),
            prim.chan_alloc.as_ref().map(|chan_alloc| chan_alloc.ul_dl_assigned),
        ))
    })
}

fn first_d_setup_call_id(msgs: &[SapMsg], called_issi: u32) -> u16 {
    let (mut sdu, _) = find_lcmc_req(msgs, called_issi, CmcePduTypeDl::DSetup).expect("Expected D-SETUP to called ISSI");
    let d_setup = DSetup::from_bitbuf(&mut sdu).expect("Failed to parse DSetup");
    d_setup.call_identifier
}

fn connected_simplex_individual_call(calling_issi: u32, called_issi: u32) -> (ComponentTest, u16, Vec<SapMsg>) {
    let dltime = TdmaTime { h: 0, m: 1, f: 1, t: 1 };
    let mut test = ComponentTest::new(StackMode::Bs, Some(dltime));

    let components = vec![TetraEntity::Cmce];
    let sinks = vec![TetraEntity::Mle, TetraEntity::Umac, TetraEntity::Brew];
    test.populate_entities(components, sinks);
    test.config.state_write().subscribers.register(called_issi);

    test.submit_message(build_individual_u_setup_msg_with_mode(calling_issi, called_issi, false));
    test.run_stack(Some(1));
    let setup_msgs = test.dump_sinks();
    let call_id = first_d_setup_call_id(&setup_msgs, called_issi);

    test.submit_message(build_u_connect_msg(called_issi, call_id, false));
    test.run_stack(Some(1));
    let connect_msgs = test.dump_sinks();

    (test, call_id, connect_msgs)
}

fn connected_brew_originated_simplex_call(remote_issi: u32, local_issi: u32) -> (ComponentTest, u16, uuid::Uuid) {
    let dltime = TdmaTime { h: 0, m: 1, f: 1, t: 1 };
    let mut test = ComponentTest::new(StackMode::Bs, Some(dltime));

    let components = vec![TetraEntity::Cmce];
    let sinks = vec![TetraEntity::Mle, TetraEntity::Umac, TetraEntity::Brew];
    test.populate_entities(components, sinks);
    test.config.state_write().subscribers.register(local_issi);

    let brew_uuid = uuid::Uuid::parse_str("a9661625-c1f2-42bb-b256-c44e14677307").unwrap();
    test.submit_message(SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Brew,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::CmceCallControl(CallControl::NetworkCircuitSetupRequest {
            brew_uuid,
            call: NetworkCircuitCall {
                source_issi: remote_issi,
                destination: local_issi,
                number: String::new(),
                priority: 1,
                service: 0,
                mode: 0,
                duplex: 0,
                method: 0,
                communication: 0,
                grant: TransmissionGrant::NotGranted.into_raw() as u8,
                permission: 0,
                timeout: 0,
                ownership: 0,
                queued: 0,
            },
        }),
    });
    test.run_stack(Some(1));
    let setup_msgs = test.dump_sinks();
    let call_id = first_d_setup_call_id(&setup_msgs, local_issi);

    test.submit_message(build_u_connect_msg(local_issi, call_id, false));
    test.run_stack(Some(1));
    test.dump_sinks();

    test.submit_message(SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Brew,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::CmceCallControl(CallControl::NetworkCircuitConnectConfirm {
            brew_uuid,
            grant: TransmissionGrant::Granted.into_raw() as u8,
            permission: 0,
        }),
    });
    test.run_stack(Some(1));
    test.dump_sinks();

    (test, call_id, brew_uuid)
}

/// Extract tx_reporters from D-SETUP messages in the sink output.
/// D-SETUPs are identified as LcmcMleUnitdataReq with a chan_alloc that has a usage field.
fn extract_d_setup_reporters(msgs: &mut Vec<SapMsg>) -> Vec<tetra_core::TxReporter> {
    let mut reporters = vec![];
    for msg in msgs.iter_mut() {
        if msg.dest == TetraEntity::Mle {
            if let SapMsgInner::LcmcMleUnitdataReq(ref mut prim) = msg.msg {
                if prim.chan_alloc.as_ref().is_some_and(|ca| ca.usage.is_some()) {
                    if let Some(reporter) = prim.tx_reporter.take() {
                        reporters.push(reporter);
                    }
                }
            }
        }
    }
    reporters
}

/// Count D-SETUP messages in sink output without taking reporters.
fn count_d_setups(msgs: &[SapMsg]) -> usize {
    msgs.iter()
        .filter(|msg| {
            msg.dest == TetraEntity::Mle
                && matches!(&msg.msg, SapMsgInner::LcmcMleUnitdataReq(prim)
                    if dl_pdu_type(&prim.sdu) == Some(CmcePduTypeDl::DSetup))
        })
        .count()
}

#[test]
fn test_individual_setup_uses_central_subscriber_registry_for_local_destination() {
    debug::setup_logging_verbose();

    let dltime = TdmaTime { h: 0, m: 1, f: 1, t: 1 };
    let mut test = ComponentTest::new(StackMode::Bs, Some(dltime));

    let components = vec![TetraEntity::Cmce];
    let sinks = vec![TetraEntity::Mle, TetraEntity::Umac, TetraEntity::Brew];
    test.populate_entities(components, sinks);

    let calling_issi = 1000001;
    let called_issi = 1000002;
    test.config.state_write().subscribers.register(called_issi);

    test.submit_message(build_individual_u_setup_msg(calling_issi, called_issi));
    test.run_stack(Some(1));

    let msgs = test.dump_sinks();
    assert!(
        count_d_setups(&msgs) > 0,
        "Expected local D-SETUP for centrally registered called ISSI"
    );
    assert!(
        !msgs.iter().any(|msg| matches!(
            &msg.msg,
            SapMsgInner::CmceCallControl(CallControl::NetworkCircuitSetupRequest { .. })
        )),
        "Local registered ISSI should not be routed over Brew"
    );
}

#[test]
fn test_simplex_individual_connect_grants_calling_ms_initial_floor() {
    debug::setup_logging_verbose();

    let calling_issi = 1000001;
    let called_issi = 1000002;
    let (_test, call_id, msgs) = connected_simplex_individual_call(calling_issi, called_issi);

    let (mut connect_sdu, connect_alloc) =
        find_lcmc_req(&msgs, calling_issi, CmcePduTypeDl::DConnect).expect("Expected D-CONNECT to calling ISSI");
    let d_connect = DConnect::from_bitbuf(&mut connect_sdu).expect("Failed to parse DConnect");
    assert_eq!(d_connect.call_identifier, call_id);
    assert_eq!(d_connect.transmission_grant, TransmissionGrant::Granted);
    assert_eq!(connect_alloc, Some(UlDlAssignment::Both));

    let (mut ack_sdu, ack_alloc) =
        find_lcmc_req(&msgs, called_issi, CmcePduTypeDl::DConnectAcknowledge).expect("Expected D-CONNECT ACKNOWLEDGE to called ISSI");
    let d_ack = DConnectAcknowledge::from_bitbuf(&mut ack_sdu).expect("Failed to parse DConnectAcknowledge");
    assert_eq!(d_ack.call_identifier, call_id);
    assert_eq!(d_ack.transmission_grant, TransmissionGrant::GrantedToOtherUser.into_raw() as u8);
    assert_eq!(ack_alloc, Some(UlDlAssignment::Both));

    assert!(msgs.iter().any(|msg| matches!(
        &msg.msg,
        SapMsgInner::CmceCallControl(CallControl::FloorGranted { source_issi, dest_gssi, .. })
            if *source_issi == calling_issi && *dest_gssi == called_issi
    )));
}

#[test]
fn test_brew_originated_simplex_connect_confirm_makes_local_ms_listener() {
    debug::setup_logging_verbose();

    let dltime = TdmaTime { h: 0, m: 1, f: 1, t: 1 };
    let mut test = ComponentTest::new(StackMode::Bs, Some(dltime));

    let components = vec![TetraEntity::Cmce];
    let sinks = vec![TetraEntity::Mle, TetraEntity::Umac, TetraEntity::Brew];
    test.populate_entities(components, sinks);

    let remote_issi = 2200699;
    let local_issi = 2200769;
    let brew_uuid = uuid::Uuid::parse_str("a9661625-c1f2-42bb-b256-c44e14677307").unwrap();
    test.config.state_write().subscribers.register(local_issi);

    test.submit_message(SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Brew,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::CmceCallControl(CallControl::NetworkCircuitSetupRequest {
            brew_uuid,
            call: NetworkCircuitCall {
                source_issi: remote_issi,
                destination: local_issi,
                number: String::new(),
                priority: 1,
                service: 0,
                mode: 0,
                duplex: 0,
                method: 0,
                communication: 0,
                grant: 1,
                permission: 0,
                timeout: 0,
                ownership: 0,
                queued: 0,
            },
        }),
    });
    test.run_stack(Some(1));
    let setup_msgs = test.dump_sinks();

    assert!(setup_msgs.iter().any(|msg| matches!(
        &msg.msg,
        SapMsgInner::CmceCallControl(CallControl::NetworkCircuitSetupAccept { brew_uuid: accepted_uuid })
            if *accepted_uuid == brew_uuid
    )));
    let (mut setup_sdu, _) = find_lcmc_req(&setup_msgs, local_issi, CmcePduTypeDl::DSetup).expect("Expected D-SETUP to local ISSI");
    let d_setup = DSetup::from_bitbuf(&mut setup_sdu).expect("Failed to parse DSetup");
    assert_eq!(d_setup.calling_party_address_ssi, Some(remote_issi));
    assert!(!d_setup.hook_method_selection);
    assert_eq!(d_setup.transmission_grant, TransmissionGrant::GrantedToOtherUser);
    let call_id = d_setup.call_identifier;

    test.submit_message(build_u_connect_msg(local_issi, call_id, false));
    test.run_stack(Some(1));
    let connect_request_msgs = test.dump_sinks();
    assert!(connect_request_msgs.iter().any(|msg| matches!(
        &msg.msg,
        SapMsgInner::CmceCallControl(CallControl::NetworkCircuitConnectRequest { brew_uuid: request_uuid, .. })
            if *request_uuid == brew_uuid
    )));

    test.submit_message(SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Brew,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::CmceCallControl(CallControl::NetworkCircuitConnectConfirm {
            brew_uuid,
            grant: TransmissionGrant::Granted.into_raw() as u8,
            permission: 0,
        }),
    });
    test.run_stack(Some(1));
    let confirm_msgs = test.dump_sinks();

    let (mut ack_sdu, ack_alloc) =
        find_lcmc_req(&confirm_msgs, local_issi, CmcePduTypeDl::DConnectAcknowledge).expect("Expected D-CONNECT ACKNOWLEDGE to local ISSI");
    let d_ack = DConnectAcknowledge::from_bitbuf(&mut ack_sdu).expect("Failed to parse DConnectAcknowledge");
    assert_eq!(d_ack.call_identifier, call_id);
    assert_eq!(d_ack.transmission_grant, TransmissionGrant::GrantedToOtherUser.into_raw() as u8);
    assert_eq!(ack_alloc, Some(UlDlAssignment::Both));

    assert!(confirm_msgs.iter().any(|msg| matches!(
        &msg.msg,
        SapMsgInner::CmceCallControl(CallControl::Open(circuit))
            if circuit.direction == Direction::Both && circuit.ts == 2
    )));
    assert!(
        !confirm_msgs.iter().any(|msg| matches!(
            &msg.msg,
            SapMsgInner::CmceCallControl(CallControl::Open(circuit))
                if (circuit.direction == Direction::Ul || circuit.direction == Direction::Dl) && circuit.ts == 2
        )),
        "Brew-originated simplex media should open the shared traffic circuit once"
    );

    assert!(confirm_msgs.iter().any(|msg| matches!(
        &msg.msg,
        SapMsgInner::CmceCallControl(CallControl::NetworkCircuitMediaReady {
            brew_uuid: ready_uuid,
            call_id: ready_call_id,
            ts: 2,
        }) if *ready_uuid == brew_uuid && *ready_call_id == call_id
    )));
    assert!(
        !confirm_msgs.iter().any(|msg| matches!(
            &msg.msg,
            SapMsgInner::CmceCallControl(CallControl::FloorGranted { source_issi, .. })
                if *source_issi == local_issi
        )),
        "Brew-originated media must not grant the local called MS uplink floor"
    );
}

#[test]
fn test_brew_originated_simplex_remote_idle_hands_floor_to_queued_local_ms() {
    debug::setup_logging_verbose();

    let remote_issi = 2200699;
    let local_issi = 2200769;
    let (mut test, call_id, brew_uuid) = connected_brew_originated_simplex_call(remote_issi, local_issi);

    test.submit_message(build_u_tx_demand_msg(local_issi, call_id));
    test.run_stack(Some(1));
    let demand_msgs = test.dump_sinks();
    let (mut queued_sdu, queued_alloc) =
        find_lcmc_req(&demand_msgs, local_issi, CmcePduTypeDl::DTxGranted).expect("Expected queued D-TX GRANTED");
    let queued = DTxGranted::from_bitbuf(&mut queued_sdu).expect("Failed to parse queued DTxGranted");
    assert_eq!(queued.call_identifier, call_id);
    assert_eq!(queued.transmission_grant, TransmissionGrant::RequestQueued.into_raw() as u8);
    assert_eq!(queued_alloc, Some(UlDlAssignment::Dl));
    assert!(
        !demand_msgs.iter().any(|msg| matches!(
            &msg.msg,
            SapMsgInner::CmceCallControl(CallControl::NetworkCircuitSimplexGranted { .. })
        )),
        "Local queued demand must not tell Brew that local already has the floor"
    );

    test.submit_message(SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Brew,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::CmceCallControl(CallControl::NetworkCircuitSimplexIdle {
            brew_uuid,
            grant: TransmissionGrant::NotGranted.into_raw() as u8,
            permission: 0,
        }),
    });
    test.run_stack(Some(1));
    let idle_msgs = test.dump_sinks();

    let (mut grant_sdu, grant_alloc) =
        find_lcmc_req(&idle_msgs, local_issi, CmcePduTypeDl::DTxGranted).expect("Expected local floor grant after Brew idle");
    let granted = DTxGranted::from_bitbuf(&mut grant_sdu).expect("Failed to parse granted DTxGranted");
    assert_eq!(granted.call_identifier, call_id);
    assert_eq!(granted.transmission_grant, TransmissionGrant::Granted.into_raw() as u8);
    assert_eq!(grant_alloc, Some(UlDlAssignment::Ul));

    assert!(idle_msgs.iter().any(|msg| matches!(
        &msg.msg,
        SapMsgInner::CmceCallControl(CallControl::FloorGranted { source_issi, .. })
            if *source_issi == local_issi
    )));
    assert!(idle_msgs.iter().any(|msg| matches!(
        &msg.msg,
        SapMsgInner::CmceCallControl(CallControl::NetworkCircuitSimplexGranted {
            brew_uuid: msg_uuid,
            grant,
            permission: 0,
        }) if *msg_uuid == brew_uuid && *grant == TransmissionGrant::Granted.into_raw() as u8
    )));
}

#[test]
fn test_brew_originated_simplex_local_tx_ceased_notifies_brew_idle() {
    debug::setup_logging_verbose();

    let remote_issi = 2200699;
    let local_issi = 2200769;
    let (mut test, call_id, brew_uuid) = connected_brew_originated_simplex_call(remote_issi, local_issi);

    test.submit_message(SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Brew,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::CmceCallControl(CallControl::NetworkCircuitSimplexIdle {
            brew_uuid,
            grant: TransmissionGrant::NotGranted.into_raw() as u8,
            permission: 0,
        }),
    });
    test.run_stack(Some(1));
    test.dump_sinks();

    test.submit_message(build_u_tx_demand_msg(local_issi, call_id));
    test.run_stack(Some(1));
    test.dump_sinks();

    test.submit_message(build_u_tx_ceased_msg(local_issi, call_id));
    test.run_stack(Some(1));
    let ceased_msgs = test.dump_sinks();

    assert!(ceased_msgs.iter().any(|msg| matches!(
        &msg.msg,
        SapMsgInner::CmceCallControl(CallControl::NetworkCircuitSimplexIdle {
            brew_uuid: msg_uuid,
            grant,
            permission: 0,
        }) if *msg_uuid == brew_uuid && *grant == TransmissionGrant::NotGranted.into_raw() as u8
    )));
}

#[test]
fn test_brew_simplex_granted_resumes_remote_downlink_without_ul_timer() {
    debug::setup_logging_verbose();

    let remote_issi = 2200699;
    let local_issi = 2200769;
    let (mut test, call_id, brew_uuid) = connected_brew_originated_simplex_call(remote_issi, local_issi);

    test.submit_message(SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Brew,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::CmceCallControl(CallControl::NetworkCircuitSimplexGranted {
            brew_uuid,
            grant: TransmissionGrant::Granted.into_raw() as u8,
            permission: 0,
        }),
    });
    test.run_stack(Some(1));
    let granted_msgs = test.dump_sinks();

    let (mut grant_sdu, grant_alloc) =
        find_lcmc_req(&granted_msgs, local_issi, CmcePduTypeDl::DTxGranted).expect("Expected listener D-TX GRANTED");
    let granted = DTxGranted::from_bitbuf(&mut grant_sdu).expect("Failed to parse listener DTxGranted");
    assert_eq!(granted.call_identifier, call_id);
    assert_eq!(granted.transmission_grant, TransmissionGrant::GrantedToOtherUser.into_raw() as u8);
    assert_eq!(grant_alloc, Some(UlDlAssignment::Dl));

    assert!(granted_msgs.iter().any(|msg| matches!(
        &msg.msg,
        SapMsgInner::CmceCallControl(CallControl::RemoteFloorGranted { call_id: msg_call_id, ts: 2 })
            if *msg_call_id == call_id
    )));
    assert!(
        !granted_msgs.iter().any(|msg| matches!(
            &msg.msg,
            SapMsgInner::CmceCallControl(CallControl::FloorGranted { source_issi, .. })
                if *source_issi == remote_issi
        )),
        "Remote Brew floor must not use local FloorGranted because that arms UL inactivity"
    );
}

#[test]
fn test_simplex_individual_tx_ceased_without_queued_demand_releases_floor() {
    debug::setup_logging_verbose();

    let calling_issi = 1000001;
    let called_issi = 1000002;
    let (mut test, call_id, _) = connected_simplex_individual_call(calling_issi, called_issi);

    test.submit_message(build_u_tx_ceased_msg(calling_issi, call_id));
    test.run_stack(Some(1));
    let ceased_msgs = test.dump_sinks();

    let (mut ceased_sdu, ceased_alloc) =
        find_lcmc_req(&ceased_msgs, calling_issi, CmcePduTypeDl::DTxCeased).expect("Expected D-TX CEASED to former speaker");
    let ceased = DTxCeased::from_bitbuf(&mut ceased_sdu).expect("Failed to parse DTxCeased");
    assert_eq!(ceased.call_identifier, call_id);
    assert!(!ceased.transmission_request_permission);
    assert_eq!(ceased_alloc, Some(UlDlAssignment::Dl));

    let (mut listener_ceased_sdu, listener_ceased_alloc) =
        find_lcmc_req(&ceased_msgs, called_issi, CmcePduTypeDl::DTxCeased).expect("Expected D-TX CEASED to listener");
    let listener_ceased = DTxCeased::from_bitbuf(&mut listener_ceased_sdu).expect("Failed to parse listener DTxCeased");
    assert_eq!(listener_ceased.call_identifier, call_id);
    assert_eq!(listener_ceased_alloc, Some(UlDlAssignment::Dl));

    assert!(
        find_lcmc_req(&ceased_msgs, calling_issi, CmcePduTypeDl::DTxGranted).is_none(),
        "U-TX CEASED without a queued requester must not auto-grant the peer"
    );
    assert!(
        find_lcmc_req(&ceased_msgs, called_issi, CmcePduTypeDl::DTxGranted).is_none(),
        "U-TX CEASED without a queued requester must not send a listener grant"
    );

    assert!(ceased_msgs.iter().any(|msg| matches!(
        &msg.msg,
        SapMsgInner::CmceCallControl(CallControl::FloorReleased { call_id: released_call_id, .. })
            if *released_call_id == call_id
    )));
}

#[test]
fn test_simplex_individual_tx_demand_queues_and_hands_off_on_ceased() {
    debug::setup_logging_verbose();

    let calling_issi = 1000001;
    let called_issi = 1000002;
    let (mut test, call_id, _) = connected_simplex_individual_call(calling_issi, called_issi);

    test.submit_message(build_u_tx_demand_msg(called_issi, call_id));
    test.run_stack(Some(1));
    let demand_msgs = test.dump_sinks();

    let (mut queued_sdu, queued_alloc) =
        find_lcmc_req(&demand_msgs, called_issi, CmcePduTypeDl::DTxGranted).expect("Expected queued D-TX GRANTED");
    let queued = DTxGranted::from_bitbuf(&mut queued_sdu).expect("Failed to parse queued DTxGranted");
    assert_eq!(queued.transmission_grant, TransmissionGrant::RequestQueued.into_raw() as u8);
    assert_eq!(queued_alloc, Some(UlDlAssignment::Dl));
    assert_eq!(queued.transmitting_party_address_ssi, Some(calling_issi as u64));

    test.submit_message(build_u_tx_ceased_msg(calling_issi, call_id));
    test.run_stack(Some(1));
    let ceased_msgs = test.dump_sinks();

    let (mut grant_sdu, grant_alloc) =
        find_lcmc_req(&ceased_msgs, called_issi, CmcePduTypeDl::DTxGranted).expect("Expected granted D-TX GRANTED");
    let grant = DTxGranted::from_bitbuf(&mut grant_sdu).expect("Failed to parse granted DTxGranted");
    assert_eq!(grant.transmission_grant, TransmissionGrant::Granted.into_raw() as u8);
    assert_eq!(grant_alloc, Some(UlDlAssignment::Ul));
    assert_eq!(grant.transmitting_party_address_ssi, Some(called_issi as u64));

    let (mut listener_sdu, listener_alloc) =
        find_lcmc_req(&ceased_msgs, calling_issi, CmcePduTypeDl::DTxGranted).expect("Expected listener D-TX GRANTED");
    let listener = DTxGranted::from_bitbuf(&mut listener_sdu).expect("Failed to parse listener DTxGranted");
    assert_eq!(listener.transmission_grant, TransmissionGrant::GrantedToOtherUser.into_raw() as u8);
    assert_eq!(listener_alloc, Some(UlDlAssignment::Dl));
    assert_eq!(listener.transmitting_party_address_ssi, Some(called_issi as u64));

    assert!(ceased_msgs.iter().any(|msg| matches!(
        &msg.msg,
        SapMsgInner::CmceCallControl(CallControl::FloorGranted { source_issi, dest_gssi, .. })
            if *source_issi == called_issi && *dest_gssi == calling_issi
    )));
}

#[test]
fn test_simplex_individual_current_speaker_tx_demand_is_granted() {
    debug::setup_logging_verbose();

    let calling_issi = 1000001;
    let called_issi = 1000002;
    let (mut test, call_id, _) = connected_simplex_individual_call(calling_issi, called_issi);

    test.submit_message(build_u_tx_demand_msg(calling_issi, call_id));
    test.run_stack(Some(1));
    let demand_msgs = test.dump_sinks();

    let (mut grant_sdu, grant_alloc) =
        find_lcmc_req(&demand_msgs, calling_issi, CmcePduTypeDl::DTxGranted).expect("Expected granted D-TX GRANTED");
    let grant = DTxGranted::from_bitbuf(&mut grant_sdu).expect("Failed to parse granted DTxGranted");
    assert_eq!(grant.transmission_grant, TransmissionGrant::Granted.into_raw() as u8);
    assert_eq!(grant_alloc, Some(UlDlAssignment::Ul));
    assert_eq!(grant.transmitting_party_address_ssi, Some(calling_issi as u64));

    assert!(
        find_lcmc_req(&demand_msgs, called_issi, CmcePduTypeDl::DTxGranted).is_none(),
        "Current-speaker demand should not re-announce a listener grant"
    );
    assert!(
        !demand_msgs
            .iter()
            .any(|msg| matches!(&msg.msg, SapMsgInner::CmceCallControl(CallControl::FloorGranted { .. }))),
        "Current-speaker demand should not emit a duplicate floor grant"
    );
}

/// Test that late-entry D-SETUP re-sends are throttled when the previous
/// D-SETUP's TxReceipt is still in Pending state (UMAC hasn't transmitted it yet),
/// and that they resume once the receipt reaches a final state.
#[test]
fn test_dsetup_late_entry_throttle() {
    debug::setup_logging_verbose();

    // Start at timeslot 1 so circuit creation aligns cleanly with tick_start checks
    let dltime = TdmaTime { h: 0, m: 1, f: 1, t: 1 };
    let mut test = ComponentTest::new(StackMode::Bs, Some(dltime));

    let components = vec![TetraEntity::Cmce];
    let sinks = vec![TetraEntity::Mle, TetraEntity::Umac, TetraEntity::Brew];
    test.populate_entities(components, sinks);

    register_subscriber(&mut test, TEST_ISSI, TEST_GSSI);

    // Send U-SETUP to start a group call
    let u_setup_msg = build_u_setup_msg(TEST_ISSI, TEST_GSSI);
    test.submit_message(u_setup_msg);
    test.run_stack(Some(1));

    // Collect initial output — should contain D-SETUP (initial send with no tracked receipt)
    let initial_msgs = test.dump_sinks();
    let initial_setups = count_d_setups(&initial_msgs);
    assert!(initial_setups > 0, "Expected initial D-SETUP after U-SETUP");

    // Run a few more ticks to get through the D_SETUP_REPEATS backup window.
    // The backup send goes through (receipt is None) and creates a tracked receipt.
    test.run_stack(Some(8));
    let mut backup_msgs = test.dump_sinks();
    let backup_reporters = extract_d_setup_reporters(&mut backup_msgs);

    // We should have at least one reporter from the backup send
    assert!(
        !backup_reporters.is_empty(),
        "Expected backup D-SETUP with tx_reporter in initial window"
    );
    let last_reporter = &backup_reporters[backup_reporters.len() - 1];
    assert_eq!(last_reporter.get_state(), TxState::Pending);

    // Run for 2 full late-entry intervals (720 ticks). With the receipt still Pending,
    // ALL late-entry D-SETUPs should be suppressed.
    test.run_stack(Some(720));
    let throttled_msgs = test.dump_sinks();
    let throttled_count = count_d_setups(&throttled_msgs);
    assert_eq!(
        throttled_count, 0,
        "Late-entry D-SETUPs should be suppressed while receipt is Pending"
    );

    // Now mark the previous D-SETUP as transmitted (simulating UMAC sending it over the air)
    last_reporter.mark_transmitted();

    // Run for 2 more late-entry intervals. Now D-SETUPs should go through.
    test.run_stack(Some(720));
    let mut unthrottled_msgs = test.dump_sinks();
    let unthrottled_count = count_d_setups(&unthrottled_msgs);
    assert!(
        unthrottled_count > 0,
        "Late-entry D-SETUPs should resume once receipt reaches final state"
    );

    // Each re-send that went through should have created a fresh reporter
    let new_reporters = extract_d_setup_reporters(&mut unthrottled_msgs);
    assert_eq!(
        new_reporters.len(),
        unthrottled_count,
        "Each re-sent D-SETUP should carry a fresh tx_reporter"
    );
}
