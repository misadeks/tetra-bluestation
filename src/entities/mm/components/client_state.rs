#[derive(Debug)]
pub enum ClientMgrErr {
    ClientNotFound { issi: u32 },
    GroupNotFound { gssi: u32 },
    IssiInGroupRange { issi: u32 },
    GssiInClientRange { gssi: u32 },    
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum MmClientState {
    Unknown,
    Attached,
    Detached,
}

pub struct MmClientProperties {
    pub ssi: u32,
    pub state: MmClientState,
    pub groups: std::collections::HashSet<u32>,
    // pub last_seen: TdmaTime,
}

impl MmClientProperties {
    pub fn new(ssi: u32) -> Self {
        MmClientProperties {
            ssi,
            state: MmClientState::Unknown,
            groups: std::collections::HashSet::new(),
            // last_seen: TdmaTime::default(),
        }
    }
}

/// Stub function, to be replaced with checks based on configuration file
fn is_individual(_issi: u32) -> bool { return true; }
/// Stub function, to be replaced with checks based on configuration file
fn in_group_range(_gssi: u32) -> bool { return true; }
/// Stub function, to be replaced with checks based on configuration file
fn is_group(_gssi: u32) -> bool { return true; }
/// Stub function, to be replaced with checks based on configuration file
fn may_attach(_issi: u32, _gssi: u32) -> bool { return true; }


pub struct MmClientMgr {
    clients: std::collections::HashMap<u32, MmClientProperties>,
}

impl MmClientMgr {
    pub fn new() -> Self {
        MmClientMgr {
            clients: std::collections::HashMap::new(),
        }
    }

    pub fn get_client_by_issi(&mut self, issi: u32) -> Option<&MmClientProperties> {
        self.clients.get(&issi)
    }

    pub fn client_is_known(&self, issi: u32) -> bool {
        self.clients.contains_key(&issi)
    }

    /// Registers or updates a client.
    ///
    /// IMPORTANT: If the client already exists, this function **must not** discard the previous
    /// state, otherwise the stored DGNA group list gets cleared.
    pub fn register_client(&mut self, issi: u32, attached: bool) -> Result<bool, ClientMgrErr> {
        if !is_individual(issi) {
            return Err(ClientMgrErr::IssiInGroupRange { issi });
        }

        // If client already exists, only update the state and keep groups.
        if let Some(client) = self.clients.get_mut(&issi) {
            let old_state = client.state;
            client.state = if attached { MmClientState::Attached } else { MmClientState::Unknown };
            tracing::debug!(
                "register_client: update existing issi={} state {:?} -> {:?} groups={:?}",
                issi,
                old_state,
                client.state,
                client.groups
            );
            return Ok(true);
        }

        // Create new client entry.
        let mut elem = MmClientProperties::new(issi);
        elem.state = if attached { MmClientState::Attached } else { MmClientState::Unknown };
        self.clients.insert(issi, elem);
        tracing::debug!("register_client: new issi={} state={:?}", issi, if attached { MmClientState::Attached } else { MmClientState::Unknown });
        Ok(true)
    }

    /// Removes a client from the registry, returning its properties if found
    pub fn remove_client(&mut self, ssi: u32) -> Option<MmClientProperties> {
        self.clients.remove(&ssi)
    }

    /// Detaches all groups from a client
    pub fn client_detach_all_groups(&mut self, issi: u32) -> Result<bool, ClientMgrErr> {
        if let Some(client) = self.clients.get_mut(&issi) {
            let old_groups: Vec<u32> = client.groups.iter().copied().collect();
            client.groups.clear();
            tracing::debug!("client_detach_all_groups: issi={} cleared {:?}", issi, old_groups);
            Ok(true)
        } else {
            Err(ClientMgrErr::ClientNotFound { issi })
        }
    }

    /// Attaches or detaches a client from a group
    pub fn client_group_attach(&mut self, issi: u32, gssi: u32, do_attach: bool) -> Result<bool, ClientMgrErr> {

        // Checks
        if !in_group_range(gssi) {
            return Err(ClientMgrErr::GssiInClientRange { gssi });
        };
        if !is_group(gssi) {
            return Err(ClientMgrErr::GroupNotFound { gssi });
        };
        if !may_attach(issi, gssi) {
            return Err(ClientMgrErr::GroupNotFound { gssi });
        };

        if let Some(client) = self.clients.get_mut(&issi) {
            if do_attach {
                let was_new = client.groups.insert(gssi);
                tracing::debug!("client_group_attach: issi={} attach gssi={} ({})", issi, gssi, if was_new { "new" } else { "existing" });
            } else {
                let was_present = client.groups.remove(&gssi);
                tracing::debug!("client_group_attach: issi={} detach gssi={} ({})", issi, gssi, if was_present { "removed" } else { "not_present" });
            }
            tracing::debug!("client_group_attach: issi={} groups_now={:?}", issi, client.groups);
            Ok(true)
        } else {
            Err(ClientMgrErr::ClientNotFound { issi })
        }
    }
}