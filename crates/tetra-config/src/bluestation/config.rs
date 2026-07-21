use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use tetra_core::freqs::{DuplexTable, FreqInfo};

use crate::bluestation::{CfgCellInfo, CfgCodeplug, CfgControl, CfgMs, CfgNetInfo, CfgPhyIo, PhyBackend, StackState};

use super::sec_brew::CfgBrew;
use super::sec_telemetry::CfgTelemetry;

/// Wrapper for a string that should be treated as a secret. Display and Debug will redact the actual value,
/// to prevent accidental logging of secrets.
#[derive(Clone)]
pub struct SecretField {
    pub val: String,
}

impl From<String> for SecretField {
    fn from(val: String) -> Self {
        Self { val }
    }
}

impl From<SecretField> for String {
    fn from(secret: SecretField) -> Self {
        secret.val
    }
}

impl AsRef<str> for SecretField {
    fn as_ref(&self) -> &str {
        &self.val
    }
}

impl std::fmt::Display for SecretField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "********")
    }
}

impl std::fmt::Debug for SecretField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretField").field("val", &"********").finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum StackMode {
    Bs,
    Ms,
    Mon,
}

#[derive(Debug, Clone)]
pub struct StackConfig {
    pub stack_mode: StackMode,
    pub debug_log: Option<String>,

    pub phy_io: CfgPhyIo,
    pub net: CfgNetInfo,
    pub cell: CfgCellInfo,

    /// Programmable 8-entry duplex-spacing table (TS 100 392-15 cl. 6). Defaults
    /// to the ETSI spec table when no `[duplex_table]` section is configured.
    pub duplex_table: DuplexTable,

    /// Radio-style codeplug (folders/talkgroups/networks/carrier_overrides/scan) — **Plane B**.
    /// Empty for BS mode and legacy MS configs. Data only: read by the
    /// management/TNMM UI and the cell-selection engine; does not itself change
    /// on-air behaviour.
    pub codeplug: CfgCodeplug,

    /// Mobile Station configuration. Required when `stack_mode == Ms`.
    pub ms: Option<CfgMs>,

    /// Brew protocol (TetraPack/BrandMeister) configuration
    pub brew: Option<CfgBrew>,

    /// Telemetry endpoint configuration
    pub telemetry: Option<CfgTelemetry>,

    /// Control endpoint configuration
    pub control: Option<CfgControl>,
}

impl StackConfig {
    /// Validate that all required configuration fields are properly set.
    pub fn validate(&self) -> Result<(), &str> {
        // Check input device settings
        match self.phy_io.backend {
            PhyBackend::SoapySdr => {
                if self.phy_io.soapysdr.is_none() {
                    return Err("soapysdr configuration must be provided for Soapysdr backend");
                };
            }
            PhyBackend::None => {} // For testing
            PhyBackend::Undefined => {
                return Err("phy_io backend must be defined");
            }
        };

        // Sanity check on main carrier property fields in SYSINFO
        if self.phy_io.backend == PhyBackend::SoapySdr {
            let soapy_cfg = self
                .phy_io
                .soapysdr
                .as_ref()
                .expect("SoapySdr config must be set for SoapySdr PhyIo");

            match self.stack_mode {
                StackMode::Bs => {
                    // A BS *defines* the cell, so its configured RF must be
                    // exactly the carrier it broadcasts. Derive the DL/UL from
                    // cell_info and require the soapy freqs to match.
                    let Ok(freq_info) = FreqInfo::from_components_with_table(
                        self.cell.freq_band,
                        self.cell.main_carrier,
                        self.cell.freq_offset_hz,
                        self.cell.reverse_operation,
                        self.cell.duplex_spacing_id,
                        self.cell.custom_duplex_spacing,
                        &self.duplex_table,
                    ) else {
                        return Err("Invalid cell info frequency settings");
                    };

                    let (dlfreq, ulfreq) = freq_info.get_freqs();

                    println!("    {:?}", freq_info);
                    println!("    Derived DL freq: {} Hz, UL freq: {} Hz\n", dlfreq, ulfreq);

                    if soapy_cfg.dl_freq as u32 != dlfreq {
                        return Err("PhyIo DlFrequency does not match computed FreqInfo");
                    }
                    if soapy_cfg.ul_freq as u32 != ulfreq {
                        return Err("PhyIo UlFrequency does not match computed FreqInfo");
                    }
                }
                StackMode::Ms | StackMode::Mon => {
                    // A radio-style MS is programmed by downlink: its RX is seeded
                    // from the scan list (or an explicit tx_freq) and the uplink is
                    // derived at camp time from the cell's own D-MLE-SYSINFO
                    // (EN 300 392-2 cl. 18.4.2.2). cell_info RF is not required.
                    // It must, however, have *some* RX source to tune.
                    let has_rx_source =
                        soapy_cfg.dl_freq > 0.0 || !self.codeplug.scan_candidate_frequencies().is_empty();
                    if !has_rx_source {
                        return Err(
                            "MS mode requires an RX source: program at least one [[frequency_list]] \
                             or set [phy_io.soapysdr] tx_freq",
                        );
                    }
                }
            }
        }

        if self.cell.ms_txpwr_max_cell > 7 {
            return Err("ms_txpwr_max_cell must be 0-7 (3 bits)");
        }

        // Validate the codeplug (Plane B). Empty codeplug is a no-op.
        // Note: kept as an owned String on the error path, mapped to &str below.
        if let Err(_e) = self.codeplug.validate() {
            return Err("Invalid codeplug configuration");
        }

        // MS mode requires an [ms] section with a valid own ISSI.
        if self.stack_mode == StackMode::Ms {
            let Some(ref ms) = self.ms else {
                return Err("[ms] configuration section is required when stack_mode = \"Ms\"");
            };
            if ms.issi == 0 || ms.issi > 0xFF_FFFF {
                return Err("ms.issi must be a valid 24-bit ISSI (1..=16777215)");
            }
            if ms.subscriber_class < 1 || ms.subscriber_class > 16 {
                return Err("ms.subscriber_class must be in range 1-16");
            }
        }

        // Validate timezone if configured
        if let Some(ref tz) = self.cell.timezone {
            if tz.parse::<chrono_tz::Tz>().is_err() {
                return Err("Invalid IANA timezone name in cell.timezone");
            }
        }

        Ok(())
    }
}

/// Global shared configuration: immutable config + mutable state.
#[derive(Clone)]
pub struct SharedConfig {
    /// Read-only configuration (immutable after construction).
    cfg: Arc<StackConfig>,
    /// Mutable state guarded with RwLock (write by the stack, read by others).
    state: Arc<RwLock<StackState>>,
}

impl SharedConfig {
    pub fn from_parts(cfg: StackConfig, state: Option<StackState>) -> Self {
        // Check config for validity before returning the SharedConfig object
        match cfg.validate() {
            Ok(_) => {}
            Err(e) => panic!("Invalid stack configuration: {}", e),
        }

        Self {
            cfg: Arc::new(cfg),
            state: Arc::new(RwLock::new(state.unwrap_or_default())),
        }
    }

    /// Access immutable config.
    pub fn config(&self) -> Arc<StackConfig> {
        Arc::clone(&self.cfg)
    }

    /// Read guard for mutable state.
    pub fn state_read(&self) -> std::sync::RwLockReadGuard<'_, StackState> {
        self.state.read().expect("StackState RwLock blocked")
    }

    /// Write guard for mutable state.
    pub fn state_write(&self) -> std::sync::RwLockWriteGuard<'_, StackState> {
        self.state.write().expect("StackState RwLock blocked")
    }
}
