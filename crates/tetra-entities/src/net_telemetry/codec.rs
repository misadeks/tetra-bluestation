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

    // -----------------------------------------------------------------------
    // TNMM-SAP (cl. 15.3) telemetry roundtrips. TelemetryEvent itself is not
    // PartialEq, so we compare the (PartialEq) tnmm payloads after decoding.
    // -----------------------------------------------------------------------

    use crate::tnmm::{
        CellType, ClassOfUsage, DisableStatus, GroupIdentity, GroupIdentityAttachDetachTypeIdentifier,
        GroupIdentityLifetime, RegistrationRejectCause, RegistrationStatus, ServiceStatus,
        TnmmAttachDetachGroupIdentityIndication, TnmmRegistrationIndication, TnmmServiceIndication,
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
}
