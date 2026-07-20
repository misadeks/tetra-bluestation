use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, BurstType, PhyBlockNum, PhyBlockType, Sap, TdmaTime, TrainingSequence, unimplemented_log};
use tetra_pdus::phy::traits::rxtx_dev::{RfPath, RxBurstBits, RxTxDev, TxSlotBits};
use tetra_saps::tlmb::TlmbMonitorInd;
use tetra_saps::tp::TpUnitdataInd;
use tetra_saps::{SapMsg, SapMsgInner};

use crate::phy::components::{burst_consts::*, slotter, train_consts::TIMESLOT_TYPE4_BITS};
use crate::{MessageQueue, TetraEntityTrait};

/// Consecutive demodulated downlink slots without a decodable burst
/// (training-sequence/AACH decode failure) after which the MS PHY declares a
/// serving-cell downlink failure to the MLE (ETSI TS 100 392-2 cl. 18.3.4.5.3).
///
/// Kept below the demodulator's own `slots_since_last_valid_burst >= 100`
/// revert-to-`DlUnsynchronized` threshold so that the failure is surfaced while
/// the demodulator is still producing (empty) slots — and the run loop is still
/// ticking — rather than after it has stopped, when no ticks would run.
const DOWNLINK_FAILURE_SLOTS: u32 = 50;

/// A fully-built uplink burst waiting to be transmitted in a specific slot.
///
/// Held between the `TpUnitdataReq` that produces it and the `drive_rx` call
/// whose device transaction actually schedules it onto the air. The bits are the
/// type-5 modulation bits of a Normal or Control Uplink Burst (SN1..SNmax); the
/// modulator places them within the slot per the burst delay (cl. 9.4.3.4).
struct PendingTx {
    /// Absolute TDMA time (demodulator-local basis) of the uplink slot the
    /// burst is scheduled in: the exact granted opportunity `dltime + 2`
    /// (cl. 9.3.9). Retirement compares against the TX generation frontier
    /// ([`RxTxDev::tx_air_time`]), which advances past this slot once the
    /// modulator has produced the burst.
    time: TdmaTime,
    /// Uplink burst modulation bits (NUB or CUB).
    burst: Vec<u8>,
}

/// MS-mode physical layer.
///
/// Unlike [`super::phy_bs::PhyBs`], which is timing master (its blocking
/// `rxtx_timeslot` call is the clock for the whole stack), the MS PHY is
/// **receive-driven**: it continuously demodulates the downlink, recovers
/// TDMA slot timing from the SYNC burst (ref. ETSI TS 100 392-2 clause 7),
/// and drives the stack clock from RX. Uplink transmission is added in a later
/// phase.
///
/// On every demodulated downlink slot the burst is split into its constituent
/// blocks (inverse of [`super::components::slotter::build_sdb`] /
/// [`super::components::slotter::build_ndb`], Clause 9.4.4.2.5/9.4.4.2.6) and
/// forwarded to [`crate::lmac::lmac_ms::LmacMs`] as `TpUnitdataInd`.
pub struct PhyMs<D: RxTxDev> {
    #[allow(dead_code)]
    config: SharedConfig,

    /// Downlink TDMA time, recovered from the received burst timing (a relative
    /// slot count until the BSCH content is decoded in a later phase).
    dltime: TdmaTime,

    /// Whether downlink frame synchronization has been achieved.
    synced: bool,

    /// Consecutive demodulated downlink slots with no decodable burst while
    /// synchronized. Drives the radio-link-failure declaration once it reaches
    /// [`DOWNLINK_FAILURE_SLOTS`] (cl. 18.3.4.5.3).
    slots_without_burst: u32,

    /// Uplink burst awaiting transmission in a granted slot, if any. `None`
    /// means the TX stream stays idle (silence) this cycle — the MS only puts
    /// energy on air during a granted opportunity.
    pending_tx: Option<PendingTx>,

    /// Whether the antenna is currently switched to the TX path. Tracks the
    /// half-duplex changeover hook so it is only toggled on transitions (no-op
    /// on full-duplex hardware).
    tx_path_active: bool,

    /// RX/TX device.
    rxtxdev: D,
}

