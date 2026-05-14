use std::collections::HashMap;

use tetra_core::{EndpointId, LinkId, MleHandle, TdmaTime, TetraAddress};

pub struct MleConnState {
    _handle: MleHandle,
    addr: TetraAddress,
    link_id: LinkId,
    endpoint_id: EndpointId,
    ts_created: TdmaTime,
    ts_last_used: TdmaTime,
}

pub struct MleRouter {
    states: HashMap<MleHandle, MleConnState>,
    next_handle: MleHandle,
}

impl MleRouter {
    pub fn new() -> Self {
        Self {
            states: HashMap::new(),
            next_handle: 1,
        }
    }

    pub fn create_handle(&mut self, _addr: TetraAddress, _link_id: LinkId, _endpoint_id: EndpointId, _ts: TdmaTime) -> MleHandle {
        let handle = self.next_handle;
        // TODO: re-enable handle insertion once MLE handle routing is used.
        self.next_handle += 1;
        handle
    }

    pub fn use_handle(&mut self, handle: MleHandle, ts: TdmaTime) -> (TetraAddress, LinkId, EndpointId) {
        if let Some(conn) = self.states.get_mut(&handle) {
            conn.ts_last_used = ts;
            (conn.addr, conn.link_id, conn.endpoint_id)
        } else {
            tracing::warn!("Unknown MLE handle: {}", handle);
            (TetraAddress::issi(0), 0, 0)
        }
    }

    pub fn delete_handle(&mut self, handle: MleHandle) -> Option<MleConnState> {
        self.states.remove(&handle)
    }

    pub fn dump_mappings(&self) {
        tracing::info!("MLE Router mappings:");
        for (handle, conn) in &self.states {
            tracing::info!(
                "Handle {} -> Addr: {}, Link ID: {}, Endpoint ID: {}, Created: {}, Last Used: {}",
                handle,
                conn.addr,
                conn.link_id,
                conn.endpoint_id,
                conn.ts_created,
                conn.ts_last_used
            );
        }
    }
}
