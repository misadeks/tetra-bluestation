use serde::Deserialize;

/// ETSI TS 100 392-15 V1.5.1 (2011-02), clause 6: Duplex spacing
const TETRA_DUPLEX_SPACING: [[Option<u32>; 16]; 8] = [
    [
        None,
        Some(1600),
        Some(10000),
        Some(10000),
        Some(10000),
        Some(10000),
        Some(10000),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    ],
    [
        None,
        Some(4500),
        None,
        Some(36000),
        Some(7000),
        None,
        None,
        None,
        Some(45000),
        Some(45000),
        None,
        None,
        None,
        None,
        None,
        None,
    ],
    [
        Some(0),
        Some(0),
        Some(0),
        Some(0),
        Some(0),
        Some(0),
        Some(0),
        Some(0),
        Some(0),
        Some(0),
        Some(0),
        Some(0),
        Some(0),
        Some(0),
        Some(0),
        Some(0),
    ],
    [
        None,
        None,
        None,
        Some(8000),
        Some(8000),
        None,
        None,
        None,
        Some(18000),
        Some(18000),
        None,
        None,
        None,
        None,
        None,
        None,
    ],
    [
        None,
        None,
        None,
        Some(18000),
        Some(5000),
        None,
        Some(30000),
        Some(30000),
        None,
        Some(39000),
        None,
        None,
        None,
        None,
        None,
        None,
    ],
    [
        None,
        None,
        None,
        None,
        Some(9500),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    ],
    [
        None, None, None, None, None, None, None, None, None, None, None, None, None, None, None, None,
    ],
    [
        None, None, None, None, None, None, None, None, None, None, None, None, None, None, None, None,
    ],
];

/// A radio's programmable 8-entry duplex-spacing table.
///
/// TETRA carries the duplex spacing as a 3-bit index (0..7) in MAC-SYSINFO /
/// D-MLE-SYSINFO (ETSI TS 100 392-2). ETSI TS 100 392-15 V1.5.1 clause 6 defines
/// the *default* spacing for each (band, index) pair (see [`TETRA_DUPLEX_SPACING`]).
///
/// A programmed radio ("codeplug") may carry its own duplex-spacing table with up
/// to eight entries. This type models exactly that: each of the 8 indices may
/// carry an override spacing in Hz. `None` means "use the ETSI TS 100 392-15
/// default for this (band, index)"; `Some(hz)` overrides that default (for all
/// bands) with the operator-programmed value. An all-`None` table is therefore
/// behaviourally identical to the bare spec table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplexTable {
    /// Per-index override spacing in Hz. `None` = fall back to the spec default.
    entries: [Option<u32>; 8],
}

impl Default for DuplexTable {
    /// A table that defers every index to the ETSI TS 100 392-15 defaults.
    fn default() -> Self {
        Self { entries: [None; 8] }
    }
}

impl DuplexTable {
    /// Number of duplex-spacing indices (the 3-bit SYSINFO field, cl. 21.4.4.1).
    pub const LEN: usize = 8;

    /// Build a table from explicit per-index overrides (`None` = spec default).
    pub fn new(entries: [Option<u32>; 8]) -> Self {
        Self { entries }
    }

    /// Set (or clear, with `None`) the override for a duplex index.
    ///
    /// Panics if `index >= 8` (the field is only 3 bits wide).
    pub fn set(&mut self, index: u8, spacing_hz: Option<u32>) {
        assert!((index as usize) < Self::LEN, "Invalid duplex index {}", index);
        self.entries[index as usize] = spacing_hz;
    }

    /// The raw per-index override array.
    pub fn entries(&self) -> &[Option<u32>; 8] {
        &self.entries
    }

