use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use toml::Value;

/// SoapySDR configuration
#[derive(Debug, Clone)]
pub struct CfgSoapySdr {
    /// Uplink frequency in Hz
    pub ul_freq: f64,
    /// Downlink frequency in Hz
    pub dl_freq: f64,
    /// PPM frequency error correction
    pub ppm_err: f64,
    /// Argument string to select a specific SDR device.
    /// If None, devices will be enumerated until the first supported device is found.
    pub device: Option<String>,
    /// RX antenna. Device specific default will be used if None.
    pub rx_ant: Option<String>,
    /// TX antenna. Device specific default will be used if None.
    pub tx_ant: Option<String>,
    /// RX gain values.
    /// Device specific defaults will be used for gains that are not set.
    pub rx_gains: HashMap<String, f64>,
    /// TX gain values.
    /// Device specific defaults will be used for gains that are not set.
    pub tx_gains: HashMap<String, f64>,
    /// RX and TX sample rate. Device specific default will be used if None.
    pub fs: Option<f64>,
    /// RX channel number
    pub rx_ch: Option<usize>,
    /// TX channel number
    pub tx_ch: Option<usize>,
}

impl CfgSoapySdr {
    /// Get corrected UL frequency with PPM error applied
    pub fn ul_freq_corrected(&self) -> (f64, f64) {
        let ppm = self.ppm_err;
        let err = (self.ul_freq / 1_000_000.0) * ppm;
        (self.ul_freq + err, err)
    }

    /// Get corrected DL frequency with PPM error applied
    pub fn dl_freq_corrected(&self) -> (f64, f64) {
        let ppm = self.ppm_err;
        let err = (self.dl_freq / 1_000_000.0) * ppm;
        (self.dl_freq + err, err)
    }
}

#[derive(Deserialize, Serialize)]
pub struct SoapySdrDto {
    pub rx_freq: f64,
    pub tx_freq: f64,
    pub ppm_err: Option<f64>,

    pub device: Option<String>,

    pub rx_antenna: Option<String>,
    pub tx_antenna: Option<String>,

    pub sample_rate: Option<f64>,
    pub rx_channel: Option<usize>,
    pub tx_channel: Option<usize>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

/// Inverse of the soapy mapping in `phy_dto_to_cfg` for TOML write-back
/// (Plane B, non-standard). RX/TX frequencies map back to `rx_freq`/`tx_freq`;
/// the `rx_gains`/`tx_gains` maps are re-expanded into `rx_gain_*` / `tx_gain_*`
/// flattened keys (the gain-name is lower-cased, matching the loader).
pub fn cfg_to_soapy_dto(s: &CfgSoapySdr) -> SoapySdrDto {
    let mut extra: HashMap<String, Value> = HashMap::new();
    for (name, val) in &s.rx_gains {
        extra.insert(format!("rx_gain_{name}"), Value::Float(*val));
    }
    for (name, val) in &s.tx_gains {
        extra.insert(format!("tx_gain_{name}"), Value::Float(*val));
    }
    SoapySdrDto {
        rx_freq: s.ul_freq,
        tx_freq: s.dl_freq,
        ppm_err: Some(s.ppm_err),
        device: s.device.clone(),
        rx_antenna: s.rx_ant.clone(),
        tx_antenna: s.tx_ant.clone(),
        sample_rate: s.fs,
        rx_channel: s.rx_ch,
        tx_channel: s.tx_ch,
        extra,
    }
}
