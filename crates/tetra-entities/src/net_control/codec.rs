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
            colour_code: 1,
            attached_groups: vec![100, 200],
            restart_required: false,
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
    /// against (schema `bluestation-ms-interface-1`).
    #[test]
    fn test_json_schema_freeze_golden_wire_format() {
        use crate::management::{ManagementCommand, ManagementResponse, MS_INTERFACE_SCHEMA_VERSION};
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

        // --- Plane B (management) responses ---
        assert_eq!(
            enc_resp(&ControlResponse::Management(ManagementResponse::InterfaceVersion {
                handle: 7,
                version: MS_INTERFACE_SCHEMA_VERSION.to_string(),
            })),
            r#"{"Management":{"InterfaceVersion":{"handle":7,"version":"bluestation-ms-interface-1"}}}"#
        );
        // Guard the frozen constant itself so a bump is a deliberate, visible edit.
        assert_eq!(MS_INTERFACE_SCHEMA_VERSION, "bluestation-ms-interface-1");
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
    }
}
