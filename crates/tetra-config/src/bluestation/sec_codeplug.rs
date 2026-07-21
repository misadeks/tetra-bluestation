//! Codeplug: a radio-style programming structure for MS mode (**Plane B**,
//! non-standard management surface).
//!
//! ETSI does not define a "codeplug" — it is a permitted implementation choice
//! for how an operator programs a radio. Every *value* carried here, however,
//! maps to a real air-interface element and is validated against its ETSI
//! bit-width / allowed set:
//!   - channel band/carrier/offset -> the frequency element math of
//!     EN 300 392-2 / TS 100 392-15 (DL = band*100 MHz + carrier*25 kHz + offset);
//!   - `colour_code` -> D-MLE-SYNC colour code, 6 bits (EN 300 392-2 cl. 18.4.2.1);
//!   - `duplex_index` -> the 3-bit duplex-spacing field (TS 100 392-15 cl. 6);
//!   - talkgroup `gssi` -> 24-bit GSSI; `class_of_usage` -> 3-bit class of usage;
//!   - `allowed_networks` mcc/mnc -> 10-bit MCC / 14-bit MNC (cl. 18.4.2.1).
//!
//! The codeplug is *data only*: it is read by the management/TNMM UI and (in a
//! later phase) by the cell-selection/scan engine. It does not itself change any
//! on-air behaviour.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use toml::Value;

// --- ETSI-derived field bounds (for validation only) ---
const MAX_FREQ_BAND: u8 = 8; // FreqInfo/from_components accepts band 0..=8
const MAX_CARRIER: u16 = 3999; // 12-bit main carrier, FreqInfo requires < 4000
const MAX_COLOUR_CODE: u8 = 63; // 6-bit colour code (EN 300 392-2 cl. 18.4.2.1)
const MAX_DUPLEX_INDEX: u8 = 7; // 3-bit duplex-spacing field (TS 100 392-15 cl. 6)
const MAX_GSSI: u32 = 0xFF_FFFF; // 24-bit group short subscriber identity
const MAX_CLASS_OF_USAGE: u8 = 7; // 3-bit class of usage (EN 300 392-2)
const MAX_MCC: u16 = 1023; // 10-bit Mobile Country Code
const MAX_MNC: u16 = 16383; // 14-bit Mobile Network Code

/// Compute the absolute downlink frequency in Hz for a `(band, carrier, offset)`
/// triple, mirroring [`tetra_core::freqs::FreqInfo::get_freqs`] DL math.
pub fn channel_dl_hz(band: u8, carrier: u16, freq_offset_hz: i16) -> u32 {
    (band as i64 * 100_000_000 + carrier as i64 * 25_000 + freq_offset_hz as i64) as u32
}

/// Reverse of [`channel_dl_hz`] for the DTO `dl_freq` convenience form: split an
/// absolute DL frequency into `(band, carrier, freq_offset)`.
///
/// Only the offsets reachable from a positive 25 kHz remainder are derivable
/// (0, +6250, +12500). A channel needing the -6250 offset must be programmed in
/// the explicit `band` + `carrier` form.
fn components_from_dl_hz(dl_hz: u32) -> Result<(u8, u16, i16), String> {
    let band = (dl_hz / 100_000_000) as u8;
    let rem = dl_hz - band as u32 * 100_000_000;
    let carrier = (rem / 25_000) as u16;
    let off = (rem - carrier as u32 * 25_000) as i16;
    let freq_offset = match off {
        0 => 0,
        6250 => 6250,
        12500 => 12500,
        _ => {
            return Err(format!(
                "dl_freq {} Hz does not fall on a valid 25 kHz grid + offset (0/6250/12500); \
                 use explicit band+carrier for a -6250 offset",
                dl_hz
            ));
        }
    };
    Ok((band, carrier, freq_offset))
}

/// Operating mode for the MS channel selector (**[impl policy]**).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum ScanMode {
    /// Camp on a single programmed channel; no scanning.
    Fixed,
    /// Cycle through a programmed list of channels.
    List,
    /// Enumerate a frequency range (band + carrier start/stop/step).
    Range,
}

impl Default for ScanMode {
    fn default() -> Self {
        ScanMode::Fixed
    }
}

/// A group folder in the codeplug tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgFolder {
    /// Stable identifier referenced by channels/talkgroups.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Display order within the tree.
    pub order: u32,
}

