//! Codeplug: a radio-style programming structure for MS mode (**Plane B**,
//! non-standard management surface).
//!
//! ETSI does not define a "codeplug" — it is a permitted implementation choice
//! for how an operator programs a radio. Every *value* carried here, however,
//! maps to a real air-interface element and is validated against its ETSI
//! bit-width / allowed set:
//!   - scan frequency / carrier_override band-carrier-offset -> the frequency
//!     element math of EN 300 392-2 / TS 100 392-15
//!     (DL = band*100 MHz + carrier*25 kHz + offset);
//!   - `colour_code` -> D-MLE-SYNC colour code, 6 bits (EN 300 392-2 cl. 18.4.2.1);
//!   - `duplex_index` -> the 3-bit duplex-spacing field (TS 100 392-15 cl. 6);
//!   - talkgroup `gssi` -> 24-bit GSSI; `class_of_usage` -> 3-bit class of usage;
//!   - `network` mcc/mnc -> 10-bit MCC / 14-bit MNC (cl. 18.4.2.1).
//!
//! Radio model (how a portable is programmed, not how a BS is defined):
//!   - **talkgroups** are the user-selectable groups (organised into **folders**,
//!     each with an explicit display `order`);
//!   - **networks** is the codeplug-wide list of allowed MCC/MNC (a cell is only
//!     suitable if its network is allowed); an empty list means "home network
//!     only" (the MCC/MNC from `[net_info]`);
//!   - the MS **scans** a list of downlink frequencies (or a carrier range),
//!     finds a suitable serving cell, camps on it and derives its uplink/duplex
//!     from that cell's D-MLE-SYSINFO (EN 300 392-2 cl. 18.4.2.2);
//!   - a **carrier_override** pins extra parameters (colour-code lock, custom
//!     duplex spacing, rx-only) to one specific frequency, applied when the
//!     scanner lands on it.
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

/// Reverse of [`channel_dl_hz`] for the `dl_freq` convenience form: split an
/// absolute DL frequency into `(band, carrier, freq_offset)`.
///
/// Only the offsets reachable from a positive 25 kHz remainder are derivable
/// (0, +6250, +12500). A carrier needing the -6250 offset must be programmed in
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
    /// Camp on a single programmed frequency; no scanning.
    Fixed,
    /// Cycle through a programmed list of frequencies.
    List,
    /// Enumerate a frequency range (band + carrier start/stop/step).
    Range,
}

impl Default for ScanMode {
    fn default() -> Self {
        ScanMode::Fixed
    }
}

/// A group folder in the codeplug tree (organises talkgroups).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgFolder {
    /// Stable identifier referenced by talkgroups.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Display order within the tree (ascending; ties broken by name).
    pub order: u32,
}

/// A programmed talkgroup — the user-selectable "channel" on the radio.
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
    /// Display order within its folder (ascending; ties broken by name).
    pub order: u32,
}

/// An allowed network (MCC/MNC) for cell suitability filtering. Codeplug-wide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgNetwork {
    /// Mobile Country Code (10-bit).
    pub mcc: u16,
    /// Mobile Network Code (14-bit).
    pub mnc: u16,
    /// Optional human-readable name.
    pub name: Option<String>,
    /// Selection priority (ascending: lower value = more preferred).
    pub priority: u32,
}

/// A per-frequency override: pins extra camp parameters to one specific carrier.
/// Applied when the scanner lands on this frequency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgCarrierOverride {
    /// Human-readable label (unique within the codeplug).
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
    /// Optional per-carrier custom duplex spacing in Hz.
    pub custom_duplex_spacing: Option<u32>,
    /// Receive-only: never transmit (registration/uplink suppressed) here.
    pub rx_only: bool,
}

