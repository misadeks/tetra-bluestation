use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{Sap, TdmaTime, unimplemented_log};
use tetra_pdus::phy::traits::rxtx_dev::RxTxDev;
use tetra_saps::SapMsg;

use crate::{MessageQueue, TetraEntityTrait};

/// MS-mode physical layer.
///
/// Unlike [`super::phy_bs::PhyBs`], which is timing master (its blocking
/// `rxtx_timeslot` call is the clock for the whole stack), the MS PHY is
/// **receive-driven**: it continuously demodulates the downlink, recovers
/// TDMA frame timing from the SYNC burst (ref. ETSI TS 100 392-2 clause 7),
/// and only later transmits uplink bursts in granted slots.
///
/// This is a Phase 0 skeleton: it constructs the RX/TX device and registers
/// as the `Phy` entity so an MS stack can be assembled. The downlink receive
/// path and the RX-driven clock are implemented in Phase 1
/// (`phy_ms::rx`/`MessageRouter` MS run loop).
pub struct PhyMs<D: RxTxDev> {
    config: SharedConfig,

    /// Downlink TDMA time, recovered from the received SYNC burst (Phase 1).
    dltime: TdmaTime,

    /// Whether downlink frame synchronization has been achieved.
    synced: bool,

    /// RX/TX device.
    rxtxdev: D,
}

impl<D: RxTxDev> PhyMs<D> {
    pub fn new(config: SharedConfig, rxtxdev: D) -> Self {
        Self {
            config,
            dltime: TdmaTime::default(),
            synced: false,
            rxtxdev,
        }
    }
}

impl<D: RxTxDev + Send + 'static> TetraEntityTrait for PhyMs<D> {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Phy
    }

    fn rx_prim(&mut self, _queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);

        match message.sap {
            // Uplink transmit requests (Phase 3).
            Sap::TpSap => {
                unimplemented_log!("PhyMs TpSap (uplink transmit) not implemented yet");
            }
            Sap::TpcSap => {
                unimplemented_log!("PhyMs TpcSap not implemented yet");
            }
            _ => {
                panic!("PhyMs received unexpected SAP: {:?}", message.sap);
            }
        }
    }
}
