//! Resampling, buffering and timestamp handling
//! between SDR device and modulator/demodulator code.

use rustfft;
use tetra_config::bluestation::{SharedConfig, StackMode};
use tetra_core::TdmaTime;

use tetra_pdus::phy::traits::rxtx_dev::RxSlotBits;
use tetra_pdus::phy::traits::rxtx_dev::RxTxDev;
use tetra_pdus::phy::traits::rxtx_dev::RxTxDevError;
use tetra_pdus::phy::traits::rxtx_dev::TxSlotBits;
use tetra_pdus::phy::traits::rxtx_dev::RfPath;

use crate::phy::components::soapy_dev;

use super::demodulator;
use super::dsp_types::*;
use super::fcfb;
use super::modulator;
use super::soapyio;

/// Fixed sample offset added to the downlink-derived timing reference when
/// aligning the MS uplink modulator. It absorbs the residual delay between the
/// RX and TX signal chains (analog front end + DSP pipeline) so the burst lands
/// within the uplink burst timing / guard period (ETSI TS 100 392-2 cl. 9.4.3.4;
/// note TETRA has no active timing-advance procedure -- MS-BS propagation is
/// absorbed by the uplink guard period). Zero is the nominal starting point;
/// this is the single knob to tune against a real base station if the burst
/// lands slightly early/late in the uplink slot. Positive = transmit later.
const MS_TX_SAMPLE_DELAY: SampleCount = 0;

pub struct SdrConfig<'a> {
    /// SoapySDR device arguments
    pub dev_args: &'a [(&'a str, &'a str)],
    /// SDR RX center frequency
    pub rx_freq: Option<f64>,
    /// SDR TX center frequency
    pub tx_freq: Option<f64>,
}

#[derive(Default)]
pub struct PhyConfig<'a> {
    /// Downlink/uplink carrier frequency pairs to monitor.
    /// Uplink frequency can be set to None to monitor downlink only.
    pub monitor_frequencies: &'a [(f64, Option<f64>)],
    /// Downlink carrier frequencies for a BS.
    pub bs_dl_frequencies: &'a [f64],
    /// Uplink carrier frequencies for a BS.
    pub bs_ul_frequencies: &'a [f64],
    /// Uplink carrier frequencies for an MS to transmit on. Modulated with the
    /// uplink burst timing (discontinuous bursts, ETSI TS 100 392-2 cl. 9.4.3.4).
    pub ms_ul_frequencies: &'a [f64],
}

pub struct RxTxDevSoapySdr {
    sdr: soapyio::SoapyIo,
    rx_dsp: Option<RxDsp>,
    tx_dsp: Option<TxDsp>,
}

type FftPlanner = rustfft::FftPlanner<RealSample>;