impl CfgCarrierOverride {
    /// Absolute downlink frequency in Hz.
    pub fn dl_freq_hz(&self) -> u32 {
        channel_dl_hz(self.freq_band, self.main_carrier, self.freq_offset_hz)
    }
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
    /// Absolute downlink frequencies (Hz) used for `Fixed` (first entry) and
    /// `List` modes.
    pub frequencies: Vec<u32>,
    /// Carrier range used for `Range` mode.
    pub range: Option<CfgScanRange>,
    /// Per-candidate dwell time in milliseconds.
    pub dwell_ms: u32,
}

impl Default for CfgScan {
    fn default() -> Self {
        Self {
            mode: ScanMode::Fixed,
            frequencies: Vec::new(),
            range: None,
            dwell_ms: 1000,
        }
    }
}

impl CfgScan {
    /// Enumerate the downlink frequencies (Hz) this scan covers. For `Range`
    /// mode the carriers are expanded on the fly.
    pub fn candidate_frequencies(&self) -> Vec<u32> {
        match self.mode {
            ScanMode::Fixed => self.frequencies.iter().take(1).copied().collect(),
            ScanMode::List => self.frequencies.clone(),
            ScanMode::Range => {
                let mut out = Vec::new();
                if let Some(ref r) = self.range {
                    let step = r.step.max(1);
                    let mut c = r.start_carrier;
                    while c <= r.stop_carrier {
                        out.push(channel_dl_hz(r.band, c, 0));
                        c = c.saturating_add(step);
                    }
                }
                out
            }
        }
    }
}

/// The complete codeplug: folders, talkgroups, allowed networks, carrier
/// overrides and scan settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CfgCodeplug {
    pub folders: Vec<CfgFolder>,
    pub talkgroups: Vec<CfgTalkgroup>,
    /// Codeplug-wide allowed networks; empty = home MCC/MNC only.
    pub networks: Vec<CfgNetwork>,
    /// Per-frequency overrides, matched by downlink frequency.
    pub carrier_overrides: Vec<CfgCarrierOverride>,
    /// Present only when a `[scan]` section is configured.
    pub scan: Option<CfgScan>,
}

impl CfgCodeplug {
    /// True when nothing is programmed (so the whole codeplug can be omitted
    /// from serialized output and skipped by validation).
    pub fn is_empty(&self) -> bool {
        self.folders.is_empty()
            && self.talkgroups.is_empty()
            && self.networks.is_empty()
            && self.carrier_overrides.is_empty()
            && self.scan.is_none()
    }

    /// Look up a carrier override by name.
    pub fn carrier_override(&self, name: &str) -> Option<&CfgCarrierOverride> {
        self.carrier_overrides.iter().find(|c| c.name == name)
    }

    /// Find the carrier override (if any) programmed for a given downlink freq.
    pub fn carrier_override_for_freq(&self, dl_hz: u32) -> Option<&CfgCarrierOverride> {
        self.carrier_overrides.iter().find(|c| c.dl_freq_hz() == dl_hz)
    }

    /// Folders in display order (`order` ascending, ties broken by `name`).
    pub fn folders_sorted(&self) -> Vec<&CfgFolder> {
        let mut v: Vec<&CfgFolder> = self.folders.iter().collect();
        v.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
        v
    }

    /// Talkgroups belonging to `folder` (or the un-foldered ones when `folder`
    /// is `None`), in display order (`order` ascending, ties broken by `name`).
    pub fn talkgroups_in_folder(&self, folder: Option<&str>) -> Vec<&CfgTalkgroup> {
        let mut v: Vec<&CfgTalkgroup> = self
            .talkgroups
            .iter()
            .filter(|t| t.folder.as_deref() == folder)
            .collect();
        v.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
        v
    }

