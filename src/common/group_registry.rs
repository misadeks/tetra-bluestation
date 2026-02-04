use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// GSSI -> {SSI, SSI, ...}
static GROUP_MEMBERS: OnceLock<Mutex<HashMap<u32, HashSet<u32>>>> = OnceLock::new();

fn members_map() -> &'static Mutex<HashMap<u32, HashSet<u32>>> {
    GROUP_MEMBERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn attach(ssi: u32, gssi: u32) {
    let mut m = members_map().lock().unwrap();
    m.entry(gssi).or_default().insert(ssi);
}

pub fn detach(ssi: u32, gssi: u32) {
    let mut m = members_map().lock().unwrap();
    if let Some(set) = m.get_mut(&gssi) {
        set.remove(&ssi);
        if set.is_empty() {
            m.remove(&gssi);
        }
    }
}

pub fn members(gssi: u32) -> Vec<u32> {
    let m = members_map().lock().unwrap();
    m.get(&gssi)
        .map(|s| s.iter().copied().collect::<Vec<u32>>())
        .unwrap_or_default()
}

/// Remove `ssi` from all known groups.
///
/// Useful when an MS detaches from all groups or when the stack resets state.
pub fn detach_all(ssi: u32) {
    let mut m = members_map().lock().unwrap();
    m.retain(|_, set| {
        set.remove(&ssi);
        !set.is_empty()
    });
}