impl RxTxDevSoapySdr {
    pub fn new(cfg: &SharedConfig) -> Self {
        let mut fft_planner = rustfft::FftPlanner::new();

        let config_guard = cfg.config();
        let soapy_cfg = config_guard
            .as_ref()
            .phy_io
            .soapysdr
            .as_ref()
            .expect("Soapysdr config must be set for Soapysdr PhyIo");

        let (dl_corrected, dl_err) = soapy_cfg.dl_freq_corrected();
        let (ul_corrected, ul_err) = soapy_cfg.ul_freq_corrected();

        tracing::info!(
            "Freqs: DL / UL: {:.6} MHz / {:.6} MHz   PPM: {:.2} -> err {:.0} / {:.0} hz, adj {:.6} MHz / {:.6} MHz",
            soapy_cfg.dl_freq / 1e6,
            soapy_cfg.ul_freq / 1e6,
            soapy_cfg.ppm_err,
            dl_err,
            ul_err,
            dl_corrected / 1e6,
            ul_corrected / 1e6
        );

        // Frequency bindings are held here so the PhyConfig slices stay valid
        // for the lifetime of the RxDsp/TxDsp construction below.
        let dl_freqs: [f64; 1];
        let ul_freqs: [f64; 1];
        let ms_ul_freqs: [f64; 1];
        let monitor_freqs: [(f64, Option<f64>); 1];

        let phy_config = match config_guard.stack_mode {
            StackMode::Ms => {
                // MS mode: receive the downlink as a monitor (recovering slot
                // timing from the synchronization training sequence, ETSI TS 100
                // 392-2 cl. 9.4.4.3.4) and transmit discontinuous uplink bursts
                // on the uplink carrier. Only random/reserved-access bursts are
                // emitted, and only when granted, so the TX chain is otherwise
                // idle.
                monitor_freqs = [(dl_corrected, None)];
                ms_ul_freqs = [ul_corrected];
                soapy_dev::PhyConfig {
                    monitor_frequencies: &monitor_freqs,
                    ms_ul_frequencies: &ms_ul_freqs,
                    ..Default::default()
                }
            }
            _ => {
                // BS mode: transmit on the downlink, demodulate the uplink.
                dl_freqs = [dl_corrected];
                ul_freqs = [ul_corrected];
                soapy_dev::PhyConfig {
                    bs_dl_frequencies: &dl_freqs,
                    bs_ul_frequencies: &ul_freqs,
                    ..Default::default()
                }
            }
        };

        let mut sdr = soapyio::SoapyIo::new(cfg).unwrap();

        Self {
            rx_dsp: if sdr.rx_enabled() {
                Some(RxDsp::new(&mut fft_planner, &mut sdr, &phy_config))
            } else {
                None
            },

            // Only build a TX chain when there are carriers to modulate: BS
            // downlink carriers or MS uplink carriers. In either case nothing is
            // put on air unless a burst is actually scheduled.
            tx_dsp: if sdr.tx_enabled()
                && (!phy_config.bs_dl_frequencies.is_empty() || !phy_config.ms_ul_frequencies.is_empty())
            {
                Some(TxDsp::new(&mut fft_planner, &mut sdr, &phy_config))
            } else {
                None
            },

            sdr,
        }
    }

    /// Process a block of received signal.
    /// Return true if processing can be continued,
    /// false if a slot has been demodulated and rxtx_timeslot should return.
    fn process_rx_block(&mut self) -> Result<bool, RxTxDevError> {
        if let Some(rx_dsp) = &mut self.rx_dsp {
            rx_dsp.process_block(&mut self.sdr)
        } else {
            Ok(false)
        }
    }

    /// Produce a block of transmit signal.
    /// Return true if processing can be continued,
    /// false if more data is needed
    /// or if it wants to wait before producing more.
    fn process_tx_block(&mut self, tx_slot: &[TxSlotBits]) -> Result<bool, RxTxDevError> {
        // Read the current downlink timing reference (and RX block position)
        // from the RX DSP first, so the immutable borrows end before we take a
        // mutable borrow of the TX DSP. `dl_reference` aligns the MS uplink
        // modulator to the recovered air-interface clock (see TxDsp::process_block).
        let rx_block_count = self.rx_dsp.as_ref().map(|rx_dsp| rx_dsp.rx_block_count);
        let dl_reference = self.rx_dsp.as_ref().and_then(|rx_dsp| rx_dsp.dl_reference_time());
        if let Some(tx_dsp) = &mut self.tx_dsp {
            if self.sdr.tx_possible() {
                tx_dsp.process_block(&mut self.sdr, rx_block_count, dl_reference, tx_slot)
            } else {
                Ok(false)
            }
        } else {
            Ok(false)
        }
    }
}