    /// Allowed networks in preference order (`priority` ascending, ties broken
    /// by mcc then mnc).
    pub fn networks_sorted(&self) -> Vec<&CfgNetwork> {
        let mut v: Vec<&CfgNetwork> = self.networks.iter().collect();
        v.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.mcc.cmp(&b.mcc))
                .then_with(|| a.mnc.cmp(&b.mnc))
        });
        v
    }

    /// Validate ranges (against ETSI bit-widths) and cross-references
    /// (folder ids). Returns a human-readable error string.
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

        // Networks: valid, unique MCC/MNC pairs.
        let mut nets: HashSet<(u16, u16)> = HashSet::new();
        for net in &self.networks {
            if net.mcc > MAX_MCC {
                return Err(format!("network mcc {} exceeds 10-bit range", net.mcc));
            }
            if net.mnc > MAX_MNC {
                return Err(format!("network mnc {} exceeds 14-bit range", net.mnc));
            }
            if !nets.insert((net.mcc, net.mnc)) {
                return Err(format!("duplicate network {}/{}", net.mcc, net.mnc));
            }
        }

        // Carrier overrides: unique names, valid RF params.
        let mut override_names: HashSet<&str> = HashSet::new();
        for co in &self.carrier_overrides {
            if co.name.trim().is_empty() {
                return Err("carrier_override name must not be empty".to_string());
            }
            if !override_names.insert(co.name.as_str()) {
                return Err(format!("duplicate carrier_override name '{}'", co.name));
            }
            validate_rf(&co.name, co.freq_band, co.main_carrier, co.freq_offset_hz)?;
            if let Some(cc) = co.colour_code {
                if cc > MAX_COLOUR_CODE {
                    return Err(format!("carrier_override '{}' colour_code must be 0..={} (6-bit)", co.name, MAX_COLOUR_CODE));
                }
            }
            if let Some(di) = co.duplex_index {
                if di > MAX_DUPLEX_INDEX {
                    return Err(format!("carrier_override '{}' duplex_index must be 0..={} (3-bit)", co.name, MAX_DUPLEX_INDEX));
                }
            }
        }

        // Scan.
        if let Some(ref scan) = self.scan {
            if scan.dwell_ms == 0 {
                return Err("scan dwell_ms must be greater than 0".to_string());
            }
            for f in &scan.frequencies {
                if *f == 0 {
                    return Err("scan frequency must be greater than 0 Hz".to_string());
                }
            }
            match scan.mode {
                ScanMode::Fixed => {
                    if scan.frequencies.len() != 1 {
                        return Err("scan mode 'Fixed' requires exactly one frequency".to_string());
                    }
                }
                ScanMode::List => {
                    if scan.frequencies.is_empty() {
                        return Err("scan mode 'List' requires at least one frequency".to_string());
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
        }

        Ok(())
    }
}

/// Validate a `(band, carrier, offset)` RF triple against ETSI bit-widths / the
/// allowed offset set.
fn validate_rf(label: &str, band: u8, carrier: u16, offset: i16) -> Result<(), String> {
    if band > MAX_FREQ_BAND {
        return Err(format!("'{}' freq_band must be 0..={}", label, MAX_FREQ_BAND));
    }
    if carrier > MAX_CARRIER {
        return Err(format!("'{}' main_carrier must be 0..={}", label, MAX_CARRIER));
    }
    if !matches!(offset, 0 | 6250 | -6250 | 12500) {
        return Err(format!("'{}' freq_offset must be one of 0, 6250, -6250, 12500 Hz", label));
    }
    Ok(())
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
pub struct TalkgroupDto {
    pub gssi: u32,
    pub name: String,
    pub folder: Option<String>,
    pub class_of_usage: Option<u8>,
    pub order: Option<u32>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct NetworkDto {
    pub mcc: u16,
    pub mnc: u16,
    pub name: Option<String>,
    pub priority: Option<u32>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct CarrierOverrideDto {
    pub name: String,
    /// Absolute downlink frequency in Hz (convenience; alternative to band+carrier).
    pub dl_freq: Option<u32>,
    pub band: Option<u8>,
    pub carrier: Option<u16>,
    pub freq_offset: Option<i16>,
    pub colour_code: Option<u8>,
    pub duplex_index: Option<u8>,
    pub custom_duplex_spacing: Option<u32>,
    pub rx_only: Option<bool>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
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
    pub frequencies: Option<Vec<u32>>,
    pub range: Option<ScanRangeDto>,
    pub dwell_ms: Option<u32>,
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

fn talkgroup_dto_to_cfg(dto: TalkgroupDto) -> CfgTalkgroup {
    CfgTalkgroup {
        gssi: dto.gssi,
        name: dto.name,
        folder: dto.folder,
        class_of_usage: dto.class_of_usage,
        order: dto.order.unwrap_or(0),
    }
}

fn network_dto_to_cfg(dto: NetworkDto) -> CfgNetwork {
    CfgNetwork {
        mcc: dto.mcc,
        mnc: dto.mnc,
        name: dto.name,
        priority: dto.priority.unwrap_or(0),
    }
}

fn carrier_override_dto_to_cfg(dto: CarrierOverrideDto) -> Result<CfgCarrierOverride, String> {
    // Resolve RF from either dl_freq or explicit band+carrier(+offset).
    let (freq_band, main_carrier, freq_offset_hz) = match (dto.dl_freq, dto.band, dto.carrier) {
        (Some(dl), None, None) => components_from_dl_hz(dl)?,
        (None, Some(band), Some(carrier)) => (band, carrier, dto.freq_offset.unwrap_or(0)),
        (Some(dl), Some(band), Some(carrier)) => {
            // Both forms given: they must agree.
            let off = dto.freq_offset.unwrap_or(0);
            if channel_dl_hz(band, carrier, off) != dl {
                return Err(format!(
                    "carrier_override '{}': dl_freq {} Hz disagrees with band {} carrier {} offset {}",
                    dto.name, dl, band, carrier, off
                ));
            }
            (band, carrier, off)
        }
        _ => {
            return Err(format!(
                "carrier_override '{}' must specify either dl_freq or both band and carrier",
                dto.name
            ));
        }
    };
    Ok(CfgCarrierOverride {
        name: dto.name,
        freq_band,
        main_carrier,
        freq_offset_hz,
        colour_code: dto.colour_code,
        duplex_index: dto.duplex_index,
        custom_duplex_spacing: dto.custom_duplex_spacing,
        rx_only: dto.rx_only.unwrap_or(false),
    })
}

fn scan_dto_to_cfg(dto: ScanDto) -> CfgScan {
    CfgScan {
        mode: dto.mode.unwrap_or_default(),
        frequencies: dto.frequencies.unwrap_or_default(),
        range: dto.range.map(|r| CfgScanRange {
            band: r.band,
            start_carrier: r.start_carrier,
            stop_carrier: r.stop_carrier,
            step: r.step.unwrap_or(1),
        }),
        dwell_ms: dto.dwell_ms.unwrap_or(1000),
    }
}

/// Assemble a [`CfgCodeplug`] from the parsed DTO sections. RF resolution errors
/// (e.g. an off-grid `dl_freq`) surface here; range/cross-reference validation is
/// performed separately by [`CfgCodeplug::validate`].
pub fn codeplug_dto_to_cfg(
    folders: Option<Vec<FolderDto>>,
    talkgroups: Option<Vec<TalkgroupDto>>,
    networks: Option<Vec<NetworkDto>>,
    carrier_overrides: Option<Vec<CarrierOverrideDto>>,
    scan: Option<ScanDto>,
) -> Result<CfgCodeplug, String> {
    let folders = folders.unwrap_or_default().into_iter().map(folder_dto_to_cfg).collect();
    let talkgroups = talkgroups.unwrap_or_default().into_iter().map(talkgroup_dto_to_cfg).collect();
    let networks = networks.unwrap_or_default().into_iter().map(network_dto_to_cfg).collect();
    let carrier_overrides = carrier_overrides
        .unwrap_or_default()
        .into_iter()
        .map(carrier_override_dto_to_cfg)
        .collect::<Result<Vec<_>, _>>()?;
    let scan = scan.map(scan_dto_to_cfg);
    Ok(CfgCodeplug {
        folders,
        talkgroups,
        networks,
        carrier_overrides,
        scan,
    })
}

/// Inverse projections for TOML write-back (Plane B). Runtime carrier overrides
/// always serialize in the explicit `band`+`carrier`+`freq_offset` form (lossless).
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
                order: Some(t.order),
                extra: HashMap::new(),
            })
            .collect(),
    )
}

pub fn cfg_to_network_dtos(cp: &CfgCodeplug) -> Option<Vec<NetworkDto>> {
    if cp.networks.is_empty() {
        return None;
    }
    Some(
        cp.networks
            .iter()
            .map(|n| NetworkDto {
                mcc: n.mcc,
                mnc: n.mnc,
                name: n.name.clone(),
                priority: Some(n.priority),
                extra: HashMap::new(),
            })
            .collect(),
    )
}

pub fn cfg_to_carrier_override_dtos(cp: &CfgCodeplug) -> Option<Vec<CarrierOverrideDto>> {
    if cp.carrier_overrides.is_empty() {
        return None;
    }
    Some(
        cp.carrier_overrides
            .iter()
            .map(|c| CarrierOverrideDto {
                name: c.name.clone(),
                dl_freq: None,
                band: Some(c.freq_band),
                carrier: Some(c.main_carrier),
                freq_offset: Some(c.freq_offset_hz),
                colour_code: c.colour_code,
                duplex_index: c.duplex_index,
                custom_duplex_spacing: c.custom_duplex_spacing,
                rx_only: Some(c.rx_only),
                extra: HashMap::new(),
            })
            .collect(),
    )
}

pub fn cfg_to_scan_dto(cp: &CfgCodeplug) -> Option<ScanDto> {
    cp.scan.as_ref().map(|s| ScanDto {
        mode: Some(s.mode),
        frequencies: if s.frequencies.is_empty() { None } else { Some(s.frequencies.clone()) },
        range: s.range.as_ref().map(|r| ScanRangeDto {
            band: r.band,
            start_carrier: r.start_carrier,
            stop_carrier: r.stop_carrier,
            step: Some(r.step),
            extra: HashMap::new(),
        }),
        dwell_ms: Some(s.dwell_ms),
        extra: HashMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_override(name: &str) -> CfgCarrierOverride {
        CfgCarrierOverride {
            name: name.to_string(),
            freq_band: 4,
            main_carrier: 1593,
            freq_offset_hz: 0,
            colour_code: Some(1),
            duplex_index: Some(7),
            custom_duplex_spacing: Some(9_400_000),
            rx_only: true,
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
    fn test_override_dto_dl_freq_form() {
        let dto = CarrierOverrideDto {
            name: "A".to_string(),
            dl_freq: Some(439_825_000),
            ..Default::default()
        };
        let co = carrier_override_dto_to_cfg(dto).unwrap();
        assert_eq!((co.freq_band, co.main_carrier, co.freq_offset_hz), (4, 1593, 0));
    }

    #[test]
    fn test_override_dto_band_carrier_form() {
        let dto = CarrierOverrideDto {
            name: "A".to_string(),
            band: Some(4),
            carrier: Some(1593),
            freq_offset: Some(-6250),
            ..Default::default()
        };
        let co = carrier_override_dto_to_cfg(dto).unwrap();
        assert_eq!(co.freq_offset_hz, -6250);
        assert_eq!(co.dl_freq_hz(), 439_825_000 - 6250);
    }

    #[test]
    fn test_override_dto_conflicting_forms_rejected() {
        let dto = CarrierOverrideDto {
            name: "A".to_string(),
            dl_freq: Some(439_825_000),
            band: Some(4),
            carrier: Some(1000),
            ..Default::default()
        };
        assert!(carrier_override_dto_to_cfg(dto).is_err());
    }

    #[test]
    fn test_validate_accepts_wellformed() {
        let cp = CfgCodeplug {
            folders: vec![CfgFolder {
                id: "work".to_string(),
                name: "Work".to_string(),
                order: 1,
            }],
            talkgroups: vec![CfgTalkgroup {
                gssi: 101,
                name: "Dispatch".to_string(),
                folder: Some("work".to_string()),
                class_of_usage: Some(0),
                order: 0,
            }],
            networks: vec![CfgNetwork {
                mcc: 901,
                mnc: 9999,
                name: Some("Home".to_string()),
                priority: 0,
            }],
            carrier_overrides: vec![sample_override("BS-1")],
            scan: Some(CfgScan {
                mode: ScanMode::List,
                frequencies: vec![439_825_000, 439_850_000],
                ..CfgScan::default()
            }),
        };
        assert!(cp.validate().is_ok(), "{:?}", cp.validate());
    }

    #[test]
    fn test_validate_rejects_unknown_folder() {
        let cp = CfgCodeplug {
            talkgroups: vec![CfgTalkgroup {
                gssi: 101,
                name: "X".to_string(),
                folder: Some("nope".to_string()),
                class_of_usage: None,
                order: 0,
            }],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_validate_fixed_mode_requires_one_freq() {
        let cp = CfgCodeplug {
            scan: Some(CfgScan {
                mode: ScanMode::Fixed,
                frequencies: vec![439_825_000, 439_850_000],
                ..CfgScan::default()
            }),
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_duplicate_override_name() {
        let cp = CfgCodeplug {
            carrier_overrides: vec![sample_override("BS-1"), sample_override("BS-1")],
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
                order: 0,
            }],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_duplicate_network() {
        let cp = CfgCodeplug {
            networks: vec![
                CfgNetwork { mcc: 901, mnc: 1, name: None, priority: 0 },
                CfgNetwork { mcc: 901, mnc: 1, name: None, priority: 1 },
            ],
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

    #[test]
    fn test_networks_sorted_by_priority() {
        let cp = CfgCodeplug {
            networks: vec![
                CfgNetwork { mcc: 901, mnc: 2, name: None, priority: 5 },
                CfgNetwork { mcc: 901, mnc: 1, name: None, priority: 1 },
            ],
            ..CfgCodeplug::default()
        };
        let sorted = cp.networks_sorted();
        assert_eq!(sorted[0].mnc, 1);
        assert_eq!(sorted[1].mnc, 2);
    }

    #[test]
    fn test_talkgroups_in_folder_ordered() {
        let cp = CfgCodeplug {
            folders: vec![CfgFolder { id: "w".to_string(), name: "W".to_string(), order: 0 }],
            talkgroups: vec![
                CfgTalkgroup { gssi: 2, name: "B".to_string(), folder: Some("w".to_string()), class_of_usage: None, order: 2 },
                CfgTalkgroup { gssi: 1, name: "A".to_string(), folder: Some("w".to_string()), class_of_usage: None, order: 1 },
            ],
            ..CfgCodeplug::default()
        };
        let tgs = cp.talkgroups_in_folder(Some("w"));
        assert_eq!(tgs[0].gssi, 1);
        assert_eq!(tgs[1].gssi, 2);
    }

    #[test]
    fn test_range_candidate_frequencies() {
        let scan = CfgScan {
            mode: ScanMode::Range,
            range: Some(CfgScanRange { band: 4, start_carrier: 1593, stop_carrier: 1595, step: 1 }),
            ..CfgScan::default()
        };
        let freqs = scan.candidate_frequencies();
        assert_eq!(freqs, vec![439_825_000, 439_850_000, 439_875_000]);
    }

    #[test]
    fn test_carrier_override_for_freq() {
        let cp = CfgCodeplug {
            carrier_overrides: vec![sample_override("BS-1")],
            ..CfgCodeplug::default()
        };
        assert!(cp.carrier_override_for_freq(439_825_000).is_some());
        assert!(cp.carrier_override_for_freq(439_850_000).is_none());
    }
}