    /// Resolve the duplex spacing in Hz for a `(band, index)` pair.
    ///
    /// A programmed override for `index` takes precedence over the spec default;
    /// when no override is present the ETSI TS 100 392-15 clause 6 default for
    /// `(band, index)` is used. Returns `None` if neither is defined (an
    /// unprogrammed index on a band with no standardized spacing).
    ///
    /// Panics if `index >= 8`.
    pub fn spacing(&self, band: u8, index: u8) -> Option<u32> {
        assert!((index as usize) < Self::LEN, "Invalid duplex index {}", index);
        match self.entries[index as usize] {
            Some(hz) => Some(hz),
            None => FreqInfo::get_default_duplex_spacing(band, index),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FreqInfo {
    /// Frequency band in 100MHz increments
    pub band: u8,
    /// Carrier number, 0-4000
    pub carrier: u16,
    /// Frequency offset from 25 kHz aligned carrier. In Hz, -6500, 0, 6250, 12500
    pub freq_offset_hz: i16,
    /// Duplex spacing setting (index in duplex spacing table)
    pub duplex_spacing_id: u8,
    /// Duplex spacing in Hz. Usually taken from duplex spacing table,
    /// but can be overridden if clients use a custom duplex spacing table.
    pub duplex_spacing_val: u32,
    /// Reverse operation flag, if true, UL is above DL frequency
    pub reverse_operation: bool,
}

impl FreqInfo {
    pub fn freq_offset_id_to_hz(offset_index: u8) -> Option<i16> {
        match offset_index {
            0 => Some(0),
            1 => Some(6250),
            2 => Some(-6250),
            3 => Some(12500),
            _ => None,
        }
    }

    pub fn freq_offset_hz_to_id(offset_hz: i16) -> Option<u8> {
        match offset_hz {
            0 => Some(0),
            6250 => Some(1),
            -6250 => Some(2),
            12500 => Some(3),
            _ => None,
        }
    }

    /// Construct FreqInfo from band, carrier, frequency offset, duplex spacing index and reverse operation flag.
    /// Optionally accepts a custom duplex spacing value in Hz, if a duplex spacing table is used by the radios.
    ///
    /// The duplex spacing is resolved from (in priority order): the per-channel
    /// `custom_duplex_spacing`, then the ETSI TS 100 392-15 default for
    /// `(band, duplex_index)`. To resolve against a programmed
    /// [`DuplexTable`] use [`FreqInfo::from_components_with_table`].
    pub fn from_components(
        band: u8,
        carrier: u16,
        freq_offset_val: i16,
        reverse_operation: bool,
        duplex_index: u8,
        custom_duplex_spacing: Option<u32>,
    ) -> Result<Self, String> {
        Self::from_components_with_table(
            band,
            carrier,
            freq_offset_val,
            reverse_operation,
            duplex_index,
            custom_duplex_spacing,
            &DuplexTable::default(),
        )
    }

    /// Like [`FreqInfo::from_components`] but resolves the duplex spacing against a
    /// programmed [`DuplexTable`].
    ///
    /// Priority: per-channel `custom_duplex_spacing` > the table's override for
    /// `duplex_index` > the ETSI TS 100 392-15 clause 6 default for
    /// `(band, duplex_index)`.
    #[allow(clippy::too_many_arguments)]
    pub fn from_components_with_table(
        band: u8,
        carrier: u16,
        freq_offset_val: i16,
        reverse_operation: bool,
        duplex_index: u8,
        custom_duplex_spacing: Option<u32>,
        duplex_table: &DuplexTable,
    ) -> Result<Self, String> {
        assert!(band <= 8, "Invalid frequency band {}", band);
        assert!(carrier < 4000, "Invalid carrier number {}", carrier);
        assert!(
            freq_offset_val == 0 || freq_offset_val == 6250 || freq_offset_val == -6250 || freq_offset_val == 12500,
            "Invalid frequency offset {}",
            freq_offset_val
        );
        let duplex_spacing_val = if let Some(cds) = custom_duplex_spacing {
            cds
        } else {
            duplex_table
                .spacing(band, duplex_index)
                .ok_or_else(|| format!("Invalid duplex spacing for band {}, duplex index {}", band, duplex_index))?
        };

        Ok(Self {
            band,
            carrier,
            freq_offset_hz: freq_offset_val,
            duplex_spacing_id: duplex_index,
            duplex_spacing_val,
            reverse_operation,
        })
    }

    /// Get the standardized duplex spacing in hz for the current frequency band and a given
    /// duplex spacing table index, as given in the Sysinfo message
    pub fn get_default_duplex_spacing(band: u8, duplex_setting: u8) -> Option<u32> {
        assert!(duplex_setting < 8, "Invalid duplex setting {}", duplex_setting);
        let duplex_spacing = TETRA_DUPLEX_SPACING[duplex_setting as usize][band as usize];
        duplex_spacing.map(|v| v * 1000)
    }

    /// Get the downlink and uplink frequencies for this instance
    pub fn get_freqs(&self) -> (u32, u32) {
        // Compute dlfreq
        let mut dl_freq = 100000000 * self.band as i32;
        dl_freq += self.carrier as i32 * 25000;
        dl_freq += self.freq_offset_hz as i32;
        let dl_freq = dl_freq as u32;

        // Derive ulfreq
        let ul_freq = if !self.reverse_operation {
            dl_freq - self.duplex_spacing_val
        } else {
            dl_freq + self.duplex_spacing_val
        };
        (dl_freq, ul_freq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_freqinfo_from_components() {
        let freq = 400_000_000 + 1001 * 25_000;
        let duplex_index = 0;
        let duplex_spacing = 10000000;
        let reverse_operation = false;
        let band = 4;
        let carrier = 1001;
        let freq_offset = 0;

        let f1 = FreqInfo::from_components(band, carrier, freq_offset, reverse_operation, duplex_index, None).unwrap();
        let (dlfreq, ulfreq) = f1.get_freqs();

        assert_eq!(f1.band, band);
        assert_eq!(f1.carrier, carrier);
        assert_eq!(f1.freq_offset_hz, freq_offset);
        assert_eq!(f1.duplex_spacing_val, duplex_spacing);
        assert_eq!(f1.duplex_spacing_id, duplex_index);
        assert!(!f1.reverse_operation);
        assert_eq!(freq, dlfreq);
        assert_eq!(dlfreq - duplex_spacing, ulfreq);
        assert!(!f1.reverse_operation);
    }

    #[test]
    fn test_duplex_table_defaults_to_spec() {
        // An all-None table must resolve identically to the bare spec table.
        let table = DuplexTable::default();
        for band in 0u8..=7 {
            for index in 0u8..8 {
                assert_eq!(
                    table.spacing(band, index),
                    FreqInfo::get_default_duplex_spacing(band, index),
                    "mismatch at band {} index {}",
                    band,
                    index
                );
            }
        }
    }

    #[test]
    fn test_duplex_table_override_takes_precedence() {
        // Band 4, index 5 has a spec default of 9 500 kHz; override it.
        assert_eq!(FreqInfo::get_default_duplex_spacing(4, 5), Some(9_500_000));
        let mut table = DuplexTable::default();
        table.set(5, Some(9_400_000));
        assert_eq!(table.spacing(4, 5), Some(9_400_000));
        // A different, non-overridden index still falls back to the spec default.
        assert_eq!(table.spacing(4, 3), FreqInfo::get_default_duplex_spacing(4, 3));
    }

    #[test]
    fn test_duplex_table_override_defines_unspecified_index() {
        // Index 7 has no spec default on any band (None); an override defines it.
        assert_eq!(FreqInfo::get_default_duplex_spacing(5, 7), None);
        let mut table = DuplexTable::default();
        table.set(7, Some(10_000_000));
        assert_eq!(table.spacing(5, 7), Some(10_000_000));
    }

    #[test]
    fn test_from_components_with_table_uses_override() {
        let mut table = DuplexTable::default();
        table.set(5, Some(9_400_000));
        let f = FreqInfo::from_components_with_table(4, 1001, 0, false, 5, None, &table).unwrap();
        assert_eq!(f.duplex_spacing_val, 9_400_000);
        let (dl, ul) = f.get_freqs();
        assert_eq!(dl - 9_400_000, ul);
    }

    #[test]
    fn test_custom_spacing_overrides_table() {
        // Per-channel custom spacing wins over the table override.
        let mut table = DuplexTable::default();
        table.set(5, Some(9_400_000));
        let f = FreqInfo::from_components_with_table(4, 1001, 0, false, 5, Some(9_500_000), &table).unwrap();
        assert_eq!(f.duplex_spacing_val, 9_500_000);
    }
}