impl RxTxDev for RxTxDevSoapySdr {
    fn rxtx_timeslot<'a>(
        &'a mut self,
        tx_slot: &[TxSlotBits],
        // TODO multiple demodulators
    ) -> Result<Vec<Option<RxSlotBits<'a>>>, RxTxDevError> {
        // First generate as much TX signal as possible at the moment.
        while self.process_tx_block(tx_slot)? {}

        while self.process_rx_block()? {
            // Continue producing TX signal if possible.
            while self.process_tx_block(tx_slot)? {}
        }

        if let Some(rx_dsp) = &mut self.rx_dsp {
            Ok(rx_dsp.take_slot_bits())
        } else {
            Ok(Default::default())
        }
    }

    fn set_rf_path(&mut self, path: RfPath) {
        // Current hardware is full duplex (separate RX and TX chains), so the
        // antenna does not need to be switched and this is intentionally a
        // no-op beyond a trace.
        //
        // FUTURE (half-duplex front end): drive the antenna T/R changeover here,
        // e.g. toggle a GPIO / RF-switch line so the shared antenna is connected
        // to the PA for RfPath::Tx and to the LNA for RfPath::Rx. The SoapySDR
        // GPIO API (`self.sdr` -> `Device::write_gpio`) or a board-specific
        // control line is the intended extension point. Keep the switch settling
        // time within the uplink guard period (ETSI TS 100 392-2 cl. 9.4.3.4).
        tracing::trace!(?path, "set_rf_path (full-duplex: no antenna switching)");
    }

    fn tx_air_time(&self) -> Option<TdmaTime> {
        // Local-basis TDMA slot at the TX *generation frontier* — the point the
        // modulator is currently producing signal for. This is the line the MS
        // uplink scheduler must stay ahead of.
        //
        // It is deliberately the generation frontier (`block_count`), not the
        // true hardware "now": block generation runs a look-ahead window
        // (`dmin`..`dmax`) ahead of real time, and the modulator only emits a
        // burst while it sweeps forward through the slot. A burst scheduled at
        // true-now + a couple slots can therefore still land *behind* the
        // already-advanced generation pointer and be produced as silence. Using
        // the frontier makes the lead self-calibrating against that look-ahead.
        //
        // `reference_time` is the modem-sample position of local slot 0 (from
        // the downlink demodulator); the frontier is in the same modem-sample
        // domain, so their difference divided by the slot length is the slot
        // index. `None` until downlink lock, before TX is possible, or before
        // any block has been generated.
        let reference_time = self.rx_dsp.as_ref()?.dl_reference_time()?;
        if !self.sdr.tx_possible() {
            return None;
        }
        let frontier = self.tx_dsp.as_ref()?.generation_frontier()?;
        let slot = (frontier - reference_time).div_euclid(modulator::SAMPLES_SLOT) as i32;
        Some(TdmaTime::from_int(slot))
    }
}

struct RxDsp {
    rx_fcfb: fcfb::AnalysisInputProcessor,

    rx_block_size: fcfb::InputBlockSize,
    rx_buffer: Vec<ComplexSample>,
    /// How much of rx_buffer has been filled
    rx_buffer_i: usize,
    rx_block_count: fcfb::BlockCount,

    monitors: Vec<MonitorDlUlPair>,
    ul_demodulators: Vec<DemodulatorChannel>,
}

impl RxDsp {
    fn new(fft_planner: &mut FftPlanner, sdr: &mut soapyio::SoapyIo, phy_config: &PhyConfig) -> Self {
        let sdr_sample_rate = sdr.rx_sample_rate();
        let rx_fcfb_params = fcfb::AnalysisInputParameters {
            // Use a bin spacing of 500 Hz.
            // This is a submultiple of the 72 kHz modem sample rate
            // and allows tuning in steps of 500 Hz.
            fft_size: (sdr_sample_rate / 500.0).round() as usize,
            center_frequency: sdr.rx_center_frequency().unwrap(),
            sample_rate: sdr_sample_rate,
            overlap: fcfb::Overlap::O1_4,
        };

        let fcfb = fcfb::AnalysisInputProcessor::new(fft_planner, rx_fcfb_params);
        let rx_block_size = fcfb.input_block_size();

        Self {
            rx_block_size,
            rx_buffer: vec![num::zero(); rx_block_size.overlap + rx_block_size.new],
            rx_buffer_i: 0,
            rx_fcfb: fcfb,
            rx_block_count: 0,

            monitors: phy_config
                .monitor_frequencies
                .iter()
                .map(|(dl_freq, ul_freq)| MonitorDlUlPair {
                    dl: DemodulatorChannel::new(fft_planner, rx_fcfb_params, *dl_freq, demodulator::Mode::DlUnsynchronized),
                    ul: ul_freq
                        .as_ref()
                        .map(|ul_freq| DemodulatorChannel::new(fft_planner, rx_fcfb_params, *ul_freq, demodulator::Mode::Idle)),
                })
                .collect(),

            ul_demodulators: phy_config
                .bs_ul_frequencies
                .iter()
                .map(|ul_freq| DemodulatorChannel::new(fft_planner, rx_fcfb_params, *ul_freq, demodulator::Mode::Ul))
                .collect(),
        }
    }

