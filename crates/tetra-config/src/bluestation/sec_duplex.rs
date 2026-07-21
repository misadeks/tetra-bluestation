use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use toml::Value;

use tetra_core::freqs::DuplexTable;

/// On-disk shape of the optional `[duplex_table]` section.
///
/// A programmed radio ("codeplug") may carry its own 8-entry duplex-spacing
/// table (the 3-bit duplex-spacing index of MAC-SYSINFO / D-MLE-SYSINFO,
/// ETSI TS 100 392-2). Each override is a `[duplex_index, spacing_hz]` pair;
/// indices not listed fall back to the ETSI TS 100 392-15 clause 6 default for
/// the operating band. An absent section (or empty `overrides`) means "use the
/// spec defaults for every index".
///
/// Example:
/// ```toml
/// [duplex_table]
/// overrides = [[5, 9400000]]
/// ```
#[derive(Default, Deserialize, Serialize)]
pub struct DuplexTableDto {
    /// `[duplex_index (0..7), spacing_hz]` override pairs.
    pub overrides: Option<Vec<(u8, u32)>>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

/// Build a runtime [`DuplexTable`] from the DTO, validating each override.
///
/// Rejects out-of-range indices (must be 0..7, the 3-bit field) and duplicate
/// indices (an index may be programmed at most once).
pub fn duplex_dto_to_cfg(dto: DuplexTableDto) -> Result<DuplexTable, String> {
    let mut table = DuplexTable::default();
    if let Some(overrides) = dto.overrides {
        for (index, spacing_hz) in overrides {
            if (index as usize) >= DuplexTable::LEN {
                return Err(format!(
                    "duplex_table override index {} out of range (must be 0-{})",
                    index,
                    DuplexTable::LEN - 1
                ));
            }
            if table.entries()[index as usize].is_some() {
                return Err(format!("duplex_table override index {} specified more than once", index));
            }
            table.set(index, Some(spacing_hz));
        }
    }
    Ok(table)
}

/// Inverse of [`duplex_dto_to_cfg`] for TOML write-back (Plane B, non-standard).
///
/// Emits one `[duplex_index, spacing_hz]` pair per programmed override, in
/// index order. A table with no overrides yields `overrides: None` so the whole
/// section can be omitted from the serialized config.
pub fn cfg_to_duplex_dto(table: &DuplexTable) -> DuplexTableDto {
    let overrides: Vec<(u8, u32)> = table
        .entries()
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| entry.map(|hz| (idx as u8, hz)))
        .collect();
    DuplexTableDto {
        overrides: if overrides.is_empty() { None } else { Some(overrides) },
        extra: HashMap::new(),
    }
}

/// True when the table carries no programmed overrides (pure spec defaults), so
/// the `[duplex_table]` section can be dropped from serialized output.
pub fn duplex_table_is_default(table: &DuplexTable) -> bool {
    table.entries().iter().all(Option::is_none)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_duplex_dto_roundtrip() {
        let dto = DuplexTableDto {
            overrides: Some(vec![(5, 9_400_000), (1, 1_600_000)]),
            extra: HashMap::new(),
        };
        let table = duplex_dto_to_cfg(dto).unwrap();
        assert_eq!(table.entries()[5], Some(9_400_000));
        assert_eq!(table.entries()[1], Some(1_600_000));
        assert_eq!(table.entries()[0], None);

        // Round-trip back to DTO: overrides emitted in index order.
        let back = cfg_to_duplex_dto(&table);
        assert_eq!(back.overrides, Some(vec![(1, 1_600_000), (5, 9_400_000)]));
    }

    #[test]
    fn test_duplex_dto_rejects_out_of_range_index() {
        let dto = DuplexTableDto {
            overrides: Some(vec![(8, 9_400_000)]),
            extra: HashMap::new(),
        };
        assert!(duplex_dto_to_cfg(dto).is_err());
    }

    #[test]
    fn test_duplex_dto_rejects_duplicate_index() {
        let dto = DuplexTableDto {
            overrides: Some(vec![(5, 9_400_000), (5, 1_600_000)]),
            extra: HashMap::new(),
        };
        assert!(duplex_dto_to_cfg(dto).is_err());
    }

    #[test]
    fn test_default_table_serializes_empty() {
        let table = DuplexTable::default();
        assert!(duplex_table_is_default(&table));
        assert_eq!(cfg_to_duplex_dto(&table).overrides, None);
    }
}
