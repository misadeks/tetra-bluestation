use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use toml::Value;

/// Mobile Station (MS) specific configuration.
///
/// Only relevant when `stack_mode = "Ms"`. The MS reuses `net_info` (home MCC/MNC)
/// and `cell_info` (RF tuning: band/carrier/duplex) for radio configuration; this
/// section holds the MS identity and affiliation.
///
/// Note: MS transmit parameters (class of MS / power class, ref. EN 300 392-2
/// clause 6 and the class-of-MS element in clause 16) are intentionally not
/// modelled yet. They are only needed once uplink transmission and registration
/// are implemented (plan phases 3-4) and will be added against the exact PDU
/// field definitions at that point.
#[derive(Debug, Clone)]
pub struct CfgMs {
    /// Own Individual Short Subscriber Identity (ISSI), 24 bits. The MS address.
    pub issi: u32,

    /// Subscriber class this MS belongs to (1-16). Used to check cell access
    /// permission against the cell `subscriber_class` bitmask advertised in
    /// D-MLE-SYSINFO (ref. EN 300 392-2 clause 18.4.2.2).
    pub subscriber_class: u8,

    /// Group identities (GSSIs) to attach to once registered (ref. clause 16
    /// group identity attachment). Empty for a pure receive-only monitor.
    pub attach_groups: Vec<u32>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct CfgMsDto {
    pub issi: u32,
    pub subscriber_class: Option<u8>,
    pub attach_groups: Option<Vec<u32>>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

pub fn ms_dto_to_cfg(dto: CfgMsDto) -> CfgMs {
    CfgMs {
        issi: dto.issi,
        subscriber_class: dto.subscriber_class.unwrap_or(1),
        attach_groups: dto.attach_groups.unwrap_or_default(),
    }
}

/// Inverse of [`ms_dto_to_cfg`] for TOML write-back (Plane B, non-standard).
pub fn cfg_to_ms_dto(ms: &CfgMs) -> CfgMsDto {
    CfgMsDto {
        issi: ms.issi,
        subscriber_class: Some(ms.subscriber_class),
        attach_groups: Some(ms.attach_groups.clone()),
        extra: HashMap::new(),
    }
}
