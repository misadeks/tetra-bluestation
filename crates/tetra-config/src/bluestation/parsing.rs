use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use toml::Value;

use crate::bluestation::{
    CarrierOverrideDto, CellInfoDto, CfgControlDto, CfgMsDto, DuplexTableDto, FolderDto, FrequencyListDto, NetInfoDto,
    NetworkDto, TalkgroupDto, apply_control_patch, cell_dto_to_cfg, cfg_control_to_dto, cfg_to_carrier_override_dtos,
    cfg_to_cell_dto, cfg_to_duplex_dto, cfg_to_folder_dtos, cfg_to_frequency_list_dtos, cfg_to_ms_dto, cfg_to_net_dto,
    cfg_to_network_dtos, cfg_to_phy_dto, cfg_to_talkgroup_dtos, codeplug_dto_to_cfg, duplex_dto_to_cfg,
    duplex_table_is_default, ms_dto_to_cfg, net_dto_to_cfg,
};

use super::config::{StackConfig, StackMode};
use super::sec_brew::{CfgBrewDto, apply_brew_patch, cfg_brew_to_dto};
use super::sec_telemetry::{CfgTelemetryDto, apply_telemetry_patch, cfg_telemetry_to_dto};
use super::{PhyIoDto, phy_dto_to_cfg};

/// Current on-disk config schema version emitted by the serializer.
pub const CURRENT_CONFIG_VERSION: &str = "0.7";

/// Config schema versions this build can load. `0.6` is the legacy (pre-codeplug)
/// schema and is accepted for backward compatibility; it migrates trivially since
/// the codeplug is an optional additive section (a legacy config simply has none).
pub const SUPPORTED_CONFIG_VERSIONS: &[&str] = &["0.6", "0.7"];

