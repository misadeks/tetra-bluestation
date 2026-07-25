//! Telemetry codec — bitcode-based binary serialization of [`TelemetryEvent`]s.

use crate::{net_telemetry::events::TelemetryEvent, network::transports::NetworkError};

// ---------------------------------------------------------------------------
// Codecs
// ---------------------------------------------------------------------------

/// Codec for telemetry events using bitcode for serialization.
#[derive(Default)]
pub struct TelemetryCodecBitcode;

impl TelemetryCodecBitcode {
    /// Encode a [`TelemetryEvent`] to bitcode bytes.
    pub fn encode(&self, event: &TelemetryEvent) -> Vec<u8> {
        bitcode::encode(event)
    }

    /// Decode bitcode bytes into a [`TelemetryEvent`].
    pub fn decode(&self, payload: &[u8]) -> Result<TelemetryEvent, NetworkError> {
        bitcode::decode(payload).map_err(|e| NetworkError::SerializationError(format!("telemetry decode: {}", e)))
    }
}

/// Codec for telemetry events using JSON for serialization.
#[derive(Default)]
pub struct TelemetryCodecJson;

impl TelemetryCodecJson {
    /// Encode a [`TelemetryEvent`] to JSON bytes.
    pub fn encode(&self, event: &TelemetryEvent) -> Vec<u8> {
        serde_json::to_vec(event).unwrap_or_default()
    }