    /// Sample-counter reference (beginning of hyperframe number 0) of the first
    /// synchronized downlink demodulator. Used to align the MS uplink transmit
    /// modulator to the recovered air-interface timing. `None` until downlink
    /// lock is achieved (before that, nothing is transmitted anyway).
    fn dl_reference_time(&self) -> Option<SampleCount> {
        self.monitors
            .iter()
            .find_map(|pair| pair.dl.demodulator.synchronized_reference_time())
    }

    fn process_block(&mut self, sdr: &mut soapyio::SoapyIo) -> Result<bool, RxTxDevError> {
        self.receive_block(sdr)?;
        let fcfb_result = self.rx_fcfb.process(&self.rx_buffer[..], self.rx_block_count);

        let mut continue_processing = true;

        for pair in self.monitors.iter_mut() {
            let continue_dl = pair.dl.process(fcfb_result, self.rx_block_count);
            if let Some(ul) = &mut pair.ul {
                ul.demodulator.sync_to_demodulator(&pair.dl.demodulator);
                continue_processing = ul.process(fcfb_result, self.rx_block_count) && continue_processing;
            } else {
                continue_processing = continue_dl && continue_processing;
            }
        }

        for demod in self.ul_demodulators.iter_mut() {
            continue_processing = demod.process(fcfb_result, self.rx_block_count) && continue_processing;
        }

        Ok(continue_processing)
    }

    fn receive_block(&mut self, sdr: &mut soapyio::SoapyIo) -> Result<(), RxTxDevError> {
        self.rx_block_count += 1;

        // Copy overlapping part from previous block to the beginning
        self.rx_buffer
            .copy_within(self.rx_block_size.new..self.rx_block_size.new + self.rx_block_size.overlap, 0);
        self.rx_buffer_i = self.rx_block_size.overlap;

        loop {
            let result = sdr.receive(&mut self.rx_buffer[self.rx_buffer_i..])?;

            let block_size = self.rx_block_size.new as SampleCount;
            let expected_count = self.rx_block_count as SampleCount * block_size + self.rx_buffer_i as SampleCount;
            let samples_lost = result.count - expected_count;
            if samples_lost != 0 {
                // Samples have been lost.
                // Mark RX buffer as empty and skip the right number of samples
                // to receive the next full processing block in the next iteration.

                // Expected sample count for the next read,
                // assuming no more samples are lost.
                let next_count = result.count + result.len as SampleCount;
                // div_euclid always rounds down (towards negative numbers),
                // so use it with negations to round up to the next block.
                let next_possible_block = -next_count.div_euclid(-block_size) + 1;
                let next_block_beginning = next_possible_block * block_size;

                let mut samples_to_skip = next_block_beginning - next_count;

                tracing::warn!(
                    "Lost {} samples, skipping {} more samples and {} processing blocks",
                    samples_lost,
                    samples_to_skip,
                    next_possible_block - self.rx_block_count
                );

                self.rx_block_count = next_possible_block;
                self.rx_buffer_i = 0;

                // Repeat reads until the correct number of samples has been skipped.
                while samples_to_skip > 0 {
                    let result = sdr.receive(&mut self.rx_buffer[0..samples_to_skip as usize])?;
                    samples_to_skip -= result.len as SampleCount;
                }
            } else {
                self.rx_buffer_i += result.len;
                if self.rx_buffer_i == self.rx_buffer.len() {
                    // tracing::trace!("Received processing block {} ({} samples in SDR buffer)",
                    //     self.rx_block_count,
                    //     // incorrect if time is not available but does not really matter
                    //     sdr.rx_current_count().unwrap_or(0) - (result.count + result.len as SampleCount - 1),
                    // );
                    return Ok(());
                }
            }
        }
    }

