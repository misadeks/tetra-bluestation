use std::{collections::HashMap, time::Duration};

use serde::{Deserialize, Serialize};
use toml::Value;

use crate::bluestation::SecretField;

/// Brew protocol (TetraPack/BrandMeister) configuration
#[derive(Debug, Clone)]
pub struct CfgBrew {
    /// TetraPack server hostname or IP
    pub host: String,
    /// TetraPack server port
    pub port: u16,
    /// Use TLS (wss:// / https://)
    pub tls: bool,
    /// Optional username for HTTP Digest auth
    pub username: Option<String>,
    /// Optional password for HTTP Digest auth
    pub password: Option<SecretField>,
    /// Reconnection delay
    pub reconnect_delay: Duration,
    /// Extra initial jitter playout delay in frames (added on top of adaptive baseline)
    pub jitter_initial_latency_frames: u8,

    /// Set to true when SDS between local and Brew clients is enabled
    pub feature_sds_enabled: bool,
    /// If present, restrict Brew calls to these remote SSIs
    pub whitelisted_ssis: Option<Vec<u32>>,
    /// Optional PBX gateway ISSIs that should be routable over Brew even if they don't match normal Tetrapack ISSI constraints.
    pub pbx_gateway_issis: Option<Vec<u32>>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct CfgBrewDto {
    /// TetraPack server hostname or IP
    pub host: String,
    /// TetraPack server port
    #[serde(default = "default_brew_port")]
    pub port: u16,
    /// Use TLS (wss:// / https://)
    pub tls: bool,
    /// Optional username for HTTP Digest auth
    pub username: u32,
    /// Optional password for HTTP Digest auth
    pub password: String,
    /// Reconnection delay in seconds
    #[serde(default = "default_brew_reconnect_delay")]
    pub reconnect_delay_secs: u64,
    /// Extra initial jitter playout delay in frames (added on top of adaptive baseline)
    #[serde(default)]
    pub jitter_initial_latency_frames: u8,

    /// If present, restrict Brew calls to these remote SSIs
    pub whitelisted_ssis: Option<Vec<u32>>,

    /// Set to true when SDS between local and Brew clients is enabled
    #[serde(default = "default_brew_feature_sds_enabled")]
    pub feature_sds_enabled: bool,

    /// Optional PBX gateway ISSIs that should be routable over Brew even if they don't match normal Tetrapack ISSI constraints.
    pub pbx_gateway_issis: Option<Vec<u32>>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

fn default_brew_port() -> u16 {
    443
}

fn default_brew_reconnect_delay() -> u64 {
    15
}

fn default_brew_feature_sds_enabled() -> bool {
    true
}

/// Convert a CfgBrewDto (from TOML) into a CfgBrew (used in the stack config)
pub fn apply_brew_patch(src: CfgBrewDto) -> CfgBrew {
    CfgBrew {
        host: src.host,
        port: src.port,
        tls: src.tls,
        username: Some(src.username.to_string()),
        password: Some(SecretField::from(src.password)),
        reconnect_delay: Duration::from_secs(src.reconnect_delay_secs),
        jitter_initial_latency_frames: src.jitter_initial_latency_frames,
        feature_sds_enabled: src.feature_sds_enabled,
        whitelisted_ssis: src.whitelisted_ssis,
        pbx_gateway_issis: src.pbx_gateway_issis,
    }
}

/// Inverse of [`apply_brew_patch`] for TOML write-back (Plane B, non-standard).
///
/// NOTE: Brew is a BS-only integration (TetraPack/BrandMeister) and is not used
/// in MS mode, so an MS config carries no `[brew]` section; this inverse exists
/// only to keep the DTO serializer total. Two fields are asymmetric with the
/// runtime type and round-trip only for configs that originated from a valid
/// TOML file: the DTO `username` is a `u32` while the runtime keeps it as an
/// `Option<String>` (parsed back here; defaults to 0 if absent/non-numeric), and
/// the DTO `password` is a plain `String` while the runtime wraps it in a
/// `SecretField` (written back as plaintext — the TOML file is the canonical
/// secret store; redaction is for logs only).
pub fn cfg_brew_to_dto(b: &CfgBrew) -> CfgBrewDto {
    CfgBrewDto {
        host: b.host.clone(),
        port: b.port,
        tls: b.tls,
        username: b.username.as_deref().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0),
        password: b.password.as_ref().map(|s| s.as_ref().to_string()).unwrap_or_default(),
        reconnect_delay_secs: b.reconnect_delay.as_secs(),
        jitter_initial_latency_frames: b.jitter_initial_latency_frames,
        whitelisted_ssis: b.whitelisted_ssis.clone(),
        feature_sds_enabled: b.feature_sds_enabled,
        pbx_gateway_issis: b.pbx_gateway_issis.clone(),
        extra: HashMap::new(),
    }
}
