use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use toml::Value;

use crate::bluestation::{
    CellInfoDto, CfgControlDto, CfgMsDto, NetInfoDto, apply_control_patch, cell_dto_to_cfg, cfg_control_to_dto, cfg_to_cell_dto,
    cfg_to_ms_dto, cfg_to_net_dto, cfg_to_phy_dto, ms_dto_to_cfg, net_dto_to_cfg,
};

use super::config::{StackConfig, StackMode};
use super::sec_brew::{CfgBrewDto, apply_brew_patch, cfg_brew_to_dto};
use super::sec_telemetry::{CfgTelemetryDto, apply_telemetry_patch, cfg_telemetry_to_dto};
use super::{PhyIoDto, phy_dto_to_cfg};

/// Build `StackConfig` from a TOML configuration file
pub fn from_toml_str(toml_str: &str) -> Result<StackConfig, Box<dyn std::error::Error>> {
    let root: TomlConfigRoot = toml::from_str(toml_str)?;

    // Various sanity checks
    let expected_config_version = "0.6";
    if !root.config_version.eq(expected_config_version) {
        return Err(format!(
            "Unrecognized config_version: {}, expect {}",
            root.config_version, expected_config_version
        )
        .into());
    }
    if !root.extra.is_empty() {
        return Err(format!("Unrecognized top-level fields: {:?}", sorted_keys(&root.extra)).into());
    }

    if !root.phy_io.extra.is_empty() {
        return Err(format!("Unrecognized fields: phy_io::{:?}", sorted_keys(&root.phy_io.extra)).into());
    }
    if let Some(ref soapy) = root.phy_io.soapysdr {
        let extra_keys = sorted_keys(&soapy.extra);
        let extra_keys_filtered = extra_keys
            .iter()
            .filter(|key| !(key.starts_with("rx_gain_") || key.starts_with("tx_gain_")))
            .collect::<Vec<&&str>>();
        if !extra_keys_filtered.is_empty() {
            return Err(format!("Unrecognized fields: phy_io.soapysdr::{:?}", extra_keys_filtered).into());
        }
    }
    if !root.net_info.extra.is_empty() {
        return Err(format!("Unrecognized fields in net_info: {:?}", sorted_keys(&root.net_info.extra)).into());
    }
    if !root.cell_info.extra.is_empty() {
        return Err(format!("Unrecognized fields in cell_info: {:?}", sorted_keys(&root.cell_info.extra)).into());
    }

    // Optional brew section
    if let Some(ref brew) = root.brew {
        if !brew.extra.is_empty() {
            return Err(format!("Unrecognized fields in brew config: {:?}", sorted_keys(&brew.extra)).into());
        }
    }

    // Optional telemetry section
    if let Some(ref telemetry) = root.telemetry {
        if !telemetry.extra.is_empty() {
            return Err(format!("Unrecognized fields in telemetry config: {:?}", sorted_keys(&telemetry.extra)).into());
        }
    }

    // Optional ms section (required when stack_mode = Ms; presence checked in validate())
    if let Some(ref ms) = root.ms {
        if !ms.extra.is_empty() {
            return Err(format!("Unrecognized fields in ms config: {:?}", sorted_keys(&ms.extra)).into());
        }
    }

    // Build config from required and optional values
    let mut cfg = StackConfig {
        stack_mode: root.stack_mode,
        debug_log: root.debug_log,
        phy_io: phy_dto_to_cfg(root.phy_io),
        net: net_dto_to_cfg(root.net_info),
        cell: cell_dto_to_cfg(root.cell_info),
        ms: root.ms.map(ms_dto_to_cfg),
        brew: None,
        telemetry: None,
        control: None,
    };

    if let Some(brew) = root.brew {
        cfg.brew = Some(apply_brew_patch(brew));
    }

    if let Some(telemetry) = root.telemetry {
        cfg.telemetry = Some(apply_telemetry_patch(telemetry)?);
    }

    if let Some(command) = root.command {
        cfg.control = Some(apply_control_patch(command)?);
    }

    Ok(cfg)
}

/// Build `SharedConfig` from any reader.
pub fn from_reader<R: Read>(reader: R) -> Result<StackConfig, Box<dyn std::error::Error>> {
    let mut contents = String::new();
    let mut reader = BufReader::new(reader);
    reader.read_to_string(&mut contents)?;
    from_toml_str(&contents)
}

/// Build `SharedConfig` from a file path.
pub fn from_file<P: AsRef<Path>>(path: P) -> Result<StackConfig, Box<dyn std::error::Error>> {
    let f = File::open(path)?;
    let r = BufReader::new(f);
    let cfg = from_reader(r)?;
    Ok(cfg)
}

fn sorted_keys(map: &HashMap<String, Value>) -> Vec<&str> {
    let mut v: Vec<&str> = map.keys().map(|s| s.as_str()).collect();
    v.sort_unstable();
    v
}

/// ----------------------- DTOs for input shape -----------------------

#[derive(Deserialize, Serialize)]
struct TomlConfigRoot {
    config_version: String,
    stack_mode: StackMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    debug_log: Option<String>,

