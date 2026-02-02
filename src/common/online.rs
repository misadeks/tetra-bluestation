//! Online subscriber tracking (best-effort).
//!
//! This module provides a best-effort view of "online" SSIs based on recently
//! observed air-interface activity (e.g., uplink SDS).
//!
//! It does NOT guarantee true group membership. For groups (GSSI), it keeps a
//! "recently active for this group" set, derived from observed traffic.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default, Debug)]
struct OnlineState {
    last_seen: HashMap<u32, u64>,
    group_last_seen: HashMap<u32, HashMap<u32, u64>>,
}

static STATE: OnceLock<Mutex<OnlineState>> = OnceLock::new();

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn state() -> &'static Mutex<OnlineState> {
    STATE.get_or_init(|| Mutex::new(OnlineState::default()))
}

/// Record that an SSI was seen "online" right now.
pub fn record_seen(ssi: u32) {
    let mut st = state().lock().unwrap();
    st.last_seen.insert(ssi, now_secs());
}

/// Record that an SSI was recently active for a given GSSI group.
pub fn record_group_seen(gssi: u32, ssi: u32) {
    let mut st = state().lock().unwrap();
    let t = now_secs();
    st.last_seen.insert(ssi, t);
    st.group_last_seen.entry(gssi).or_default().insert(ssi, t);
}

/// Snapshot online SSIs within TTL seconds.
pub fn snapshot_online(ttl_secs: u64) -> Vec<u32> {
    let cutoff = now_secs().saturating_sub(ttl_secs);
    let st = state().lock().unwrap();
    let mut v: Vec<u32> = st
        .last_seen
        .iter()
        .filter_map(|(ssi, ts)| if *ts >= cutoff { Some(*ssi) } else { None })
        .collect();
    v.sort_unstable();
    v
}

/// Snapshot SSIs recently active for `gssi` within TTL seconds (best-effort).
pub fn snapshot_group_online(gssi: u32, ttl_secs: u64) -> Vec<u32> {
    let cutoff = now_secs().saturating_sub(ttl_secs);
    let st = state().lock().unwrap();
    let Some(m) = st.group_last_seen.get(&gssi) else { return Vec::new(); };
    let mut v: Vec<u32> = m
        .iter()
        .filter_map(|(ssi, ts)| if *ts >= cutoff { Some(*ssi) } else { None })
        .collect();
    v.sort_unstable();
    v
}

/// Snapshot all groups with recent activity within TTL seconds (best-effort).
///
/// Returns a sorted list of (gssi, [ssi...]) pairs.
pub fn snapshot_group_online_all(ttl_secs: u64) -> Vec<(u32, Vec<u32>)> {
    let cutoff = now_secs().saturating_sub(ttl_secs);
    let st = state().lock().unwrap();
    let mut groups: Vec<(u32, Vec<u32>)> = Vec::new();
    for (gssi, m) in &st.group_last_seen {
        let mut ids: Vec<u32> = m
            .iter()
            .filter_map(|(ssi, ts)| if *ts >= cutoff { Some(*ssi) } else { None })
            .collect();
        if ids.is_empty() {
            continue;
        }
        ids.sort_unstable();
        groups.push((*gssi, ids));
    }
    groups.sort_unstable_by_key(|(g, _)| *g);
    groups
}