    fn take_slot_bits<'a>(&'a mut self) -> Vec<Option<RxSlotBits<'a>>> {
        // TODO: avoid dynamic allocation here?
        let mut slot_bits = Vec::with_capacity(2 * self.monitors.len() + self.ul_demodulators.len());

        for pair in self.monitors.iter_mut() {
            slot_bits.push(pair.dl.demodulator.take_demodulated_slot());
            slot_bits.push(if let Some(ul) = &mut pair.ul {
                ul.demodulator.take_demodulated_slot()
            } else {
                None
            });
        }

        for demod in self.ul_demodulators.iter_mut() {
            slot_bits.push(demod.demodulator.take_demodulated_slot());
        }

        slot_bits
    }
}

struct TxDsp {
    fcfb: fcfb::SynthesisOutputProcessor,
    block_count: fcfb::BlockCount,
    initial_time: i64,
    modulators: Vec<ModulatorChannel>,
}

impl TxDsp {
    fn new(fft_planner: &mut FftPlanner, sdr: &mut soapyio::SoapyIo, phy_config: &PhyConfig) -> Self {
        let sdr_sample_rate = sdr.tx_sample_rate();
        let fcfb_params = fcfb::SynthesisOutputParameters {
            ifft_size: (sdr_sample_rate / 500.0).round() as usize,
            center_frequency: sdr.tx_center_frequency().unwrap(),
            sample_rate: sdr_sample_rate,
            overlap: fcfb::Overlap::O1_4,
        };

        let fcfb = fcfb::SynthesisOutputProcessor::new(fft_planner, fcfb_params);

        let mut modulators = Vec::<ModulatorChannel>::new();
        for dl_freq in phy_config.bs_dl_frequencies {
            modulators.push(ModulatorChannel::new(fft_planner, fcfb_params, *dl_freq, modulator::Mode::Dl));
        }
        for ul_freq in phy_config.ms_ul_frequencies {
            modulators.push(ModulatorChannel::new(fft_planner, fcfb_params, *ul_freq, modulator::Mode::Ul));
        }

        Self {
            fcfb,
            block_count: 0,
            initial_time: 0, // TODO: get it from RX
            modulators,
        }
    }