/// Build `StackConfig` from a TOML configuration file
pub fn from_toml_str(toml_str: &str) -> Result<StackConfig, Box<dyn std::error::Error>> {
    let root: TomlConfigRoot = toml::from_str(toml_str)?;

    // Various sanity checks
    if !SUPPORTED_CONFIG_VERSIONS.contains(&root.config_version.as_str()) {
        return Err(format!(
            "Unrecognized config_version: {}, expected one of {:?}",
            root.config_version, SUPPORTED_CONFIG_VERSIONS
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

    // BS mode defines the cell, so its RF must be authored explicitly. A
    // radio-style MS omits these (RX seeded from the scan list, UL derived from
    // the cell's SYSINFO at camp time).
    if root.stack_mode == StackMode::Bs {
        if root.cell_info.main_carrier.is_none() || root.cell_info.freq_band.is_none() {
            return Err("BS mode requires [cell_info] main_carrier and freq_band".into());
        }
        if let Some(ref soapy) = root.phy_io.soapysdr {
            if soapy.tx_freq.is_none() || soapy.rx_freq.is_none() {
                return Err("BS mode requires [phy_io.soapysdr] tx_freq and rx_freq".into());
            }
        }
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

    // Optional duplex_table section
    if let Some(ref dt) = root.duplex_table {
        if !dt.extra.is_empty() {
            return Err(format!("Unrecognized fields in duplex_table config: {:?}", sorted_keys(&dt.extra)).into());
        }
    }

    // Build the codeplug (Plane B, optional). RF resolution + validation errors
    // surface here with a descriptive message.
    let codeplug = codeplug_dto_to_cfg(root.folder, root.talkgroup, root.network, root.carrier_override, root.frequency_list)?;
    codeplug.validate()?;

    // Build config from required and optional values
    let mut cfg = StackConfig {
        stack_mode: root.stack_mode,
        debug_log: root.debug_log,
        phy_io: phy_dto_to_cfg(root.phy_io),
        net: net_dto_to_cfg(root.net_info),
        cell: cell_dto_to_cfg(root.cell_info),
        duplex_table: match root.duplex_table {
            Some(dt) => duplex_dto_to_cfg(dt)?,
            None => Default::default(),
        },
        codeplug,
        ms: root.ms.map(ms_dto_to_cfg),
        brew: None,
        telemetry: None,
        control: None,
    };

    // Radio-style MS: seed the SDR's initial RX (downlink) center from the first
    // programmed scan candidate when no explicit tx_freq was authored. The MLE
    // scan/cell-selection engine retunes it at runtime; the uplink is left unset
    // until the MS camps and derives it from the cell's SYSINFO (EN 300 392-2
    // cl. 18.4.2.2).
    if cfg.stack_mode == StackMode::Ms {
        if let Some(soapy) = cfg.phy_io.soapysdr.as_mut() {
            if soapy.dl_freq == 0.0 {
                if let Some(first) = cfg.codeplug.scan_candidate_frequencies().first().copied() {
                    soapy.dl_freq = first as f64;
                    soapy.dl_freq_seeded = true;
                }
            }
        }
    }

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
    #[serde(skip_serializing_if = "Option::is_none")]
    duplex_table: Option<DuplexTableDto>,

    // Codeplug sections (Plane B, arrays-of-tables). All optional/additive.
    #[serde(skip_serializing_if = "Option::is_none")]
    folder: Option<Vec<FolderDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    talkgroup: Option<Vec<TalkgroupDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network: Option<Vec<NetworkDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    carrier_override: Option<Vec<CarrierOverrideDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency_list: Option<Vec<FrequencyListDto>>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    extra: HashMap<String, Value>,
}

/// Redaction sentinel written in place of secret values by
/// [`to_toml_string_redacted`] and recognised by [`restore_redacted_secrets`]
/// (**NON-STANDARD**, Plane B). A secret arriving from the wire equal to this
/// token means "unchanged" — the current on-disk value is preserved rather than
/// overwritten with the token. Matches `SecretField`'s log redaction string.
pub const REDACTED_SECRET: &str = "********";

fn cfg_to_root(cfg: &StackConfig) -> TomlConfigRoot {
    TomlConfigRoot {
        config_version: CURRENT_CONFIG_VERSION.to_string(),
        stack_mode: cfg.stack_mode,
        debug_log: cfg.debug_log.clone(),
        phy_io: cfg_to_phy_dto(&cfg.phy_io),
        net_info: cfg_to_net_dto(&cfg.net),
        cell_info: cfg_to_cell_dto(&cfg.cell),
        brew: cfg.brew.as_ref().map(cfg_brew_to_dto),
        telemetry: cfg.telemetry.as_ref().map(cfg_telemetry_to_dto),
        command: cfg.control.as_ref().map(cfg_control_to_dto),
        ms: cfg.ms.as_ref().map(cfg_to_ms_dto),
        duplex_table: if duplex_table_is_default(&cfg.duplex_table) {
            None
        } else {
            Some(cfg_to_duplex_dto(&cfg.duplex_table))
        },
        folder: cfg_to_folder_dtos(&cfg.codeplug),
        talkgroup: cfg_to_talkgroup_dtos(&cfg.codeplug),
        network: cfg_to_network_dtos(&cfg.codeplug),
        carrier_override: cfg_to_carrier_override_dtos(&cfg.codeplug),
        frequency_list: cfg_to_frequency_list_dtos(&cfg.codeplug),
        extra: HashMap::new(),
    }
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
/// **This is the on-disk path and writes real secret values as plaintext** (the
/// DTO secret fields are plain `String`s and the TOML file is their canonical
/// store; `SecretField` redaction applies to logs only and is not routed through
/// here). For the over-the-wire read path use [`to_toml_string_redacted`].
pub fn to_toml_string(cfg: &StackConfig) -> Result<String, String> {
    let root = cfg_to_root(cfg);
    toml::to_string_pretty(&root).map_err(|e| format!("TOML serialization failed: {e}"))
}

/// Like [`to_toml_string`] but with every secret value replaced by
/// [`REDACTED_SECRET`] (**NON-STANDARD**, Plane B GetConfig wire path).
///
/// Used when serving the config to a (possibly remote) UI: plaintext
/// credentials must never leave the process. The UI edits the redacted document
/// and sends it back; [`restore_redacted_secrets`] then preserves the real
/// secrets on write-back so the round-trip does not clobber them with the token.
/// Redacted fields: control/telemetry HTTP Basic passwords and the brew password.
pub fn to_toml_string_redacted(cfg: &StackConfig) -> Result<String, String> {
    let mut root = cfg_to_root(cfg);
    if let Some(c) = root.command.as_mut() {
        if c.password.is_some() {
            c.password = Some(REDACTED_SECRET.to_string());
        }
    }
    if let Some(t) = root.telemetry.as_mut() {
        if t.password.is_some() {
            t.password = Some(REDACTED_SECRET.to_string());
        }
    }
    if let Some(b) = root.brew.as_mut() {
        if !b.password.is_empty() {
            b.password = REDACTED_SECRET.to_string();
        }
    }
    toml::to_string_pretty(&root).map_err(|e| format!("TOML serialization failed: {e}"))
}

/// Restore secrets that a UI sent back unchanged (equal to [`REDACTED_SECRET`])
/// from the currently-active `current` config into `incoming`
/// (**NON-STANDARD**, Plane B SetConfig write path).
///
/// A UI reads a redacted config ([`to_toml_string_redacted`]), edits unrelated
/// fields and posts it back; the secret fields still carry the sentinel. This
/// substitutes the real current value for each sentinel so a benign round-trip
/// never overwrites a live secret with the token. A genuinely new secret value
/// (not the sentinel) is kept as supplied. Removing a section entirely is an
/// honest edit and is left as-is (the secret is dropped with its section).
pub fn restore_redacted_secrets(mut incoming: StackConfig, current: &StackConfig) -> StackConfig {
    if let Some(ctrl) = incoming.control.as_mut() {
        if let Some((user, pass)) = ctrl.credentials.clone() {
            if pass == REDACTED_SECRET {
                let restored = current
                    .control
                    .as_ref()
                    .and_then(|c| c.credentials.as_ref())
                    .map(|(_, cp)| cp.clone())
                    .unwrap_or(pass);
                ctrl.credentials = Some((user, restored));
            }
        }
    }
    if let Some(tel) = incoming.telemetry.as_mut() {
        if let Some((user, pass)) = tel.credentials.clone() {
            if pass == REDACTED_SECRET {
                let restored = current
                    .telemetry
                    .as_ref()
                    .and_then(|t| t.credentials.as_ref())
                    .map(|(_, cp)| cp.clone())
                    .unwrap_or(pass);
                tel.credentials = Some((user, restored));
            }
        }
    }
    if let Some(brew) = incoming.brew.as_mut() {
        if let Some(pass) = brew.password.clone() {
            if pass.as_ref() == REDACTED_SECRET {
                if let Some(cur) = current.brew.as_ref().and_then(|b| b.password.clone()) {
                    brew.password = Some(cur);
                }
            }
        }
    }
    incoming
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

    // A radio-style MS that omits tx_freq/rx_freq and the [cell_info] RF block:
    // the RX is seeded from the scan list and the UL is derived over the air.
    const MS_TOML_NO_FIXED_RF: &str = r#"
config_version = "0.6"
stack_mode = "Ms"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
ppm_err = 0
device = "driver=sx"
sample_rate = 600000

[net_info]
mcc = 901
mnc = 9999

[cell_info]
location_area = 1
colour_code = 1

[ms]
issi = 1000001
subscriber_class = 1
attach_groups = []

[[frequency_list]]
name = "primary"
mode = "List"
frequencies = [439825000, 439850000]
dwell_ms = 800
"#;

    #[test]
    fn ms_without_fixed_rf_seeds_rx_and_validates() {
        let cfg = from_toml_str(MS_TOML_NO_FIXED_RF).expect("parse radio-style MS");
        cfg.validate().expect("validate radio-style MS");

        let soapy = cfg.phy_io.soapysdr.as_ref().expect("soapy");
        // RX seeded from the first scan candidate; UL left unset until camp.
        assert_eq!(soapy.dl_freq, 439_825_000.0);
        assert!(soapy.dl_freq_seeded);
        assert_eq!(soapy.ul_freq, 0.0);

        // cell_info RF fields are absent -> default to zero (unused for MS).
        assert_eq!(cfg.cell.freq_band, 0);
        assert_eq!(cfg.cell.main_carrier, 0);
        // Non-RF identity fields still parse.
        assert_eq!(cfg.cell.location_area, 1);
    }

    #[test]
    fn ms_without_fixed_rf_roundtrips_without_reintroducing_rf() {
        let cfg = from_toml_str(MS_TOML_NO_FIXED_RF).expect("parse");
        let rendered = to_toml_string(&cfg).expect("serialize");
        // The seeded RX and unset UL must NOT be written back as fixed freqs,
        // and the cell RF block must stay omitted.
        assert!(!rendered.contains("tx_freq"), "seeded RX must not serialize as tx_freq");
        assert!(!rendered.contains("rx_freq"), "unset UL must not serialize as rx_freq");
        assert!(!rendered.contains("main_carrier"), "MS cell RF must stay omitted");
        assert!(!rendered.contains("freq_band"), "MS cell RF must stay omitted");
        // Re-parses through the same validator.
        from_toml_str(&rendered).expect("reparse").validate().expect("revalidate");
    }

    #[test]
    fn bs_requires_cell_and_soapy_rf() {
        // A BS config lacking cell_info RF must be rejected (the BS defines the cell).
        const BS_NO_RF: &str = r#"
config_version = "0.6"
stack_mode = "Bs"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
tx_freq = 439825000
rx_freq = 430425000
ppm_err = 0

[net_info]
mcc = 901
mnc = 9999

[cell_info]
location_area = 1
colour_code = 1
"#;
        assert!(from_toml_str(BS_NO_RF).is_err(), "BS without cell_info RF must error");
    }

    #[test]
    fn to_toml_string_emits_config_version() {
        let cfg = from_toml_str(MS_TOML).expect("initial parse");
        let rendered = to_toml_string(&cfg).expect("serialize");
        assert!(rendered.contains(&format!("config_version = \"{}\"", CURRENT_CONFIG_VERSION)));
    }

    // An MS config whose duplex spacing comes from a programmed [duplex_table]
    // override (index 7) instead of a per-channel custom_duplex_spacing.
    const MS_TOML_DUPLEX_TABLE: &str = r#"
config_version = "0.6"
stack_mode = "Ms"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
tx_freq = 439825000
rx_freq = 430425000
device = "driver=sx"
sample_rate = 600000
rx_antenna = "RX"
tx_antenna = "TX"
rx_gain_lna = 48.0
tx_gain_dac = 0.0

[net_info]
mcc = 901
mnc = 9999

[cell_info]
freq_band = 4
main_carrier = 1593
duplex_spacing = 7
freq_offset = 0
reverse_operation = false
location_area = 1
colour_code = 1

[duplex_table]
overrides = [[7, 9400000]]

[ms]
issi = 1000001
subscriber_class = 1
attach_groups = []
"#;

    #[test]
    fn duplex_table_section_drives_resolution_and_roundtrips() {
        // Parsing succeeds only because the [duplex_table] override supplies the
        // spacing for index 7 (which has no ETSI default), letting validate()'s
        // derived DL/UL match the configured soapy freqs.
        let cfg = from_toml_str(MS_TOML_DUPLEX_TABLE).expect("parse with duplex_table");
        assert_eq!(cfg.duplex_table.entries()[7], Some(9_400_000));

        // Round-trip: the section must survive serialize -> reparse.
        let rendered = to_toml_string(&cfg).expect("serialize");
        assert!(rendered.contains("[duplex_table]"));
        let reparsed = from_toml_str(&rendered).expect("reparse");
        assert_eq!(reparsed.duplex_table.entries()[7], Some(9_400_000));
    }

    #[test]
    fn default_config_omits_duplex_table_section() {
        // MS_TOML has no [duplex_table]; the serialized form must not invent one.
        let cfg = from_toml_str(MS_TOML).expect("parse");
        assert!(super::duplex_table_is_default(&cfg.duplex_table));
        let rendered = to_toml_string(&cfg).expect("serialize");
        assert!(!rendered.contains("[duplex_table]"));
    }

    #[test]
    fn duplex_table_rejects_bad_index() {
        let bad = MS_TOML_DUPLEX_TABLE.replace("[[7, 9400000]]", "[[9, 9400000]]");
        assert!(from_toml_str(&bad).is_err());
    }

    #[test]
    fn legacy_config_version_still_accepted() {
        // MS_TOML declares config_version = "0.6" (legacy, pre-codeplug).
        assert!(MS_TOML.contains("config_version = \"0.6\""));
        let cfg = from_toml_str(MS_TOML).expect("legacy 0.6 config must still load");
        assert!(cfg.codeplug.is_empty());
    }

    #[test]
    fn unsupported_config_version_rejected() {
        let bad = MS_TOML.replace("config_version = \"0.6\"", "config_version = \"9.9\"");
        assert!(from_toml_str(&bad).is_err());
    }

    // A radio-style MS config carrying a codeplug: folders, talkgroups, allowed
    // networks, carrier overrides (both RF forms), and a list-mode scan set.
    const MS_TOML_CODEPLUG: &str = r#"
config_version = "0.7"
stack_mode = "Ms"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
tx_freq = 439825000
rx_freq = 430425000
device = "driver=sx"
sample_rate = 600000
rx_antenna = "RX"
tx_antenna = "TX"
rx_gain_lna = 48.0
tx_gain_dac = 0.0

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

[[folder]]
id = "work"
name = "Work"
order = 1

[[talkgroup]]
gssi = 101
name = "Dispatch"
folder = "work"
class_of_usage = 0
order = 1

[[network]]
mcc = 901
mnc = 9999
name = "Home"
priority = 0

[[carrier_override]]
name = "BS-1"
band = 4
carrier = 1593
freq_offset = 0
colour_code = 1
duplex_index = 7
custom_duplex_spacing = 9400000
rx_only = true

[[carrier_override]]
name = "BS-2-byfreq"
dl_freq = 439850000

[[frequency_list]]
name = "primary"
mode = "List"
frequencies = [439825000, 439850000]
dwell_ms = 800
"#;

    #[test]
    fn codeplug_parses_and_roundtrips() {
        let cfg = from_toml_str(MS_TOML_CODEPLUG).expect("parse codeplug config");
        assert_eq!(cfg.codeplug.carrier_overrides.len(), 2);
        assert_eq!(cfg.codeplug.folders.len(), 1);
        assert_eq!(cfg.codeplug.talkgroups.len(), 1);
        assert_eq!(cfg.codeplug.networks.len(), 1);
        assert_eq!(cfg.codeplug.frequency_lists.len(), 1);
        // dl_freq form resolved to band/carrier.
        let co2 = cfg.codeplug.carrier_override("BS-2-byfreq").expect("carrier_override");
        assert_eq!(co2.dl_freq_hz(), 439_850_000);
        assert_eq!((co2.freq_band, co2.main_carrier, co2.freq_offset_hz), (4, 1594, 0));

        // Round-trip: serialize -> reparse must preserve the codeplug.
        let rendered = to_toml_string(&cfg).expect("serialize");
        assert!(rendered.contains("[[carrier_override]]"));
        assert!(rendered.contains("[[frequency_list]]"));
        let reparsed = from_toml_str(&rendered).expect("reparse");
        assert_eq!(reparsed.codeplug, cfg.codeplug);
    }

    #[test]
    fn codeplug_invalid_reference_rejected() {
        // Talkgroup references a folder that does not exist.
        let bad = MS_TOML_CODEPLUG.replace(r#"folder = "work""#, r#"folder = "ghost""#);
        assert!(from_toml_str(&bad).is_err());
    }

    #[test]
    fn shipped_example_configs_parse_and_validate() {
        // The example configs at the repo root must always load and validate.
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../example_config/");
        for name in ["config.toml", "config-ms.toml"] {
            let path = format!("{root}{name}");
            let cfg = from_file(&path).unwrap_or_else(|e| panic!("failed to parse {name}: {e}"));
            cfg.validate().unwrap_or_else(|e| panic!("failed to validate {name}: {e}"));
        }
    }

    // An MS config that additionally configures a control endpoint with HTTP
    // Basic credentials, so the secret redaction/restore paths have a secret.
    const MS_TOML_WITH_SECRET: &str = r#"
config_version = "0.6"
stack_mode = "Ms"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
tx_freq = 439825000
rx_freq = 430425000
device = "driver=sx"
sample_rate = 600000
rx_antenna = "RX"
tx_antenna = "TX"
rx_gain_lna = 48.0
tx_gain_dac = 0.0

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

[command]
host = "127.0.0.1"
port = 9000
username = "ui-operator"
password = "supersecret"
"#;

    #[test]
    fn to_toml_string_redacted_hides_secrets_but_keeps_username() {
        let cfg = from_toml_str(MS_TOML_WITH_SECRET).expect("parse");
        let disk = to_toml_string(&cfg).expect("disk serialize");
        let wire = to_toml_string_redacted(&cfg).expect("wire serialize");

        // On-disk keeps the real secret; the wire form must not leak it.
        assert!(disk.contains("supersecret"));
        assert!(!wire.contains("supersecret"), "wire form must not leak the password");
        assert!(wire.contains(REDACTED_SECRET), "wire form carries the sentinel");
        // Non-secret fields still travel so the UI can display/edit them.
        assert!(wire.contains("ui-operator"));
        assert!(wire.contains("127.0.0.1"));
    }

    #[test]
    fn restore_redacted_secrets_preserves_unchanged_secret() {
        let current = from_toml_str(MS_TOML_WITH_SECRET).expect("current");
        // Simulate a UI that read the redacted config and posted it back verbatim.
        let wire = to_toml_string_redacted(&current).expect("wire");
        let incoming = from_toml_str(&wire).expect("reparse wire");
        assert_eq!(
            incoming.control.as_ref().unwrap().credentials.as_ref().unwrap().1,
            REDACTED_SECRET,
            "sanity: incoming carries the sentinel"
        );

        let merged = restore_redacted_secrets(incoming, &current);
        // The real secret is restored, never persisted as the sentinel.
        assert_eq!(merged.control.as_ref().unwrap().credentials.as_ref().unwrap().1, "supersecret");
        let disk = to_toml_string(&merged).expect("disk");
        assert!(disk.contains("supersecret"));
        assert!(!disk.contains(REDACTED_SECRET));
    }

    #[test]
    fn restore_redacted_secrets_keeps_a_genuinely_new_secret() {
        let current = from_toml_str(MS_TOML_WITH_SECRET).expect("current");
        // UI supplies a brand-new password (not the sentinel).
        let updated = MS_TOML_WITH_SECRET.replace("supersecret", "rotated-pass");
        let incoming = from_toml_str(&updated).expect("parse updated");

        let merged = restore_redacted_secrets(incoming, &current);
        assert_eq!(merged.control.as_ref().unwrap().credentials.as_ref().unwrap().1, "rotated-pass");
    }
}
