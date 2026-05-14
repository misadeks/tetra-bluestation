mod common;

use tetra_config::bluestation::StackMode;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, Sap, SsiType, TdmaTime, TetraAddress};
use tetra_pdus::cmce::pdus::d_connect_acknowledge::DConnectAcknowledge;
use tetra_pdus::cmce::pdus::d_setup::DSetup;
use tetra_pdus::cmce::pdus::u_alert::UAlert;
use tetra_pdus::cmce::pdus::u_connect::UConnect;
use tetra_saps::control::brew::{BrewSubscriberAction, MmSubscriberUpdate};
use tetra_saps::control::call_control::{CallControl, CircuitDlMediaSource, NetworkCircuitCall};
use tetra_saps::lcmc::LcmcMleUnitdataInd;
use tetra_saps::sapmsg::{SapMsg, SapMsgInner};
use uuid::Uuid;

use crate::common::ComponentTest;

fn register_subscriber(test: &mut ComponentTest, issi: u32) {
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
    let _ = test.dump_sinks();
}

fn build_setup_request(brew_uuid: Uuid, source_issi: u32, destination_issi: u32, number: &str) -> SapMsg {
    SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Brew,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::CmceCallControl(CallControl::NetworkCircuitSetupRequest {
            brew_uuid,
            call: NetworkCircuitCall {
                source_issi,
                destination: destination_issi,
                number: number.to_string(),
                priority: 0,
                service: 0,
                mode: 0,
                duplex: 0,
                method: 0,
                communication: 0,
                grant: 0,
                permission: 0,
                timeout: 7,
                ownership: 0,
                queued: 0,
            },
        }),
    }
}

fn build_u_alert(call_id: u16, destination_issi: u32) -> SapMsg {
    let pdu = UAlert {
        call_identifier: call_id,
        reserved: true,
        simplex_duplex_selection: false,
        basic_service_information: None,
        facility: None,
        proprietary: None,
    };
    let mut sdu = BitBuffer::new_autoexpand(32);
    pdu.to_bitbuf(&mut sdu).expect("Failed to serialize UAlert");
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
            received_tetra_address: TetraAddress::new(destination_issi, SsiType::Issi),
            chan_change_resp_req: false,
            chan_change_handle: None,
        }),
    }
}

fn build_u_connect(call_id: u16, destination_issi: u32) -> SapMsg {
    let pdu = UConnect {
        call_identifier: call_id,
        hook_method_selection: false,
        simplex_duplex_selection: false,
        basic_service_information: None,
        facility: None,
        proprietary: None,
    };
    let mut sdu = BitBuffer::new_autoexpand(32);
    pdu.to_bitbuf(&mut sdu).expect("Failed to serialize UConnect");
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
            received_tetra_address: TetraAddress::new(destination_issi, SsiType::Issi),
            chan_change_resp_req: false,
            chan_change_handle: None,
        }),
    }
}

fn build_connect_confirm(brew_uuid: Uuid) -> SapMsg {
    SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Brew,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::CmceCallControl(CallControl::NetworkCircuitConnectConfirm {
            brew_uuid,
            grant: 1,
            permission: 0,
        }),
    }
}

