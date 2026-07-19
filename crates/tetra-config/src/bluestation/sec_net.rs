use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use toml::Value;

#[derive(Debug, Clone)]
pub struct CfgNetInfo {
    /// 10 bits, from 18.4.2.1 D-MLE-SYNC
    pub mcc: u16,
    /// 14 bits, from 18.4.2.1 D-MLE-SYNC
    pub mnc: u16,
}

#[derive(Default, Deserialize, Serialize)]
pub struct NetInfoDto {
    pub mcc: u16,
    pub mnc: u16,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

pub fn net_dto_to_cfg(ni: NetInfoDto) -> CfgNetInfo {
    CfgNetInfo { mcc: ni.mcc, mnc: ni.mnc }
}

/// Inverse of [`net_dto_to_cfg`]: project the runtime `CfgNetInfo` back to its
/// on-disk DTO for TOML write-back (Plane B, non-standard). See
/// `crate::bluestation::parsing::to_toml_string`.
pub fn cfg_to_net_dto(ni: &CfgNetInfo) -> NetInfoDto {
    NetInfoDto {
        mcc: ni.mcc,
        mnc: ni.mnc,
        extra: HashMap::new(),
    }
}
