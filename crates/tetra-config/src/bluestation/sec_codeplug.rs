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
const MAX_ISSI: u32 = 0xFF_FFFF; // 24-bit individual short subscriber identity
const MAX_EXT_SUBSCRIBER_DIGITS: usize = 24; // External subscriber number: n <= 24 (cl. 14.8.20)
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

/// How a programmed frequency list enumerates its downlink carriers
/// (**[impl policy]**). A single-frequency `List` is the former "fixed channel".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum FrequencyListMode {
    /// An explicit list of downlink frequencies.
    List,
    /// A frequency range (band + carrier start/stop/step) enumerated on the fly.
    Range,
}

impl Default for FrequencyListMode {
    fn default() -> Self {
        FrequencyListMode::List
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

/// A carrier range for a `FrequencyListMode::Range` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgFrequencyRange {
    pub band: u8,
    pub start_carrier: u16,
    pub stop_carrier: u16,
    /// Step in carrier units (multiples of 25 kHz). Must be >= 1.
    pub step: u16,
    /// Carrier frequency offsets (Hz) to probe for each enumerated carrier.
    /// TETRA permits four offsets from the 25 kHz raster (EN 300 392-2, the
    /// D-MLE-SYNC "Frequency band"/"Offset" elements): 0, +6250, -6250, +12500.
    /// Empty is treated as `[0]` (nominal raster only, the historical behaviour).
    pub offsets: Vec<i16>,
}

/// The four carrier frequency offsets (Hz) permitted by TETRA (EN 300 392-2,
/// D-MLE-SYNC "Offset" 2-bit field): none, +6.25 kHz, -6.25 kHz, +12.5 kHz.
pub const LEGAL_FREQ_OFFSETS_HZ: [i16; 4] = [0, 6250, -6250, 12500];

/// A named frequency list the radio scans (**[impl policy]**). Each list is
/// either an explicit set of downlink frequencies (`List`) or an enumerated
/// carrier range (`Range`). The radio scans every programmed list, combined
/// into one candidate set (see [`CfgCodeplug::scan_candidate_frequencies`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgFrequencyList {
    /// Human-readable label (unique within the codeplug).
    pub name: String,
    pub mode: FrequencyListMode,
    /// Absolute downlink frequencies (Hz) for a `List` list (a single entry is
    /// the former "fixed channel").
    pub frequencies: Vec<u32>,
    /// Carrier range for a `Range` list.
    pub range: Option<CfgFrequencyRange>,
    /// Per-candidate dwell time in milliseconds.
    pub dwell_ms: u32,
}

impl Default for CfgFrequencyList {
    fn default() -> Self {
        Self {
            name: String::new(),
            mode: FrequencyListMode::List,
            frequencies: Vec::new(),
            range: None,
            dwell_ms: 1000,
        }
    }
}

impl CfgFrequencyList {
    /// Enumerate the downlink frequencies (Hz) this list covers. For a `Range`
    /// list the carriers are expanded on the fly.
    pub fn candidate_frequencies(&self) -> Vec<u32> {
        match self.mode {
            FrequencyListMode::List => self.frequencies.clone(),
            FrequencyListMode::Range => {
                let mut out = Vec::new();
                if let Some(ref r) = self.range {
                    let step = r.step.max(1);
                    let offsets: &[i16] = if r.offsets.is_empty() { &[0] } else { &r.offsets };
                    let mut c = r.start_carrier;
                    while c <= r.stop_carrier {
                        for &off in offsets {
                            let hz = channel_dl_hz(r.band, c, off);
                            if !out.contains(&hz) {
                                out.push(hz);
                            }
                        }
                        c = c.saturating_add(step);
                    }
                }
                out
            }
        }
    }
}

/// A named **scan list**: a set of talkgroups the radio monitors together
/// (**[impl policy]**). A scan list references programmed talkgroups by GSSI; on
/// the air "activating" a scan list means the MS attaches to (affiliates with)
/// those group identities via the standalone group attach/detach procedure
/// (EN 300 392-2 cl. 16.8.2), so their downlink traffic is received.
/// De-activating detaches the groups that no other active scan list still needs.
///
/// `active` is the *default* (programmed) activation state loaded at start-up;
/// the management UI can toggle a scan list live at runtime (which the stack
/// resolves to a group attach/detach), so the running state may differ from this
/// programmed default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgScanlist {
    /// Human-readable label (unique within the codeplug).
    pub name: String,
    /// Member talkgroups, by GSSI (each must reference a programmed talkgroup).
    pub talkgroups: Vec<u32>,
    /// Programmed default: whether this scan list is active at start-up.
    pub active: bool,
    /// Display order within the scan-list menu (ascending; ties broken by name).
    pub order: u32,
}

/// External-network gateway kind (PABX vs PSTN). This is a codeplug/UI
/// categorisation only — TETRA carries **no** on-air "PABX vs PSTN" flag; both
/// are ordinary calls to an external number (cl. 14.8.20). The distinction lets
/// the UI label the destination and, if the operator's dial plan needs it,
/// prepend a different access-code `prefix` per gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CfgGatewayKind {
    /// Public Switched Telephone Network gateway.
    #[default]
    Pstn,
    /// Private Automatic Branch eXchange gateway.
    Pabx,
}