    fn process_block(
        &mut self,
        sdr: &mut soapyio::SoapyIo,
        latest_rx_block: Option<fcfb::BlockCount>,
        dl_reference: Option<SampleCount>,
        tx_slot: &[TxSlotBits],
    ) -> Result<bool, RxTxDevError> {
        // MS uplink: align the uplink modulator(s) to the downlink timing
        // recovered by the RX demodulator, offset by the fixed RX->TX chain
        // delay (see MS_TX_SAMPLE_DELAY). Without this the uplink modulator's timing
        // reference stays at 0 while the burst time carries the base station's
        // absolute (large) TDMA clock, so the burst is scheduled at an
        // impossible far-future sample position and only silence is ever
        // transmitted. This is a no-op for downlink (BS) modulators, which are
        // the timing master. Re-applied every block because the demodulator
        // continuously micro-adjusts its reference.
        if let Some(reference_time) = dl_reference {
            let aligned = reference_time + MS_TX_SAMPLE_DELAY;
            for modulator in self.modulators.iter_mut() {
                modulator.set_reference_time(aligned);
            }
        }

        let current_sample = sdr.tx_current_count()?;
        // Current time as block count
        let current_block = current_sample.div_euclid(self.fcfb.output_block_size() as SampleCount);

        let d = self.block_count - current_block;
        // Skip TX blocks in the past or in too near future
        let dmin = 2; // how many blocks in future minimum
        if d < dmin {
            let new_block_count = current_block + dmin;
            tracing::warn!(
                "Too late to produce TX block {}, skipping {} TX blocks",
                self.block_count,
                new_block_count - self.block_count
            );
            self.block_count = new_block_count;
        }
        // Limit how far into future TX blocks are generated.
        let dmax = 60;
        if d > dmax {
            return Ok(false);
        }
        // Also limit how far from the latest RX block TX blocks are generated.
        // This prevents TX from ending up in an infinite loop
        // which does not give a chance for RX signal to get processed.
        //
        // This is not strictly necessary right now but might become useful
        // with different modulator operating modes in the future.
        //
        // Maybe the limit using hardware time above is redundant.
        if let Some(latest_rx_block) = latest_rx_block {
            let d_rx = self.block_count - latest_rx_block;
            if d_rx > dmax {
                return Ok(false);
            }
        }

        for (modulator, tx_slot) in self.modulators.iter_mut().zip(tx_slot) {
            if !modulator.process(&mut self.fcfb, self.block_count, tx_slot) {
                return Ok(false);
            }
        }

        let tx_signal = self.fcfb.process();

        // TODO: compensate for delay of SDR
        let sdr_sample_count = tx_signal.len() as SampleCount * self.block_count;

        // Increment block count before calling sdr.transmit with ?,
        // so we do not end up producing the same block again even if transmit fails.
        self.block_count += 1;

        sdr.transmit(tx_signal, Some(sdr_sample_count))?;

        // tracing::trace!("Produced transmit block {} ({} samples in future)",
        //     self.block_count - 1,
        //     sdr_sample_count - sdr.tx_current_count().unwrap_or(0),
        // );

        Ok(true)
    }

    /// Modem-sample position of the current TX generation frontier — the next
    /// block the modulators will produce (`block_first = block_count *
    /// modem_block_len`, the same quantity the modulator times bursts against).
    ///
    /// This leads the true hardware "now" by the TX look-ahead window (the
    /// `dmin`..`dmax` block gating in [`Self::process_block`]). Uplink bursts
    /// must be scheduled *ahead of this frontier*, not merely ahead of true
    /// time: block generation only advances, so a slot whose start already sits
    /// behind the frontier is produced as silence and never revisited. `None`
    /// before any modulator exists.
    fn generation_frontier(&self) -> Option<SampleCount> {
        let modem_block_len = self.modulators.first()?.modem_block_len() as SampleCount;
        Some(self.block_count * modem_block_len)
    }
}

struct DemodulatorChannel {
    downconverter: fcfb::AnalysisOutputProcessor,
    demodulator: demodulator::Demodulator,
}

impl DemodulatorChannel {
    fn new(
        fft_planner: &mut FftPlanner,
        analysis_in_params: fcfb::AnalysisInputParameters,
        frequency: f64,
        mode: demodulator::Mode,
    ) -> Self {
        Self {
            downconverter: fcfb::AnalysisOutputProcessor::new_with_frequency(
                fft_planner,
                analysis_in_params,
                demodulator::SAMPLE_RATE,
                frequency,
                Some(25000.0),
            ),
            demodulator: demodulator::Demodulator::new(mode),
        }
    }

    /// Return true if processing should be continued,
    /// false if a new demodulated slot is available.
    fn process(&mut self, fcfb_result: &fcfb::AnalysisIntermediateResult, block_count: fcfb::BlockCount) -> bool {
        let samples = self.downconverter.process(fcfb_result);
        for (i, sample) in samples.iter().enumerate() {
            // TODO: include delay of FCFB in sample count
            self.demodulator.sample(
                *sample,
                block_count as SampleCount * samples.len() as SampleCount + i as SampleCount,
            );
        }
        !self.demodulator.demodulated_slot_available()
    }
}