/// A programmed channel: an RF carrier the MS may camp on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgChannel {
    /// Unique channel name (codeplug key).
    pub name: String,
    /// Frequency band (100 MHz increments).
    pub freq_band: u8,
    /// Main carrier number (12-bit).
    pub main_carrier: u16,
    /// Offset from the 25 kHz carrier: 0, 6250, -6250 or 12500 Hz.
    pub freq_offset_hz: i16,
    /// Optional colour-code filter (only camp on a cell with this colour code).
    pub colour_code: Option<u8>,
    /// Optional duplex-spacing index hint (else derived from the cell's SYSINFO).
    pub duplex_index: Option<u8>,
    /// Optional per-channel custom duplex spacing in Hz.
    pub custom_duplex_spacing: Option<u32>,
    /// Folder id this channel belongs to.
    pub folder: Option<String>,
    /// Receive-only: never transmit (registration/uplink suppressed) on this channel.
    pub rx_only: bool,
    /// Default talkgroups (GSSIs) associated with this channel.
    pub default_talkgroups: Vec<u32>,
}

impl CfgChannel {
    /// Absolute downlink frequency in Hz.
    pub fn dl_freq_hz(&self) -> u32 {
        channel_dl_hz(self.freq_band, self.main_carrier, self.freq_offset_hz)
    }
}

/// A programmed talkgroup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgTalkgroup {
    /// Group Short Subscriber Identity (24-bit).
    pub gssi: u32,
    /// Human-readable name.
    pub name: String,
    /// Folder id this talkgroup belongs to.
    pub folder: Option<String>,
    /// Optional class of usage (3-bit, EN 300 392-2 group identity).
    pub class_of_usage: Option<u8>,
}

/// A permitted network (MCC/MNC) for cell suitability filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CfgAllowedNetwork {
    pub mcc: u16,
    pub mnc: u16,
}

/// A carrier range for `ScanMode::Range` enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgScanRange {
    pub band: u8,
    pub start_carrier: u16,
    pub stop_carrier: u16,
    /// Step in carrier units (multiples of 25 kHz). Must be >= 1.
    pub step: u16,
}

/// Scan / channel-selection configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgScan {
    pub mode: ScanMode,
    /// Channel names used for `Fixed` (first entry) and `List` modes.
    pub channels: Vec<String>,
    /// Carrier range used for `Range` mode.
    pub range: Option<CfgScanRange>,
    /// Per-candidate dwell time in milliseconds.
    pub dwell_ms: u32,
    /// Permitted networks; empty = home MCC/MNC only.
    pub allowed_networks: Vec<CfgAllowedNetwork>,
}

impl Default for CfgScan {
    fn default() -> Self {
        Self {
            mode: ScanMode::Fixed,
            channels: Vec::new(),
            range: None,
            dwell_ms: 1000,
            allowed_networks: Vec::new(),
        }
    }
}

/// The complete codeplug: folders, channels, talkgroups and scan settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CfgCodeplug {
    pub folders: Vec<CfgFolder>,
    pub channels: Vec<CfgChannel>,
    pub talkgroups: Vec<CfgTalkgroup>,
    /// Present only when a `[scan]` section is configured.
    pub scan: Option<CfgScan>,
}

impl CfgCodeplug {
    /// True when nothing is programmed (so the whole codeplug can be omitted
    /// from serialized output and skipped by validation).
    pub fn is_empty(&self) -> bool {
        self.folders.is_empty() && self.channels.is_empty() && self.talkgroups.is_empty() && self.scan.is_none()
    }

    /// Look up a channel by name.
    pub fn channel(&self, name: &str) -> Option<&CfgChannel> {
        self.channels.iter().find(|c| c.name == name)
    }