/// A programmed external-network gateway (PABX/PSTN). Plane B, data only.
///
/// A phone contact reaches the external network by an ordinary individual
/// U-SETUP whose called party SSI is this gateway's `gateway_issi` (CPTI = SSI)
/// and which additionally carries the dialled digits in the External subscriber
/// number IE (cl. 14.8.20). The SwMI's gateway subscriber routes the digits into
/// the PABX/PSTN. `prefix`, if set, is prepended to the contact's number before
/// it is placed in the IE (an operator dial-plan access code).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgGateway {
    /// Stable identifier referenced by phone contacts.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// PABX or PSTN (UI categorisation; no on-air effect).
    pub kind: CfgGatewayKind,
    /// The gateway subscriber's Individual Short Subscriber Identity (24-bit):
    /// the called-party SSI placed in the U-SETUP for a call through this
    /// gateway.
    pub gateway_issi: u32,
    /// Optional access-code digits prepended to the dialled number before it is
    /// encoded into the External subscriber number IE.
    pub prefix: Option<String>,
}

/// Where a contact places a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfgContactTarget {
    /// An on-network individual subscriber, addressed by its ISSI.
    Issi(u32),
    /// An external number reached through a programmed gateway. `number` is the
    /// dialled digits (0-9, `*`, `#`, `+`); `gateway` references a [`CfgGateway`]
    /// `id`.
    Phone { number: String, gateway: String },
}

/// A programmed contact (phone-book entry). Plane B, data only — a contact does
/// nothing on-air by itself; selecting one drives an individual (ISSI) or
/// external (phone) call origination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgContact {
    /// Human-readable name (unique within the codeplug).
    pub name: String,
    /// Optional callsign.
    pub callsign: Option<String>,
    /// Where the contact places a call.
    pub target: CfgContactTarget,
    /// Display order within the contact list (ascending; ties broken by name).
    pub order: u32,
}


#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CfgCodeplug {
    pub folders: Vec<CfgFolder>,
    pub talkgroups: Vec<CfgTalkgroup>,
    /// Codeplug-wide allowed networks; empty = home MCC/MNC only.
    pub networks: Vec<CfgNetwork>,
    /// Per-frequency overrides, matched by downlink frequency.
    pub carrier_overrides: Vec<CfgCarrierOverride>,
    /// Programmed frequency lists the radio scans (combined into one candidate
    /// set). Empty = no scanning (stay on the single configured carrier).
    pub frequency_lists: Vec<CfgFrequencyList>,
    /// Programmed talkgroup scan lists (groups the radio monitors together).
    /// The UI can activate/deactivate these at runtime.
    pub scanlists: Vec<CfgScanlist>,
    /// Programmed external-network gateways (PABX/PSTN) referenced by phone
    /// contacts.
    pub gateways: Vec<CfgGateway>,
    /// Programmed contacts (phone book).
    pub contacts: Vec<CfgContact>,
    /// Home-mode display SDS protocol identifier (cl. 29.4.3.9): the 8-bit SDS-TL
    /// protocol identifier this radio uses for a "home mode display" status/text
    /// message sent to another radio's home screen. Data only — the UI reads it
    /// when composing such a message. `None` = not programmed.
    pub home_display_pid: Option<u8>,
}

impl CfgCodeplug {
    /// True when nothing is programmed (so the whole codeplug can be omitted
    /// from serialized output and skipped by validation).
    pub fn is_empty(&self) -> bool {
        self.folders.is_empty()
            && self.talkgroups.is_empty()
            && self.networks.is_empty()
            && self.carrier_overrides.is_empty()
            && self.frequency_lists.is_empty()
            && self.scanlists.is_empty()
            && self.gateways.is_empty()
            && self.contacts.is_empty()
            && self.home_display_pid.is_none()
    }

    /// Scan lists in display order (`order` ascending, ties broken by `name`).
    pub fn scanlists_sorted(&self) -> Vec<&CfgScanlist> {
        let mut v: Vec<&CfgScanlist> = self.scanlists.iter().collect();
        v.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
        v
    }

    /// Look up a scan list by name.
    pub fn scanlist(&self, name: &str) -> Option<&CfgScanlist> {
        self.scanlists.iter().find(|s| s.name == name)
    }

