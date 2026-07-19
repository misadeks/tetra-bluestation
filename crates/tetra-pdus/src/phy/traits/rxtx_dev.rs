use tetra_core::TdmaTime;
use tetra_core::TrainingSequence;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum RxTxDevError {
    RxEndOfData,
    RxReadError,
}

#[derive(Debug, Default)]
pub struct RxBurstBits<'a> {
    pub train_type: TrainingSequence,
    pub bits: &'a [u8],
}

#[derive(Debug, Default)]
pub struct RxSlotBits<'a> {
    /// Number of slot received
    pub time: TdmaTime,
    /// Burst received in full slot
    pub slot: RxBurstBits<'a>,
    /// Burst received in subslot 1
    pub subslot1: RxBurstBits<'a>,
    /// Burst received in subslot 2
    pub subslot2: RxBurstBits<'a>,
}

#[derive(Debug, Default)]
pub struct TxSlotBits<'a> {
    /// Number of slot to transmit
    pub time: TdmaTime,
    /// Burst to transmit in full slot
    pub slot: Option<&'a [u8]>,
    // /// Burst to transmit in subslot 1
    // pub subslot1: Option<&'a [u8]>,
    // /// Burst to transmit in subslot 2
    // pub subslot2: Option<&'a [u8]>,
}

/// RF signal path for the radio front end.
///
/// On full-duplex hardware (independent RX and TX chains) both paths are always
/// live and switching is a no-op. On half-duplex hardware the antenna is shared,
/// so a T/R changeover (e.g. an antenna relay or a GPIO-driven RF switch) must
/// connect the antenna to the PA only while transmitting and back to the LNA for
/// receive. `RfPath` selects which way that switch should point.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RfPath {
    /// Antenna routed to the receiver.
    Rx,
    /// Antenna routed to the transmitter.
    Tx,
}

/// Trait for RX/TX devices that work with full slots.
pub trait RxTxDev {
    fn rxtx_timeslot(&mut self, tx_slot: &[TxSlotBits]) -> Result<Vec<Option<RxSlotBits<'_>>>, RxTxDevError>;

    /// Switch the radio's antenna / RF path between receive and transmit.
    ///
    /// This is the changeover hook for half-duplex front ends: an implementation
    /// backed by a shared antenna would drive its T/R relay or RF switch here
    /// (e.g. toggle a GPIO) so the PA is only connected to the antenna during an
    /// uplink burst. The MS PHY calls this around the transmit window.
    ///
    /// The default is a no-op, which is correct for full-duplex radios with
    /// separate RX and TX chains (the current hardware): both paths are always
    /// connected and no switching is required.
    fn set_rf_path(&mut self, _path: RfPath) {}

    /// The TDMA slot currently at the device's *true* transmit frontier, i.e.
    /// "now" on the hardware TX clock, expressed in the same local TDMA basis as
    /// the demodulated downlink time.
    ///
    /// This is the clock the MS uplink scheduler must stay ahead of. It differs
    /// from the demodulated downlink time returned by [`Self::rxtx_timeslot`]:
    /// that downlink time is delayed by the RX processing pipeline (samples are
    /// delivered over the SDR transport and demodulated some slots after they
    /// were captured), whereas this reflects the true hardware clock the
    /// modulator transmits against. The MS PHY schedules uplink bursts relative
    /// to this frontier so they land in a slot the transmitter can actually
    /// still reach (see `PhyMs::schedule_uplink_time`).
    ///
    /// Returns `None` when the frontier is unknown — before downlink lock (no
    /// timing reference) or before transmission is possible. The default is
    /// `None`, which is appropriate for devices/tests that never transmit.
    fn tx_air_time(&self) -> Option<TdmaTime> {
        None
    }
}