impl<D: RxTxDev> PhyMs<D> {
    pub fn new(config: SharedConfig, rxtxdev: D) -> Self {
        Self {
            config,
            dltime: TdmaTime::default(),
            synced: false,
            slots_without_burst: 0,
            pending_tx: None,
            tx_path_active: false,
            rxtxdev,
        }
    }

    /// Notify the MLE of a serving-cell downlink monitoring transition.
    ///
    /// Emitted only on a change of the downlink-available state (failure or
    /// recovery). Internal IPC only (not an air-interface PDU); the MLE maps it
    /// onto the standardized MLE-BREAK / MLE-REOPEN primitives
    /// (cl. 18.3.4.5.3 / 18.3.4.7).
    fn emit_monitor_ind(queue: &mut MessageQueue, downlink_available: bool) {
        queue.push_back(SapMsg {
            sap: Sap::TlmbSap,
            src: TetraEntity::Phy,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::TlmbMonitorInd(TlmbMonitorInd { downlink_available }),
        });
    }

    /// Forward a single decoded downlink block to the LMAC over the TP SAP.
    fn send_rxblock_to_lmac(
        queue: &mut MessageQueue,
        train_type: TrainingSequence,
        burst_type: BurstType,
        block_type: PhyBlockType,
        block_num: PhyBlockNum,
        bits: BitBuffer,
    ) {
        let sapmsg = SapMsg {
            sap: Sap::TpSap,
            src: TetraEntity::Phy,
            dest: TetraEntity::Lmac,
            msg: SapMsgInner::TpUnitdataInd(TpUnitdataInd {
                train_type,
                burst_type,
                block_type,
                block_num,
                block: bits,
            }),
        };
        queue.push_back(sapmsg);
    }

    /// Reassemble the 30-bit broadcast block (BBK, carrying the AACH) that a
    /// normal downlink burst carries in two segments either side of the
    /// training sequence (Clause 9.4.4.2.5).
    fn extract_ndb_bbk(bits: &[u8]) -> BitBuffer {
        let mut bbk = BitBuffer::new(NDB_BBK_BITS);
        bbk.copy_bits_from_bitarr(&bits[NDB_BBK1_OFFSET..NDB_BBK1_OFFSET + NDB_BBK1_BITS]);
        bbk.copy_bits_from_bitarr(&bits[NDB_BBK2_OFFSET..NDB_BBK2_OFFSET + NDB_BBK2_BITS]);
        bbk.seek(0);
        bbk
    }

