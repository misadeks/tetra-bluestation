use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use once_cell::sync::Lazy;

/// GSSI -> {SSI, SSI, ...}
pub static GROUP_MEMBERS: Lazy<Mutex<HashMap<u32, HashSet<u32>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn attach(ssi: u32, gssi: u32) {
    let mut m = GROUP_MEMBERS.lock().unwrap();
    m.entry(gssi).or_default().insert(ssi);
}

pub fn detach(ssi: u32, gssi: u32) {
    let mut m = GROUP_MEMBERS.lock().unwrap();
    if let Some(set) = m.get_mut(&gssi) {
        set.remove(&ssi);
        if set.is_empty() {
            m.remove(&gssi);
        }
    }
}

pub fn members(gssi: u32) -> Vec<u32> {
    let m = GROUP_MEMBERS.lock().unwrap();
    m.get(&gssi)
        .map(|s| s.iter().copied().collect())
        .unwrap_or_default()
}

/// Remove `ssi` from all known groups.
///
/// Useful when an MS detaches from all groups or when the stack resets state.
pub fn detach_all(ssi: u32) {
    let mut m = GROUP_MEMBERS.lock().unwrap();
    let keys: Vec<u32> = m.keys().copied().collect();
    for gssi in keys {
        detach(ssi, gssi);
    }
}
