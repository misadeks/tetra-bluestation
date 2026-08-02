//! Command codec — bitcode-based and JSON-based serialization of
//! [`Command`]s and [`CommandResponse`]s.

use crate::{
    net_control::commands::{ControlCommand, ControlResponse},
    network::transports::NetworkError,
};

// ---------------------------------------------------------------------------
// Codecs
// ---------------------------------------------------------------------------

/// Codec for commands using bitcode for serialization.
#[derive(Default)]
pub struct ControlCodecBitcode;

impl ControlCodecBitcode {
    /// Encode a [`Command`] to bitcode bytes.
    pub fn encode_command(&self, cmd: &ControlCommand) -> Vec<u8> {
        bitcode::encode(cmd)
    }

    /// Decode bitcode bytes into a [`Command`].
    pub fn decode_command(&self, payload: &[u8]) -> Result<ControlCommand, NetworkError> {
        bitcode::decode(payload).map_err(|e| NetworkError::SerializationError(format!("command decode: {}", e)))
    }

    /// Encode a [`CommandResponse`] to bitcode bytes.
    pub fn encode_response(&self, resp: &ControlResponse) -> Vec<u8> {
        bitcode::encode(resp)
    }

    /// Decode bitcode bytes into a [`CommandResponse`].
    pub fn decode_response(&self, payload: &[u8]) -> Result<ControlResponse, NetworkError> {
        bitcode::decode(payload).map_err(|e| NetworkError::SerializationError(format!("command response decode: {}", e)))
    }
}

/// Codec for commands using JSON for serialization.
#[derive(Default)]
pub struct ControlCodecJson;

impl ControlCodecJson {
    /// Encode a [`Command`] to JSON bytes.
    pub fn encode_command(&self, cmd: &ControlCommand) -> Vec<u8> {
        serde_json::to_vec(cmd).unwrap_or_default()
    }

    /// Decode JSON bytes into a [`Command`].
    pub fn decode_command(&self, payload: &[u8]) -> Result<ControlCommand, NetworkError> {
        serde_json::from_slice(payload).map_err(|e| NetworkError::SerializationError(format!("command decode: {}", e)))
    }

    /// Encode a [`CommandResponse`] to JSON bytes.
    pub fn encode_response(&self, resp: &ControlResponse) -> Vec<u8> {
        serde_json::to_vec(resp).unwrap_or_default()
    }

