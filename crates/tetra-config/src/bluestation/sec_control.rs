use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use toml::Value;

/// Control endpoint configuration
#[derive(Debug, Clone)]
pub struct CfgControl {
    /// Control server hostname or IP
    pub host: String,
    /// Control server port
    pub port: u16,
    /// Use TLS (wss://)
    pub use_tls: bool,
    /// Optional path to a DER-encoded CA certificate for self-signed TLS
    pub ca_cert: Option<String>,
    /// Optional (username, password) for HTTP Basic authentication
    pub credentials: Option<(String, String)>,
}

#[derive(Deserialize, Serialize)]
pub struct CfgControlDto {
    /// Control server hostname or IP
    pub host: String,
    /// Control server port
    pub port: u16,
    /// Use TLS (wss://)
    #[serde(default)]
    pub use_tls: bool,
    /// Optional path to a DER-encoded CA certificate for self-signed TLS
    pub ca_cert: Option<String>,
    /// Optional username for HTTP Basic auth
    pub username: Option<String>,
    /// Optional password for HTTP Basic auth
    pub password: Option<String>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

/// Convert a [`CfgControlDto`] (from TOML) into a [`CfgControl`].
///
/// Returns an error string if `ca_cert` is set but `use_tls` is `false`.
pub fn apply_control_patch(src: CfgControlDto) -> Result<CfgControl, String> {
    if src.ca_cert.is_some() && !src.use_tls {
        return Err("control: ca_cert requires use_tls = true".to_string());
    }

    Ok(CfgControl {
        host: src.host,
        port: src.port,
        use_tls: src.use_tls,
        credentials: match (src.username, src.password) {
            (Some(u), Some(p)) => Some((u, p)),
            (None, None) => None,
            _ => return Err("control: both username and password must be set for credentials".to_string()),
        },
        ca_cert: src.ca_cert,
    })
}

/// Inverse of [`apply_control_patch`] for TOML write-back (Plane B, non-standard).
/// The `credentials` tuple is split back into `username`/`password`. Secrets are
/// written as plaintext: the TOML file is their canonical store (redaction is for
/// logs only, via `SecretField`, which this DTO does not use).
pub fn cfg_control_to_dto(c: &CfgControl) -> CfgControlDto {
    let (username, password) = match &c.credentials {
        Some((u, p)) => (Some(u.clone()), Some(p.clone())),
        None => (None, None),
    };
    CfgControlDto {
        host: c.host.clone(),
        port: c.port,
        use_tls: c.use_tls,
        ca_cert: c.ca_cert.clone(),
        username,
        password,
        extra: HashMap::new(),
    }
}