#[test]
fn test_incoming_brew_setup_alert_connect_flow() {
    let dltime = TdmaTime { h: 0, m: 1, f: 1, t: 1 };
    let source_issi = 2200760;
    let destination_issi = 2200699;
    let source_extension = 112;
    let source_extension_str = source_extension.to_string();
    let brew_uuid = Uuid::new_v4();

    let mut test = ComponentTest::new(StackMode::Bs, Some(dltime));
    test.populate_entities(
        vec![TetraEntity::Cmce],
        vec![TetraEntity::Mle, TetraEntity::Umac, TetraEntity::Brew],
    );
    register_subscriber(&mut test, destination_issi);

    test.submit_message(build_setup_request(brew_uuid, source_issi, destination_issi, &source_extension_str));
    test.run_stack(Some(1));

    let setup_msgs = test.dump_sinks();
    let mut saw_setup_accept = false;
    let mut call_id: Option<u16> = None;

    for msg in setup_msgs {
        if let SapMsgInner::CmceCallControl(CallControl::NetworkCircuitSetupAccept { brew_uuid: msg_uuid }) = msg.msg {
            assert_eq!(msg_uuid, brew_uuid);
            saw_setup_accept = true;
            continue;
        }

        if let SapMsgInner::LcmcMleUnitdataReq(mut prim) = msg.msg {
            if prim.main_address.ssi != destination_issi {
                continue;
            }
            if let Ok(d_setup) = DSetup::from_bitbuf(&mut prim.sdu) {
                assert_eq!(d_setup.calling_party_address_ssi, Some(source_issi));
                assert_eq!(d_setup.calling_party_extension, Some(source_extension));
                call_id = Some(d_setup.call_identifier);
            }
        }
    }

    assert!(saw_setup_accept, "Expected NetworkCircuitSetupAccept to Brew");
    let call_id = call_id.expect("Expected D-SETUP to local called ISSI");

    test.submit_message(build_u_alert(call_id, destination_issi));
    test.run_stack(Some(1));

    let alert_msgs = test.dump_sinks();
    let saw_alert = alert_msgs
        .into_iter()
        .any(|msg| matches!(msg.msg, SapMsgInner::CmceCallControl(CallControl::NetworkCircuitAlert { brew_uuid: msg_uuid }) if msg_uuid == brew_uuid));
    assert!(saw_alert, "Expected NetworkCircuitAlert to Brew");

    test.submit_message(build_u_connect(call_id, destination_issi));
    test.run_stack(Some(1));

    let connect_req_msgs = test.dump_sinks();
    let mut saw_connect_request = false;

    for msg in connect_req_msgs {
        if let SapMsgInner::CmceCallControl(CallControl::NetworkCircuitConnectRequest { brew_uuid: msg_uuid, call }) = msg.msg {
            if msg_uuid == brew_uuid {
                saw_connect_request = true;
                assert_eq!(call.destination, destination_issi);
            }
        }
    }

    assert!(saw_connect_request, "Expected NetworkCircuitConnectRequest to Brew");

    test.submit_message(build_connect_confirm(brew_uuid));
    test.run_stack(Some(1));

    let connect_msgs = test.dump_sinks();
    let mut saw_media_ready = false;
    let mut saw_connect_ack = false;
    let mut saw_umac_open_swmi = false;

    for msg in connect_msgs {
        match msg.msg {
            SapMsgInner::CmceCallControl(CallControl::NetworkCircuitMediaReady {
                brew_uuid: msg_uuid,
                call_id: msg_call_id,
                ..
            }) => {
                if msg_uuid == brew_uuid && msg_call_id == call_id {
                    saw_media_ready = true;
                }
            }
            SapMsgInner::CmceCallControl(CallControl::Open(circuit)) => {
                if circuit.dl_media_source == CircuitDlMediaSource::SwMI {
                    saw_umac_open_swmi = true;
                }
            }
            SapMsgInner::LcmcMleUnitdataReq(mut prim) => {
                if prim.main_address.ssi != destination_issi {
                    continue;
                }
                if let Ok(pdu) = DConnectAcknowledge::from_bitbuf(&mut prim.sdu) {
                    if pdu.call_identifier == call_id {
                        saw_connect_ack = true;
                    }
                }
            }
            _ => {}
        }
    }

    assert!(saw_media_ready, "Expected NetworkCircuitMediaReady to Brew");
    assert!(saw_connect_ack, "Expected D-CONNECT ACKNOWLEDGE to local called ISSI");
    assert!(saw_umac_open_swmi, "Expected UMAC Open circuit with SwMI downlink source");
}