    /// Decode JSON bytes into a [`CommandResponse`].
    pub fn decode_response(&self, payload: &[u8]) -> Result<ControlResponse, NetworkError> {
        serde_json::from_slice(payload).map_err(|e| NetworkError::SerializationError(format!("command response decode: {}", e)))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_bitcode_command_a() {
        let codec = ControlCodecBitcode;
        let cmd = ControlCommand::CommandA {
            handle: 1,
            parameter: 1234,
        };
        let bytes = codec.encode_command(&cmd);
        let decoded = codec.decode_command(&bytes).unwrap();
        let ControlCommand::CommandA { handle, parameter } = decoded else {
            panic!("expected CommandA");
        };
        assert_eq!(handle, 1);
        assert_eq!(parameter, 1234);
    }

    #[test]
    fn test_roundtrip_json_command_a() {
        let codec = ControlCodecJson;
        let cmd = ControlCommand::CommandA {
            handle: 1,
            parameter: 1234,
        };
        let bytes = codec.encode_command(&cmd);
        let decoded = codec.decode_command(&bytes).unwrap();
        let ControlCommand::CommandA { handle, parameter } = decoded else {
            panic!("expected CommandA");
        };
        assert_eq!(handle, 1);
        assert_eq!(parameter, 1234);
    }

    #[test]
    fn test_roundtrip_bitcode_response() {
        let codec = ControlCodecBitcode;
        let resp = ControlResponse::CommandAResponse { handle: 1, result: 42 };
        let bytes = codec.encode_response(&resp);
        let decoded = codec.decode_response(&bytes).unwrap();
        let ControlResponse::CommandAResponse { handle, result } = decoded else {
            panic!("expected CommandAResponse");
        };
        assert_eq!(handle, 1);
        assert_eq!(result, 42);
    }

    #[test]
    fn test_roundtrip_json_response() {
        let codec = ControlCodecJson;
        let resp = ControlResponse::SendSdsResponse { handle: 2, success: true };
        let bytes = codec.encode_response(&resp);
        let decoded = codec.decode_response(&bytes).unwrap();
        let ControlResponse::SendSdsResponse { handle, success } = decoded else {
            panic!("expected SendSdsResponse");
        };
        assert_eq!(handle, 2);
        assert!(success);
    }

    #[test]
    fn test_decode_invalid_bytes() {
        let codec = ControlCodecBitcode;
        // Use truncated bytes that cannot form a valid Command
        assert!(codec.decode_command(&[]).is_err());
    }

    /// Plane B (non-standard) management command/response survive a JSON
    /// round-trip over the reused control transport.
    #[test]
    fn test_roundtrip_json_management() {
        use crate::management::{ManagementCommand, ManagementResponse, MsRuntimeState, RegistrationState};
        use crate::tnmm::ServiceStatus;
        let codec = ControlCodecJson;

        let cmd = ControlCommand::Management(ManagementCommand::GetState { handle: 5 });
        let decoded = codec.decode_command(&codec.encode_command(&cmd)).unwrap();
        let ControlCommand::Management(ManagementCommand::GetState { handle }) = decoded else {
            panic!("expected Management GetState");
        };
        assert_eq!(handle, 5);

        let state = MsRuntimeState {
            registration_state: RegistrationState::Registered,
            service_status: ServiceStatus::InService,
            own_issi: 1000001,
            home_mcc: 901,
            home_mnc: 9999,
            serving_la: 1,
            rssi_dbfs: Some(-42.5),
            colour_code: 1,
            attached_groups: vec![100, 200],
            active_scanlists: vec!["Alpha".to_string()],
            restart_required: false,
            selection_mode_manual: false,
        };
        let resp = ControlResponse::Management(ManagementResponse::State {
            handle: 5,
            state: Box::new(state.clone()),
        });
        let decoded = codec.decode_response(&codec.encode_response(&resp)).unwrap();
        let ControlResponse::Management(ManagementResponse::State { handle, state: got }) = decoded else {
            panic!("expected Management State");
        };
        assert_eq!(handle, 5);
        assert_eq!(*got, state);
    }

    /// Plane B (non-standard) config command/response variants survive a JSON
    /// round-trip (GetConfig/SetConfig/ApplyConfig + Config/Ack).
    #[test]
    fn test_roundtrip_json_management_config() {
        use crate::management::{ManagementCommand, ManagementResponse};
        let codec = ControlCodecJson;

        // SetConfig carries a TOML payload.
        let cmd = ControlCommand::Management(ManagementCommand::SetConfig {
            handle: 9,
            toml: "config_version = \"0.6\"\n".to_string(),
        });
        let decoded = codec.decode_command(&codec.encode_command(&cmd)).unwrap();
        let ControlCommand::Management(ManagementCommand::SetConfig { handle, toml }) = decoded else {
            panic!("expected Management SetConfig");
        };
        assert_eq!(handle, 9);
        assert_eq!(toml, "config_version = \"0.6\"\n");

        // Config response.
        let resp = ControlResponse::Management(ManagementResponse::Config {
            handle: 9,
            toml: "config_version = \"0.6\"\n".to_string(),
        });
        let decoded = codec.decode_response(&codec.encode_response(&resp)).unwrap();
        let ControlResponse::Management(ManagementResponse::Config { handle, toml }) = decoded else {
            panic!("expected Management Config");
        };
        assert_eq!(handle, 9);
        assert!(toml.contains("config_version"));

        // Ack response.
        let resp = ControlResponse::Management(ManagementResponse::Ack {
            handle: 9,
            accepted: true,
            restart_required: true,
            message: "staged".to_string(),
        });
        let decoded = codec.decode_response(&codec.encode_response(&resp)).unwrap();
        let ControlResponse::Management(ManagementResponse::Ack {
            handle,
            accepted,
            restart_required,
            message,
        }) = decoded
        else {
            panic!("expected Management Ack");
        };
        assert_eq!(handle, 9);
        assert!(accepted);
        assert!(restart_required);
        assert_eq!(message, "staged");
    }

    /// T4 JSON-schema freeze (Plane A + Plane B wire format).
    ///
    /// Round-trip tests alone do NOT freeze the schema: a symmetric serde rename
    /// (variant or field) changes encode and decode together and still passes.
    /// These golden-string assertions pin the exact on-the-wire JSON so any
    /// accidental rename of a wrapper, variant, or field breaks the build with a
    /// clear diff. The strings here are the contract the reference UI is built
    /// against (schema `bluestation-ms-interface-3`).
    #[test]
    fn test_json_schema_freeze_golden_wire_format() {
        use crate::management::{MS_INTERFACE_SCHEMA_VERSION, ManagementCommand, ManagementResponse};
        let codec = ControlCodecJson;
        let enc_cmd = |c: &ControlCommand| String::from_utf8(codec.encode_command(c)).unwrap();
        let enc_resp = |r: &ControlResponse| String::from_utf8(codec.encode_response(r)).unwrap();

        // --- Plane B (management) commands ---
        assert_eq!(
            enc_cmd(&ControlCommand::Management(ManagementCommand::GetState { handle: 5 })),
            r#"{"Management":{"GetState":{"handle":5}}}"#
        );
        assert_eq!(
            enc_cmd(&ControlCommand::Management(ManagementCommand::GetInterfaceVersion { handle: 7 })),
            r#"{"Management":{"GetInterfaceVersion":{"handle":7}}}"#
        );
        assert_eq!(
            enc_cmd(&ControlCommand::Management(ManagementCommand::GetConfig { handle: 3 })),
            r#"{"Management":{"GetConfig":{"handle":3}}}"#
        );
        assert_eq!(
            enc_cmd(&ControlCommand::Management(ManagementCommand::SetConfig {
                handle: 9,
                toml: "x=1".to_string(),
            })),
            r#"{"Management":{"SetConfig":{"handle":9,"toml":"x=1"}}}"#
        );
        assert_eq!(
            enc_cmd(&ControlCommand::Management(ManagementCommand::ApplyConfig { handle: 4 })),
            r#"{"Management":{"ApplyConfig":{"handle":4}}}"#
        );
        assert_eq!(
            enc_cmd(&ControlCommand::Management(ManagementCommand::ActivateScanlist {
                handle: 6,
                name: "Alpha".to_string(),
                active: true,
            })),
            r#"{"Management":{"ActivateScanlist":{"handle":6,"name":"Alpha","active":true}}}"#
        );
        assert_eq!(
            enc_cmd(&ControlCommand::Management(ManagementCommand::SetCellSelectionMode {
                handle: 10,
                manual: true,
            })),
            r#"{"Management":{"SetCellSelectionMode":{"handle":10,"manual":true}}}"#
        );
        assert_eq!(
            enc_cmd(&ControlCommand::Management(ManagementCommand::StartCellScan { handle: 11 })),
            r#"{"Management":{"StartCellScan":{"handle":11}}}"#
        );
        assert_eq!(
            enc_cmd(&ControlCommand::Management(ManagementCommand::StopCellScan { handle: 12 })),
            r#"{"Management":{"StopCellScan":{"handle":12}}}"#
        );
        assert_eq!(
            enc_cmd(&ControlCommand::Management(ManagementCommand::CampOnCell {
                handle: 13,
                carrier_hz: 430425000,
                register: true,
            })),
            r#"{"Management":{"CampOnCell":{"handle":13,"carrier_hz":430425000,"register":true}}}"#
        );

        // --- Plane B (management) responses ---
        assert_eq!(
            enc_resp(&ControlResponse::Management(ManagementResponse::InterfaceVersion {
                handle: 7,
                version: MS_INTERFACE_SCHEMA_VERSION.to_string(),
            })),
            r#"{"Management":{"InterfaceVersion":{"handle":7,"version":"bluestation-ms-interface-5"}}}"#
        );
        // Guard the frozen constant itself so a bump is a deliberate, visible edit.
        assert_eq!(MS_INTERFACE_SCHEMA_VERSION, "bluestation-ms-interface-5");
        assert_eq!(
            enc_resp(&ControlResponse::Management(ManagementResponse::Config {
                handle: 3,
                toml: "x=1".to_string(),
            })),
            r#"{"Management":{"Config":{"handle":3,"toml":"x=1"}}}"#
        );
        assert_eq!(
            enc_resp(&ControlResponse::Management(ManagementResponse::Ack {
                handle: 9,
                accepted: true,
                restart_required: true,
                message: "staged".to_string(),
            })),
            r#"{"Management":{"Ack":{"handle":9,"accepted":true,"restart_required":true,"message":"staged"}}}"#
        );

        // --- Plane A (TNMM-SAP, standardized) request ack ---
        // Freezes the standardized primitive-ack variant + field names on the wire.
        assert_eq!(
            enc_resp(&ControlResponse::TnmmAck {
                handle: 6,
                accepted: true,
                detail: None,
            }),
            r#"{"TnmmAck":{"handle":6,"accepted":true,"detail":null}}"#
        );

        // --- Plane A (TNSDS-SAP, standardized cl. 13.3) requests + ack ---
        assert_eq!(
            enc_cmd(&ControlCommand::TnsdsUnitdata {
                handle: 8,
                request: tetra_saps::tnsds::TnsdsUnitdataRequest {
                    called_party_ssi: 1000,
                    called_party_is_group: false,
                    user_data: tetra_saps::control::enums::sds_user_data::SdsUserData::Type1(3),
                },
            }),
            r#"{"TnsdsUnitdata":{"handle":8,"request":{"called_party_ssi":1000,"called_party_is_group":false,"user_data":{"Type1":3}}}}"#
        );
        assert_eq!(
            enc_cmd(&ControlCommand::TnsdsStatus {
                handle: 9,
                request: tetra_saps::tnsds::TnsdsStatusRequest {
                    called_party_ssi: 91,
                    called_party_is_group: true,
                    status_number: 32768,
                },
            }),
            r#"{"TnsdsStatus":{"handle":9,"request":{"called_party_ssi":91,"called_party_is_group":true,"status_number":32768}}}"#
        );
        assert_eq!(
            enc_resp(&ControlResponse::TnsdsAck {
                handle: 8,
                accepted: true,
                detail: None,
            }),
            r#"{"TnsdsAck":{"handle":8,"accepted":true,"detail":null}}"#
        );
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

    fn sample_tncc_setup_request() -> tetra_saps::tncc::TnccSetupRequest {
        use tetra_saps::tncc as t;
        t::TnccSetupRequest {
            access_priority: Some(t::AccessPriority::LowPriority),
            area_selection: Some(t::AreaSelection::AreaNotDefined),
            basic_service_information: sample_tncc_basic(),
            call_priority: t::CallPriority::PriorityNotDefined,
            called_party_type_identifier: t::CalledPartyTypeIdentifier::Ssi,
            called_party_sna: None,
            called_party_ssi: Some(91),
            called_party_extension: None,
            external_subscriber_number_called: None,
            clir_control: Some(t::ClirControl::NotImplementedOrUseDefaultMode),
            hook_method_selection: t::HookMethodSelection::NoHookSignallingDirectThroughConnect,
            request_to_transmit_send_data: t::RequestToTransmitSendData::RequestToTransmitSendData,
            simplex_duplex_selection: t::SimplexDuplexSelection::SimplexOperation,
            traffic_stealing: Some(t::TrafficStealing::DoNotStealTraffic),
        }
    }

    fn sample_tncc_commands() -> Vec<ControlCommand> {
        use tetra_saps::tncc as t;
        vec![
            ControlCommand::TnccSetup {
                handle: 1,
                request: Box::new(sample_tncc_setup_request()),
            },
            ControlCommand::TnccSetupResponse {
                handle: 2,
                call_identifier: 7,
                response: t::TnccSetupResponse {
                    access_priority: None,
                    basic_service_information: Some(sample_tncc_basic()),
                    clir_control: None,
                    hook_method_selection: t::HookMethodSelection::NoHookSignallingDirectThroughConnect,
                    simplex_duplex_selection: t::SimplexDuplexSelection::SimplexOperation,
                    traffic_stealing: None,
                },
            },
            ControlCommand::TnccComplete {
                handle: 3,
                call_identifier: 7,
                request: t::TnccCompleteRequest {
                    access_priority: None,
                    basic_service_information_offered: Some(sample_tncc_basic()),
                    hook_method: t::HookMethodSelection::NoHookSignallingDirectThroughConnect,
                    simplex_duplex: t::SimplexDuplexSelection::SimplexOperation,
                    traffic_stealing: None,
                },
            },
            ControlCommand::TnccTx {
                handle: 4,
                call_identifier: 7,
                request: t::TnccTxRequest {
                    access_priority: None,
                    encryption_flag: t::EncryptionFlag::ClearEndToEndTransmission,
                    traffic_stealing: None,
                    transmission_condition: t::TransmissionCondition::RequestToTransmit,
                    tx_demand_priority: t::TxDemandPriority::LowPriority,
                },
            },
            ControlCommand::TnccRelease {
                handle: 5,
                call_identifier: 7,
                request: t::TnccReleaseRequest {
                    access_priority: None,
                    disconnect_cause: t::DisconnectCause::UserRequestedDisconnection,
                    disconnect_type: t::DisconnectType::DisconnectCall,
                    traffic_stealing: None,
                },
            },
        ]
    }

    #[test]
    fn test_roundtrip_json_and_bitcode_all_tncc_commands() {
        let json = ControlCodecJson;
        let bitcode = ControlCodecBitcode;
        for cmd in sample_tncc_commands() {
            let json_decoded = json.decode_command(&json.encode_command(&cmd)).unwrap();
            assert_eq!(serde_json::to_string(&json_decoded).unwrap(), serde_json::to_string(&cmd).unwrap());
            let bitcode_decoded = bitcode.decode_command(&bitcode.encode_command(&cmd)).unwrap();
            assert_eq!(
                serde_json::to_string(&bitcode_decoded).unwrap(),
                serde_json::to_string(&cmd).unwrap()
            );
        }
        let resp = ControlResponse::TnccAck {
            handle: 6,
            accepted: true,
            detail: None,
        };
        let json_decoded = json.decode_response(&json.encode_response(&resp)).unwrap();
        assert_eq!(serde_json::to_string(&json_decoded).unwrap(), serde_json::to_string(&resp).unwrap());
        let bitcode_decoded = bitcode.decode_response(&bitcode.encode_response(&resp)).unwrap();
        assert_eq!(
            serde_json::to_string(&bitcode_decoded).unwrap(),
            serde_json::to_string(&resp).unwrap()
        );
    }

    #[test]
    fn test_json_schema_freeze_golden_wire_format_tncc() {
        let codec = ControlCodecJson;
        let enc_cmd = |c: &ControlCommand| String::from_utf8(codec.encode_command(c)).unwrap();
        let enc_resp = |r: &ControlResponse| String::from_utf8(codec.encode_response(r)).unwrap();
        let commands = sample_tncc_commands();
        let expected = vec![
            r#"{"TnccSetup":{"handle":1,"request":{"access_priority":"LowPriority","area_selection":"AreaNotDefined","basic_service_information":{"circuit_mode_service":"SpeechService","communication_type":"PointToMultipoint","data_service":null,"data_call_capacity":null,"encryption_flag":"ClearEndToEndTransmission","speech_service":"TetraEncodedOneTimeslotSpeech"},"call_priority":"PriorityNotDefined","called_party_type_identifier":"Ssi","called_party_sna":null,"called_party_ssi":91,"called_party_extension":null,"external_subscriber_number_called":null,"clir_control":"NotImplementedOrUseDefaultMode","hook_method_selection":"NoHookSignallingDirectThroughConnect","request_to_transmit_send_data":"RequestToTransmitSendData","simplex_duplex_selection":"SimplexOperation","traffic_stealing":"DoNotStealTraffic"}}}"#,
            r#"{"TnccSetupResponse":{"handle":2,"call_identifier":7,"response":{"access_priority":null,"basic_service_information":{"circuit_mode_service":"SpeechService","communication_type":"PointToMultipoint","data_service":null,"data_call_capacity":null,"encryption_flag":"ClearEndToEndTransmission","speech_service":"TetraEncodedOneTimeslotSpeech"},"clir_control":null,"hook_method_selection":"NoHookSignallingDirectThroughConnect","simplex_duplex_selection":"SimplexOperation","traffic_stealing":null}}}"#,
            r#"{"TnccComplete":{"handle":3,"call_identifier":7,"request":{"access_priority":null,"basic_service_information_offered":{"circuit_mode_service":"SpeechService","communication_type":"PointToMultipoint","data_service":null,"data_call_capacity":null,"encryption_flag":"ClearEndToEndTransmission","speech_service":"TetraEncodedOneTimeslotSpeech"},"hook_method":"NoHookSignallingDirectThroughConnect","simplex_duplex":"SimplexOperation","traffic_stealing":null}}}"#,
            r#"{"TnccTx":{"handle":4,"call_identifier":7,"request":{"access_priority":null,"encryption_flag":"ClearEndToEndTransmission","traffic_stealing":null,"transmission_condition":"RequestToTransmit","tx_demand_priority":"LowPriority"}}}"#,
            r#"{"TnccRelease":{"handle":5,"call_identifier":7,"request":{"access_priority":null,"disconnect_cause":"UserRequestedDisconnection","disconnect_type":"DisconnectCall","traffic_stealing":null}}}"#,
        ];
        for (cmd, expected_json) in commands.iter().zip(expected) {
            assert_eq!(enc_cmd(cmd), expected_json);
        }
        assert_eq!(
            enc_resp(&ControlResponse::TnccAck {
                handle: 6,
                accepted: true,
                detail: None
            }),
            r#"{"TnccAck":{"handle":6,"accepted":true,"detail":null}}"#
        );
    }

    #[test]
    fn test_roundtrip_ms_uplink_speech_command() {
        // U-plane uplink speech offload: a 274-bit TCH/S type-1 block carried
        // one-bit-per-byte must survive both codecs byte-for-byte, symmetric to
        // the downlink MsSpeechFrame telemetry event.
        let data: Vec<u8> = (0..274u16).map(|i| (i % 2) as u8).collect();
        let cmd = ControlCommand::MsUplinkSpeech {
            call_identifier: 7,
            frame_bits: 274,
            data: data.clone(),
        };
        let json = ControlCodecJson;
        let bitcode = ControlCodecBitcode;

        let json_decoded = json.decode_command(&json.encode_command(&cmd)).unwrap();
        assert_eq!(serde_json::to_string(&json_decoded).unwrap(), serde_json::to_string(&cmd).unwrap());
        let bitcode_decoded = bitcode.decode_command(&bitcode.encode_command(&cmd)).unwrap();
        assert_eq!(
            serde_json::to_string(&bitcode_decoded).unwrap(),
            serde_json::to_string(&cmd).unwrap()
        );

        let ControlCommand::MsUplinkSpeech {
            call_identifier,
            frame_bits,
            data: got,
        } = json_decoded
        else {
            panic!("expected MsUplinkSpeech");
        };
        assert_eq!(call_identifier, 7);
        assert_eq!(frame_bits, 274);
        assert_eq!(got, data);
    }
}