    /// Validate ranges (against ETSI bit-widths) and cross-references
    /// (folder/channel/talkgroup ids). Returns a human-readable error string.
    pub fn validate(&self) -> Result<(), String> {
        if self.is_empty() {
            return Ok(());
        }

        // Folders: unique, non-empty ids.
        let mut folder_ids: HashSet<&str> = HashSet::new();
        for f in &self.folders {
            if f.id.trim().is_empty() {
                return Err("codeplug folder id must not be empty".to_string());
            }
            if f.name.trim().is_empty() {
                return Err(format!("codeplug folder '{}' name must not be empty", f.id));
            }
            if !folder_ids.insert(f.id.as_str()) {
                return Err(format!("duplicate codeplug folder id '{}'", f.id));
            }
        }

        // Talkgroups: valid GSSI, unique, valid class of usage, existing folder.
        let mut gssis: HashSet<u32> = HashSet::new();
        for tg in &self.talkgroups {
            if tg.gssi == 0 || tg.gssi > MAX_GSSI {
                return Err(format!("talkgroup '{}' gssi must be a 24-bit value (1..={})", tg.name, MAX_GSSI));
            }
            if !gssis.insert(tg.gssi) {
                return Err(format!("duplicate talkgroup gssi {}", tg.gssi));
            }
            if tg.name.trim().is_empty() {
                return Err(format!("talkgroup gssi {} name must not be empty", tg.gssi));
            }
            if let Some(cou) = tg.class_of_usage {
                if cou > MAX_CLASS_OF_USAGE {
                    return Err(format!(
                        "talkgroup gssi {} class_of_usage must be 0..={} (3-bit)",
                        tg.gssi, MAX_CLASS_OF_USAGE
                    ));
                }
            }
            if let Some(ref folder) = tg.folder {
                if !folder_ids.contains(folder.as_str()) {
                    return Err(format!("talkgroup gssi {} references unknown folder '{}'", tg.gssi, folder));
                }
            }
        }

        // Channels: unique names, valid RF params, existing folder + talkgroups.
        let mut channel_names: HashSet<&str> = HashSet::new();
        for ch in &self.channels {
            if ch.name.trim().is_empty() {
                return Err("codeplug channel name must not be empty".to_string());
            }
            if !channel_names.insert(ch.name.as_str()) {
                return Err(format!("duplicate codeplug channel name '{}'", ch.name));
            }
            if ch.freq_band > MAX_FREQ_BAND {
                return Err(format!("channel '{}' freq_band must be 0..={}", ch.name, MAX_FREQ_BAND));
            }
            if ch.main_carrier > MAX_CARRIER {
                return Err(format!("channel '{}' main_carrier must be 0..={}", ch.name, MAX_CARRIER));
            }
            if !matches!(ch.freq_offset_hz, 0 | 6250 | -6250 | 12500) {
                return Err(format!(
                    "channel '{}' freq_offset must be one of 0, 6250, -6250, 12500 Hz",
                    ch.name
                ));
            }
            if let Some(cc) = ch.colour_code {
                if cc > MAX_COLOUR_CODE {
                    return Err(format!("channel '{}' colour_code must be 0..={} (6-bit)", ch.name, MAX_COLOUR_CODE));
                }
            }
            if let Some(di) = ch.duplex_index {
                if di > MAX_DUPLEX_INDEX {
                    return Err(format!("channel '{}' duplex_index must be 0..={} (3-bit)", ch.name, MAX_DUPLEX_INDEX));
                }
            }
            if let Some(ref folder) = ch.folder {
                if !folder_ids.contains(folder.as_str()) {
                    return Err(format!("channel '{}' references unknown folder '{}'", ch.name, folder));
                }
            }
            for gssi in &ch.default_talkgroups {
                if *gssi == 0 || *gssi > MAX_GSSI {
                    return Err(format!("channel '{}' default talkgroup {} is not a valid GSSI", ch.name, gssi));
                }
                if !self.talkgroups.is_empty() && !gssis.contains(gssi) {
                    return Err(format!(
                        "channel '{}' default talkgroup {} is not a programmed talkgroup",
                        ch.name, gssi
                    ));
                }
            }
        }

        // Scan.
        if let Some(ref scan) = self.scan {
            if scan.dwell_ms == 0 {
                return Err("scan dwell_ms must be greater than 0".to_string());
            }
            for net in &scan.allowed_networks {
                if net.mcc > MAX_MCC {
                    return Err(format!("scan allowed_networks mcc {} exceeds 10-bit range", net.mcc));
                }
                if net.mnc > MAX_MNC {
                    return Err(format!("scan allowed_networks mnc {} exceeds 14-bit range", net.mnc));
                }
            }
            match scan.mode {
                ScanMode::Fixed => {
                    if scan.channels.len() != 1 {
                        return Err("scan mode 'Fixed' requires exactly one channel".to_string());
                    }
                }
                ScanMode::List => {
                    if scan.channels.is_empty() {
                        return Err("scan mode 'List' requires at least one channel".to_string());
                    }
                }
                ScanMode::Range => {
                    let Some(ref r) = scan.range else {
                        return Err("scan mode 'Range' requires a [scan.range] section".to_string());
                    };
                    if r.band > MAX_FREQ_BAND {
                        return Err(format!("scan range band must be 0..={}", MAX_FREQ_BAND));
                    }
                    if r.step == 0 {
                        return Err("scan range step must be >= 1".to_string());
                    }
                    if r.start_carrier > MAX_CARRIER || r.stop_carrier > MAX_CARRIER {
                        return Err(format!("scan range carriers must be 0..={}", MAX_CARRIER));
                    }
                    if r.start_carrier >= r.stop_carrier {
                        return Err("scan range start_carrier must be < stop_carrier".to_string());
                    }
                }
            }
            // List/Fixed channel names must reference programmed channels.
            if matches!(scan.mode, ScanMode::Fixed | ScanMode::List) {
                for name in &scan.channels {
                    if self.channel(name).is_none() {
                        return Err(format!("scan references unknown channel '{}'", name));
                    }
                }
            }
        }

        Ok(())
    }
}