    /// Split a demodulated downlink slot into its type-5 blocks and forward
    /// them to the LMAC. The broadcast block (AACH) is sent first so the upper
    /// MAC can interpret the rest of the slot.
    fn split_dl_slot_and_send_to_lmac(queue: &mut MessageQueue, burst: &RxBurstBits<'_>) {
        let train_seq = burst.train_type;
        let bits = burst.bits;

        match train_seq {
            TrainingSequence::SyncTrainSeq => {
                // Synchronization downlink burst (SDB), Clause 9.4.4.2.6:
                // SB1 = BSCH (SYNC), BBK = AACH, SB2 = BNCH (broadcast).
                assert!(bits.len() == TIMESLOT_TYPE4_BITS);

                let bbk = BitBuffer::from_bitarr(&bits[SB_BBK_OFFSET..SB_BBK_OFFSET + SB_BBK_BITS]);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::SDB, PhyBlockType::BBK, PhyBlockNum::Undefined, bbk);

                let sb1 = BitBuffer::from_bitarr(&bits[SB_BLK1_OFFSET..SB_BLK1_OFFSET + SB_BLK1_BITS]);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::SDB, PhyBlockType::SB1, PhyBlockNum::Block1, sb1);

                let sb2 = BitBuffer::from_bitarr(&bits[SB_BLK2_OFFSET..SB_BLK2_OFFSET + SB_BLK2_BITS]);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::SDB, PhyBlockType::SB2, PhyBlockNum::Block2, sb2);
            }

            TrainingSequence::NormalTrainSeq1 => {
                // Normal downlink burst (NDB), Clause 9.4.4.2.5: the full slot
                // is a single logical block spanning both half-slots (SCH_F).
                assert!(bits.len() == TIMESLOT_TYPE4_BITS);

                let bbk = Self::extract_ndb_bbk(bits);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::NDB, PhyBlockType::BBK, PhyBlockNum::Undefined, bbk);

                let mut blk = BitBuffer::new(NDB_BLK_BITS * 2);
                blk.copy_bits_from_bitarr(&bits[NDB_BLK1_OFFSET..NDB_BLK1_OFFSET + NDB_BLK_BITS]);
                blk.copy_bits_from_bitarr(&bits[NDB_BLK2_OFFSET..NDB_BLK2_OFFSET + NDB_BLK_BITS]);
                blk.seek(0);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::NDB, PhyBlockType::NDB, PhyBlockNum::Both, blk);
            }

            TrainingSequence::NormalTrainSeq2 => {
                // Normal downlink burst (NDB): two independent half-slot blocks
                // (SCH_HD).
                assert!(bits.len() == TIMESLOT_TYPE4_BITS);

                let bbk = Self::extract_ndb_bbk(bits);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::NDB, PhyBlockType::BBK, PhyBlockNum::Undefined, bbk);

                let blk1 = BitBuffer::from_bitarr(&bits[NDB_BLK1_OFFSET..NDB_BLK1_OFFSET + NDB_BLK_BITS]);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::NDB, PhyBlockType::NDB, PhyBlockNum::Block1, blk1);

                let blk2 = BitBuffer::from_bitarr(&bits[NDB_BLK2_OFFSET..NDB_BLK2_OFFSET + NDB_BLK_BITS]);
                Self::send_rxblock_to_lmac(queue, train_seq, BurstType::NDB, PhyBlockType::NDB, PhyBlockNum::Block2, blk2);
            }

            other => {
                tracing::warn!("PhyMs: unexpected downlink training sequence {:?}, dropping slot", other);
            }
        }
    }

    /// Number of timeslots the uplink leads... i.e. the granted uplink slot is
    /// this many timeslots after the downlink slot that carried the grant. This
    /// is the fixed TETRA frame alignment (ETSI TS 100 392-2 cl. 9.3.9: "the
    /// ... uplink shall be delayed by the fixed period of 2 timeslots from the
    /// ... downlink"), and is the same offset UMAC's random access uses
    /// (`dltime.add_timeslots(2)`).
    const UPLINK_TIMESLOT_OFFSET: i32 = 2;

    /// Translate the granted uplink slot into the demodulator-local TDMA basis.
    ///
    /// The modulator times a burst as `reference_time + to_int(slot) *
    /// SAMPLES_SLOT`, where `reference_time` is anchored to the demodulator's
    /// *local* slot numbering (the first synchronized slot is numbered 0; see
    /// `demodulator.rs`). UMAC, however, expresses the granted uplink slot in
    /// the **network-absolute** TDMA time it decodes from each BSCH
    /// (`umac_ms.rs`, MAC-SYNC cl. 21.4.4.2). Those two bases differ by the
    /// network frame number in force when the MS synchronized (hundreds of
    /// slots), so feeding the network time straight into the modulator places
    /// the burst hundreds of slots into the future and only silence is emitted.
    ///
    /// Because every uplink is a random-access or reserved transmission at the
    /// fixed DL+2 opportunity, the granted slot is unambiguously
    /// `self.dltime + UPLINK_TIMESLOT_OFFSET` in the local basis, where
    /// `self.dltime` is the just-demodulated downlink slot (kept in the local
    /// basis by `drive_rx`). This gives the correct timeslot-within-frame phase
    /// *and* frame number of the uplink opportunity. With discontinuous TX (the
    /// transmitter is silent between bursts, so the generation frontier is not
    /// pinned ahead of the DAC — see `soapy_dev::TxDsp`), this exact slot is
    /// reachable, so it is transmitted as-is with no frame-advance.
    fn local_uplink_time(&self) -> TdmaTime {
        self.dltime.add_timeslots(Self::UPLINK_TIMESLOT_OFFSET)
    }

    /// Build the modulation bits of an uplink burst from an LMAC transmit
    /// request and tag it with the slot it must be sent in.
    ///
    /// The LMAC has already channel-encoded the MAC block to a type-5 block and
    /// selected the burst type from the logical channel (SCH/F -> Normal Uplink
    /// Burst, SCH/HU -> Control Uplink Burst; ETSI TS 100 392-2 cl. 9.4.4.2).
    /// Here we lay the type-5 block into its burst fields around the training
    /// sequence (the inverse of the BS uplink receiver) via
    /// [`slotter::build_nub`] / [`slotter::build_cub`].
    ///
    /// `time` is the granted uplink slot expressed in the **demodulator-local**
    /// TDMA basis (see [`Self::local_uplink_time`]), which is the basis the
    /// modulator's `reference_time` is anchored to.
    fn build_pending_tx(prim: tetra_saps::tp::TpUnitdataReqSlot, time: TdmaTime) -> PendingTx {
        let mut type5 = prim.blk1.expect("PhyMs uplink burst must carry blk1");
        type5.seek(0);

        let burst: Vec<u8> = match prim.burst_type {
            BurstType::NUB => {
                // SCH/F: the type-5 block spans both half-slots (two 216-bit
                // sub-blocks bkn1/bkn2), split either side of the normal training
                // sequence (cl. 9.4.4.2.4 / Table 9.5).
                let mut blk1 = [0u8; NUB_BLK_BITS];
                let mut blk2 = [0u8; NUB_BLK_BITS];
                type5.to_bitarr(&mut blk1);
                type5.to_bitarr(&mut blk2);
                slotter::build_nub(prim.train_type, &blk1, &blk2).to_vec()
            }
            BurstType::CUB => {
                // SCH/HU random/reserved access: a single 168-bit control block
                // split either side of the extended training sequence
                // (cl. 9.4.4.2.1 / Table 9.3).
                let mut blk = [0u8; CUB_BLK_BITS * 2];
                type5.to_bitarr(&mut blk);
                slotter::build_cub(&blk).to_vec()
            }
            other => panic!("PhyMs: unsupported uplink burst type {:?}", other),
        };

        PendingTx { time, burst }
    }
}