    /// Decode JSON bytes into a [`TelemetryEvent`].
    pub fn decode(&self, payload: &[u8]) -> Result<TelemetryEvent, NetworkError> {
        serde_json::from_slice(payload).map_err(|e| NetworkError::SerializationError(format!("telemetry decode: {}", e)))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_bitcode_registration() {
        let codec = TelemetryCodecBitcode;
        let event = TelemetryEvent::MsRegistration { issi: 1234 };
        let bytes = codec.encode(&event);
        let decoded = codec.decode(&bytes).unwrap();
        let TelemetryEvent::MsRegistration { issi } = decoded else {
            panic!("expected Registration");
        };
        assert_eq!(issi, 1234);
    }

    #[test]
    fn test_roundtrip_json_registration() {
        let codec = TelemetryCodecJson;
        let event = TelemetryEvent::MsRegistration { issi: 1234 };
        let bytes = codec.encode(&event);
        let decoded = codec.decode(&bytes).unwrap();
        let TelemetryEvent::MsRegistration { issi } = decoded else {
            panic!("expected Registration");
        };
        assert_eq!(issi, 1234);
    }

    #[test]
    fn test_decode_invalid_bytes() {
        let codec = TelemetryCodecBitcode;
        assert!(codec.decode(&[0xFF, 0x00]).is_err());
    }

    #[test]
    fn test_roundtrip_ms_speech_frame() {
        // U-plane downlink speech offload: a 274-bit TCH/S type-1 block carried
        // one-bit-per-byte, with sequence, BFI and talker, must survive both
        // codecs byte-for-byte.
        let data: Vec<u8> = (0..274u16).map(|i| (i % 2) as u8).collect();
        let event = TelemetryEvent::MsSpeechFrame {
            call_identifier: 68,
            timeslot: 2,
            sequence: 42,
            transmitting_party_ssi: Some(2200699),
            frame_bits: 274,
            bad_frame: true,
            data: data.clone(),
        };

        for decoded in [
            TelemetryCodecBitcode.decode(&TelemetryCodecBitcode.encode(&event)).unwrap(),
            TelemetryCodecJson.decode(&TelemetryCodecJson.encode(&event)).unwrap(),
        ] {
            let TelemetryEvent::MsSpeechFrame {
                call_identifier,
                timeslot,
                sequence,
                transmitting_party_ssi,
                frame_bits,
                bad_frame,
                data: got,
            } = decoded
            else {
                panic!("expected MsSpeechFrame");
            };
            assert_eq!(call_identifier, 68);
            assert_eq!(timeslot, 2);
            assert_eq!(sequence, 42);
            assert_eq!(transmitting_party_ssi, Some(2200699));
            assert_eq!(frame_bits, 274);
            assert!(bad_frame);
            assert_eq!(got, data);
        }
    }

    #[test]
    fn test_roundtrip_ms_scan_events() {
        // Manual survey (Plane B): a found-cell result and the completion must
        // survive both codecs.
        let result = TelemetryEvent::MsScanResult {
            carrier_hz: 439_825_000,
            mcc: 901,
            mnc: 9999,
            location_area: Some(1),
            colour_code: None,
            rssi_dbfs: Some(-55.0),
            registration_required: Some(true),
            late_entry_supported: true,
        };
        for decoded in [
            TelemetryCodecBitcode.decode(&TelemetryCodecBitcode.encode(&result)).unwrap(),
            TelemetryCodecJson.decode(&TelemetryCodecJson.encode(&result)).unwrap(),
        ] {
            let TelemetryEvent::MsScanResult {
                carrier_hz,
                mcc,
                mnc,
                location_area,
                colour_code,
                rssi_dbfs,
                registration_required,
                late_entry_supported,
            } = decoded
            else {
                panic!("expected MsScanResult");
            };
            assert_eq!(carrier_hz, 439_825_000);
            assert_eq!(mcc, 901);
            assert_eq!(mnc, 9999);
            assert_eq!(location_area, Some(1));
            assert_eq!(colour_code, None);
            assert_eq!(rssi_dbfs, Some(-55.0));
            assert_eq!(registration_required, Some(true));
            assert!(late_entry_supported);
        }

        let complete = TelemetryEvent::MsScanComplete { found: 3, scanned: 8 };
        for decoded in [
            TelemetryCodecBitcode.decode(&TelemetryCodecBitcode.encode(&complete)).unwrap(),
            TelemetryCodecJson.decode(&TelemetryCodecJson.encode(&complete)).unwrap(),
        ] {
            let TelemetryEvent::MsScanComplete { found, scanned } = decoded else {
                panic!("expected MsScanComplete");
            };
            assert_eq!(found, 3);
            assert_eq!(scanned, 8);
        }
    }
    // TNMM-SAP (cl. 15.3) telemetry roundtrips. TelemetryEvent itself is not
    // PartialEq, so we compare the (PartialEq) tnmm payloads after decoding.
    // -----------------------------------------------------------------------

    use crate::tnmm::{
        CellType, ClassOfUsage, DisableStatus, GroupIdentity, GroupIdentityAttachDetachTypeIdentifier, GroupIdentityLifetime,
        RegistrationRejectCause, RegistrationStatus, ServiceStatus, TnmmAttachDetachGroupIdentityIndication, TnmmRegistrationIndication,
        TnmmServiceIndication,
    };

    fn sample_registration_indication() -> TnmmRegistrationIndication {
        TnmmRegistrationIndication {
            registration_status: RegistrationStatus::Success,
            registration_reject_cause: None,
            cell_type_where_registered: CellType::CaCell,
            la_where_registered: 42,
            mcc_where_registered: 901,
            mnc_where_registered: 1,
            swmis_required_cell_types: None,
            energy_economy_mode: None,
            energy_economy_mode_status: None,
            group_identities: Some(vec![GroupIdentity {
                gtsi: 0x0000_0DEAD_BEEF & 0xFFFF_FFFF_FFFF,
                group_identity_attach_detach_type_identifier: GroupIdentityAttachDetachTypeIdentifier::Attachment,
                group_identity_lifetime: Some(GroupIdentityLifetime::AttachmentNeededForNextItsiAttach),
                class_of_usage: Some(ClassOfUsage::ClassOfUsage4),
                group_identity_detachment_reason: None,
            }]),
            group_identity_attach_detach_mode: None,
        }
    }

    #[test]
    fn test_roundtrip_json_tnmm_registration_indication() {
        let codec = TelemetryCodecJson;
        let inner = sample_registration_indication();
        let event = TelemetryEvent::TnmmRegistrationIndication(Box::new(inner.clone()));
        let bytes = codec.encode(&event);
        let decoded = codec.decode(&bytes).unwrap();
        let TelemetryEvent::TnmmRegistrationIndication(got) = decoded else {
            panic!("expected TnmmRegistrationIndication");
        };
        assert_eq!(*got, inner);
    }

    #[test]
    fn test_roundtrip_bitcode_tnmm_registration_indication() {
        let codec = TelemetryCodecBitcode;
        let inner = sample_registration_indication();
        let event = TelemetryEvent::TnmmRegistrationIndication(Box::new(inner.clone()));
        let bytes = codec.encode(&event);
        let decoded = codec.decode(&bytes).unwrap();
        let TelemetryEvent::TnmmRegistrationIndication(got) = decoded else {
            panic!("expected TnmmRegistrationIndication");
        };
        assert_eq!(*got, inner);
    }

    #[test]
    fn test_roundtrip_json_tnmm_service_indication() {
        let codec = TelemetryCodecJson;
        let inner = TnmmServiceIndication {
            service_status: ServiceStatus::OutOfService,
            disable_status: DisableStatus::Enabled,
        };
        let event = TelemetryEvent::TnmmServiceIndication(inner);
        let bytes = codec.encode(&event);
        let decoded = codec.decode(&bytes).unwrap();
        let TelemetryEvent::TnmmServiceIndication(got) = decoded else {
            panic!("expected TnmmServiceIndication");
        };
        assert_eq!(got, inner);
    }

    #[test]
    fn test_roundtrip_bitcode_tnmm_group_identity_indication() {
        let codec = TelemetryCodecBitcode;
        let inner = TnmmAttachDetachGroupIdentityIndication {
            group_identities: vec![GroupIdentity {
                gtsi: 0x00_0000_0001,
                group_identity_attach_detach_type_identifier: GroupIdentityAttachDetachTypeIdentifier::Attachment,
                group_identity_lifetime: Some(GroupIdentityLifetime::AttachmentNeededForNextItsiAttach),
                class_of_usage: Some(ClassOfUsage::ClassOfUsage4),
                group_identity_detachment_reason: None,
            }],
        };
        let event = TelemetryEvent::TnmmAttachDetachGroupIdentityIndication(inner.clone());
        let bytes = codec.encode(&event);
        let decoded = codec.decode(&bytes).unwrap();
        let TelemetryEvent::TnmmAttachDetachGroupIdentityIndication(got) = decoded else {
            panic!("expected TnmmAttachDetachGroupIdentityIndication");
        };
        assert_eq!(got, inner);
    }

    #[test]
    fn test_roundtrip_json_registration_reject_cause_none_mapping() {
        // A "failure" indication carrying a mapped reject cause survives JSON.
        let codec = TelemetryCodecJson;
        let mut inner = sample_registration_indication();
        inner.registration_status = RegistrationStatus::Failure;
        inner.registration_reject_cause = Some(RegistrationRejectCause::LaNotAllowed);
        inner.group_identities = None;
        let event = TelemetryEvent::TnmmRegistrationIndication(Box::new(inner.clone()));
        let bytes = codec.encode(&event);
        let decoded = codec.decode(&bytes).unwrap();
        let TelemetryEvent::TnmmRegistrationIndication(got) = decoded else {
            panic!("expected TnmmRegistrationIndication");
        };
        assert_eq!(*got, inner);
    }

    fn sample_tncc_basic() -> tetra_saps::tncc::TnccBasicServiceInformation {
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

    fn sample_tncc_events() -> Vec<TelemetryEvent> {
        use tetra_saps::tncc as t;
        vec![
            TelemetryEvent::TnccAlertIndication {
                call_identifier: 7,
                indication: t::TnccAlertIndication {
                    basic_service_information_offered: Some(sample_tncc_basic()),
                    call_queued: Some(t::CallQueued::CallIsNotQueued),
                    call_time_out_set_up_phase: t::CallTimeoutSetupPhase::Value4,
                    notification_indicator: Some(3),
                    simplex_duplex: t::SimplexDuplexSelection::SimplexOperation,
                },
            },
            TelemetryEvent::TnccCompleteIndication {
                call_identifier: 7,
                indication: t::TnccCompleteIndication {
                    call_time_out: t::CallTimeout::Value1,
                    notification_indicator: Some(4),
                    transmission_grant: t::TransmissionGrant::TransmissionGranted,
                    transmission_request_permission: t::TransmissionRequestPermission::AllowedToRequestForTransmission,
                    transmission_status: t::TransmissionStatus::TransmissionGranted,
                },
            },
            TelemetryEvent::TnccCompleteConfirm {
                call_identifier: 7,
                confirm: t::TnccCompleteConfirm {
                    call_time_out: t::CallTimeout::Value1,
                    notification_indicator: None,
                    transmission_grant: t::TransmissionGrant::TransmissionGranted,
                    transmission_request_permission: t::TransmissionRequestPermission::AllowedToRequestForTransmission,
                    transmission_status: t::TransmissionStatus::TransmissionGranted,
                },
            },
            TelemetryEvent::TnccNotifyIndication {
                call_identifier: 7,
                indication: t::TnccNotifyIndication {
                    call_status: Some(t::CallStatus::CallContinue),
                    call_time_out_in_set_up_phase: None,
                    call_time_out: Some(t::CallTimeout::Value2),
                    call_ownership: Some(t::CallOwnership::ACallOwner),
                    notification_indicator: Some(5),
                    poll_response_percentage: None,
                    poll_response_number: None,
                    poll_response_addresses: None,
                    poll_request: Some(false),
                },
            },
            TelemetryEvent::TnccProceedIndication {
                call_identifier: 7,
                indication: t::TnccProceedIndication {
                    basic_service_information_offered: Some(sample_tncc_basic()),
                    call_status: Some(t::CallStatus::CallIsProgressing),
                    hook_method: Some(t::HookMethodSelection::NoHookSignallingDirectThroughConnect),
                    notification_indicator: Some(1),
                    simplex_duplex: Some(t::SimplexDuplexSelection::SimplexOperation),
                },
            },
            TelemetryEvent::TnccReleaseIndication {
                call_identifier: 7,
                indication: t::TnccReleaseIndication {
                    disconnect_cause: t::DisconnectCause::UserRequestedDisconnection,
                    notification_indicator: Some(1),
                },
            },
            TelemetryEvent::TnccReleaseConfirm {
                call_identifier: 7,
                confirm: t::TnccReleaseConfirm {
                    disconnect_cause: t::DisconnectCause::UserRequestedDisconnection,
                    disconnect_status: t::DisconnectStatus::DisconnectionSuccessful,
                    notification_indicator: None,
                },
            },
            TelemetryEvent::TnccSetupIndication {
                call_identifier: 7,
                indication: Box::new(t::TnccSetupIndication {
                    basic_service_information: sample_tncc_basic(),
                    call_priority: t::CallPriority::PriorityNotDefined,
                    call_time_out: t::CallTimeout::Value1,
                    called_party_ssi: 91,
                    called_party_extension: None,
                    calling_party_ssi: Some(1001),
                    calling_party_extension: None,
                    external_subscriber_number_calling: None,
                    clir_control: None,
                    hook_method_selection: t::HookMethodSelection::NoHookSignallingDirectThroughConnect,
                    notification_indicator: Some(2),
                    simplex_duplex_selection: t::SimplexDuplexSelection::SimplexOperation,
                    transmission_grant: t::TransmissionGrant::TransmissionGrantedToAnotherUser,
                    transmission_request_permission: t::TransmissionRequestPermission::AllowedToRequestForTransmission,
                }),
            },
            TelemetryEvent::TnccSetupConfirm {
                call_identifier: 7,
                confirm: Box::new(t::TnccSetupConfirm {
                    basic_service_information: sample_tncc_basic(),
                    call_priority: Some(t::CallPriority::LowestPriority),
                    call_ownership: t::CallOwnership::ACallOwner,
                    call_amalgamation: t::CallAmalgamation::CallNotAmalgamated,
                    call_time_out: t::CallTimeout::Value1,
                    hook_method_selection: t::HookMethodSelection::NoHookSignallingDirectThroughConnect,
                    notification_indicator: None,
                    simplex_duplex_selection: t::SimplexDuplexSelection::SimplexOperation,
                    transmission_grant: t::TransmissionGrant::TransmissionGranted,
                    transmission_request_permission: t::TransmissionRequestPermission::AllowedToRequestForTransmission,
                }),
            },
            TelemetryEvent::TnccTxIndication {
                call_identifier: 7,
                indication: t::TnccTxIndication {
                    encryption_flag: t::EncryptionFlag::ClearEndToEndTransmission,
                    notification_indicator: Some(1),
                    transmitting_party_ssi: Some(1001),
                    transmitting_party_extension: None,
                    external_subscriber_number: None,
                    transmit_request_permission: t::TransmissionRequestPermission::AllowedToRequestForTransmission,
                    transmission_status: t::TransmissionStatus::TransmissionGranted,
                },
            },
            TelemetryEvent::TnccTxConfirm {
                call_identifier: 7,
                confirm: t::TnccTxConfirm {
                    encryption_flag: t::EncryptionFlag::ClearEndToEndTransmission,
                    transmit_request_permission: t::TransmissionRequestPermission::AllowedToRequestForTransmission,
                    transmission_status: t::TransmissionStatus::TransmissionGranted,
                },
            },
        ]
    }

    #[test]
    fn test_roundtrip_json_and_bitcode_all_tncc_events() {
        let json = TelemetryCodecJson;
        let bitcode = TelemetryCodecBitcode;
        for event in sample_tncc_events() {
            let json_wire = json.encode(&event);
            let json_decoded = json.decode(&json_wire).unwrap();
            assert_eq!(
                serde_json::to_string(&json_decoded).unwrap(),
                serde_json::to_string(&event).unwrap()
            );

            let bitcode_wire = bitcode.encode(&event);
            let bitcode_decoded = bitcode.decode(&bitcode_wire).unwrap();
            assert_eq!(
                serde_json::to_string(&bitcode_decoded).unwrap(),
                serde_json::to_string(&event).unwrap()
            );
        }
    }

    #[test]
    fn test_json_schema_freeze_golden_wire_format_tncc() {
        let codec = TelemetryCodecJson;
        let enc = |e: &TelemetryEvent| String::from_utf8(codec.encode(e)).unwrap();
        let events = sample_tncc_events();
        let expected = vec![
            r#"{"TnccAlertIndication":{"call_identifier":7,"indication":{"basic_service_information_offered":{"circuit_mode_service":"SpeechService","communication_type":"PointToMultipoint","data_service":null,"data_call_capacity":null,"encryption_flag":"ClearEndToEndTransmission","speech_service":"TetraEncodedOneTimeslotSpeech"},"call_queued":"CallIsNotQueued","call_time_out_set_up_phase":"Value4","notification_indicator":3,"simplex_duplex":"SimplexOperation"}}}"#,
            r#"{"TnccCompleteIndication":{"call_identifier":7,"indication":{"call_time_out":"Value1","notification_indicator":4,"transmission_grant":"TransmissionGranted","transmission_request_permission":"AllowedToRequestForTransmission","transmission_status":"TransmissionGranted"}}}"#,
            r#"{"TnccCompleteConfirm":{"call_identifier":7,"confirm":{"call_time_out":"Value1","notification_indicator":null,"transmission_grant":"TransmissionGranted","transmission_request_permission":"AllowedToRequestForTransmission","transmission_status":"TransmissionGranted"}}}"#,
            r#"{"TnccNotifyIndication":{"call_identifier":7,"indication":{"call_status":"CallContinue","call_time_out_in_set_up_phase":null,"call_time_out":"Value2","call_ownership":"ACallOwner","notification_indicator":5,"poll_response_percentage":null,"poll_response_number":null,"poll_response_addresses":null,"poll_request":false}}}"#,
            r#"{"TnccProceedIndication":{"call_identifier":7,"indication":{"basic_service_information_offered":{"circuit_mode_service":"SpeechService","communication_type":"PointToMultipoint","data_service":null,"data_call_capacity":null,"encryption_flag":"ClearEndToEndTransmission","speech_service":"TetraEncodedOneTimeslotSpeech"},"call_status":"CallIsProgressing","hook_method":"NoHookSignallingDirectThroughConnect","notification_indicator":1,"simplex_duplex":"SimplexOperation"}}}"#,
            r#"{"TnccReleaseIndication":{"call_identifier":7,"indication":{"disconnect_cause":"UserRequestedDisconnection","notification_indicator":1}}}"#,
            r#"{"TnccReleaseConfirm":{"call_identifier":7,"confirm":{"disconnect_cause":"UserRequestedDisconnection","disconnect_status":"DisconnectionSuccessful","notification_indicator":null}}}"#,
            r#"{"TnccSetupIndication":{"call_identifier":7,"indication":{"basic_service_information":{"circuit_mode_service":"SpeechService","communication_type":"PointToMultipoint","data_service":null,"data_call_capacity":null,"encryption_flag":"ClearEndToEndTransmission","speech_service":"TetraEncodedOneTimeslotSpeech"},"call_priority":"PriorityNotDefined","call_time_out":"Value1","called_party_ssi":91,"called_party_extension":null,"calling_party_ssi":1001,"calling_party_extension":null,"external_subscriber_number_calling":null,"clir_control":null,"hook_method_selection":"NoHookSignallingDirectThroughConnect","notification_indicator":2,"simplex_duplex_selection":"SimplexOperation","transmission_grant":"TransmissionGrantedToAnotherUser","transmission_request_permission":"AllowedToRequestForTransmission"}}}"#,
            r#"{"TnccSetupConfirm":{"call_identifier":7,"confirm":{"basic_service_information":{"circuit_mode_service":"SpeechService","communication_type":"PointToMultipoint","data_service":null,"data_call_capacity":null,"encryption_flag":"ClearEndToEndTransmission","speech_service":"TetraEncodedOneTimeslotSpeech"},"call_priority":"LowestPriority","call_ownership":"ACallOwner","call_amalgamation":"CallNotAmalgamated","call_time_out":"Value1","hook_method_selection":"NoHookSignallingDirectThroughConnect","notification_indicator":null,"simplex_duplex_selection":"SimplexOperation","transmission_grant":"TransmissionGranted","transmission_request_permission":"AllowedToRequestForTransmission"}}}"#,
            r#"{"TnccTxIndication":{"call_identifier":7,"indication":{"encryption_flag":"ClearEndToEndTransmission","notification_indicator":1,"transmitting_party_ssi":1001,"transmitting_party_extension":null,"external_subscriber_number":null,"transmit_request_permission":"AllowedToRequestForTransmission","transmission_status":"TransmissionGranted"}}}"#,
            r#"{"TnccTxConfirm":{"call_identifier":7,"confirm":{"encryption_flag":"ClearEndToEndTransmission","transmit_request_permission":"AllowedToRequestForTransmission","transmission_status":"TransmissionGranted"}}}"#,
        ];
        for (event, expected_json) in events.iter().zip(expected) {
            assert_eq!(enc(event), expected_json);
        }
    }
}