struct ModulatorChannel {
    upconverter: fcfb::SynthesisInputProcessor,
    modulator: modulator::Modulator,
    /// Buffer for modulated signal at modulator sample rate.
    buffer: fcfb::InputBuffer,
    /// How much of buffer is filled
    buffer_i: usize,
    /// Last uplink burst time for which a placement diagnostic was logged, so
    /// the per-block diagnostic fires at most once per queued burst.
    diag_last_time: Option<TdmaTime>,
}

impl ModulatorChannel {
    fn new(
        fft_planner: &mut FftPlanner,
        synthesis_out_params: fcfb::SynthesisOutputParameters,
        frequency: f64,
        mode: modulator::Mode,
    ) -> Self {
        let upconverter = fcfb::SynthesisInputProcessor::new_with_frequency(
            fft_planner,
            synthesis_out_params,
            modulator::SAMPLE_RATE,
            frequency,
            Some(25000.0),
        );
        Self {
            buffer: upconverter.make_input_buffer(),
            buffer_i: 0,
            upconverter,
            modulator: modulator::Modulator::new(mode),
            diag_last_time: None,
        }
    }

    fn set_reference_time(&mut self, reference_time: SampleCount) {
        self.modulator.set_reference_time(reference_time);
    }

    /// Number of new modem-rate samples the modulator produces per TX block.
    /// This is the multiplier between the TX block count and the modem sample
    /// counter (`block_first = block_count * modem_block_len`), so it converts
    /// the generation frontier into the sample domain the burst timing uses.
    fn modem_block_len(&self) -> usize {
        self.upconverter.input_block_size().new
    }

    fn process(&mut self, fcfb: &mut fcfb::SynthesisOutputProcessor, block_count: fcfb::BlockCount, tx_slot: &TxSlotBits) -> bool {
        let buf = self.buffer.buffer_in();

        // Diagnostic: when an uplink burst is queued, log once per burst where
        // its active part lands relative to the TX generation window. The
        // modulator only emits when the TX sample counter sweeps through
        // `slot_begin`; if `slot_begin` is already behind `block_first` when the
        // block is generated the burst is perpetually "in the past" and only
        // silence is transmitted (SN0 never fires). `ahead_samples` shows the
        // sign/magnitude of the misalignment for tuning `MS_TX_SAMPLE_DELAY`.
        if tx_slot.slot.is_some() && self.diag_last_time != Some(tx_slot.time) {
            if let Some(slot_begin) = self.modulator.ul_slot_begin(tx_slot) {
                let block_first = block_count as SampleCount * buf.len() as SampleCount;
                let block_last = block_first + buf.len() as SampleCount;
                tracing::info!(
                    ts = %tx_slot.time,
                    block_count,
                    block_first,
                    block_last,
                    slot_begin,
                    ahead_samples = slot_begin - block_first,
                    "Modulator(Ul): pending burst vs TX generation window"
                );
                self.diag_last_time = Some(tx_slot.time);
            }
        }

        while self.buffer_i < buf.len() {
            // TODO: include delay of FCFB in sample count
            match self.modulator.sample(
                block_count as SampleCount * buf.len() as SampleCount + self.buffer_i as SampleCount,
                tx_slot,
            ) {
                Ok(sample) => {
                    buf[self.buffer_i] = sample;
                    self.buffer_i += 1;
                }
                Err(modulator::Error::NeedMoreData) => {
                    return false;
                }
            }
        }
        fcfb.add(self.upconverter.process(self.buffer.buffer(), block_count));

        let _ = self.buffer.prepare_for_new_samples();
        self.buffer_i = 0;
        true
    }
}

struct MonitorDlUlPair {
    dl: DemodulatorChannel,
    ul: Option<DemodulatorChannel>,
}
