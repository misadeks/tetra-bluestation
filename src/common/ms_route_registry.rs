use std::collections::HashMap;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use crate::common::address::TetraAddress;

#[derive(Clone, Copy, Debug)]
pub struct MsRoute {
    pub endpoint_id: i32,
    pub link_id: i32,
    pub addr: TetraAddress,
}

#[derive(Default)]
struct RouteState {
    /// SSI -> route (forward lookup)
    by_ssi: HashMap<u32, MsRoute>,
    /// (endpoint_id, link_id) -> SSI (reverse lookup)
    by_ep_link: HashMap<(i32, i32), u32>,
}

pub static ROUTES: Lazy<Mutex<RouteState>> =
    Lazy::new(|| Mutex::new(RouteState::default()));

pub fn upsert(ssi: u32, route: MsRoute) {
    let mut st = ROUTES.lock().unwrap();
    st.by_ep_link.insert((route.endpoint_id, route.link_id), ssi);
    st.by_ssi.insert(ssi, route);
}

pub fn get(ssi: u32) -> Option<MsRoute> {
    ROUTES.lock().unwrap().by_ssi.get(&ssi).copied()
}

/// Reverse lookup: resolve SSI by endpoint/link id, used when upper layers (e.g. CMCE)
/// route by endpoint/link but LLC needs a main_address.
pub fn get_ssi_by_endpoint_link(endpoint_id: i32, link_id: i32) -> Option<u32> {
    ROUTES
        .lock()
        .unwrap()
        .by_ep_link
        .get(&(endpoint_id, link_id))
        .copied()
}
