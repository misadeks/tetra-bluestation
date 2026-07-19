use crate::MessageQueue;
use as_any::AsAny;
use tetra_config::bluestation::SharedConfig;
use tetra_core::{TdmaTime, tetra_entities::TetraEntity};
use tetra_saps::SapMsg;

/// Trait for TETRA entities
/// Used by MessageRouter for passing messages between entities
pub trait TetraEntityTrait: Send + AsAny {
    /// Returns the entity type identifier
    fn entity(&self) -> TetraEntity;

    /// Handle incoming SAP primitive
    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg);

    /// Update configuration (optional)
    #[allow(dead_code)]
    fn set_config(&mut self, _config: SharedConfig) {}

    /// Drive receive-timing for RX-first (MS/Mon) modes.
    ///
    /// BS-mode entities are timing masters and do not implement this. An MS/Mon
    /// PHY overrides it to block on the downlink, forward the demodulated bursts
    /// into `queue`, and return the TDMA time recovered from the received slot
    /// (ref. ETSI TS 100 392-2 clause 7) so the [`crate::MessageRouter`] can
    /// drive the stack clock from RX. Returns `None` when no slot was produced.
    fn drive_rx(&mut self, _queue: &mut MessageQueue) -> Option<TdmaTime> {
        None
    }

    /// Called at the start of each TDMA tick
    fn tick_start(&mut self, _queue: &mut MessageQueue, _ts: TdmaTime) {}

    /// Called at the end of each TDMA tick
    fn tick_end(&mut self, _queue: &mut MessageQueue, _ts: TdmaTime) -> bool {
        false
    }

    /// Begin the MS de-registration (ITSI detach) procedure at shutdown
    /// (ETSI TS 100 392-2 clause 16.6.1). Called by the [`crate::MessageRouter`]
    /// when the stack is asked to stop. An MS MM entity that is currently
    /// registered overrides this to emit a U-ITSI DETACH PDU and returns `true`
    /// to indicate the stack should keep running (bounded) so the burst can be
    /// transmitted over the air before the SDR streams close. The default is a
    /// no-op returning `false` (nothing to detach / not applicable).
    fn begin_deregistration(&mut self, _queue: &mut MessageQueue) -> bool {
        false
    }

    /// While shutting down, returns `true` if a de-registration initiated by
    /// [`Self::begin_deregistration`] is still in progress (the U-ITSI DETACH
    /// has not yet had time to be transmitted). The router keeps driving the
    /// stack until this returns `false` (or a bound is reached).
    fn deregistration_pending(&self) -> bool {
        false
    }
}