impl<D: RxTxDev + Send + 'static> TetraEntityTrait for PhyMs<D> {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Phy
    }

    fn rx_prim(&mut self, _queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);

        match message.sap {
            // Uplink transmit request: build the burst and hold it until the
            // next device transaction schedules it in its granted slot. We do
            // not touch the radio here; drive_rx owns the TX timing.
            Sap::TpSap => {
                let SapMsgInner::TpUnitdataReq(prim) = message.msg else {
                    panic!("PhyMs TpSap expected TpUnitdataReq, got {:?}", message.msg);
                };
                // Schedule at the exact granted uplink opportunity in the
                // demodulator-local basis (`dltime + 2`, ETSI TS 100 392-2
                // cl. 9.3.9 Frame alignment). `local_uplink_time` recovers that
                // slot's phase in the basis the modulator's `reference_time` is
                // anchored to (see `local_uplink_time`).
                //
                // With discontinuous uplink TX (the transmitter emits nothing
                // between bursts — see `soapy_dev::TxDsp` idle-skip) the TX
                // generation frontier is no longer pinned `dmax` blocks ahead of
                // the DAC: on the next burst the `dmin` fast-forward re-anchors
                // the frontier just ahead of the DAC, so the sweep starts before
                // `dltime + 2` and produces the burst in it. The granted slot is
                // therefore reachable and is honoured *exactly* for both
                // contention random access and reserved bursts (the MAC-END-HU
                // at the BS-granted slot, cl. 23.5.2.2.2) — no frame-advance, no
                // reserved-drop. TETRA has no timing-advance procedure;
                // propagation is absorbed by the uplink guard period
                // (cl. 9.4.3.4).
                let net_time = prim.time;
                let reserved = prim.reserved_access;
                let granted = self.local_uplink_time();

                let pending = Self::build_pending_tx(prim, granted);
                tracing::info!(
                    scheduled = %pending.time,
                    reserved,
                    network = ?net_time,
                    dl = %self.dltime,
                    bits = pending.burst.len(),
                    "PhyMs: queued uplink burst at exact grant (dltime+2)"
                );
                if self.pending_tx.is_some() {
                    tracing::warn!("PhyMs: overwriting an uplink burst that was not yet transmitted");
                }
                self.pending_tx = Some(pending);
            }
            Sap::TpcSap => {
                unimplemented_log!("PhyMs TpcSap not implemented yet");
            }
            _ => {
                panic!("PhyMs received unexpected SAP: {:?}", message.sap);
            }
        }
    }

    /// Receive-driven clock source for the MS run loop.
    ///
    /// Blocks on the downlink until the device demodulates a slot, forwards any
    /// decoded bursts to the LMAC, and returns the recovered TDMA time so the
    /// [`crate::MessageRouter`] can drive the stack clock (ETSI TS 100 392-2
    /// clause 7). While unsynchronized, `rxtx_timeslot` keeps consuming RX
    /// samples internally until the synchronization training sequence is found,
    /// so this naturally paces the loop from the air interface.
    ///
    /// When an uplink burst is pending it is handed to the device in the same
    /// transaction, scheduled at its granted slot time. The TX stream stays open
    /// but only carries energy during that slot; the rest of the time an empty
    /// TX slot is passed so the modulator emits silence.
    fn drive_rx(&mut self, queue: &mut MessageQueue) -> Option<TdmaTime> {
        let mut recovered: Option<TdmaTime> = None;
        let mut has_burst = false;

        // Only present a TX slot when there is a burst to send ("TX only when
        // needed"). The borrow of `self.pending_tx` is confined to this block so
        // the pending burst can be retired afterwards.
        {
            let tx_slots: Vec<TxSlotBits> = match &self.pending_tx {
                Some(pending) => vec![TxSlotBits {
                    time: pending.time,
                    slot: Some(&pending.burst),
                    ..Default::default()
                }],
                None => Vec::new(),
            };

            // Half-duplex antenna changeover: point the front end at the PA
            // while a burst is queued. No-op on full-duplex hardware.
            if !tx_slots.is_empty() && !self.tx_path_active {
                self.rxtxdev.set_rf_path(RfPath::Tx);
                self.tx_path_active = true;
            }

            let rx = self.rxtxdev.rxtx_timeslot(&tx_slots).expect("Got error from rxtx_timeslot");

            // The MS configures a single downlink demodulator, but the device
            // may return several entries (e.g. an unused uplink slot); process
            // whichever slots were actually demodulated.
            for rx_slot in rx.into_iter().flatten() {
                recovered = Some(rx_slot.time);
                if rx_slot.slot.train_type != TrainingSequence::NotFound {
                    has_burst = true;
                    Self::split_dl_slot_and_send_to_lmac(queue, &rx_slot.slot);
                }
            }
        }

        if let Some(time) = recovered {
            self.dltime = time;
            if has_burst {
                // A decodable burst was received: reset the failure counter and,
                // on the transition into sync (initial acquisition or recovery
                // from a declared downlink failure), notify the MLE. On initial
                // acquisition the MLE is not out-of-service, so the recovery
                // indication is an idempotent no-op there.
                self.slots_without_burst = 0;
                if !self.synced {
                    self.synced = true;
                    tracing::info!(ts = %self.dltime, "PhyMs: downlink synchronized");
                    Self::emit_monitor_ind(queue, true);
                }
            } else if self.synced {
                // Synchronized but this demodulated slot carried no decodable
                // burst (training-sequence/AACH decode failure, cl. 18.3.4.5.3).
                // Declare a serving-cell downlink failure to the MLE once the
                // failure persists past the threshold.
                self.slots_without_burst = self.slots_without_burst.saturating_add(1);
                if self.slots_without_burst >= DOWNLINK_FAILURE_SLOTS {
                    self.synced = false;
                    tracing::warn!(
                        ts = %self.dltime,
                        slots = self.slots_without_burst,
                        "PhyMs: serving-cell downlink failure (cl. 18.3.4.5.3)"
                    );
                    Self::emit_monitor_ind(queue, false);
                }
            }

            // Retire the pending burst once the TX generation frontier has
            // passed its scheduled slot: by then the modulator has already
            // swept through and produced the burst, so it is safe to drop it and
            // hand the antenna back to RX. Using the frontier (`tx_air_time`) —
            // the same clock the scheduling in `rx_prim` used — keeps the two
            // consistent; comparing against the pipeline-delayed downlink time
            // would retire the burst before it is produced.
            if self.tx_path_active {
                let now = self.rxtxdev.tx_air_time();
                let sent = match (&self.pending_tx, now) {
                    (Some(pending), Some(now)) => now.diff(pending.time) > 0,
                    (None, _) => true,
                    (Some(_), None) => false,
                };
                if sent {
                    self.pending_tx = None;
                    self.rxtxdev.set_rf_path(RfPath::Rx);
                    self.tx_path_active = false;
                }
            }
        }

        recovered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetra_config::bluestation::{SharedConfig, from_toml_str};
    use tetra_pdus::phy::traits::rxtx_dev::{RxBurstBits, RxSlotBits, RxTxDevError};
    use tetra_saps::tp::TpUnitdataReqSlot;

    /// Minimal valid MS config (mirrors `example_config/config-ms.toml`).
    const MS_TOML: &str = r#"
config_version = "0.6"
stack_mode = "Ms"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
tx_freq = 439825000
rx_freq = 430425000
ppm_err = 0
device = "driver=sx"
sample_rate = 600000
rx_antenna = "RX"
tx_antenna = "TX"
rx_gain_lna = 48.0
rx_gain_pga = 8.0
tx_gain_dac = 0.0
tx_gain_mixer = 0.0

[net_info]
mcc = 901
mnc = 9999

[cell_info]
freq_band = 4
main_carrier = 1593
duplex_spacing = 7
custom_duplex_spacing = 9400000
freq_offset = 0
reverse_operation = false
location_area = 1
colour_code = 1

[ms]
issi = 1000001
subscriber_class = 1
attach_groups = []
"#;

    /// A mock RX/TX device that records what the PHY asked the radio to do and
    /// replays a programmable downlink time, so PhyMs uplink scheduling can be
    /// tested without any SDR.
    #[derive(Default)]
    struct MockRxTx {
        /// One entry per `rxtx_timeslot` call: the uplink burst that was
        /// presented as `(slot time, burst bits)`, or `None` if the TX slot was
        /// empty (no burst to send).
        tx_calls: Vec<Option<(TdmaTime, Vec<u8>)>>,
        /// Antenna path switches, in the order they were requested.
        rf_path: Vec<RfPath>,
        /// Downlink TDMA time the next `rxtx_timeslot` call reports.
        next_time: TdmaTime,
        /// True hardware TX frontier the PHY sees via `tx_air_time`. `None`
        /// models "not locked / TX not possible yet".
        next_air_time: Option<TdmaTime>,
        /// When true, the next `rxtx_timeslot` returns a slot carrying a
        /// decodable burst (a valid training sequence); when false it returns a
        /// `NotFound` slot (demodulated but no decodable burst). Used to drive
        /// the radio-link-failure detection tests. An `ExtendedTrainSeq` with no
        /// bits is used so `split_dl_slot_and_send_to_lmac` takes its warn-and-
        /// drop path (no full burst buffer needed).
        rx_burst: bool,
    }

    impl RxTxDev for MockRxTx {
        fn rxtx_timeslot(&mut self, tx_slot: &[TxSlotBits]) -> Result<Vec<Option<RxSlotBits<'_>>>, RxTxDevError> {
            let record = tx_slot
                .first()
                .map(|s| (s.time, s.slot.map(<[u8]>::to_vec).unwrap_or_default()));
            self.tx_calls.push(record);

            let time = self.next_time;
            let train_type = if self.rx_burst {
                TrainingSequence::ExtendedTrainSeq
            } else {
                TrainingSequence::NotFound
            };
            Ok(vec![Some(RxSlotBits {
                time,
                // NotFound => drive_rx recovers the time but does not try to
                // split a (non-existent) full downlink burst. ExtendedTrainSeq
                // => a decodable burst is present (drive_rx counts it as sync).
                slot: RxBurstBits {
                    train_type,
                    bits: &[],
                },
                ..Default::default()
            })])
        }

        fn set_rf_path(&mut self, path: RfPath) {
            self.rf_path.push(path);
        }

        fn tx_air_time(&self) -> Option<TdmaTime> {
            self.next_air_time
        }
    }

    fn phy_ms(dev: MockRxTx) -> PhyMs<MockRxTx> {
        let cfg = from_toml_str(MS_TOML).expect("valid MS test config");
        PhyMs::new(SharedConfig::from_parts(cfg, None), dev)
    }

    /// A TP-UNITDATA request carrying a SCH/HU control block for `time`.
    /// `reserved` marks it as reserved-access (exact-slot-or-drop) vs contention.
    fn cub_uplink_req(time: TdmaTime, reserved: bool) -> SapMsg {
        let type5 = BitBuffer::from_bitarr(&[0u8; CUB_BLK_BITS * 2]);
        SapMsg {
            sap: Sap::TpSap,
            src: TetraEntity::Lmac,
            dest: TetraEntity::Phy,
            msg: SapMsgInner::TpUnitdataReq(TpUnitdataReqSlot {
                train_type: TrainingSequence::ExtendedTrainSeq,
                burst_type: BurstType::CUB,
                bbk: None,
                blk1: Some(type5),
                blk2: None,
                time: Some(time),
                reserved_access: reserved,
            }),
        }
    }

    /// With no pending uplink burst, the PHY presents an empty TX slot (silence)
    /// and never switches the antenna. "TX only when needed."
    #[test]
    fn test_no_burst_transmits_nothing() {
        let mut phy = phy_ms(MockRxTx::default());
        let mut queue = MessageQueue::new();

        phy.drive_rx(&mut queue);

        assert_eq!(phy.rxtxdev.tx_calls, vec![None], "empty TX slot when nothing pending");
        assert!(phy.rxtxdev.rf_path.is_empty(), "no antenna switch without a burst");
    }

    /// A queued uplink burst is scheduled at the exact granted slot
    /// (`dltime + 2`), presented to the device, the antenna switched to TX, and
    /// once the TX generation frontier passes the scheduled slot the burst is
    /// retired and the antenna handed back to RX.
    #[test]
    fn test_pending_burst_scheduled_then_retired() {
        let base = TdmaTime::default();
        // At rx_prim the PHY's dltime is the default `base`, so the burst is
        // scheduled at the exact granted opportunity base+2 (cl. 9.3.9),
        // independent of the frontier.
        let ul_time = base.add_timeslots(2);

        let mut phy = phy_ms(MockRxTx::default());
        let mut queue = MessageQueue::new();

        // Upper layers request an uplink transmission.
        phy.rx_prim(&mut queue, cub_uplink_req(ul_time, false));
        assert!(phy.pending_tx.is_some(), "burst queued by rx_prim");
        assert_eq!(
            phy.pending_tx.as_ref().unwrap().time.to_int(),
            ul_time.to_int(),
            "scheduled at the exact granted slot dltime+2"
        );

        // True frontier still before the scheduled slot: burst is presented,
        // antenna switched to TX, burst kept pending.
        phy.rxtxdev.next_time = base.add_timeslots(1);
        phy.rxtxdev.next_air_time = Some(base.add_timeslots(1));
        phy.drive_rx(&mut queue);
        let call0 = phy.rxtxdev.tx_calls[0].as_ref().expect("burst presented on first drive");
        assert_eq!(call0.0.to_int(), ul_time.to_int(), "scheduled at the granted slot time");
        assert_eq!(call0.1.len(), CUB_BURST_BITS, "control uplink burst bits");
        assert_eq!(phy.rxtxdev.rf_path, vec![RfPath::Tx]);
        assert!(phy.pending_tx.is_some(), "kept pending until frontier passes");

        // True frontier now past the scheduled slot: burst still presented this
        // cycle, then retired and antenna returned to RX.
        phy.rxtxdev.next_time = ul_time.add_timeslots(1);
        phy.rxtxdev.next_air_time = Some(ul_time.add_timeslots(1));
        phy.drive_rx(&mut queue);
        assert!(phy.rxtxdev.tx_calls[1].is_some(), "burst presented while still pending");
        assert!(phy.pending_tx.is_none(), "burst retired after frontier passed");
        assert_eq!(phy.rxtxdev.rf_path, vec![RfPath::Tx, RfPath::Rx]);

        // Nothing pending anymore: empty TX slot, no further antenna switching.
        phy.rxtxdev.next_time = ul_time.add_timeslots(2);
        phy.rxtxdev.next_air_time = Some(ul_time.add_timeslots(2));
        phy.drive_rx(&mut queue);
        assert!(phy.rxtxdev.tx_calls[2].is_none(), "TX idle once burst is gone");
        assert_eq!(phy.rxtxdev.rf_path, vec![RfPath::Tx, RfPath::Rx], "no extra switches");
    }

    /// Scheduling is now independent of the TX generation frontier: with
    /// discontinuous TX the granted slot `dltime + 2` (cl. 9.3.9) is always
    /// reachable, so both contention and reserved bursts are queued at exactly
    /// that slot even when the frontier happens to be far ahead — no
    /// frame-advance, no drop.
    #[test]
    fn test_burst_scheduled_at_exact_grant_regardless_of_frontier() {
        let base = TdmaTime::default();
        let granted = base.add_timeslots(2); // dltime(base) + 2

        for reserved in [false, true] {
            let mut phy = phy_ms(MockRxTx::default());
            let mut queue = MessageQueue::new();

            // Frontier reported far ahead of the opportunity — must not matter.
            phy.rxtxdev.next_air_time = Some(base.add_timeslots(10));
            phy.rx_prim(&mut queue, cub_uplink_req(granted, reserved));

            let pending = phy
                .pending_tx
                .as_ref()
                .expect("burst queued at exact grant regardless of frontier");
            assert_eq!(
                pending.time.to_int(),
                granted.to_int(),
                "burst (reserved={reserved}) scheduled at exactly dltime+2"
            );
        }
    }

    /// Count the downlink-monitoring indications the PHY emitted to the MLE and
    /// their availability flags, in order.
    fn monitor_inds(queue: &MessageQueue) -> Vec<bool> {
        queue
            .iter()
            .filter_map(|m| match &m.msg {
                SapMsgInner::TlmbMonitorInd(inner) => {
                    assert_eq!(m.dest, TetraEntity::Mle, "monitor indication addressed to MLE");
                    Some(inner.downlink_available)
                }
                _ => None,
            })
            .collect()
    }

    /// On initial acquisition the PHY emits exactly one "downlink available"
    /// monitoring indication and does not re-emit it on subsequent good slots.
    #[test]
    fn test_downlink_sync_emits_available_once() {
        let mut phy = phy_ms(MockRxTx::default());
        phy.rxtxdev.rx_burst = true;
        let mut queue = MessageQueue::new();

        for _ in 0..5 {
            phy.drive_rx(&mut queue);
        }

        assert!(phy.synced, "synchronized after a decodable burst");
        assert_eq!(monitor_inds(&queue), vec![true], "exactly one available indication");
    }

    /// After sync, a sustained run of demodulated-but-undecodable slots (past
    /// the threshold) makes the PHY declare a downlink failure exactly once
    /// (cl. 18.3.4.5.3); a shorter gap does not.
    #[test]
    fn test_downlink_failure_declared_after_threshold() {
        let mut phy = phy_ms(MockRxTx::default());
        let mut queue = MessageQueue::new();

        // Acquire sync.
        phy.rxtxdev.rx_burst = true;
        phy.drive_rx(&mut queue);
        assert!(phy.synced);

        // A gap shorter than the threshold: no failure yet.
        phy.rxtxdev.rx_burst = false;
        for _ in 0..(DOWNLINK_FAILURE_SLOTS - 1) {
            phy.drive_rx(&mut queue);
        }
        assert!(phy.synced, "still in service below the failure threshold");
        assert_eq!(monitor_inds(&queue), vec![true], "no failure indication yet");

        // Cross the threshold: exactly one failure indication.
        phy.drive_rx(&mut queue);
        assert!(!phy.synced, "downlink failure declared at the threshold");
        assert_eq!(monitor_inds(&queue), vec![true, false]);

        // Further missing slots do not re-declare failure.
        for _ in 0..10 {
            phy.drive_rx(&mut queue);
        }
        assert_eq!(monitor_inds(&queue), vec![true, false], "failure declared only once");
    }

    /// After a declared failure, the return of a decodable burst re-syncs and
    /// emits a "downlink available" recovery indication.
    #[test]
    fn test_downlink_recovery_emits_available() {
        let mut phy = phy_ms(MockRxTx::default());
        let mut queue = MessageQueue::new();

        phy.rxtxdev.rx_burst = true;
        phy.drive_rx(&mut queue);

        // Drive a full failure.
        phy.rxtxdev.rx_burst = false;
        for _ in 0..DOWNLINK_FAILURE_SLOTS {
            phy.drive_rx(&mut queue);
        }
        assert!(!phy.synced);
        assert_eq!(monitor_inds(&queue), vec![true, false]);

        // Recover: a decodable burst returns.
        phy.rxtxdev.rx_burst = true;
        phy.drive_rx(&mut queue);
        assert!(phy.synced, "re-synchronized on burst return");
        assert_eq!(monitor_inds(&queue), vec![true, false, true], "recovery indication emitted");
    }
}