    /// Union of the GSSIs of every scan list whose programmed default is
    /// `active` (in display order, duplicates removed). This is the set of
    /// groups the radio affiliates to at start-up on top of any base
    /// `[ms] attach_groups`.
    pub fn default_active_scanlist_gssis(&self) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        for sl in self.scanlists_sorted() {
            if sl.active {
                for &g in &sl.talkgroups {
                    if !out.contains(&g) {
                        out.push(g);
                    }
                }
            }
        }
        out
    }

    /// The combined downlink candidate set (Hz) the radio scans across ALL
    /// programmed frequency lists, in list-then-entry order with duplicates
    /// removed (**[impl policy]**). Empty when no list is programmed, in which
    /// case the radio stays on its single configured carrier (no scanning).
    pub fn scan_candidate_frequencies(&self) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        for list in &self.frequency_lists {
            for f in list.candidate_frequencies() {
                if !out.contains(&f) {
                    out.push(f);
                }
            }
        }
        out
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

    /// Contacts in display order (`order` ascending, ties broken by `name`).
    pub fn contacts_sorted(&self) -> Vec<&CfgContact> {
        let mut v: Vec<&CfgContact> = self.contacts.iter().collect();
        v.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
        v
    }

    /// Look up a contact by name.
    pub fn contact(&self, name: &str) -> Option<&CfgContact> {
        self.contacts.iter().find(|c| c.name == name)
    }

    /// Look up a gateway by id.
    pub fn gateway(&self, id: &str) -> Option<&CfgGateway> {
        self.gateways.iter().find(|g| g.id == id)
    }

    /// The programmed home-mode display SDS protocol identifier (cl. 29.4.3.9),
    /// if any. Data only — used by the UI when composing a home-display message.
    pub fn home_display_pid(&self) -> Option<u8> {
        self.home_display_pid
    }

    /// Resolve a phone contact to the values a call origination needs: the
    /// gateway's ISSI (the called-party SSI) and the full dialled digit string
    /// (`gateway.prefix` + `number`) to place in the External subscriber number
    /// IE (cl. 14.8.20). Returns `None` for a non-phone contact or a dangling
    /// gateway reference.
    pub fn resolve_phone_contact(&self, contact: &CfgContact) -> Option<(u32, String)> {
        let CfgContactTarget::Phone { number, gateway } = &contact.target else {
            return None;
        };
        let gw = self.gateway(gateway)?;
        let digits = format!("{}{}", gw.prefix.as_deref().unwrap_or(""), number);
        Some((gw.gateway_issi, digits))
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

    /// True when a cell advertising `(mcc, mnc)` belongs to an allowed network.
    ///
    /// Radio-style cell suitability (**[impl policy]** built on the codeplug
    /// allowed-network list; the network identity itself is the D-MLE-SYNC
    /// MCC/MNC, EN 300 392-2 cl. 18.4.2.1). The home network
    /// (`home_mcc`/`home_mnc`, from `[net_info]`) is always allowed so a
    /// correctly-programmed radio never rejects its own cell. When the codeplug
    /// programs no additional networks, only the home network is allowed;
    /// otherwise the cell's network must also appear in the programmed list.
    pub fn is_network_allowed(&self, mcc: u16, mnc: u16, home_mcc: u16, home_mnc: u16) -> bool {
        if mcc == home_mcc && mnc == home_mnc {
            return true;
        }
        self.networks.iter().any(|n| n.mcc == mcc && n.mnc == mnc)
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

        // Frequency lists.
        let mut list_names: HashSet<&str> = HashSet::new();
        for fl in &self.frequency_lists {
            if fl.name.trim().is_empty() {
                return Err("frequency_list name must not be empty".to_string());
            }
            if !list_names.insert(fl.name.as_str()) {
                return Err(format!("duplicate frequency_list name '{}'", fl.name));
            }
            if fl.dwell_ms == 0 {
                return Err(format!("frequency_list '{}' dwell_ms must be greater than 0", fl.name));
            }
            for f in &fl.frequencies {
                if *f == 0 {
                    return Err(format!("frequency_list '{}' frequency must be greater than 0 Hz", fl.name));
                }
            }
            match fl.mode {
                FrequencyListMode::List => {
                    if fl.frequencies.is_empty() {
                        return Err(format!(
                            "frequency_list '{}' (List) requires at least one frequency",
                            fl.name
                        ));
                    }
                }
                FrequencyListMode::Range => {
                    let Some(ref r) = fl.range else {
                        return Err(format!(
                            "frequency_list '{}' (Range) requires a [frequency_list.range] section",
                            fl.name
                        ));
                    };
                    if r.band > MAX_FREQ_BAND {
                        return Err(format!("frequency_list '{}' range band must be 0..={}", fl.name, MAX_FREQ_BAND));
                    }
                    if r.step == 0 {
                        return Err(format!("frequency_list '{}' range step must be >= 1", fl.name));
                    }
                    if r.start_carrier > MAX_CARRIER || r.stop_carrier > MAX_CARRIER {
                        return Err(format!("frequency_list '{}' range carriers must be 0..={}", fl.name, MAX_CARRIER));
                    }
                    if r.start_carrier >= r.stop_carrier {
                        return Err(format!("frequency_list '{}' range start_carrier must be < stop_carrier", fl.name));
                    }
                    for &off in &r.offsets {
                        if !LEGAL_FREQ_OFFSETS_HZ.contains(&off) {
                            return Err(format!(
                                "frequency_list '{}' range offset {} Hz is invalid (allowed: 0, 6250, -6250, 12500)",
                                fl.name, off
                            ));
                        }
                    }
                }
            }
        }

        // Scan lists: unique non-empty names, at least one member, each member
        // GSSI references a programmed talkgroup, no duplicate GSSIs within a
        // list.
        let mut scanlist_names: HashSet<&str> = HashSet::new();
        for sl in &self.scanlists {
            if sl.name.trim().is_empty() {
                return Err("scanlist name must not be empty".to_string());
            }
            if !scanlist_names.insert(sl.name.as_str()) {
                return Err(format!("duplicate scanlist name '{}'", sl.name));
            }
            if sl.talkgroups.is_empty() {
                return Err(format!("scanlist '{}' requires at least one talkgroup", sl.name));
            }
            let mut seen: HashSet<u32> = HashSet::new();
            for &g in &sl.talkgroups {
                if !seen.insert(g) {
                    return Err(format!("scanlist '{}' has duplicate talkgroup gssi {}", sl.name, g));
                }
                if !gssis.contains(&g) {
                    return Err(format!(
                        "scanlist '{}' references unknown talkgroup gssi {}",
                        sl.name, g
                    ));
                }
            }
        }

        // Gateways: unique non-empty ids/names, valid gateway ISSI, valid
        // optional access-code prefix digits (cl. 14.8.20 digit set).
        let mut gateway_ids: HashSet<&str> = HashSet::new();
        for gw in &self.gateways {
            if gw.id.trim().is_empty() {
                return Err("gateway id must not be empty".to_string());
            }
            if gw.name.trim().is_empty() {
                return Err(format!("gateway '{}' name must not be empty", gw.id));
            }
            if !gateway_ids.insert(gw.id.as_str()) {
                return Err(format!("duplicate gateway id '{}'", gw.id));
            }
            if gw.gateway_issi == 0 || gw.gateway_issi > MAX_ISSI {
                return Err(format!("gateway '{}' gateway_issi must be a 24-bit value (1..={})", gw.id, MAX_ISSI));
            }
            if let Some(ref prefix) = gw.prefix {
                validate_dial_digits(&format!("gateway '{}' prefix", gw.id), prefix)?;
            }
        }

        // Contacts: unique non-empty names, exactly one valid target. A phone
        // contact must reference a programmed gateway, carry valid dial digits,
        // and (with the gateway prefix) stay within the 24-digit IE limit
        // (cl. 14.8.20); an ISSI contact must carry a 24-bit ISSI.
        let mut contact_names: HashSet<&str> = HashSet::new();
        for c in &self.contacts {
            if c.name.trim().is_empty() {
                return Err("contact name must not be empty".to_string());
            }
            if !contact_names.insert(c.name.as_str()) {
                return Err(format!("duplicate contact name '{}'", c.name));
            }
            match &c.target {
                CfgContactTarget::Issi(issi) => {
                    if *issi == 0 || *issi > MAX_ISSI {
                        return Err(format!("contact '{}' issi must be a 24-bit value (1..={})", c.name, MAX_ISSI));
                    }
                }
                CfgContactTarget::Phone { number, gateway } => {
                    let Some(gw) = self.gateway(gateway) else {
                        return Err(format!("contact '{}' references unknown gateway '{}'", c.name, gateway));
                    };
                    validate_dial_digits(&format!("contact '{}' number", c.name), number)?;
                    let total = gw.prefix.as_deref().map_or(0, |p| p.chars().filter(|ch| !ch.is_whitespace()).count())
                        + number.chars().filter(|ch| !ch.is_whitespace()).count();
                    if total > MAX_EXT_SUBSCRIBER_DIGITS {
                        return Err(format!(
                            "contact '{}' dialled digits (gateway prefix + number = {}) exceed the {}-digit External subscriber number limit (cl. 14.8.20)",
                            c.name, total, MAX_EXT_SUBSCRIBER_DIGITS
                        ));
                    }
                }
            }
        }

        Ok(())
    }
}