    phy_io: PhyIoDto,
    net_info: NetInfoDto,
    cell_info: CellInfoDto,

    #[serde(skip_serializing_if = "Option::is_none")]
    brew: Option<CfgBrewDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telemetry: Option<CfgTelemetryDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<CfgControlDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ms: Option<CfgMsDto>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    extra: HashMap<String, Value>,
}

/// Serialize a runtime [`StackConfig`] back to a canonical TOML string
/// (**NON-STANDARD**, Plane B config write-back).
///
/// This is the inverse of [`from_toml_str`]: it projects the runtime config
/// through the DTO layer (`cfg_to_*_dto`) — the single on-disk schema source of
/// truth — and renders it with `toml`. The result re-parses through
/// [`from_toml_str`] to an equivalent `StackConfig` (round-trip closed), so a
/// GetConfig -> edit -> SetConfig -> reload cycle is stable. `config_version` is
/// emitted verbatim so the file stays loadable.
///
/// Secrets are written as plaintext: the DTO fields are plain `String`s and the
/// TOML file is their canonical store (`SecretField` redaction applies to logs
/// only and is not routed through here).
pub fn to_toml_string(cfg: &StackConfig) -> Result<String, String> {
    let root = TomlConfigRoot {
        config_version: "0.6".to_string(),
        stack_mode: cfg.stack_mode,
        debug_log: cfg.debug_log.clone(),
        phy_io: cfg_to_phy_dto(&cfg.phy_io),
        net_info: cfg_to_net_dto(&cfg.net),
        cell_info: cfg_to_cell_dto(&cfg.cell),
        brew: cfg.brew.as_ref().map(cfg_brew_to_dto),
        telemetry: cfg.telemetry.as_ref().map(cfg_telemetry_to_dto),
        command: cfg.control.as_ref().map(cfg_control_to_dto),
        ms: cfg.ms.as_ref().map(cfg_to_ms_dto),
        extra: HashMap::new(),
    };
    toml::to_string_pretty(&root).map_err(|e| format!("TOML serialization failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A representative MS config exercising phy/net/cell/ms sections
    // (mirrors example_config/config-ms.toml).
    const MS_TOML: &str = r#"
config_version = "0.6"
stack_mode = "Ms"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
tx_freq = 439825000
rx_freq = 430425000
ppm_err = 0
device = "driver=sx"
sample_rate = 600000
rx_antenna = "RX"
tx_antenna = "TX"
rx_gain_lna = 48.0
rx_gain_pga = 8.0
tx_gain_dac = 0.0
tx_gain_mixer = 0.0

[net_info]
mcc = 901
mnc = 9999

[cell_info]
freq_band = 4
main_carrier = 1593
duplex_spacing = 7
custom_duplex_spacing = 9400000
freq_offset = 0
reverse_operation = false
location_area = 1
colour_code = 1

[ms]
issi = 1000001
subscriber_class = 1
attach_groups = []
"#;

    #[test]
    fn to_toml_string_roundtrips_ms_config() {
        let cfg = from_toml_str(MS_TOML).expect("initial parse");
        let rendered = to_toml_string(&cfg).expect("serialize");
        // The rendered string must re-parse through the exact same validator.
        let reparsed = from_toml_str(&rendered).expect("reparse rendered config");

        assert_eq!(cfg.stack_mode, reparsed.stack_mode);
        assert_eq!(cfg.net.mcc, reparsed.net.mcc);
        assert_eq!(cfg.net.mnc, reparsed.net.mnc);
        assert_eq!(cfg.cell.freq_band, reparsed.cell.freq_band);
        assert_eq!(cfg.cell.main_carrier, reparsed.cell.main_carrier);
        assert_eq!(cfg.cell.location_area, reparsed.cell.location_area);
        assert_eq!(cfg.cell.colour_code, reparsed.cell.colour_code);
        let soapy_a = cfg.phy_io.soapysdr.as_ref().expect("soapy");
        let soapy_b = reparsed.phy_io.soapysdr.as_ref().expect("soapy reparsed");
        assert_eq!(soapy_a.dl_freq, soapy_b.dl_freq);
        assert_eq!(soapy_a.ul_freq, soapy_b.ul_freq);
        assert_eq!(soapy_a.rx_gains, soapy_b.rx_gains);
        assert_eq!(soapy_a.tx_gains, soapy_b.tx_gains);
        let ms_a = cfg.ms.as_ref().expect("ms section");
        let ms_b = reparsed.ms.as_ref().expect("ms section reparsed");
        assert_eq!(ms_a.issi, ms_b.issi);
        assert_eq!(ms_a.subscriber_class, ms_b.subscriber_class);
        assert_eq!(ms_a.attach_groups, ms_b.attach_groups);
    }

    #[test]
    fn to_toml_string_emits_config_version() {
        let cfg = from_toml_str(MS_TOML).expect("initial parse");
        let rendered = to_toml_string(&cfg).expect("serialize");
        assert!(rendered.contains("config_version = \"0.6\""));
    }
}