// ----------------------- DTOs (on-disk TOML shape) -----------------------

#[derive(Default, Deserialize, Serialize)]
pub struct FolderDto {
    pub id: String,
    pub name: String,
    pub order: Option<u32>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct ChannelDto {
    pub name: String,
    /// Absolute downlink frequency in Hz (convenience; alternative to band+carrier).
    pub dl_freq: Option<u32>,
    pub band: Option<u8>,
    pub carrier: Option<u16>,
    pub freq_offset: Option<i16>,
    pub colour_code: Option<u8>,
    pub duplex_index: Option<u8>,
    pub custom_duplex_spacing: Option<u32>,
    pub folder: Option<String>,
    pub rx_only: Option<bool>,
    pub default_talkgroups: Option<Vec<u32>>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct TalkgroupDto {
    pub gssi: u32,
    pub name: String,
    pub folder: Option<String>,
    pub class_of_usage: Option<u8>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct AllowedNetworkDto {
    pub mcc: u16,
    pub mnc: u16,
}

#[derive(Default, Deserialize, Serialize)]
pub struct ScanRangeDto {
    pub band: u8,
    pub start_carrier: u16,
    pub stop_carrier: u16,
    pub step: Option<u16>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct ScanDto {
    pub mode: Option<ScanMode>,
    pub channels: Option<Vec<String>>,
    pub range: Option<ScanRangeDto>,
    pub dwell_ms: Option<u32>,
    pub allowed_networks: Option<Vec<AllowedNetworkDto>>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

fn folder_dto_to_cfg(dto: FolderDto) -> CfgFolder {
    CfgFolder {
        id: dto.id,
        name: dto.name,
        order: dto.order.unwrap_or(0),
    }
}

fn channel_dto_to_cfg(dto: ChannelDto) -> Result<CfgChannel, String> {
    // Resolve RF from either dl_freq or explicit band+carrier(+offset).
    let (freq_band, main_carrier, freq_offset_hz) = match (dto.dl_freq, dto.band, dto.carrier) {
        (Some(dl), None, None) => components_from_dl_hz(dl)?,
        (None, Some(band), Some(carrier)) => (band, carrier, dto.freq_offset.unwrap_or(0)),
        (Some(dl), Some(band), Some(carrier)) => {
            // Both forms given: they must agree.
            let off = dto.freq_offset.unwrap_or(0);
            if channel_dl_hz(band, carrier, off) != dl {
                return Err(format!(
                    "channel '{}': dl_freq {} Hz disagrees with band {} carrier {} offset {}",
                    dto.name, dl, band, carrier, off
                ));
            }
            (band, carrier, off)
        }
        _ => {
            return Err(format!(
                "channel '{}' must specify either dl_freq or both band and carrier",
                dto.name
            ));
        }
    };
    Ok(CfgChannel {
        name: dto.name,
        freq_band,
        main_carrier,
        freq_offset_hz,
        colour_code: dto.colour_code,
        duplex_index: dto.duplex_index,
        custom_duplex_spacing: dto.custom_duplex_spacing,
        folder: dto.folder,
        rx_only: dto.rx_only.unwrap_or(false),
        default_talkgroups: dto.default_talkgroups.unwrap_or_default(),
    })
}

fn talkgroup_dto_to_cfg(dto: TalkgroupDto) -> CfgTalkgroup {
    CfgTalkgroup {
        gssi: dto.gssi,
        name: dto.name,
        folder: dto.folder,
        class_of_usage: dto.class_of_usage,
    }
}

fn scan_dto_to_cfg(dto: ScanDto) -> CfgScan {
    CfgScan {
        mode: dto.mode.unwrap_or_default(),
        channels: dto.channels.unwrap_or_default(),
        range: dto.range.map(|r| CfgScanRange {
            band: r.band,
            start_carrier: r.start_carrier,
            stop_carrier: r.stop_carrier,
            step: r.step.unwrap_or(1),
        }),
        dwell_ms: dto.dwell_ms.unwrap_or(1000),
        allowed_networks: dto
            .allowed_networks
            .unwrap_or_default()
            .into_iter()
            .map(|n| CfgAllowedNetwork { mcc: n.mcc, mnc: n.mnc })
            .collect(),
    }
}

/// Assemble a [`CfgCodeplug`] from the parsed DTO sections. RF resolution errors
/// (e.g. an off-grid `dl_freq`) surface here; range/cross-reference validation is
/// performed separately by [`CfgCodeplug::validate`].
pub fn codeplug_dto_to_cfg(
    folders: Option<Vec<FolderDto>>,
    channels: Option<Vec<ChannelDto>>,
    talkgroups: Option<Vec<TalkgroupDto>>,
    scan: Option<ScanDto>,
) -> Result<CfgCodeplug, String> {
    let folders = folders.unwrap_or_default().into_iter().map(folder_dto_to_cfg).collect();
    let channels = channels
        .unwrap_or_default()
        .into_iter()
        .map(channel_dto_to_cfg)
        .collect::<Result<Vec<_>, _>>()?;
    let talkgroups = talkgroups.unwrap_or_default().into_iter().map(talkgroup_dto_to_cfg).collect();
    let scan = scan.map(scan_dto_to_cfg);
    Ok(CfgCodeplug {
        folders,
        channels,
        talkgroups,
        scan,
    })
}

/// Inverse projections for TOML write-back (Plane B). Runtime channels always
/// serialize in the explicit `band`+`carrier`+`freq_offset` form (lossless).
pub fn cfg_to_folder_dtos(cp: &CfgCodeplug) -> Option<Vec<FolderDto>> {
    if cp.folders.is_empty() {
        return None;
    }
    Some(
        cp.folders
            .iter()
            .map(|f| FolderDto {
                id: f.id.clone(),
                name: f.name.clone(),
                order: Some(f.order),
                extra: HashMap::new(),
            })
            .collect(),
    )
}

pub fn cfg_to_channel_dtos(cp: &CfgCodeplug) -> Option<Vec<ChannelDto>> {
    if cp.channels.is_empty() {
        return None;
    }
    Some(
        cp.channels
            .iter()
            .map(|c| ChannelDto {
                name: c.name.clone(),
                dl_freq: None,
                band: Some(c.freq_band),
                carrier: Some(c.main_carrier),
                freq_offset: Some(c.freq_offset_hz),
                colour_code: c.colour_code,
                duplex_index: c.duplex_index,
                custom_duplex_spacing: c.custom_duplex_spacing,
                folder: c.folder.clone(),
                rx_only: Some(c.rx_only),
                default_talkgroups: Some(c.default_talkgroups.clone()),
                extra: HashMap::new(),
            })
            .collect(),
    )
}

pub fn cfg_to_talkgroup_dtos(cp: &CfgCodeplug) -> Option<Vec<TalkgroupDto>> {
    if cp.talkgroups.is_empty() {
        return None;
    }
    Some(
        cp.talkgroups
            .iter()
            .map(|t| TalkgroupDto {
                gssi: t.gssi,
                name: t.name.clone(),
                folder: t.folder.clone(),
                class_of_usage: t.class_of_usage,
                extra: HashMap::new(),
            })
            .collect(),
    )
}

pub fn cfg_to_scan_dto(cp: &CfgCodeplug) -> Option<ScanDto> {
    cp.scan.as_ref().map(|s| ScanDto {
        mode: Some(s.mode),
        channels: if s.channels.is_empty() { None } else { Some(s.channels.clone()) },
        range: s.range.as_ref().map(|r| ScanRangeDto {
            band: r.band,
            start_carrier: r.start_carrier,
            stop_carrier: r.stop_carrier,
            step: Some(r.step),
            extra: HashMap::new(),
        }),
        dwell_ms: Some(s.dwell_ms),
        allowed_networks: if s.allowed_networks.is_empty() {
            None
        } else {
            Some(
                s.allowed_networks
                    .iter()
                    .map(|n| AllowedNetworkDto { mcc: n.mcc, mnc: n.mnc })
                    .collect(),
            )
        },
        extra: HashMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_channel(name: &str) -> CfgChannel {
        CfgChannel {
            name: name.to_string(),
            freq_band: 4,
            main_carrier: 1593,
            freq_offset_hz: 0,
            colour_code: Some(1),
            duplex_index: Some(7),
            custom_duplex_spacing: Some(9_400_000),
            folder: None,
            rx_only: true,
            default_talkgroups: vec![],
        }
    }

    #[test]
    fn test_channel_dl_math_matches_cell_info() {
        // band 4, carrier 1593, offset 0 -> 439.825 MHz, the example MS DL.
        assert_eq!(channel_dl_hz(4, 1593, 0), 439_825_000);
    }

    #[test]
    fn test_dl_freq_roundtrip_components() {
        let (b, c, o) = components_from_dl_hz(439_825_000).unwrap();
        assert_eq!((b, c, o), (4, 1593, 0));
        assert_eq!(channel_dl_hz(b, c, o), 439_825_000);
    }

    #[test]
    fn test_dl_freq_offset_6250() {
        let (b, c, o) = components_from_dl_hz(439_831_250).unwrap();
        assert_eq!((b, c, o), (4, 1593, 6250));
    }

    #[test]
    fn test_dl_freq_off_grid_rejected() {
        // 3 kHz past the carrier is not a valid offset.
        assert!(components_from_dl_hz(439_828_000).is_err());
    }

    #[test]
    fn test_channel_dto_dl_freq_form() {
        let dto = ChannelDto {
            name: "A".to_string(),
            dl_freq: Some(439_825_000),
            ..Default::default()
        };
        let ch = channel_dto_to_cfg(dto).unwrap();
        assert_eq!((ch.freq_band, ch.main_carrier, ch.freq_offset_hz), (4, 1593, 0));
    }

    #[test]
    fn test_channel_dto_band_carrier_form() {
        let dto = ChannelDto {
            name: "A".to_string(),
            band: Some(4),
            carrier: Some(1593),
            freq_offset: Some(-6250),
            ..Default::default()
        };
        let ch = channel_dto_to_cfg(dto).unwrap();
        assert_eq!(ch.freq_offset_hz, -6250);
        assert_eq!(ch.dl_freq_hz(), 439_825_000 - 6250);
    }

    #[test]
    fn test_channel_dto_conflicting_forms_rejected() {
        let dto = ChannelDto {
            name: "A".to_string(),
            dl_freq: Some(439_825_000),
            band: Some(4),
            carrier: Some(1000),
            ..Default::default()
        };
        assert!(channel_dto_to_cfg(dto).is_err());
    }

    #[test]
    fn test_validate_accepts_wellformed() {
        let cp = CfgCodeplug {
            folders: vec![CfgFolder {
                id: "work".to_string(),
                name: "Work".to_string(),
                order: 1,
            }],
            channels: vec![CfgChannel {
                folder: Some("work".to_string()),
                default_talkgroups: vec![101],
                ..sample_channel("BS-1")
            }],
            talkgroups: vec![CfgTalkgroup {
                gssi: 101,
                name: "Dispatch".to_string(),
                folder: Some("work".to_string()),
                class_of_usage: Some(0),
            }],
            scan: Some(CfgScan {
                mode: ScanMode::List,
                channels: vec!["BS-1".to_string()],
                allowed_networks: vec![CfgAllowedNetwork { mcc: 901, mnc: 9999 }],
                ..CfgScan::default()
            }),
        };
        assert!(cp.validate().is_ok(), "{:?}", cp.validate());
    }

    #[test]
    fn test_validate_rejects_unknown_folder() {
        let cp = CfgCodeplug {
            channels: vec![CfgChannel {
                folder: Some("nope".to_string()),
                ..sample_channel("BS-1")
            }],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_scan_unknown_channel() {
        let cp = CfgCodeplug {
            channels: vec![sample_channel("BS-1")],
            scan: Some(CfgScan {
                mode: ScanMode::List,
                channels: vec!["ghost".to_string()],
                ..CfgScan::default()
            }),
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_duplicate_channel_name() {
        let cp = CfgCodeplug {
            channels: vec![sample_channel("BS-1"), sample_channel("BS-1")],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_bad_gssi() {
        let cp = CfgCodeplug {
            talkgroups: vec![CfgTalkgroup {
                gssi: 0x100_0000, // 25-bit, too large
                name: "X".to_string(),
                folder: None,
                class_of_usage: None,
            }],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_validate_range_mode_requires_range() {
        let cp = CfgCodeplug {
            scan: Some(CfgScan {
                mode: ScanMode::Range,
                range: None,
                ..CfgScan::default()
            }),
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err());
    }
}