/// Validate a dialled digit string against the External subscriber number digit
/// set (cl. 14.8.20 / Table 14.59: `0`-`9`, `*`, `#`, `+`). Interior whitespace
/// is ignored (a display convenience); an empty or over-long number is rejected.
fn validate_dial_digits(label: &str, digits: &str) -> Result<(), String> {
    let significant: Vec<char> = digits.chars().filter(|c| !c.is_whitespace()).collect();
    if significant.is_empty() {
        return Err(format!("{} must not be empty", label));
    }
    if significant.len() > MAX_EXT_SUBSCRIBER_DIGITS {
        return Err(format!(
            "{} has {} digits, exceeding the {}-digit External subscriber number limit (cl. 14.8.20)",
            label,
            significant.len(),
            MAX_EXT_SUBSCRIBER_DIGITS
        ));
    }
    for c in significant {
        if !matches!(c, '0'..='9' | '*' | '#' | '+') {
            return Err(format!("{} contains invalid dial character '{}' (allowed: 0-9 * # +)", label, c));
        }
    }
    Ok(())
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
pub struct FrequencyRangeDto {
    pub band: u8,
    pub start_carrier: u16,
    pub stop_carrier: u16,
    pub step: Option<u16>,
    pub offsets: Option<Vec<i16>>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct FrequencyListDto {
    pub name: String,
    pub mode: Option<FrequencyListMode>,
    pub frequencies: Option<Vec<u32>>,
    pub range: Option<FrequencyRangeDto>,
    pub dwell_ms: Option<u32>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct ScanlistDto {
    pub name: String,
    pub talkgroups: Vec<u32>,
    pub active: Option<bool>,
    pub order: Option<u32>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct GatewayDto {
    pub id: String,
    pub name: String,
    pub kind: CfgGatewayKind,
    pub gateway_issi: u32,
    pub prefix: Option<String>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

/// Scalar codeplug-wide settings (the `[codeplug]` table). Distinct from the
/// arrays-of-tables sections (folders/talkgroups/…), which stay top-level.
#[derive(Default, Deserialize, Serialize)]
pub struct CodeplugSettingsDto {
    /// Home-mode display SDS protocol identifier (cl. 29.4.3.9).
    pub home_display_pid: Option<u8>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct ContactDto {
    pub name: String,
    pub callsign: Option<String>,
    /// On-network individual target (mutually exclusive with `number`/`gateway`).
    pub issi: Option<u32>,
    /// External-number target: dialled digits (requires `gateway`).
    pub number: Option<String>,
    /// Gateway id for an external-number target (requires `number`).
    pub gateway: Option<String>,
    pub order: Option<u32>,
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

fn frequency_list_dto_to_cfg(dto: FrequencyListDto) -> CfgFrequencyList {
    CfgFrequencyList {
        name: dto.name,
        mode: dto.mode.unwrap_or_default(),
        frequencies: dto.frequencies.unwrap_or_default(),
        range: dto.range.map(|r| CfgFrequencyRange {
            band: r.band,
            start_carrier: r.start_carrier,
            stop_carrier: r.stop_carrier,
            step: r.step.unwrap_or(1),
            offsets: r.offsets.unwrap_or_default(),
        }),
        dwell_ms: dto.dwell_ms.unwrap_or(1000),
    }
}

fn scanlist_dto_to_cfg(dto: ScanlistDto) -> CfgScanlist {
    CfgScanlist {
        name: dto.name,
        talkgroups: dto.talkgroups,
        active: dto.active.unwrap_or(false),
        order: dto.order.unwrap_or(0),
    }
}

fn gateway_dto_to_cfg(dto: GatewayDto) -> CfgGateway {
    CfgGateway {
        id: dto.id,
        name: dto.name,
        kind: dto.kind,
        gateway_issi: dto.gateway_issi,
        prefix: dto.prefix,
    }
}

fn contact_dto_to_cfg(dto: ContactDto) -> Result<CfgContact, String> {
    // Resolve exactly one target form: an ISSI, or a (number + gateway) phone.
    let target = match (dto.issi, dto.number.as_ref(), dto.gateway.as_ref()) {
        (Some(issi), None, None) => CfgContactTarget::Issi(issi),
        (None, Some(number), Some(gateway)) => CfgContactTarget::Phone {
            number: number.clone(),
            gateway: gateway.clone(),
        },
        (None, Some(_), None) => {
            return Err(format!("contact '{}' has a number but no gateway", dto.name));
        }
        (None, None, Some(_)) => {
            return Err(format!("contact '{}' has a gateway but no number", dto.name));
        }
        (Some(_), _, _) => {
            return Err(format!("contact '{}' must set either issi or number/gateway, not both", dto.name));
        }
        (None, None, None) => {
            return Err(format!("contact '{}' must set either issi or number+gateway", dto.name));
        }
    };
    Ok(CfgContact {
        name: dto.name,
        callsign: dto.callsign,
        target,
        order: dto.order.unwrap_or(0),
    })
}

/// Assemble a [`CfgCodeplug`] from the parsed DTO sections. RF resolution errors
/// (e.g. an off-grid `dl_freq`) surface here; range/cross-reference validation is
/// performed separately by [`CfgCodeplug::validate`].
pub fn codeplug_dto_to_cfg(
    folders: Option<Vec<FolderDto>>,
    talkgroups: Option<Vec<TalkgroupDto>>,
    networks: Option<Vec<NetworkDto>>,
    carrier_overrides: Option<Vec<CarrierOverrideDto>>,
    frequency_lists: Option<Vec<FrequencyListDto>>,
    scanlists: Option<Vec<ScanlistDto>>,
    gateways: Option<Vec<GatewayDto>>,
    contacts: Option<Vec<ContactDto>>,
    settings: Option<CodeplugSettingsDto>,
) -> Result<CfgCodeplug, String> {
    let folders = folders.unwrap_or_default().into_iter().map(folder_dto_to_cfg).collect();
    let talkgroups = talkgroups.unwrap_or_default().into_iter().map(talkgroup_dto_to_cfg).collect();
    let networks = networks.unwrap_or_default().into_iter().map(network_dto_to_cfg).collect();
    let carrier_overrides = carrier_overrides
        .unwrap_or_default()
        .into_iter()
        .map(carrier_override_dto_to_cfg)
        .collect::<Result<Vec<_>, _>>()?;
    let frequency_lists = frequency_lists
        .unwrap_or_default()
        .into_iter()
        .map(frequency_list_dto_to_cfg)
        .collect();
    let scanlists = scanlists.unwrap_or_default().into_iter().map(scanlist_dto_to_cfg).collect();
    let gateways = gateways.unwrap_or_default().into_iter().map(gateway_dto_to_cfg).collect();
    let contacts = contacts
        .unwrap_or_default()
        .into_iter()
        .map(contact_dto_to_cfg)
        .collect::<Result<Vec<_>, _>>()?;
    let home_display_pid = settings.and_then(|s| s.home_display_pid);
    Ok(CfgCodeplug {
        folders,
        talkgroups,
        networks,
        carrier_overrides,
        frequency_lists,
        scanlists,
        gateways,
        contacts,
        home_display_pid,
    })
}

/// Project the scalar codeplug settings back to the `[codeplug]` DTO table
/// (`None` when nothing scalar is programmed).
pub fn cfg_to_codeplug_settings_dto(cp: &CfgCodeplug) -> Option<CodeplugSettingsDto> {
    cp.home_display_pid.map(|pid| CodeplugSettingsDto {
        home_display_pid: Some(pid),
        extra: HashMap::new(),
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

pub fn cfg_to_frequency_list_dtos(cp: &CfgCodeplug) -> Option<Vec<FrequencyListDto>> {
    if cp.frequency_lists.is_empty() {
        return None;
    }
    Some(
        cp.frequency_lists
            .iter()
            .map(|s| FrequencyListDto {
                name: s.name.clone(),
                mode: Some(s.mode),
                frequencies: if s.frequencies.is_empty() { None } else { Some(s.frequencies.clone()) },
                range: s.range.as_ref().map(|r| FrequencyRangeDto {
                    band: r.band,
                    start_carrier: r.start_carrier,
                    stop_carrier: r.stop_carrier,
                    step: Some(r.step),
                    offsets: if r.offsets.is_empty() { None } else { Some(r.offsets.clone()) },
                    extra: HashMap::new(),
                }),
                dwell_ms: Some(s.dwell_ms),
                extra: HashMap::new(),
            })
            .collect(),
    )
}

pub fn cfg_to_scanlist_dtos(cp: &CfgCodeplug) -> Option<Vec<ScanlistDto>> {
    if cp.scanlists.is_empty() {
        return None;
    }
    Some(
        cp.scanlists
            .iter()
            .map(|s| ScanlistDto {
                name: s.name.clone(),
                talkgroups: s.talkgroups.clone(),
                active: Some(s.active),
                order: Some(s.order),
                extra: HashMap::new(),
            })
            .collect(),
    )
}

pub fn cfg_to_gateway_dtos(cp: &CfgCodeplug) -> Option<Vec<GatewayDto>> {
    if cp.gateways.is_empty() {
        return None;
    }
    Some(
        cp.gateways
            .iter()
            .map(|g| GatewayDto {
                id: g.id.clone(),
                name: g.name.clone(),
                kind: g.kind,
                gateway_issi: g.gateway_issi,
                prefix: g.prefix.clone(),
                extra: HashMap::new(),
            })
            .collect(),
    )
}

pub fn cfg_to_contact_dtos(cp: &CfgCodeplug) -> Option<Vec<ContactDto>> {
    if cp.contacts.is_empty() {
        return None;
    }
    Some(
        cp.contacts
            .iter()
            .map(|c| {
                let (issi, number, gateway) = match &c.target {
                    CfgContactTarget::Issi(issi) => (Some(*issi), None, None),
                    CfgContactTarget::Phone { number, gateway } => (None, Some(number.clone()), Some(gateway.clone())),
                };
                ContactDto {
                    name: c.name.clone(),
                    callsign: c.callsign.clone(),
                    issi,
                    number,
                    gateway,
                    order: Some(c.order),
                    extra: HashMap::new(),
                }
            })
            .collect(),
    )
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
            frequency_lists: vec![CfgFrequencyList {
                name: "primary".to_string(),
                mode: FrequencyListMode::List,
                frequencies: vec![439_825_000, 439_850_000],
                ..CfgFrequencyList::default()
            }],
            scanlists: vec![CfgScanlist {
                name: "Alpha".to_string(),
                talkgroups: vec![101],
                active: true,
                order: 0,
            }],
            gateways: vec![CfgGateway {
                id: "pstn".to_string(),
                name: "Public Phone".to_string(),
                kind: CfgGatewayKind::Pstn,
                gateway_issi: 8_000_001,
                prefix: Some("9".to_string()),
            }],
            contacts: vec![
                CfgContact {
                    name: "Dispatch Lead".to_string(),
                    callsign: Some("ALPHA1".to_string()),
                    target: CfgContactTarget::Issi(2_000_123),
                    order: 1,
                },
                CfgContact {
                    name: "Front Desk".to_string(),
                    callsign: None,
                    target: CfgContactTarget::Phone {
                        number: "1234".to_string(),
                        gateway: "pstn".to_string(),
                    },
                    order: 2,
                },
            ],
            home_display_pid: Some(130),
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

    fn phone_contact(name: &str, number: &str, gateway: &str) -> CfgContact {
        CfgContact {
            name: name.to_string(),
            callsign: None,
            target: CfgContactTarget::Phone {
                number: number.to_string(),
                gateway: gateway.to_string(),
            },
            order: 0,
        }
    }

    fn pstn_gateway(id: &str, issi: u32, prefix: Option<&str>) -> CfgGateway {
        CfgGateway {
            id: id.to_string(),
            name: format!("{id} gw"),
            kind: CfgGatewayKind::Pstn,
            gateway_issi: issi,
            prefix: prefix.map(str::to_string),
        }
    }

    #[test]
    fn contact_dto_resolves_issi_and_phone_forms() {
        let issi = contact_dto_to_cfg(ContactDto {
            name: "A".to_string(),
            issi: Some(2_000_123),
            order: Some(1),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(issi.target, CfgContactTarget::Issi(2_000_123));

        let phone = contact_dto_to_cfg(ContactDto {
            name: "B".to_string(),
            number: Some("1234".to_string()),
            gateway: Some("pabx".to_string()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            phone.target,
            CfgContactTarget::Phone { number: "1234".to_string(), gateway: "pabx".to_string() }
        );
    }

    #[test]
    fn contact_dto_rejects_ambiguous_or_incomplete_targets() {
        // Both issi and phone.
        assert!(contact_dto_to_cfg(ContactDto {
            name: "A".to_string(),
            issi: Some(1),
            number: Some("1".to_string()),
            gateway: Some("g".to_string()),
            ..Default::default()
        })
        .is_err());
        // number without gateway.
        assert!(contact_dto_to_cfg(ContactDto {
            name: "B".to_string(),
            number: Some("1".to_string()),
            ..Default::default()
        })
        .is_err());
        // neither.
        assert!(contact_dto_to_cfg(ContactDto { name: "C".to_string(), ..Default::default() }).is_err());
    }

    #[test]
    fn validate_rejects_phone_contact_with_dangling_gateway() {
        let cp = CfgCodeplug {
            contacts: vec![phone_contact("X", "123", "missing")],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err());
    }

    #[test]
    fn validate_rejects_bad_dial_chars_and_overlong_number() {
        let cp = CfgCodeplug {
            gateways: vec![pstn_gateway("g", 8_000_001, None)],
            contacts: vec![phone_contact("X", "12AB", "g")],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err(), "hex letters are not dial digits");

        // gateway prefix (2) + number (23) = 25 digits > 24-digit IE limit.
        let cp = CfgCodeplug {
            gateways: vec![pstn_gateway("g", 8_000_001, Some("99"))],
            contacts: vec![phone_contact("X", &"1".repeat(23), "g")],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err(), "prefix + number must fit 24 digits (cl. 14.8.20)");
    }

    #[test]
    fn home_display_pid_round_trips_via_settings_dto() {
        let cp = codeplug_dto_to_cfg(None, None, None, None, None, None, None, None, Some(CodeplugSettingsDto {
            home_display_pid: Some(130),
            extra: HashMap::new(),
        }))
        .unwrap();
        assert_eq!(cp.home_display_pid(), Some(130));
        assert!(!cp.is_empty(), "a programmed home_display_pid makes the codeplug non-empty");
        let dto = cfg_to_codeplug_settings_dto(&cp).expect("settings dto present");
        assert_eq!(dto.home_display_pid, Some(130));
        // Absent by default.
        assert!(cfg_to_codeplug_settings_dto(&CfgCodeplug::default()).is_none());
    }

    #[test]
    fn resolve_phone_contact_applies_gateway_prefix() {
        let cp = CfgCodeplug {
            gateways: vec![pstn_gateway("office", 8_000_002, Some("9"))],
            contacts: vec![phone_contact("Desk", "1234", "office")],
            ..CfgCodeplug::default()
        };
        cp.validate().unwrap();
        let c = cp.contact("Desk").unwrap();
        assert_eq!(cp.resolve_phone_contact(c), Some((8_000_002, "91234".to_string())));
        // An ISSI contact does not resolve as a phone.
        let issi_contact = CfgContact {
            name: "N".to_string(),
            callsign: None,
            target: CfgContactTarget::Issi(5),
            order: 0,
        };
        assert_eq!(cp.resolve_phone_contact(&issi_contact), None);
    }

    #[test]
    fn validate_rejects_duplicate_gateway_and_contact() {
        let cp = CfgCodeplug {
            gateways: vec![pstn_gateway("g", 1, None), pstn_gateway("g", 2, None)],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err(), "duplicate gateway id");

        let cp = CfgCodeplug {
            contacts: vec![
                CfgContact { name: "Dup".to_string(), callsign: None, target: CfgContactTarget::Issi(1), order: 0 },
                CfgContact { name: "Dup".to_string(), callsign: None, target: CfgContactTarget::Issi(2), order: 1 },
            ],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err(), "duplicate contact name");
    }

    fn codeplug_with_scanlist(sl: CfgScanlist) -> CfgCodeplug {
        CfgCodeplug {
            talkgroups: vec![
                CfgTalkgroup { gssi: 101, name: "A".to_string(), folder: None, class_of_usage: None, order: 0 },
                CfgTalkgroup { gssi: 102, name: "B".to_string(), folder: None, class_of_usage: None, order: 0 },
            ],
            scanlists: vec![sl],
            ..CfgCodeplug::default()
        }
    }

    #[test]
    fn test_scanlist_accepts_valid() {
        let cp = codeplug_with_scanlist(CfgScanlist {
            name: "Alpha".to_string(),
            talkgroups: vec![101, 102],
            active: true,
            order: 0,
        });
        assert!(cp.validate().is_ok(), "{:?}", cp.validate());
        assert_eq!(cp.default_active_scanlist_gssis(), vec![101, 102]);
    }

    #[test]
    fn test_scanlist_rejects_unknown_gssi() {
        let cp = codeplug_with_scanlist(CfgScanlist {
            name: "Alpha".to_string(),
            talkgroups: vec![101, 999],
            active: true,
            order: 0,
        });
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_scanlist_rejects_duplicate_gssi() {
        let cp = codeplug_with_scanlist(CfgScanlist {
            name: "Alpha".to_string(),
            talkgroups: vec![101, 101],
            active: false,
            order: 0,
        });
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_scanlist_rejects_empty_members() {
        let cp = codeplug_with_scanlist(CfgScanlist {
            name: "Alpha".to_string(),
            talkgroups: vec![],
            active: false,
            order: 0,
        });
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_scanlist_rejects_duplicate_name() {
        let cp = CfgCodeplug {
            talkgroups: vec![CfgTalkgroup { gssi: 101, name: "A".to_string(), folder: None, class_of_usage: None, order: 0 }],
            scanlists: vec![
                CfgScanlist { name: "Dup".to_string(), talkgroups: vec![101], active: true, order: 0 },
                CfgScanlist { name: "Dup".to_string(), talkgroups: vec![101], active: false, order: 1 },
            ],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_validate_list_requires_freq() {
        let cp = CfgCodeplug {
            frequency_lists: vec![CfgFrequencyList {
                name: "empty".to_string(),
                mode: FrequencyListMode::List,
                frequencies: vec![],
                ..CfgFrequencyList::default()
            }],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_duplicate_list_name() {
        let cp = CfgCodeplug {
            frequency_lists: vec![
                CfgFrequencyList {
                    name: "dup".to_string(),
                    frequencies: vec![439_825_000],
                    ..CfgFrequencyList::default()
                },
                CfgFrequencyList {
                    name: "dup".to_string(),
                    frequencies: vec![439_850_000],
                    ..CfgFrequencyList::default()
                },
            ],
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
            frequency_lists: vec![CfgFrequencyList {
                name: "r".to_string(),
                mode: FrequencyListMode::Range,
                range: None,
                ..CfgFrequencyList::default()
            }],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_illegal_range_offset() {
        let cp = CfgCodeplug {
            frequency_lists: vec![CfgFrequencyList {
                name: "r".to_string(),
                mode: FrequencyListMode::Range,
                range: Some(CfgFrequencyRange {
                    band: 4,
                    start_carrier: 1500,
                    stop_carrier: 1600,
                    step: 1,
                    offsets: vec![0, 5000],
                }),
                ..CfgFrequencyList::default()
            }],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_err());
    }

    #[test]
    fn test_validate_accepts_legal_range_offsets() {
        let cp = CfgCodeplug {
            frequency_lists: vec![CfgFrequencyList {
                name: "r".to_string(),
                mode: FrequencyListMode::Range,
                range: Some(CfgFrequencyRange {
                    band: 4,
                    start_carrier: 1500,
                    stop_carrier: 1600,
                    step: 1,
                    offsets: vec![0, 6250, -6250, 12500],
                }),
                ..CfgFrequencyList::default()
            }],
            ..CfgCodeplug::default()
        };
        assert!(cp.validate().is_ok());
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
        let list = CfgFrequencyList {
            name: "r".to_string(),
            mode: FrequencyListMode::Range,
            range: Some(CfgFrequencyRange { band: 4, start_carrier: 1593, stop_carrier: 1595, step: 1, offsets: Vec::new() }),
            ..CfgFrequencyList::default()
        };
        let freqs = list.candidate_frequencies();
        assert_eq!(freqs, vec![439_825_000, 439_850_000, 439_875_000]);
    }

    #[test]
    fn test_range_candidate_frequencies_with_offsets() {
        // For each 25 kHz carrier, also probe the +6.25 kHz offset. Nominal and
        // offset candidates interleave per carrier and are deduped.
        let list = CfgFrequencyList {
            name: "r".to_string(),
            mode: FrequencyListMode::Range,
            range: Some(CfgFrequencyRange {
                band: 4,
                start_carrier: 1593,
                stop_carrier: 1594,
                step: 1,
                offsets: vec![0, 6250],
            }),
            ..CfgFrequencyList::default()
        };
        assert_eq!(
            list.candidate_frequencies(),
            vec![439_825_000, 439_831_250, 439_850_000, 439_856_250],
        );
    }

    #[test]
    fn test_scan_candidate_frequencies_combines_lists_deduped() {
        let cp = CfgCodeplug {
            frequency_lists: vec![
                CfgFrequencyList {
                    name: "a".to_string(),
                    mode: FrequencyListMode::List,
                    frequencies: vec![439_825_000, 439_850_000],
                    ..CfgFrequencyList::default()
                },
                CfgFrequencyList {
                    name: "b".to_string(),
                    mode: FrequencyListMode::Range,
                    // 439_850_000 overlaps list "a" and must be deduped.
                    range: Some(CfgFrequencyRange { band: 4, start_carrier: 1594, stop_carrier: 1595, step: 1, offsets: Vec::new() }),
                    ..CfgFrequencyList::default()
                },
            ],
            ..CfgCodeplug::default()
        };
        assert_eq!(
            cp.scan_candidate_frequencies(),
            vec![439_825_000, 439_850_000, 439_875_000],
            "all lists combined in order, duplicates removed"
        );
    }

    #[test]
    fn test_is_network_allowed_home_only() {
        // Empty codeplug network list => only the home network is allowed.
        let cp = CfgCodeplug::default();
        assert!(cp.is_network_allowed(901, 9999, 901, 9999), "home network allowed");
        assert!(!cp.is_network_allowed(238, 6, 901, 9999), "foreign network rejected");
    }

    #[test]
    fn test_is_network_allowed_with_list() {
        let cp = CfgCodeplug {
            networks: vec![
                CfgNetwork { mcc: 238, mnc: 6, name: None, priority: 0 },
                CfgNetwork { mcc: 244, mnc: 5, name: None, priority: 1 },
            ],
            ..CfgCodeplug::default()
        };
        // Home is always allowed even when not in the list.
        assert!(cp.is_network_allowed(901, 9999, 901, 9999), "home always allowed");
        // Programmed networks are allowed.
        assert!(cp.is_network_allowed(238, 6, 901, 9999), "listed network allowed");
        assert!(cp.is_network_allowed(244, 5, 901, 9999), "listed network allowed");
        // Everything else is rejected.
        assert!(!cp.is_network_allowed(238, 7, 901, 9999), "unlisted network rejected");
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
