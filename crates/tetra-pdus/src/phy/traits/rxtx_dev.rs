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

/// Diagnostic snapshot of the MS transmit look-ahead: how far the TX
/// *generation frontier* currently leads the hardware DAC pointer ("now").
///
/// This is the **T** term of the reserved-slot reachability budget. To transmit
/// in the exact BS-granted uplink slot (`dltime + 2`, ETSI TS 100 392-2
/// cl. 9.3.9), the frontier must not already be past it, i.e.
/// `T (this look-ahead) + L (RX demod latency) <= 2 timeslots` (the fixed
/// DL->UL turnaround). This report measures T so it can be logged and split out
/// from the total (L is derived as `total - T`); it does not change any timing.
///
/// It is expressed in modem TX blocks (the exact quantity the generator gates
/// against its maximum look-ahead window) as well as the equivalent timeslots.
#[derive(Debug, Clone, Copy)]
pub struct MsTxLookahead {
    /// Look-ahead in modem TX blocks: `block_count - DAC_block`, the same
    /// quantity the generator caps at [`Self::max_blocks`].
    pub blocks: i64,
    /// The generator's maximum look-ahead window, in blocks. `blocks` is kept
    /// below this; the head-room `max_blocks - blocks` shows how much the cap
    /// could be reduced before it starts clipping generation.
    pub max_blocks: i64,
    /// `blocks` converted to TDMA timeslots (1 timeslot = 1020 modem samples).
    pub slots: f64,
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

    /// The TDMA slot at the device's transmit **generation frontier** — the
    /// point the modulator is currently producing signal for — expressed in the
    /// same local TDMA basis as the demodulated downlink time.
    ///
    /// This is the line the MS uplink scheduler must stay ahead of. Note it is
    /// deliberately *not* the demodulated downlink time (delayed by the RX
    /// pipeline) nor the true hardware "now": transmit blocks are produced a
    /// look-ahead window ahead of real time and the frontier only ever advances,
    /// so a burst must be scheduled ahead of this frontier to be emitted rather
    /// than produced as silence (see `PhyMs::schedule_uplink_time`).
    ///
    /// Returns `None` when the frontier is unknown — before downlink lock (no
    /// timing reference), before transmission is possible, or before any block
    /// has been generated. The default is `None`, which is appropriate for
    /// devices/tests that never transmit.
    fn tx_air_time(&self) -> Option<TdmaTime> {
        None
    }

    /// Diagnostic: current MS transmit look-ahead (the **T** term of the
    /// reserved-slot reachability budget; see [`MsTxLookahead`]).
    ///
    /// Returns how far the TX generation frontier leads the hardware DAC pointer
    /// right now, so it can be logged alongside the total frontier deficit to
    /// separate the TX look-ahead from the RX demodulation latency. Purely a
    /// measurement — it has no effect on scheduling. `None` on devices/tests
    /// that never transmit or before transmission is possible (the default).
    fn ms_tx_lookahead(&self) -> Option<MsTxLookahead> {
        None
    }

    /// Most recent uncalibrated downlink RSSI (dBFS) measured on the serving-cell
    /// downlink, or `None` before the first downlink slot has been demodulated /
    /// on devices that do not receive.
    ///
    /// The value is relative to the demodulator full-scale magnitude
    /// (`1.0 == 0 dBFS`), i.e. `10*log10(mean(|s|^2))` over a downlink slot; it
    /// is not referenced to an absolute antenna power. The MLE uses serving-cell
    /// signal strength as a reselection input (ETSI TS 100 392-2 cl. 18.3.4) and
    /// it is surfaced to the MS management UI as a receive-level indicator. The
    /// default is `None`, appropriate for devices/tests that never receive.
    fn dl_rssi_dbfs(&self) -> Option<f32> {
        None
    }
}
