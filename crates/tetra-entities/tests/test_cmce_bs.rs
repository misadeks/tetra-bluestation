mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use tetra_config::bluestation::{SharedConfig, StackMode};
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, Sap, SsiType, TdmaTime, TetraAddress, TxState, debug};
use tetra_entities::MessageQueue;
use tetra_entities::cmce::subentities::cc_bs::CcBsSubentity;
use tetra_pdus::cmce::enums::cmce_pdu_type_dl::CmcePduTypeDl;
use tetra_pdus::cmce::enums::party_type_identifier::PartyTypeIdentifier;
use tetra_pdus::cmce::fields::basic_service_information::BasicServiceInformation;
use tetra_pdus::cmce::pdus::d_setup::DSetup;
use tetra_pdus::cmce::pdus::d_tx_ceased::DTxCeased;
use tetra_pdus::cmce::pdus::u_setup::USetup;
use tetra_saps::control::brew::{BrewSubscriberAction, MmSubscriberUpdate};
use tetra_saps::control::call_control::CallControl;
use tetra_saps::control::enums::circuit_mode_type::CircuitModeType;
use tetra_saps::control::enums::communication_type::CommunicationType;
use tetra_saps::lcmc::LcmcMleUnitdataInd;
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
                    if prim.chan_alloc.as_ref().is_some_and(|ca| ca.usage.is_some()))
        })
        .count()
}

fn extract_first_d_setup(msgs: &mut [SapMsg]) -> Option<(u16, u8)> {
    for msg in msgs.iter_mut() {
        let SapMsgInner::LcmcMleUnitdataReq(prim) = &mut msg.msg else {
            continue;
        };
        if msg.dest != TetraEntity::Mle || prim.sdu.peek_bits(5) != Some(CmcePduTypeDl::DSetup.into_raw()) {
            continue;
        }

        let d_setup = DSetup::from_bitbuf(&mut prim.sdu).ok()?;
        let ts = prim
            .chan_alloc
            .as_ref()?
            .timeslots
            .iter()
            .position(|assigned| *assigned)
            .map(|idx| idx as u8 + 1)?;

        return Some((d_setup.call_identifier, ts));
    }

    None
}

fn count_floor_released(msgs: &[SapMsg], dest: TetraEntity, call_id: u16, ts: u8) -> usize {
    msgs.iter()
        .filter(|msg| {
            msg.dest == dest
                && matches!(
                    &msg.msg,
                    SapMsgInner::CmceCallControl(CallControl::FloorReleased {
                        call_id: msg_call_id,
                        ts: msg_ts,
                    }) if *msg_call_id == call_id && *msg_ts == ts
                )
        })
        .count()
}

fn has_d_tx_ceased(msgs: &mut [SapMsg], call_id: u16) -> bool {
    msgs.iter_mut().any(|msg| {
        let SapMsgInner::LcmcMleUnitdataReq(prim) = &mut msg.msg else {
            return false;
        };
        if msg.dest != TetraEntity::Mle || prim.sdu.peek_bits(5) != Some(CmcePduTypeDl::DTxCeased.into_raw()) {
            return false;
        }

        match DTxCeased::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => pdu.call_identifier == call_id,
            Err(_) => false,
        }
    })
}

/// Test that late-entry D-SETUP re-sends are throttled when the previous
/// D-SETUP's TxReporter is still in Pending state (UMAC hasn't transmitted it yet),
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

#[test]
fn test_ul_inactivity_timeout_releases_group_floor_once() {
    debug::setup_logging_verbose();

    let dltime = TdmaTime { h: 0, m: 1, f: 1, t: 1 };
    let mut test = ComponentTest::new(StackMode::Bs, Some(dltime));

    let components = vec![TetraEntity::Cmce];
    let sinks = vec![TetraEntity::Mle, TetraEntity::Umac, TetraEntity::Brew];
    test.populate_entities(components, sinks);

    register_subscriber(&mut test, TEST_ISSI, TEST_GSSI);

    let u_setup_msg = build_u_setup_msg(TEST_ISSI, TEST_GSSI);
    test.submit_message(u_setup_msg);
    test.run_stack(Some(1));

    let mut initial_msgs = test.dump_sinks();
    let (call_id, ts) = extract_first_d_setup(&mut initial_msgs).expect("expected D-SETUP after U-SETUP");

    test.submit_message(SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Umac,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::CmceCallControl(CallControl::UlInactivityTimeout { ts }),
    });
    test.run_stack(Some(1));

    let mut timeout_msgs = test.dump_sinks();
    assert!(
        has_d_tx_ceased(&mut timeout_msgs, call_id),
        "Expected D-TX CEASED for inactive uplink call"
    );
    assert_eq!(
        count_floor_released(&timeout_msgs, TetraEntity::Umac, call_id, ts),
        1,
        "Expected exactly one FloorReleased notification to UMAC"
    );

    test.submit_message(SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Umac,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::CmceCallControl(CallControl::UlInactivityTimeout { ts }),
    });
    test.run_stack(Some(1));

    let repeated_timeout_msgs = test.dump_sinks();
    assert_eq!(
        count_floor_released(&repeated_timeout_msgs, TetraEntity::Umac, call_id, ts),
        0,
        "Repeated inactivity timeout while in hangtime must not re-release the floor"
    );
}

#[test]
fn test_cc_ingress_drops_wrong_message_shape_without_panicking() {
    let shared = SharedConfig::from_parts(ComponentTest::get_default_test_config(StackMode::Bs), None);
    let mut cc = CcBsSubentity::new(shared);
    let mut queue = MessageQueue::new();

    let wrong_message = SapMsg {
        sap: Sap::Control,
        src: TetraEntity::Umac,
        dest: TetraEntity::Cmce,
        msg: SapMsgInner::CmceCallControl(CallControl::UlInactivityTimeout { ts: 1 }),
    };

    let result = catch_unwind(AssertUnwindSafe(|| cc.route_xx_deliver(&mut queue, wrong_message)));
    assert!(result.is_ok(), "CC radio ingress should drop wrong message shapes, not panic");
    assert!(
        queue.pop_front().is_none(),
        "Dropped ingress message should not emit follow-up messages"
    );
}
