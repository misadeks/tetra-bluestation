use crate::{MessageQueue, TetraEntityTrait};
use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, Sap, unimplemented_log};
use tetra_saps::lcmc::{LcmcMleBreakInd, LcmcMleReopenInd, LcmcMleUnitdataInd};
use tetra_saps::lmm::{
    LmmMleActivateConf, LmmMleBreakInd, LmmMleReopenInd, LmmMleRssiInd, LmmMleScanCompleteInd, LmmMleScanResultInd,
    LmmMleUnitdataInd,
};
use tetra_saps::ltpd::LtpdMleUnitdataInd;
use tetra_saps::tla::TlaTlDataReqBl;
use tetra_saps::tlmc::{TlmcConfigureReq, TlmcTuneReq, TlmcUPlaneConfigureReq, TlmcValidAddress};
use tetra_saps::{SapMsg, SapMsgInner};

use tetra_pdus::mle::enums::mle_pdu_type_dl::MlePduTypeDl;
use tetra_pdus::mle::enums::mle_protocol_discriminator::MleProtocolDiscriminator;
use tetra_pdus::mle::pdus::d_mle_sync::DMleSync;
use tetra_pdus::mle::pdus::d_mle_sysinfo::DMleSysinfo;

/// Serving cell the MS has selected and camped on
/// (ETSI TS 100 392-2 cl. 18.3.4). Populated from D-MLE-SYNC (cl. 18.4.2.1)
/// and refined from D-MLE-SYSINFO (cl. 18.4.2.2).
#[derive(Debug, Clone, Default)]
struct ServingCell {
    mcc: u16,
    mnc: u16,
    neighbor_cell_broadcast: u8,
    cell_load_ca: u8,
    late_entry_supported: bool,
    location_area: Option<u16>,
    subscriber_class: Option<u16>,
    /// Whether registration is required/mandatory on this cell, from the
    /// D-MLE-SYSINFO BS service details "registration" flag (cl. 18.4.2.2).
    /// `None` until the first SYSINFO for this cell is received.
    registration_required: Option<bool>,
    /// BS service details "system wide services" flag (cl. 18.5.2.1 Table 18.26).
    /// `false` = the cell advertises "system wide services temporarily not
    /// supported" (cl. 16.4.8). `None` until the first SYSINFO for this cell.
    system_wide_services: Option<bool>,
}

/// A cell discovered during an operator survey (**[impl policy]**, Plane B).
///
/// Built receive-only from the candidate carrier's D-MLE-SYNC (cl. 18.4.2.1) and
/// refined from its D-MLE-SYSINFO (cl. 18.4.2.2). It is NOT a serving cell: the
/// MLE never camps on it. Reported to MM as an `LmmMleScanResultInd`.
#[derive(Debug, Clone)]
struct FoundCell {
    /// Downlink carrier the cell was found on (Hz).
    carrier_hz: u32,
    mcc: u16,
    mnc: u16,
    /// Location area from D-MLE-SYSINFO; `None` if SYSINFO was not seen in time.
    location_area: Option<u16>,
    /// Colour code, if surfaced to the MLE. Currently always `None` — the colour
    /// code is a MAC-layer quantity (used for the scrambling code) and is not
    /// carried in D-MLE-SYNC, so it is not available here without threading it up
    /// from the MAC. Reported as `None` rather than invented.
    colour_code: Option<u8>,
    /// Uncalibrated downlink level (dBFS) at capture time.
    rssi_dbfs: Option<f32>,
    /// D-MLE-SYSINFO BS service details "registration" flag; `None` if SYSINFO
    /// was not seen in time.
    registration_required: Option<bool>,
    late_entry_supported: bool,
}

/// MS-side Mobile Link Entity.
///
/// Mirrors the current, compiled [`super::mle_bs::MleBs`] API (`SapMsg` has no
/// `dltime`, `handle` is passed as `0`, the `MleRouter` connection map is not
/// yet in use). It routes downlink TL-SDUs to MM/CMCE/SNDCP by MLE protocol
/// discriminator, forwards uplink MM/CMCE PDUs down to LLC, and consumes the
/// MLE broadcast primitives (SYNC / SYSINFO).
///
/// The SYNC/SYSINFO handlers perform initial cell selection
/// (ETSI TS 100 392-2 cl. 18.3.4.6) and drive the lower layers to derive the
/// scrambling code for the selected cell.
pub struct MleMs {
    config: SharedConfig,
    /// The cell the MS is currently camped on, if any.
    serving_cell: Option<ServingCell>,
    /// Guards the one-shot MLE-ACTIVATE confirmation (cl. 17.3.2) to MM: set
    /// once we have confirmed the currently selected cell to MM, reset when a
    /// new cell is selected. Prevents re-triggering registration on every
    /// repeated SYSINFO for the same cell.
    activate_confirmed: bool,
    /// Whether the MLE has declared the serving cell out of service following a
    /// downlink radio-link failure (ETSI TS 100 392-2 cl. 18.3.4.5.3). While
    /// out of service the MLE has issued MLE-BREAK to the upper layers and is
    /// waiting for the downlink to recover (cl. 18.3.4.7).
    out_of_service: bool,
    /// Most recent serving-cell downlink signal level (uncalibrated dBFS),
    /// refreshed from the PHY monitoring indication. Used as a reselection
    /// input (cl. 18.3.4) and surfaced to the MS management UI. `None` while
    /// out of service or before the first measurement.
    serving_cell_rssi_dbfs: Option<f32>,
    /// Scanning cell-selection engine state (ETSI TS 100 392-2 cl. 18.3.4 â€”
    /// initial cell selection; radio-style **[impl policy]** for how the
    /// candidate set is enumerated). `true` while the MS is actively stepping
    /// candidate downlink carriers looking for a suitable serving cell; cleared
    /// once a suitable cell is confirmed and camped, or when the codeplug
    /// programs no scan (single fixed carrier). The candidate set is derived
    /// from the codeplug and cached at scan start; `scan_index` is the position
    /// within it.
    scanning: bool,
    scan_candidates: Vec<u32>,
    scan_index: usize,
    /// Cell-selection mode (**[impl policy]**, Plane B operator control on top of
    /// cl. 18.3.4). `false` (default) = automatic: the MLE auto-camps on the
    /// first suitable cell (cl. 18.3.4.6). `true` = manual: auto-camp is
    /// suppressed; the operator drives selection with a survey and an explicit
    /// camp request. Set via `LmmMleSelectionModeReq` from MM.
    selection_mode_manual: bool,
    /// Armed by an operator `LmmMleCampReq` so that, in manual mode, the very
    /// next suitable SYNC is allowed through the normal adopt/camp path
    /// (cl. 18.3.4.6). Cleared once the cell is confirmed to MM. Ignored in
    /// automatic mode (which always camps).
    camp_armed: bool,
    /// When a camp was operator-requested with `register = true`, MM is asked to
    /// register even if the cell advertises registration-not-required. The
    /// registration decision lives in MM; the MLE only relays the cell via
    /// `LmmMleActivateConf`, so this flag is not needed by the MLE beyond
    /// logging — kept alongside `camp_armed` for observability.
    camp_force_register: bool,
    /// Receive-only survey state (**[impl policy]**). `true` while a single pass
    /// over the candidate carriers is in progress; the MLE reports each found
    /// cell to MM but never adopts a serving cell, configures L2, or transmits.
    survey: bool,
    survey_candidates: Vec<u32>,
    survey_index: usize,
    /// The cell currently being characterised on the tuned survey carrier: set
    /// from its D-MLE-SYNC, refined from D-MLE-SYSINFO, then finalized (reported
    /// + advanced). `None` between carriers / while waiting for a SYNC.
    survey_pending: Option<FoundCell>,
    /// Number of D-MLE-SYNC PDUs seen for the current `survey_pending` cell while
    /// still awaiting its SYSINFO; bounds the per-carrier SYSINFO wait so a cell
    /// that syncs but is slow to broadcast SYSINFO is still reported (partial).
    survey_sync_repeats: u8,
    /// Count of cells reported during the current survey (for the completion).
    survey_found: u32,
}

impl MleMs {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            config,
            serving_cell: None,
            activate_confirmed: false,
            out_of_service: false,
            serving_cell_rssi_dbfs: None,
            scanning: false,
            scan_candidates: Vec::new(),
            scan_index: 0,
            selection_mode_manual: false,
            camp_armed: false,
            camp_force_register: false,
            survey: false,
            survey_candidates: Vec::new(),
            survey_index: 0,
            survey_pending: None,
            survey_sync_repeats: 0,
            survey_found: 0,
        }
    }

    /// Handle an MLE-internal PDU (MLE protocol discriminator == `Mle`).
    ///
    /// The caller has already read and consumed the 3-bit MLE protocol
    /// discriminator (ETSI TS 100 392-2 cl. 18.5.21), so `sdu` is positioned at
    /// the 3-bit MLE PDU type (cl. 18.5.20). This is reached for both
    /// acknowledged (TL-DATA) and unacknowledged broadcast (TL-UNITDATA, e.g.
    /// D-NWRK-BROADCAST on the BNCH) delivery.
    fn rx_tla_mle_pdu(&mut self, _queue: &mut MessageQueue, mut sdu: BitBuffer) {
        tracing::trace!("rx_tla_mle_pdu");

        // Read the MLE PDU type (cl. 18.5.20).
        let Some(bits) = sdu.read_bits(3) else {
            tracing::warn!("insufficient bits: {}", sdu.dump_bin());
            return;
        };
        let Ok(pdu_type) = MlePduTypeDl::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, sdu.dump_bin());
            return;
        };

        match pdu_type {
            MlePduTypeDl::DNewCell => {
                unimplemented_log!("DNewCell")
            }
            MlePduTypeDl::DPrepareFail => {
                unimplemented_log!("DPrepareFail")
            }
            MlePduTypeDl::DNwrkBroadcast => {
                unimplemented_log!("DNwrkBroadcast")
            }
            MlePduTypeDl::DNwrkBroadcastExt => {
                unimplemented_log!("DNwrkBroadcastExt")
            } // TODO FIXME CHECK this option and associated int
            MlePduTypeDl::DRestoreAck => {
                unimplemented_log!("DRestoreAck")
            }
            MlePduTypeDl::DRestoreFail => {
                unimplemented_log!("DRestoreFail")
            }
            MlePduTypeDl::DChannelResponse => {
                unimplemented_log!("DChannelResponse")
            }
            MlePduTypeDl::ExtPdu => {
                unimplemented_log!("ExtPdu")
            }
        }
    }

    fn rx_tla_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tla_prim");
        match message.msg {
            SapMsgInner::TlaTlDataIndBl(_) => {
                self.rx_tla_data_ind_bl(queue, message);
            }
            SapMsgInner::TlaTlUnitdataIndBl(_) => {
                self.rx_tla_unitdata_ind_bl(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }

    fn rx_tla_data_ind_bl(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        // Take ownership of bitbuf and read protocol discriminator
        let SapMsgInner::TlaTlDataIndBl(prim) = &mut message.msg else {
            panic!()
        };
        let Some(mut sdu) = prim.tl_sdu.take() else { panic!("no tl_sdu") };
        assert!(sdu.get_pos() == 0); // We should be at the start of the MAC PDU
        let Some(bits) = sdu.read_bits(3) else {
            tracing::warn!("insufficient bits: {}", sdu.dump_bin());
            return;
        };
        let Ok(pdu_type) = MleProtocolDiscriminator::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, sdu.dump_bin());
            return;
        };

        // Dispatch to appropriate component (or to self if for MLE)
        match pdu_type {
            MleProtocolDiscriminator::Mm => {
                let m = LmmMleUnitdataInd {
                    sdu,
                    handle: 0,
                    received_address: prim.main_address,
                };
                let msg = SapMsg {
                    sap: Sap::LmmSap,
                    src: TetraEntity::Mle,
                    dest: TetraEntity::Mm,
                    msg: SapMsgInner::LmmMleUnitdataInd(m),
                };
                queue.push_back(msg);
            }
            MleProtocolDiscriminator::Cmce => {
                let m = LcmcMleUnitdataInd {
                    sdu,
                    handle: 0,
                    received_tetra_address: prim.main_address,
                    endpoint_id: prim.endpoint_id,
                    link_id: prim.link_id,
                    chan_change_resp_req: false, // TODO FIXME
                    chan_change_handle: None,    // TODO FIXME
                };
                let msg = SapMsg {
                    sap: Sap::LcmcSap,
                    src: TetraEntity::Mle,
                    dest: TetraEntity::Cmce,
                    msg: SapMsgInner::LcmcMleUnitdataInd(m),
                };
                queue.push_back(msg);
            }
            MleProtocolDiscriminator::Sndcp => {
                let m = LtpdMleUnitdataInd {
                    sdu,
                    endpoint_id: prim.endpoint_id,
                    link_id: prim.link_id,
                    received_tetra_address: prim.main_address,
                    chan_change_resp_req: false, // TODO FIXME
                    chan_change_handle: None,    // TODO FIXME
                };
                let msg = SapMsg {
                    sap: Sap::LcmcSap,
                    src: TetraEntity::Mle,
                    dest: TetraEntity::Cmce,
                    msg: SapMsgInner::LtpdMleUnitdataInd(m),
                };
                queue.push_back(msg);
            }
            MleProtocolDiscriminator::Mle => {
                self.rx_tla_mle_pdu(queue, sdu);
            }
            MleProtocolDiscriminator::TetraManagementEntity => {
                unimplemented_log!("MleProtocolDiscriminator::TetraManagementEntity");
            }
        }
    }

    fn rx_tla_unitdata_ind_bl(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        // TODO FIXME NOTE: This function is the same as the rx_tla_data_ind_bl.
        // A cursory glance at the spec does not make clear the difference, except for the relation with
        // either udata or data at the llc.
        // It seems only the SNDCP uses unacknowledged TL-UNITDATA.
        // We should investigate the exact differences and account for them

        // Take ownership of bitbuf and read protocol discriminator
        let SapMsgInner::TlaTlUnitdataIndBl(prim) = &mut message.msg else {
            panic!()
        };
        let Some(mut sdu) = prim.tl_sdu.take() else { panic!("no tl_sdu") };
        assert!(sdu.get_pos() == 0); // We should be at the start of the MAC PDU

        let Some(bits) = sdu.read_bits(3) else {
            tracing::warn!("insufficient bits: {}", sdu.dump_bin());
            return;
        };
        let Ok(pdu_type) = MleProtocolDiscriminator::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, sdu.dump_bin());
            return;
        };

        // Dispatch to appropriate component (or to self if for MLE)
        match pdu_type {
            MleProtocolDiscriminator::Mm => {
                tracing::debug!("MLE-MS: TM-UNITDATA (MLE-SDU) routed to MM"); // MM-addressed L3 PDU on the assigned/traffic channel (cl. 18.3.3)
                let m = LmmMleUnitdataInd {
                    sdu,
                    handle: 0,
                    received_address: prim.main_address,
                };
                let msg = SapMsg {
                    sap: Sap::LmmSap,
                    src: TetraEntity::Mle,
                    dest: TetraEntity::Mm,
                    msg: SapMsgInner::LmmMleUnitdataInd(m),
                };
                queue.push_back(msg);
            }
            MleProtocolDiscriminator::Cmce => {
                tracing::debug!("MLE-MS: TM-UNITDATA (MLE-SDU) routed to CMCE"); // CMCE-addressed L3 PDU on the assigned/traffic channel (cl. 18.3.3)
                let m = LcmcMleUnitdataInd {
                    sdu,
                    handle: 0,
                    endpoint_id: prim.endpoint_id,
                    link_id: prim.link_id,
                    received_tetra_address: prim.main_address,
                    chan_change_resp_req: false, // TODO FIXME
                    chan_change_handle: None,    // TODO FIXME
                };
                let msg = SapMsg {
                    sap: Sap::LcmcSap,
                    src: TetraEntity::Mle,
                    dest: TetraEntity::Cmce,
                    msg: SapMsgInner::LcmcMleUnitdataInd(m),
                };
                queue.push_back(msg);
            }
            MleProtocolDiscriminator::Sndcp => {
                let m = LtpdMleUnitdataInd {
                    sdu,
                    endpoint_id: prim.endpoint_id,
                    link_id: prim.link_id,
                    received_tetra_address: prim.main_address,
                    chan_change_resp_req: false, // TODO FIXME
                    chan_change_handle: None,    // TODO FIXME
                };
                let msg = SapMsg {
                    sap: Sap::LcmcSap,
                    src: TetraEntity::Mle,
                    dest: TetraEntity::Cmce,
                    msg: SapMsgInner::LtpdMleUnitdataInd(m),
                };
                queue.push_back(msg);
            }
            MleProtocolDiscriminator::Mle => {
                self.rx_tla_mle_pdu(queue, sdu);
            }
            MleProtocolDiscriminator::TetraManagementEntity => {
                unimplemented_log!("MleProtocolDiscriminator::TetraManagementEntity");
            }
        }
    }

    /// Radio-style cell suitability â€” allowed network (ETSI TS 100 392-2
    /// cl. 18.3.4 initial cell selection). The cell's network identity is its
    /// D-MLE-SYNC MCC/MNC (cl. 18.4.2.1); it is usable only when programmed as
    /// allowed (codeplug allowed-network list, **[impl policy]**; empty list =>
    /// home network only). The home network from `[net_info]` is always allowed.
    fn network_allowed(&self, mcc: u16, mnc: u16) -> bool {
        let cfg = self.config.config();
        cfg.codeplug.is_network_allowed(mcc, mnc, cfg.net.mcc, cfg.net.mnc)
    }

    /// Radio-style cell suitability â€” subscriber-class permission (ETSI
    /// TS 100 392-2 cl. 18.4.2.2 / 18.3.4). The cell advertises a 16-bit
    /// subscriber-class bitmap in D-MLE-SYSINFO where bit `n` set means class
    /// `n+1` is permitted access. The cell is usable only if the MS's own
    /// configured subscriber class (1..=16) is set. When the MS has no
    /// configured class (pure receive-only monitor) the check passes so
    /// monitoring is never blocked.
    fn subscriber_class_permitted(&self, cell_bitmap: u16) -> bool {
        match self.config.config().ms.as_ref() {
            Some(ms) if (1..=16).contains(&ms.subscriber_class) => {
                (cell_bitmap >> (ms.subscriber_class - 1)) & 1 == 1
            }
            _ => true,
        }
    }

    /// Request a downlink retune of the SDR (**[impl policy]** â€” MLE owns MS
    /// cell-selection policy per the agreed design). Emits a TMC-SAP tune
    /// primitive that is forwarded MLE -> UMAC (TLMC) -> LMAC (TMV) -> PHY (TPC),
    /// where PhyMs applies it to the device. Used by the scanning cell-selection
    /// engine to step the receiver across candidate carriers (cl. 18.3.4).
    fn request_tune(&self, queue: &mut MessageQueue, carrier_hz: u32) {
        tracing::info!("MLE: requesting downlink retune to {} Hz", carrier_hz);
        queue.push_back(SapMsg {
            sap: Sap::TlmcSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Umac,
            msg: SapMsgInner::TlmcTuneReq(TlmcTuneReq { carrier_hz }),
        });
    }

    /// The codeplug-programmed scan candidate downlink carriers (Hz), or empty
    /// when the codeplug does not program any frequency lists.
    ///
    /// With no `[[frequency_list]]` programmed the radio stays on its single
    /// configured carrier, so no scanning is performed and the pre-scan
    /// single-frequency behaviour is preserved. Otherwise the candidates from
    /// all programmed lists are combined (deduplicated) into one set for the
    /// scanning cell-selection engine (ETSI TS 100 392-2 cl. 18.3.4; the
    /// enumeration itself is **[impl policy]**).
    fn scan_candidate_carriers(&self) -> Vec<u32> {
        self.config.config().codeplug.scan_candidate_frequencies()
    }

    /// Whether the codeplug programs a multi-candidate scan (so the MLE drives
    /// cell selection by stepping carriers rather than camping on the single
    /// configured one).
    fn scan_enabled(&self) -> bool {
        !self.scan_candidate_carriers().is_empty()
    }

    /// Begin (or restart) the scanning cell-selection engine (ETSI TS 100 392-2
    /// cl. 18.3.4 initial cell selection). Caches the codeplug candidate set and
    /// retunes to the first candidate. No-op when scanning is not enabled or is
    /// already in progress.
    fn start_scan(&mut self, queue: &mut MessageQueue) {
        if self.scanning || !self.scan_enabled() {
            return;
        }
        self.scan_candidates = self.scan_candidate_carriers();
        self.scan_index = 0;
        self.scanning = true;
        let first = self.scan_candidates[0];
        tracing::info!(
            "MLE: starting cell-selection scan over {} candidate carrier(s); first {} Hz (cl. 18.3.4)",
            self.scan_candidates.len(),
            first
        );
        self.request_tune(queue, first);
    }

    /// Advance the scan to the next candidate carrier (wrapping). Called when the
    /// current candidate yields no suitable serving cell â€” either no carrier at
    /// all (PHY scan-dwell elapsed) or a carrier that failed suitability
    /// (disallowed network / barred subscriber class). No-op when not scanning.
    fn advance_scan(&mut self, queue: &mut MessageQueue) {
        if !self.scanning || self.scan_candidates.is_empty() {
            return;
        }
        self.scan_index = (self.scan_index + 1) % self.scan_candidates.len();
        let next = self.scan_candidates[self.scan_index];
        tracing::info!(
            "MLE: scan advancing to candidate {}/{}: {} Hz (cl. 18.3.4)",
            self.scan_index + 1,
            self.scan_candidates.len(),
            next
        );
        self.request_tune(queue, next);
    }

    /// Stop the scanning cell-selection engine â€” a suitable serving cell has
    /// been confirmed and camped (cl. 18.3.4.6).
    fn stop_scan(&mut self) {
        if self.scanning {
            tracing::info!("MLE: suitable serving cell camped â€” stopping scan (cl. 18.3.4.6)");
            self.scanning = false;
        }
    }

    /// Bound on how many D-MLE-SYNC PDUs the survey waits for a cell's SYSINFO
    /// before reporting it with the SYNC-only identity (LA / registration flag
    /// left `None`). Keeps the per-carrier dwell short while still giving the
    /// broadcast SYSINFO (BNCH) a chance to arrive.
    const SURVEY_SYSINFO_SYNC_LIMIT: u8 = 4;

    /// Begin a receive-only survey of the codeplug candidate carriers
    /// (**[impl policy]** on top of cl. 18.3.4). Clears any camp, caches the
    /// candidate set and tunes to the first candidate. Reports each found cell to
    /// MM and finishes with a completion. No-op (immediate empty completion) when
    /// the codeplug programs no candidate carriers.
    fn start_survey(&mut self, queue: &mut MessageQueue) {
        let candidates = self.scan_candidate_carriers();
        // Cancel any automatic scan and drop any camp: a survey is receive-only
        // and must not leave a stale serving cell adopted.
        self.scanning = false;
        self.serving_cell = None;
        self.activate_confirmed = false;
        self.survey_pending = None;
        self.survey_sync_repeats = 0;
        self.survey_found = 0;
        self.survey_index = 0;
        self.survey_candidates = candidates;
        if self.survey_candidates.is_empty() {
            tracing::info!("MLE: survey requested but no candidate carriers programmed - completing empty");
            self.survey = false;
            self.emit_scan_complete(queue, 0);
            return;
        }
        self.survey = true;
        let first = self.survey_candidates[0];
        tracing::info!(
            "MLE: starting operator survey over {} candidate carrier(s); first {} Hz (cl. 18.3.4, receive-only)",
            self.survey_candidates.len(),
            first
        );
        self.request_tune(queue, first);
    }

    /// Cancel a survey in progress (operator `LmmMleScanReq{start:false}`),
    /// reporting a completion for the carriers visited so far.
    fn cancel_survey(&mut self, queue: &mut MessageQueue) {
        if !self.survey {
            return;
        }
        let scanned = self.survey_index as u32;
        tracing::info!("MLE: operator survey cancelled after {} carrier(s)", scanned);
        self.survey = false;
        self.survey_pending = None;
        self.survey_sync_repeats = 0;
        self.emit_scan_complete(queue, scanned);
    }

    /// Finalize the current `survey_pending` cell: report it to MM and advance to
    /// the next candidate carrier.
    fn survey_finalize_pending(&mut self, queue: &mut MessageQueue) {
        if let Some(mut cell) = self.survey_pending.take() {
            // Attach the latest measured downlink level.
            cell.rssi_dbfs = self.serving_cell_rssi_dbfs;
            tracing::info!(
                "MLE: survey found cell MCC={} MNC={} on {} Hz (LA={:?}, reg_required={:?}, late_entry={})",
                cell.mcc,
                cell.mnc,
                cell.carrier_hz,
                cell.location_area,
                cell.registration_required,
                cell.late_entry_supported,
            );
            self.survey_found += 1;
            self.emit_scan_result(queue, &cell);
        }
        self.survey_sync_repeats = 0;
        self.survey_advance(queue);
    }

    /// Advance the survey to the next candidate carrier, or finish the pass when
    /// the last candidate has been visited.
    fn survey_advance(&mut self, queue: &mut MessageQueue) {
        self.survey_index += 1;
        if self.survey_index >= self.survey_candidates.len() {
            let scanned = self.survey_candidates.len() as u32;
            tracing::info!("MLE: survey complete - {} cell(s) over {} carrier(s)", self.survey_found, scanned);
            self.survey = false;
            self.survey_pending = None;
            self.emit_scan_complete(queue, scanned);
            return;
        }
        let next = self.survey_candidates[self.survey_index];
        tracing::info!(
            "MLE: survey advancing to candidate {}/{}: {} Hz",
            self.survey_index + 1,
            self.survey_candidates.len(),
            next
        );
        self.request_tune(queue, next);
    }

    /// Survey handling of the PHY scan-dwell heartbeat: the tuned carrier yielded
    /// no downlink within the dwell window, so finalize a pending cell (partial,
    /// SYNC-only) or record an empty carrier, then advance.
    fn survey_on_dwell(&mut self, queue: &mut MessageQueue) {
        if self.survey_pending.is_some() {
            self.survey_finalize_pending(queue);
        } else {
            tracing::debug!(
                "MLE: survey carrier {} Hz had no cell within dwell - advancing",
                self.survey_candidates.get(self.survey_index).copied().unwrap_or(0)
            );
            self.survey_advance(queue);
        }
    }

    /// Emit an `LmmMleScanResultInd` (one found cell) up to MM.
    fn emit_scan_result(&self, queue: &mut MessageQueue, cell: &FoundCell) {
        queue.push_back(SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Mm,
            msg: SapMsgInner::LmmMleScanResultInd(LmmMleScanResultInd {
                carrier_hz: cell.carrier_hz,
                mcc: cell.mcc,
                mnc: cell.mnc,
                location_area: cell.location_area,
                colour_code: cell.colour_code,
                rssi_dbfs: cell.rssi_dbfs,
                registration_required: cell.registration_required,
                late_entry_supported: cell.late_entry_supported,
            }),
        });
    }

    /// Emit an `LmmMleScanCompleteInd` up to MM.
    fn emit_scan_complete(&self, queue: &mut MessageQueue, scanned: u32) {
        queue.push_back(SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Mm,
            msg: SapMsgInner::LmmMleScanCompleteInd(LmmMleScanCompleteInd {
                found: self.survey_found,
                scanned,
            }),
        });
    }

    fn rx_tlmb_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tlmb_prim");
        match message.msg {
            SapMsgInner::TlmbSysinfoInd(_) => {
                self.rx_tlmb_tl_sysinfo_ind(queue, message);
            }
            SapMsgInner::TlmbSyncInd(_) => {
                self.rx_tlmb_tl_sync_ind(queue, message);
            }
            SapMsgInner::TlmbMonitorInd(ref inner) => {
                // Cache the serving-cell downlink level whenever the PHY refreshes
                // it (reselection input, cl. 18.3.4 / management-UI receive level)
                // and forward it to MM so it can surface it in the runtime state.
                if inner.rssi_dbfs.is_some() && self.serving_cell_rssi_dbfs != inner.rssi_dbfs {
                    self.serving_cell_rssi_dbfs = inner.rssi_dbfs;
                    self.emit_rssi_to_mm(queue);
                }
                let available = inner.downlink_available;
                self.rx_tlmb_monitor_ind(queue, available);
            }
            SapMsgInner::TlmbScanDwellInd(_) => {
                self.rx_tlmb_scan_dwell_ind(queue);
            }
            _ => {
                panic!();
            }
        }
    }

    /// Handle the PHY scan-dwell-elapsed heartbeat (**[impl policy]**, cl. 18.3.4
    /// initial cell selection): the currently-tuned candidate carrier yielded no
    /// serving-cell downlink within the dwell window. If a multi-candidate scan
    /// is programmed and we are not already camped, start the scan (on the very
    /// first heartbeat) or advance it to the next candidate carrier. When camped
    /// or no scan is programmed this is ignored (the PHY only emits it while
    /// un-synchronized, so a camped MS normally never sees it).
    fn rx_tlmb_scan_dwell_ind(&mut self, queue: &mut MessageQueue) {
        // Operator survey owns the dwell heartbeat while running: advance the
        // receive-only pass regardless of camp/scan state.
        if self.survey {
            self.survey_on_dwell(queue);
            return;
        }
        if self.serving_cell.is_some() {
            // Already camped: nothing to select. (A brief pre-recovery window
            // could see a stray heartbeat; ignore it - link failure is handled
            // by the monitoring indication path.)
            return;
        }
        // Manual cell selection: the operator drives selection via survey/camp,
        // so the automatic scan engine is suppressed (no auto-stepping).
        if self.selection_mode_manual {
            return;
        }
        if self.scanning {
            self.advance_scan(queue);
        } else {
            self.start_scan(queue);
        }
    }

    /// Handle the MS-internal serving-cell downlink monitoring indication from
    /// the PHY (ETSI TS 100 392-2 cl. 18.3.4.5.3 / 18.3.4.7).
    ///
    /// On a declared downlink failure the MLE declares the cell out of service,
    /// issues MLE-BREAK to the upper layers (CMCE), and re-arms cell selection
    /// so that when the downlink recovers the cell is re-selected and the
    /// LMM-ACTIVATE confirmation re-fired to MM â€” which then re-evaluates
    /// registration against cl. 18.3.4.7.1a (same cell/LA => no re-registration,
    /// NOTE 2; a changed LA/network => a location update). On recovery the MLE
    /// issues MLE-REOPEN.
    fn rx_tlmb_monitor_ind(&mut self, queue: &mut MessageQueue, downlink_available: bool) {
        match (downlink_available, self.out_of_service) {
            (false, false) => {
                // Serving-cell radio link failure: declare out of service.
                self.out_of_service = true;
                tracing::warn!(
                    "MLE: serving-cell downlink failure â€” declaring out of service, \
                     MLE-BREAK to upper layers (cl. 18.3.4.5.3)"
                );
                // MLE-BREAK to CMCE (cl. 17.3.3): communication resources are
                // temporarily unavailable. We do not model graceful service
                // degradation, so no permitted-service list is carried.
                queue.push_back(SapMsg {
                    sap: Sap::LcmcSap,
                    src: TetraEntity::Mle,
                    dest: TetraEntity::Cmce,
                    msg: SapMsgInner::LcmcMleBreakInd(LcmcMleBreakInd {
                        permitted_services_in_ms_graceful_service_degradation_mode: 0,
                    }),
                });
                // MLE-BREAK to MM (cl. 18.3.3 / 18.3.4.5.3) over the LMM-SAP:
                // MM regards itself as out of service and informs the TNMM user
                // (TNMM-SERVICE "out of service", cl. 15.3.4) so a UI reflects
                // the link loss. Registration state itself is unaffected.
                queue.push_back(SapMsg {
                    sap: Sap::LmmSap,
                    src: TetraEntity::Mle,
                    dest: TetraEntity::Mm,
                    msg: SapMsgInner::LmmMleBreakInd(LmmMleBreakInd {}),
                });
                // Drop the serving cell and re-arm the one-shot activate
                // confirmation so that re-acquisition (a new SYNC) re-runs cell
                // selection (cl. 18.3.4.6) and re-confirms to MM.
                self.serving_cell = None;
                self.activate_confirmed = false;
                // No serving-cell signal while out of service (UI shows no level).
                if self.serving_cell_rssi_dbfs.is_some() {
                    self.serving_cell_rssi_dbfs = None;
                    self.emit_rssi_to_mm(queue);
                }
            }
            (true, true) => {
                // Downlink recovered: reopen the link (cl. 18.3.4.7).
                self.out_of_service = false;
                tracing::info!("MLE: serving-cell downlink recovered â€” MLE-REOPEN (cl. 18.3.4.7)");
                queue.push_back(SapMsg {
                    sap: Sap::LcmcSap,
                    src: TetraEntity::Mle,
                    dest: TetraEntity::Cmce,
                    msg: SapMsgInner::LcmcMleReopenInd(LcmcMleReopenInd {}),
                });
                // MLE-REOPEN to MM over the LMM-SAP: MM leaves out of service.
                // The subsequent cell re-selection / registration re-evaluation
                // (re-armed above) determines the final service state.
                queue.push_back(SapMsg {
                    sap: Sap::LmmSap,
                    src: TetraEntity::Mle,
                    dest: TetraEntity::Mm,
                    msg: SapMsgInner::LmmMleReopenInd(LmmMleReopenInd {}),
                });
                // Cell selection re-runs on the next SYNC (serving_cell was
                // cleared on break), re-firing the LMM-ACTIVATE confirmation so
                // MM re-evaluates registration per cl. 18.3.4.7.1a.
            }
            _ => {
                // No state change (idempotent): initial acquisition reports
                // available while already in service, or repeated failure while
                // already out of service.
                tracing::trace!(
                    "MLE: monitor indication (available={}) with no state change",
                    downlink_available
                );
            }
        }
    }

    /// Forward the current serving-cell downlink level to MM (implementation-
    /// defined Plane B primitive) so it can surface it in the management runtime
    /// state the UI reads. Not an ETSI LMM primitive; RSSI is an MLE-internal
    /// reselection input (cl. 18.3.4) and is passed up only as a convenience.
    fn emit_rssi_to_mm(&self, queue: &mut MessageQueue) {
        queue.push_back(SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Mm,
            msg: SapMsgInner::LmmMleRssiInd(LmmMleRssiInd {
                rssi_dbfs: self.serving_cell_rssi_dbfs,
            }),
        });
    }

    /// ETSI TS 100 392-2 cl. 18.3.4 / 18.4.2.2: adopt the SYSINFO parameters
    /// (location area, subscriber class) for the serving cell.
    pub fn rx_tlmb_tl_sysinfo_ind(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tlmb_tl_sysinfo_ind");

        let SapMsgInner::TlmbSysinfoInd(inner) = &mut message.msg else {
            panic!()
        };

        // Parse the TL-SDU
        let pdu = match DMleSysinfo::from_bitbuf(&mut inner.tl_sdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing DMleSysinfo: {:?} {}", e, inner.tl_sdu.dump_bin());
                return;
            }
        };

        // ETSI cl. 18.4.2.2: the BS service details "registration" flag tells the
        // MS whether registration is required on this cell.
        let registration_required = pdu.bs_service_details.registration;
        // cl. 18.5.2.1 / 16.4.8: "system wide services" flag. `false` = the cell
        // advertises "system wide services temporarily not supported".
        let system_wide_services = pdu.bs_service_details.system_wide_services;

        // Operator survey (receive-only): fill in the pending cell's LA and
        // registration flag from SYSINFO, then report it and advance. Never adopt
        // a serving cell or configure L2 here.
        if self.survey {
            if let Some(cell) = self.survey_pending.as_mut() {
                cell.location_area = Some(pdu.location_area);
                cell.registration_required = Some(registration_required);
                self.survey_finalize_pending(queue);
            } else {
                tracing::debug!("MLE: survey SYSINFO with no pending SYNC cell, ignoring");
            }
            return;
        }

        let confirm = match self.serving_cell.as_mut() {
            Some(cell) => {
                let la_changed = cell.location_area != Some(pdu.location_area);
                // Re-confirm to MM when the cell returns to normal mode from
                // "system wide services temporarily not supported" (or vice
                // versa): a temporarily-registered MS must then perform a periodic
                // location update (cl. 16.4.8 / 16.4.1.0 NOTE, cond. 5).
                let sws_changed = cell.system_wide_services != Some(system_wide_services);
                let changed = la_changed || sws_changed;
                cell.location_area = Some(pdu.location_area);
                cell.subscriber_class = Some(pdu.subscriber_class);
                cell.registration_required = Some(registration_required);
                cell.system_wide_services = Some(system_wide_services);
                let (mcc, mnc) = (cell.mcc, cell.mnc);
                if changed {
                    tracing::info!(
                        "MLE: serving cell SYSINFO adopted: LA={} subscriber_class={:#x} \
                         registration_required={} system_wide_services={}",
                        pdu.location_area,
                        pdu.subscriber_class,
                        registration_required,
                        system_wide_services,
                    );
                }
                // Confirm cell selection to MM exactly once per selected cell,
                // now that both the SYNC identity and the SYSINFO parameters are
                // known (cl. 18.3.4.6 completes selection; the confirmation is the
                // LMM-ACTIVATE confirm primitive, cl. 17.3.2).
                //
                // Additionally re-confirm whenever the location area of the
                // already-selected cell changes (cl. 16.4.1.0 / 18.3.4.7.1a
                // cond. 2), or its "system wide services" status changes
                // (cl. 16.4.8): MM must re-evaluate registration. Repeated
                // identical SYSINFO (no change) is suppressed.
                if !self.activate_confirmed || changed {
                    Some((mcc, mnc, pdu.location_area, registration_required, system_wide_services))
                } else {
                    None
                }
            }
            None => {
                // SYSINFO can arrive before we have selected a cell from SYNC;
                // there is nothing to attach it to yet.
                tracing::debug!("rx_tlmb_tl_sysinfo_ind: SYSINFO before cell selection, ignoring");
                None
            }
        };

        if let Some((mcc, mnc, la, registration_required, system_wide_services)) = confirm {
            // Cell suitability (ETSI cl. 18.4.2.2 / 18.3.4): the cell must admit
            // the MS's subscriber class before it can be used for
            // registration/service. If not permitted, do not confirm the cell to
            // MM (so no registration is attempted) and leave the one-shot
            // confirmation un-armed; while scanning this keeps the cell
            // unsuitable so selection moves on.
            if !self.subscriber_class_permitted(pdu.subscriber_class) {
                tracing::warn!(
                    "MLE: serving cell {}/{} does not permit our subscriber class \
                     (cell class bitmap {:#06x}) â€” cell unsuitable, not confirming (cl. 18.4.2.2)",
                    mcc,
                    mnc,
                    pdu.subscriber_class,
                );
                // Scanning: the adopted cell turned out unsuitable on its
                // subscriber-class bitmap. Drop it so the scan-dwell heartbeat
                // can advance across subsequent (possibly empty) candidates, and
                // step on now (cl. 18.3.4 / 18.4.2.2).
                if self.scanning {
                    self.serving_cell = None;
                    self.activate_confirmed = false;
                    self.advance_scan(queue);
                }
                return;
            }
            self.activate_confirmed = true;
            // A suitable cell has been confirmed: end the scan and camp on it
            // (cl. 18.3.4.6).
            self.stop_scan();
            self.send_mle_activate_conf(queue, mcc, mnc, la, registration_required, system_wide_services);
        }
    }

    /// Send the LMM-ACTIVATE confirmation to MM (ETSI TS 100 392-2 cl. 17.3.2):
    /// a cell has been selected with the required characteristics. `registration_required`
    /// comes from the cell's D-MLE-SYSINFO BS service details and lets MM decide
    /// whether to perform a location update (cl. 16.4). MCC/MNC/LA identify the
    /// cell so MM can distinguish migrating (network change) from roaming (LA
    /// change) location updating (cl. 16.4.1.0 / 18.3.4.7.1a).
    fn send_mle_activate_conf(
        &mut self,
        queue: &mut MessageQueue,
        mcc: u16,
        mnc: u16,
        la: u16,
        registration_required: bool,
        system_wide_services: bool,
    ) {
        tracing::info!(
            "MLE: cell selection complete, confirming to MM (MCC={}, MNC={}, LA={}, \
             registration_required={}, system_wide_services={})",
            mcc,
            mnc,
            la,
            registration_required,
            system_wide_services,
        );
        let m = SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Mm,
            msg: SapMsgInner::LmmMleActivateConf(LmmMleActivateConf {
                registration_required,
                mcc,
                mnc,
                la,
                system_wide_services,
                cell_type: 0, // Todo (cl. 18): cell type not modelled yet
            }),
        };
        queue.push_back(m);
        // An operator-armed camp has now been honoured (the cell is confirmed to
        // MM); disarm so a later manual re-selection is required to camp again.
        if self.camp_armed {
            tracing::info!("MLE: operator camp completed (force_register={})", self.camp_force_register);
            self.camp_armed = false;
        }
    }

    /// ETSI TS 100 392-2 cl. 18.3.4.6: initial cell selection. Adopt the cell
    /// identity from D-MLE-SYNC (cl. 18.4.2.1) and configure the lower layers
    /// with the valid MCC/MNC so the MAC can derive the scrambling code
    /// (cl. 23.2.2 / 8.2.5).
    pub fn rx_tlmb_tl_sync_ind(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_tlmb_tl_sync_ind");

        let SapMsgInner::TlmbSyncInd(inner) = &mut message.msg else {
            panic!()
        };

        // Parse the TL-SDU
        let pdu = match DMleSync::from_bitbuf(&mut inner.tl_sdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing DMleSync: {:?} {}", e, inner.tl_sdu.dump_bin());
                return;
            }
        };

        // Is this a different cell than the one we are currently camped on?
        let newly_selected = self
            .serving_cell
            .as_ref()
            .map(|c| c.mcc != pdu.mcc || c.mnc != pdu.mnc)
            .unwrap_or(true);

        // Operator survey (receive-only): characterise the cell on the tuned
        // candidate carrier from SYNC, then wait (bounded) for its SYSINFO to add
        // LA / registration flag before reporting. Never adopt a serving cell,
        // configure L2, or confirm to MM while surveying.
        if self.survey {
            let carrier_hz = self.survey_candidates.get(self.survey_index).copied().unwrap_or(0);
            match self.survey_pending.as_ref() {
                Some(cell) if cell.mcc == pdu.mcc && cell.mnc == pdu.mnc => {
                    // Same cell re-broadcasting SYNC while we await its SYSINFO;
                    // bound the wait so a slow/absent SYSINFO still gets reported.
                    self.survey_sync_repeats = self.survey_sync_repeats.saturating_add(1);
                    if self.survey_sync_repeats >= Self::SURVEY_SYSINFO_SYNC_LIMIT {
                        self.survey_finalize_pending(queue);
                    }
                }
                _ => {
                    self.survey_sync_repeats = 0;
                    self.survey_pending = Some(FoundCell {
                        carrier_hz,
                        mcc: pdu.mcc,
                        mnc: pdu.mnc,
                        location_area: None,
                        colour_code: None,
                        rssi_dbfs: self.serving_cell_rssi_dbfs,
                        registration_required: None,
                        late_entry_supported: pdu.late_entry_supported,
                    });
                }
            }
            return;
        }

        // Manual cell selection: auto-camp is suppressed unless the operator has
        // explicitly armed a camp (LmmMleCampReq). A newly seen cell is left
        // un-adopted so the MS parks without registering.
        if self.selection_mode_manual && newly_selected && !self.camp_armed {
            tracing::debug!(
                "MLE: manual mode, SYNC for MCC/MNC {}/{} but no camp armed - not selecting",
                pdu.mcc,
                pdu.mnc
            );
            return;
        }

        // Cell suitability (ETSI cl. 18.3.4): only camp on a cell whose network
        // is allowed. The network identity is the D-MLE-SYNC MCC/MNC
        // (cl. 18.4.2.1). This was previously a warning-only TODO ("proper
        // allowed-network handling is out of Phase 2 scope"); enforcing it is
        // the Phase-D radio-style behaviour â€” a radio does not camp on a cell
        // outside its programmed allowed-network set. A disallowed cell is
        // simply not selected: an already-camped (allowed) cell is retained,
        // and while scanning this rejection lets the scan advance to the next
        // candidate.
        if newly_selected && !self.network_allowed(pdu.mcc, pdu.mnc) {
            tracing::warn!(
                "MLE: cell MCC/MNC {}/{} is not an allowed network â€” not selecting (cl. 18.3.4)",
                pdu.mcc,
                pdu.mnc
            );
            // Scanning: this candidate carrier has a serving cell, but on a
            // network the codeplug does not allow. It is unsuitable, so step the
            // scan on to the next candidate (cl. 18.3.4).
            if self.scanning {
                self.advance_scan(queue);
            }
            return;
        }

        // Adopt the cell identity, preserving any SYSINFO learned for the same cell.
        let (location_area, subscriber_class, registration_required, system_wide_services) =
            if newly_selected {
                (None, None, None, None)
            } else {
                let c = self.serving_cell.as_ref().unwrap();
                (
                    c.location_area,
                    c.subscriber_class,
                    c.registration_required,
                    c.system_wide_services,
                )
            };
        self.serving_cell = Some(ServingCell {
            mcc: pdu.mcc,
            mnc: pdu.mnc,
            neighbor_cell_broadcast: pdu.neighbor_cell_broadcast,
            cell_load_ca: pdu.cell_load_ca,
            late_entry_supported: pdu.late_entry_supported,
            location_area,
            subscriber_class,
            registration_required,
            system_wide_services,
        });

        if newly_selected {
            // A new cell was selected: MM must (re-)confirm and register on it.
            // Re-arm the one-shot LMM-ACTIVATE confirmation (sent once SYSINFO
            // for this cell arrives, cl. 17.3.2).
            self.activate_confirmed = false;
        }

        if !newly_selected {
            // Already camped on this cell; the lower layers already have the
            // scrambling code, so nothing further to do.
            return;
        }

        tracing::info!(
            "MLE: selected serving cell MCC={} MNC={} (late_entry={}, cell_load_ca={})",
            pdu.mcc,
            pdu.mnc,
            pdu.late_entry_supported,
            pdu.cell_load_ca
        );

        // Configure layer 2 for the chosen cell (cl. 18.3.4.6 / 20.3.5.4.1c):
        // provide the valid MCC/MNC so UMAC derives and installs the scrambling code.
        let m = SapMsg {
            sap: Sap::TlmcSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Umac,
            msg: SapMsgInner::TlmcConfigureReq(TlmcConfigureReq {
                valid_addresses: Some(TlmcValidAddress {
                    mcc: pdu.mcc,
                    mnc: pdu.mnc,
                    // Scrambling-only configure at cell selection: leave the MAC
                    // downlink address filter unchanged (it is seeded from config
                    // and updated via the MLE-IDENTITIES chain, cl. 17.3.2).
                    individual_ssi: None,
                    group_ssis: None,
                }),
                ..Default::default()
            }),
        };
        queue.push_back(m);
    }

    fn rx_tlmc_prim(&mut self, _queue: &mut MessageQueue, _message: SapMsg) {
        tracing::trace!("rx_tlmc_prim");
        unimplemented!("rx_tlmc_prim");
    }

    fn rx_lmm_mle_unitdata_req(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_lmm_mle_unitdata_req");
        let SapMsgInner::LmmMleUnitdataReq(prim) = &mut message.msg else {
            panic!()
        };

        let mle_prot_discriminator = MleProtocolDiscriminator::Mm;
        let sdu_len = prim.sdu.get_len();
        let mut pdu = BitBuffer::new(3 + sdu_len);
        pdu.write_bits(mle_prot_discriminator.into_raw(), 3);
        pdu.copy_bits(&mut prim.sdu, sdu_len);
        pdu.seek(0);

        let sapmsg = SapMsg {
            sap: Sap::TlaSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Llc,
            msg: SapMsgInner::TlaTlDataReqBl(TlaTlDataReqBl {
                main_address: prim.address,
                link_id: 0,
                endpoint_id: 0,
                tl_sdu: pdu,
                // Forward the L3 PDU priority / emergency flag from the
                // MLE-UNITDATA request down to the LLC (cl. 23.5.1.4.4).
                pdu_priority: prim.pdu_priority,
                is_emergency: prim.is_emergency,
                stealing_permission: false,
                subscriber_class: 0, // TODO fixme
                fcs_flag: false,
                air_interface_encryption: None,
                stealing_repeats_flag: None,
                data_class_info: None,
                req_handle: 0, // TODO FIXME; should we pass the same handle here?
                graceful_degradation: None,
                chan_alloc: None,
                tx_reporter: prim.tx_reporter.take(),
            }),
        };
        queue.push_back(sapmsg);
    }

    fn rx_lmm_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_lmm_prim");
        match &message.msg {
            SapMsgInner::LmmMleUnitdataReq(_prim) => {
                self.rx_lmm_mle_unitdata_req(queue, message);
            }
            SapMsgInner::LmmMleIdentitiesReq(_prim) => {
                self.rx_lmm_mle_identities_req(queue, message);
            }
            SapMsgInner::LmmMleSelectionModeReq(prim) => {
                self.rx_lmm_mle_selection_mode_req(prim.manual);
            }
            SapMsgInner::LmmMleScanReq(prim) => {
                let start = prim.start;
                self.rx_lmm_mle_scan_req(queue, start);
            }
            SapMsgInner::LmmMleCampReq(prim) => {
                let (carrier_hz, register) = (prim.carrier_hz, prim.register);
                self.rx_lmm_mle_camp_req(queue, carrier_hz, register);
            }
            _ => panic!(),
        }
    }

    /// MM -> MLE: set the cell-selection mode (**[impl policy]**, Plane B).
    /// Switching to manual stops any automatic scan so the operator drives
    /// selection; switching back to automatic re-arms automatic scanning if the
    /// MS is not currently camped.
    fn rx_lmm_mle_selection_mode_req(&mut self, manual: bool) {
        if self.selection_mode_manual == manual {
            return;
        }
        self.selection_mode_manual = manual;
        self.camp_armed = false;
        self.camp_force_register = false;
        if manual {
            // Stop the automatic scan; leave any current camp in place until the
            // operator surveys/camps elsewhere.
            self.scanning = false;
            tracing::info!("MLE: cell selection mode set to MANUAL (operator-driven)");
        } else {
            tracing::info!("MLE: cell selection mode set to AUTOMATIC (cl. 18.3.4.6)");
        }
    }

    /// MM -> MLE: start or cancel a receive-only survey (**[impl policy]**).
    fn rx_lmm_mle_scan_req(&mut self, queue: &mut MessageQueue, start: bool) {
        if start {
            self.start_survey(queue);
        } else {
            self.cancel_survey(queue);
        }
    }

    /// MM -> MLE: camp (and optionally register) on a chosen candidate carrier
    /// (**[impl policy]**). Arms a camp so the next suitable SYNC on the tuned
    /// carrier is adopted through the normal selection path (cl. 18.3.4.6), even
    /// in manual mode. Rejects a carrier that is not a codeplug candidate.
    fn rx_lmm_mle_camp_req(&mut self, queue: &mut MessageQueue, carrier_hz: u32, register: bool) {
        let candidates = self.scan_candidate_carriers();
        if !candidates.is_empty() && !candidates.contains(&carrier_hz) {
            tracing::warn!("MLE: camp request for {} Hz is not a codeplug candidate carrier - ignoring", carrier_hz);
            return;
        }
        // End any survey and clear any current camp so the next SYNC on the
        // requested carrier triggers a fresh selection.
        self.survey = false;
        self.survey_pending = None;
        self.scanning = false;
        self.serving_cell = None;
        self.activate_confirmed = false;
        self.camp_armed = true;
        self.camp_force_register = register;
        tracing::info!("MLE: operator camp requested on {} Hz (register={})", carrier_hz, register);
        self.request_tune(queue, carrier_hz);
    }

    /// MLE-IDENTITIES request from MM (cl. 17.3.2): the set of identities by
    /// which the MS is currently known (own ISSI + the full attached-group set).
    /// The MLE holds no air-interface state for this; it configures the layer-2
    /// (MAC) downlink address filter (cl. 23.4.1.2.1) via TL-CONFIGURE so the MAC
    /// accepts traffic addressed to the MS's own ISSI and each attached GSSI, and
    /// drops everything else. Called after registration (to seed the set) and
    /// after every successful standalone group attach/detach (cl. 16.8.2).
    fn rx_lmm_mle_identities_req(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        let SapMsgInner::LmmMleIdentitiesReq(prim) = &message.msg else {
            panic!()
        };
        tracing::info!(
            "MLE: <- MLE-IDENTITIES (issi={} attached_gssis={:?} detached_gssis={:?}); configuring MAC address filter",
            prim.issi,
            prim.attached_gssis,
            prim.detached_gssis
        );

        // Use the configured home network MCC/MNC for the valid-address element.
        // These match what UMAC already holds; supplying them keeps the element
        // well-formed and does not change the derived scrambling code.
        let cfg = self.config.config();
        let m = SapMsg {
            sap: Sap::TlmcSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Umac,
            msg: SapMsgInner::TlmcConfigureReq(TlmcConfigureReq {
                valid_addresses: Some(TlmcValidAddress {
                    mcc: cfg.net.mcc,
                    mnc: cfg.net.mnc,
                    individual_ssi: Some(prim.issi),
                    group_ssis: Some(prim.attached_gssis.clone()),
                }),
                ..Default::default()
            }),
        };
        queue.push_back(m);
    }

    fn rx_tlpd_prim(&mut self, _queue: &mut MessageQueue, _message: SapMsg) {
        tracing::trace!("rx_tlpd_prim");
        unimplemented!("rx_tlpd_prim");
    }

    fn rx_lcmc_mle_unitdata_req(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_lcmc_mle_unitdata_req");
        let SapMsgInner::LcmcMleUnitdataReq(prim) = &mut message.msg else {
            panic!()
        };

        let mle_prot_discriminator = MleProtocolDiscriminator::Cmce;
        let sdu_len = prim.sdu.get_len();
        let mut pdu = BitBuffer::new(3 + sdu_len);
        pdu.write_bits(mle_prot_discriminator.into_raw(), 3);
        pdu.copy_bits(&mut prim.sdu, sdu_len);
        pdu.seek(0);

        // Take Channel Allocation Request if any
        let chan_alloc = prim.chan_alloc.take();

        let sapmsg = SapMsg {
            sap: Sap::TlaSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Llc,
            msg: SapMsgInner::TlaTlDataReqBl(TlaTlDataReqBl {
                main_address: prim.main_address,
                link_id: prim.link_id,
                endpoint_id: prim.endpoint_id,
                tl_sdu: pdu,
                // CMCE PDU priority / emergency plumbing is deferred (CMCE MS is
                // stubbed and does not originate uplinks yet); default to no
                // explicit L3 priority so the MAC uses the access-code minimum.
                pdu_priority: None,
                is_emergency: false,
                stealing_permission: prim.stealing_permission,
                subscriber_class: 0, // TODO fixme
                fcs_flag: false,
                air_interface_encryption: None,
                stealing_repeats_flag: None,
                data_class_info: None,
                req_handle: 0, // TODO FIXME
                graceful_degradation: None,
                chan_alloc,
                tx_reporter: prim.tx_reporter.take(),
            }),
        };
        queue.push_back(sapmsg);
    }

    fn rx_lcmc_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_lcmc_prim");
        match &message.msg {
            SapMsgInner::LcmcMleUnitdataReq(_) => {
                self.rx_lcmc_mle_unitdata_req(queue, message);
            }
            SapMsgInner::LcmcMleConfigureReq(prim) => {
                // C-plane -> U-plane seam. ETSI TS 100 392-2 cl. 17.3.3 defines
                // MLE-CONFIGURE and cl. 14.5.1.4 requires CC to issue it for
                // U-plane switching / transmission-grant changes. MLE is the
                // layer that configures the MAC, so forward the U-plane transmit
                // state down to UMAC (TLMC-SAP) where the transmit scheduler
                // (cl. 23) gates uplink TCH/S emission on it. Only the grant
                // state is carried; the granted timeslot stays owned by UMAC's
                // CHANNEL ALLOCATION record (cl. 21.5.2), the single slot
                // authority.
                tracing::info!(
                    "MLE-MS: CMCE U-plane configure (switch_u_plane={}, tx_grant={})",
                    prim.switch_u_plane,
                    prim.tx_grant
                );
                queue.push_back(SapMsg {
                    sap: Sap::TlmcSap,
                    src: TetraEntity::Mle,
                    dest: TetraEntity::Umac,
                    msg: SapMsgInner::TlmcUPlaneConfigureReq(TlmcUPlaneConfigureReq {
                        switch_u_plane: prim.switch_u_plane,
                        tx_grant: prim.tx_grant,
                    }),
                });
            }
            _ => panic!(),
        }
    }
}

impl TetraEntityTrait for MleMs {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Mle
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);

        match message.sap {
            Sap::TlaSap => {
                self.rx_tla_prim(queue, message);
            }
            Sap::TlmbSap => {
                self.rx_tlmb_prim(queue, message);
            }
            Sap::TlmcSap => {
                self.rx_tlmc_prim(queue, message);
            }
            Sap::LmmSap => {
                self.rx_lmm_prim(queue, message);
            }
            Sap::TlpdSap => {
                self.rx_tlpd_prim(queue, message);
            }
            Sap::LcmcSap => {
                self.rx_lcmc_prim(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetra_config::bluestation::from_toml_str;
    use tetra_saps::lmm::LmmMleIdentitiesReq;

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

    fn ms_mle() -> MleMs {
        let cfg = from_toml_str(MS_TOML).expect("valid MS test config");
        MleMs::new(SharedConfig::from_parts(cfg, None))
    }

    /// Put the MLE into the "camped and confirmed" state a real run reaches
    /// after cell selection.
    fn camp(mle: &mut MleMs) {
        mle.serving_cell = Some(ServingCell {
            mcc: 901,
            mnc: 9999,
            location_area: Some(1),
            registration_required: Some(true),
            system_wide_services: Some(true),
            ..Default::default()
        });
        mle.activate_confirmed = true;
    }

    /// A downlink failure indication makes the MLE declare out-of-service, emit
    /// MLE-BREAK to CMCE, clear the serving cell, and re-arm cell selection
    /// (cl. 18.3.4.5.3).
    #[test]
    fn test_downlink_failure_declares_out_of_service_and_breaks() {
        let mut mle = ms_mle();
        camp(&mut mle);
        let mut queue = MessageQueue::new();

        mle.rx_tlmb_monitor_ind(&mut queue, false);

        assert!(mle.out_of_service, "declared out of service");
        assert!(mle.serving_cell.is_none(), "serving cell cleared");
        assert!(!mle.activate_confirmed, "activate confirmation re-armed");

        let breaks: Vec<_> = queue
            .iter()
            .filter(|m| matches!(m.msg, SapMsgInner::LcmcMleBreakInd(_)))
            .collect();
        assert_eq!(breaks.len(), 1, "exactly one MLE-BREAK");
        assert_eq!(breaks[0].dest, TetraEntity::Cmce, "MLE-BREAK addressed to CMCE");

        // The break is ALSO routed to MM over the LMM-SAP so MM can go out of
        // service and inform the TNMM user (cl. 18.3.3 / 15.3.4).
        let mm_breaks: Vec<_> = queue
            .iter()
            .filter(|m| matches!(m.msg, SapMsgInner::LmmMleBreakInd(_)))
            .collect();
        assert_eq!(mm_breaks.len(), 1, "exactly one LMM MLE-BREAK");
        assert_eq!(mm_breaks[0].dest, TetraEntity::Mm, "LMM MLE-BREAK addressed to MM");
    }

    /// A second failure indication while already out of service is idempotent:
    /// no further MLE-BREAK.
    #[test]
    fn test_repeated_failure_is_idempotent() {
        let mut mle = ms_mle();
        camp(&mut mle);
        let mut queue = MessageQueue::new();

        mle.rx_tlmb_monitor_ind(&mut queue, false);
        mle.rx_tlmb_monitor_ind(&mut queue, false);

        let breaks = queue
            .iter()
            .filter(|m| matches!(m.msg, SapMsgInner::LcmcMleBreakInd(_)))
            .count();
        assert_eq!(breaks, 1, "MLE-BREAK emitted only once");
    }

    /// Recovery after a failure emits MLE-REOPEN to CMCE and clears the
    /// out-of-service state (cl. 18.3.4.7).
    #[test]
    fn test_recovery_emits_reopen() {
        let mut mle = ms_mle();
        camp(&mut mle);
        let mut queue = MessageQueue::new();

        mle.rx_tlmb_monitor_ind(&mut queue, false);
        mle.rx_tlmb_monitor_ind(&mut queue, true);

        assert!(!mle.out_of_service, "back in service after recovery");
        let reopens: Vec<_> = queue
            .iter()
            .filter(|m| matches!(m.msg, SapMsgInner::LcmcMleReopenInd(_)))
            .collect();
        assert_eq!(reopens.len(), 1, "exactly one MLE-REOPEN");
        assert_eq!(reopens[0].dest, TetraEntity::Cmce, "MLE-REOPEN addressed to CMCE");

        // Reopen is ALSO routed to MM over the LMM-SAP (restores service view).
        let mm_reopens: Vec<_> = queue
            .iter()
            .filter(|m| matches!(m.msg, SapMsgInner::LmmMleReopenInd(_)))
            .collect();
        assert_eq!(mm_reopens.len(), 1, "exactly one LMM MLE-REOPEN");
        assert_eq!(mm_reopens[0].dest, TetraEntity::Mm, "LMM MLE-REOPEN addressed to MM");
    }

    /// A monitoring refresh carrying a signal level caches it as the serving-cell
    /// RSSI (reselection input, cl. 18.3.4 / management-UI readout); a subsequent
    /// downlink failure clears it (no serving-cell signal while out of service).
    #[test]
    fn test_serving_cell_rssi_cached_and_cleared() {
        use tetra_saps::tlmb::TlmbMonitorInd;
        let mut mle = ms_mle();
        camp(&mut mle);
        let mut queue = MessageQueue::new();

        let monitor = |downlink_available, rssi_dbfs| SapMsg {
            sap: Sap::TlmbSap,
            src: TetraEntity::Phy,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::TlmbMonitorInd(TlmbMonitorInd {
                downlink_available,
                rssi_dbfs,
            }),
        };

        mle.rx_tlmb_prim(&mut queue, monitor(true, Some(-12.5)));
        assert_eq!(mle.serving_cell_rssi_dbfs, Some(-12.5), "level cached from refresh");

        mle.rx_tlmb_prim(&mut queue, monitor(false, None));
        assert_eq!(mle.serving_cell_rssi_dbfs, None, "cleared on out of service");
    }

    /// An "available" indication while in service (initial acquisition) is a
    /// no-op: no MLE-REOPEN, still in service.
    #[test]
    fn test_available_while_in_service_is_noop() {
        let mut mle = ms_mle();
        camp(&mut mle);
        let mut queue = MessageQueue::new();

        mle.rx_tlmb_monitor_ind(&mut queue, true);

        assert!(!mle.out_of_service);
        assert!(mle.serving_cell.is_some(), "serving cell untouched");
        assert_eq!(
            queue.iter().filter(|m| matches!(m.msg, SapMsgInner::LcmcMleReopenInd(_))).count(),
            0,
            "no reopen when never broken"
        );
    }

    /// G1 (cl. 17.3.2 / 23.4.1.2.1): an MLE-IDENTITIES request from MM makes the
    /// MLE configure the MAC downlink address filter, forwarding the own ISSI and
    /// the full attached-group set to UMAC in a TL-CONFIGURE.
    #[test]
    fn test_mle_identities_configures_mac_filter() {
        let mut mle = ms_mle();
        let mut queue = MessageQueue::new();

        let msg = SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mm,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LmmMleIdentitiesReq(LmmMleIdentitiesReq {
                issi: 1000001,
                assi: None,
                attached_gssis: vec![91, 220],
                detached_gssis: vec![],
            }),
        };
        mle.rx_lmm_prim(&mut queue, msg);

        let out = queue.pop_front().expect("a TL-CONFIGURE must be emitted");
        assert_eq!(out.sap, Sap::TlmcSap);
        assert_eq!(out.dest, TetraEntity::Umac);
        let SapMsgInner::TlmcConfigureReq(req) = out.msg else {
            panic!("expected TlmcConfigureReq");
        };
        let va = req.valid_addresses.expect("valid addresses set");
        assert_eq!(va.individual_ssi, Some(1000001));
        assert_eq!(va.group_ssis, Some(vec![91, 220]));
        // MCC/MNC are the configured home network so the element stays well-formed.
        assert_eq!(va.mcc, 901);
        assert_eq!(va.mnc, 9999);
    }

    use tetra_pdus::mle::fields::bs_service_details::BsServiceDetails;
    use tetra_saps::tlmb::{TlmbSyncInd, TlmbSysinfoInd};

    /// Build a D-MLE-SYNC monitoring indication (cl. 18.4.2.1) for a cell.
    fn sync_ind(mcc: u16, mnc: u16) -> SapMsg {
        let mut tl_sdu = BitBuffer::new(64);
        DMleSync {
            mcc,
            mnc,
            neighbor_cell_broadcast: 0,
            cell_load_ca: 0,
            late_entry_supported: false,
        }
        .to_bitbuf(&mut tl_sdu);
        tl_sdu.seek(0);
        SapMsg {
            sap: Sap::TlmbSap,
            src: TetraEntity::Phy,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::TlmbSyncInd(TlmbSyncInd { endpoint_id: 0, tl_sdu }),
        }
    }

    /// Build a D-MLE-SYSINFO monitoring indication (cl. 18.4.2.2) carrying a
    /// subscriber-class bitmap; all BS service-detail flags off except
    /// registration + system-wide-services.
    fn sysinfo_ind(location_area: u16, subscriber_class: u16) -> SapMsg {
        let bs_service_details = BsServiceDetails {
            registration: true,
            deregistration: false,
            priority_cell: false,
            no_minimum_mode: false,
            migration: false,
            system_wide_services: true,
            voice_service: false,
            circuit_mode_data_service: false,
            sndcp_service: false,
            aie_service: false,
            advanced_link: false,
        };
        let mut tl_sdu = BitBuffer::new(64);
        DMleSysinfo { location_area, subscriber_class, bs_service_details }.to_bitbuf(&mut tl_sdu);
        tl_sdu.seek(0);
        SapMsg {
            sap: Sap::TlmbSap,
            src: TetraEntity::Phy,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::TlmbSysinfoInd(TlmbSysinfoInd {
                endpoint_id: 0,
                tl_sdu,
                mac_broadcast_info: None,
            }),
        }
    }

    /// Suitability (cl. 18.3.4): a SYNC for the home network is selected â€” the
    /// serving cell is adopted and the MAC is configured with its scrambling.
    #[test]
    fn test_allowed_home_network_selected() {
        let mut mle = ms_mle();
        let mut queue = MessageQueue::new();

        mle.rx_tlmb_prim(&mut queue, sync_ind(901, 9999));

        assert!(mle.serving_cell.is_some(), "home cell selected");
        assert_eq!(
            queue.iter().filter(|m| matches!(m.msg, SapMsgInner::TlmcConfigureReq(_))).count(),
            1,
            "MAC configured for the selected cell"
        );
    }

    /// Suitability (cl. 18.3.4): a SYNC for a foreign, non-programmed network is
    /// rejected â€” no cell is selected and the MAC is not configured.
    #[test]
    fn test_disallowed_network_not_selected() {
        let mut mle = ms_mle();
        let mut queue = MessageQueue::new();

        mle.rx_tlmb_prim(&mut queue, sync_ind(238, 6));

        assert!(mle.serving_cell.is_none(), "foreign cell not selected");
        assert_eq!(
            queue.iter().filter(|m| matches!(m.msg, SapMsgInner::TlmcConfigureReq(_))).count(),
            0,
            "MAC not configured for a disallowed network"
        );
    }

    /// Suitability (cl. 18.4.2.2): once camped, a SYSINFO whose subscriber-class
    /// bitmap admits our class (1 => bit 0) confirms the cell to MM.
    #[test]
    fn test_subscriber_class_permitted_confirms() {
        let mut mle = ms_mle();
        let mut queue = MessageQueue::new();
        mle.rx_tlmb_prim(&mut queue, sync_ind(901, 9999));

        // class 1 permitted => bit 0 set.
        mle.rx_tlmb_prim(&mut queue, sysinfo_ind(1, 0b1));

        assert!(mle.activate_confirmed, "cell confirmed");
        assert_eq!(
            queue.iter().filter(|m| matches!(m.msg, SapMsgInner::LmmMleActivateConf(_))).count(),
            1,
            "LMM-ACTIVATE confirmation sent to MM"
        );
    }

    /// Suitability (cl. 18.4.2.2): a SYSINFO whose subscriber-class bitmap does
    /// NOT admit our class leaves the cell unconfirmed â€” no registration.
    #[test]
    fn test_subscriber_class_not_permitted_not_confirmed() {
        let mut mle = ms_mle();
        let mut queue = MessageQueue::new();
        mle.rx_tlmb_prim(&mut queue, sync_ind(901, 9999));

        // class 1 NOT permitted (bit 0 clear; only classes 2 and 3 allowed).
        mle.rx_tlmb_prim(&mut queue, sysinfo_ind(1, 0b110));

        assert!(!mle.activate_confirmed, "cell not confirmed for a barred class");
        assert_eq!(
            queue.iter().filter(|m| matches!(m.msg, SapMsgInner::LmmMleActivateConf(_))).count(),
            0,
            "no LMM-ACTIVATE confirmation for a barred subscriber class"
        );
    }

    /// D-2 (tune plumbing): the MLE `request_tune` helper emits a TMC-SAP
    /// TlmcTuneReq addressed to UMAC carrying the requested carrier, so the
    /// scanning engine can step the receiver across candidate carriers.
    #[test]
    fn test_request_tune_emits_tlmc_tune_req() {
        let mle = ms_mle();
        let mut queue = MessageQueue::new();

        mle.request_tune(&mut queue, 396_000_000);

        let out = queue.pop_front().expect("a TLMC-TUNE must be emitted");
        assert_eq!(out.sap, Sap::TlmcSap);
        assert_eq!(out.src, TetraEntity::Mle);
        assert_eq!(out.dest, TetraEntity::Umac);
        let SapMsgInner::TlmcTuneReq(req) = out.msg else {
            panic!("expected TlmcTuneReq");
        };
        assert_eq!(req.carrier_hz, 396_000_000);
    }

    // ----------------------------------------------------------------------
    // Phase D-3: scanning cell-selection engine (ETSI TS 100 392-2 cl. 18.3.4)
    // ----------------------------------------------------------------------

    /// A List-mode scan codeplug programming two candidate downlink carriers.
    /// (Home network 901/9999 stays always-allowed; no extra allowed networks.)
    const MS_TOML_SCAN: &str = r#"
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

[[frequency_list]]
name = "scan"
mode = "List"
frequencies = [390000000, 396000000]
dwell_ms = 500
"#;

    /// Two candidate carriers programmed in the scan config above.
    const SCAN_F0: u32 = 390_000_000;
    const SCAN_F1: u32 = 396_000_000;

    fn ms_mle_scan() -> MleMs {
        let cfg = from_toml_str(MS_TOML_SCAN).expect("valid MS scan test config");
        MleMs::new(SharedConfig::from_parts(cfg, None))
    }

    /// A PHY scan-dwell-elapsed heartbeat (no carrier on the current candidate).
    fn scan_dwell_ind() -> SapMsg {
        use tetra_saps::tlmb::TlmbScanDwellInd;
        SapMsg {
            sap: Sap::TlmbSap,
            src: TetraEntity::Phy,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::TlmbScanDwellInd(TlmbScanDwellInd { rssi_dbfs: Some(-95.0) }),
        }
    }

    /// The single tune carrier emitted since the queue was last drained, or
    /// `None` if no TlmcTuneReq is present.
    fn tuned_carrier(queue: &MessageQueue) -> Option<u32> {
        queue.iter().find_map(|m| match &m.msg {
            SapMsgInner::TlmcTuneReq(t) => Some(t.carrier_hz),
            _ => None,
        })
    }

    /// D-3: the first scan-dwell heartbeat with no serving cell starts the scan
    /// and retunes to the first programmed candidate (cl. 18.3.4).
    #[test]
    fn test_scan_starts_on_first_dwell() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_tlmb_prim(&mut queue, scan_dwell_ind());

        assert!(mle.scanning, "scan started");
        assert_eq!(tuned_carrier(&queue), Some(SCAN_F0), "tuned to first candidate");
    }

    /// D-3: subsequent dwell heartbeats step through the candidate list and wrap.
    #[test]
    fn test_scan_advances_and_wraps_on_dwell() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_tlmb_prim(&mut queue, scan_dwell_ind()); // start -> F0
        queue = MessageQueue::new();
        mle.rx_tlmb_prim(&mut queue, scan_dwell_ind()); // -> F1
        assert_eq!(tuned_carrier(&queue), Some(SCAN_F1), "advanced to second candidate");
        queue = MessageQueue::new();
        mle.rx_tlmb_prim(&mut queue, scan_dwell_ind()); // wrap -> F0
        assert_eq!(tuned_carrier(&queue), Some(SCAN_F0), "wrapped back to first candidate");
    }

    /// D-3: a candidate carrying a cell on a disallowed network is unsuitable, so
    /// the scan advances to the next candidate (cl. 18.3.4).
    #[test]
    fn test_scan_advances_on_disallowed_network() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_tlmb_prim(&mut queue, scan_dwell_ind()); // start -> F0
        queue = MessageQueue::new();

        // A foreign cell appears on F0: reject and step to F1.
        mle.rx_tlmb_prim(&mut queue, sync_ind(238, 6));

        assert!(mle.serving_cell.is_none(), "foreign cell not adopted");
        assert!(mle.scanning, "still scanning");
        assert_eq!(tuned_carrier(&queue), Some(SCAN_F1), "advanced past the disallowed cell");
    }

    /// D-3: a candidate whose cell bars our subscriber class is unsuitable â€” the
    /// adopted cell is dropped and the scan advances (cl. 18.4.2.2 / 18.3.4).
    #[test]
    fn test_scan_advances_on_barred_class() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_tlmb_prim(&mut queue, scan_dwell_ind()); // start -> F0
        queue = MessageQueue::new();

        // Home cell adopted from SYNC (network allowed)...
        mle.rx_tlmb_prim(&mut queue, sync_ind(901, 9999));
        assert!(mle.serving_cell.is_some(), "home cell adopted pending SYSINFO");
        queue = MessageQueue::new();

        // ...but its SYSINFO bars our class (only classes 2/3): drop and advance.
        mle.rx_tlmb_prim(&mut queue, sysinfo_ind(1, 0b110));

        assert!(mle.serving_cell.is_none(), "unsuitable cell dropped");
        assert!(mle.scanning, "still scanning");
        assert!(!mle.activate_confirmed, "not confirmed");
        assert_eq!(tuned_carrier(&queue), Some(SCAN_F1), "advanced past the barred cell");
    }

    /// D-3: a suitable candidate ends the scan â€” the cell is confirmed to MM and
    /// no further candidate retune happens on a later stray dwell (cl. 18.3.4.6).
    #[test]
    fn test_scan_camps_on_suitable_cell_and_stops() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_tlmb_prim(&mut queue, scan_dwell_ind()); // start -> F0
        mle.rx_tlmb_prim(&mut queue, sync_ind(901, 9999)); // adopt home cell
        mle.rx_tlmb_prim(&mut queue, sysinfo_ind(1, 0b1)); // class permitted -> camp

        assert!(mle.activate_confirmed, "cell confirmed");
        assert!(!mle.scanning, "scan stopped after camping");
        assert_eq!(
            queue.iter().filter(|m| matches!(m.msg, SapMsgInner::LmmMleActivateConf(_))).count(),
            1,
            "LMM-ACTIVATE confirmation sent to MM"
        );

        // A later stray dwell must not retune (we are camped).
        queue = MessageQueue::new();
        mle.rx_tlmb_prim(&mut queue, scan_dwell_ind());
        assert_eq!(tuned_carrier(&queue), None, "no retune while camped");
    }

    /// D-3: with no scan programmed (default codeplug), a dwell heartbeat is a
    /// no-op â€” the radio stays on its single configured carrier.
    #[test]
    fn test_no_scan_when_not_programmed() {
        let mut mle = ms_mle();
        let mut queue = MessageQueue::new();

        mle.rx_tlmb_prim(&mut queue, scan_dwell_ind());

        assert!(!mle.scanning, "scanning not enabled without a scan codeplug");
        assert_eq!(tuned_carrier(&queue), None, "no retune when not scanning");
    }

    // ----------------------------------------------------------------------
    // Manual cell survey + register-to-cell (UI-driven, receive-only)
    // ETSI TS 100 392-2 cl. 18.3.4 (survey), 18.3.4.6 (camp), 16.4 (register).
    // ----------------------------------------------------------------------

    /// Build an operator survey start/stop request (MM -> MLE, LMM SAP).
    fn scan_req(start: bool) -> SapMsg {
        use tetra_saps::lmm::LmmMleScanReq;
        SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mm,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LmmMleScanReq(LmmMleScanReq { start }),
        }
    }

    /// Build a selection-mode request (MM -> MLE, LMM SAP).
    fn selection_mode_req(manual: bool) -> SapMsg {
        use tetra_saps::lmm::LmmMleSelectionModeReq;
        SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mm,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LmmMleSelectionModeReq(LmmMleSelectionModeReq { manual }),
        }
    }

    /// Build an operator camp request (MM -> MLE, LMM SAP).
    fn camp_req(carrier_hz: u32, register: bool) -> SapMsg {
        use tetra_saps::lmm::LmmMleCampReq;
        SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mm,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LmmMleCampReq(LmmMleCampReq { carrier_hz, register }),
        }
    }

    /// All scan-result indications emitted to MM since the queue was drained.
    fn scan_results(queue: &MessageQueue) -> Vec<&LmmMleScanResultInd> {
        queue
            .iter()
            .filter_map(|m| match &m.msg {
                SapMsgInner::LmmMleScanResultInd(r) => Some(r),
                _ => None,
            })
            .collect()
    }

    /// The single scan-complete indication emitted to MM, if any.
    fn scan_complete(queue: &MessageQueue) -> Option<&LmmMleScanCompleteInd> {
        queue.iter().find_map(|m| match &m.msg {
            SapMsgInner::LmmMleScanCompleteInd(c) => Some(c),
            _ => None,
        })
    }

    /// Survey: `StartCellScan` tunes to the first candidate and enters the
    /// receive-only survey mode without camping.
    #[test]
    fn test_survey_starts_and_tunes_first_candidate() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_lmm_prim(&mut queue, scan_req(true));

        assert!(mle.survey, "survey mode entered");
        assert!(!mle.scanning, "automatic scan not running during a survey");
        assert!(mle.serving_cell.is_none(), "no cell camped at survey start");
        assert_eq!(tuned_carrier(&queue), Some(SCAN_F0), "tuned to first candidate");
    }

    /// Survey: visits each candidate once (no wrap), reporting one result per
    /// found cell and a single completion, and never camps/registers.
    #[test]
    fn test_survey_reports_each_cell_and_completes() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_lmm_prim(&mut queue, scan_req(true)); // start -> tune F0
        queue = MessageQueue::new();

        // A cell appears on F0: SYNC then SYSINFO characterise it and advance.
        mle.rx_tlmb_prim(&mut queue, sync_ind(901, 9999));
        assert!(mle.serving_cell.is_none(), "survey never adopts a serving cell");
        mle.rx_tlmb_prim(&mut queue, sysinfo_ind(1, 0b1));
        assert_eq!(tuned_carrier(&queue), Some(SCAN_F1), "advanced to second candidate");

        // A different cell on F1.
        mle.rx_tlmb_prim(&mut queue, sync_ind(238, 6));
        mle.rx_tlmb_prim(&mut queue, sysinfo_ind(2, 0b1));

        let results = scan_results(&queue);
        assert_eq!(results.len(), 2, "one result per found cell");
        assert_eq!(results[0].carrier_hz, SCAN_F0);
        assert_eq!(results[0].mcc, 901);
        assert_eq!(results[0].mnc, 9999);
        assert_eq!(results[0].location_area, Some(1));
        assert_eq!(results[0].registration_required, Some(true));
        assert_eq!(results[1].carrier_hz, SCAN_F1);
        assert_eq!(results[1].mcc, 238);
        assert_eq!(results[1].location_area, Some(2));

        let complete = scan_complete(&queue).expect("survey completion emitted");
        assert_eq!(complete.found, 2);
        assert_eq!(complete.scanned, 2);
        assert!(!mle.survey, "survey finished after the last candidate");
        assert!(mle.serving_cell.is_none(), "never camped during survey");
        assert!(!mle.activate_confirmed, "never confirmed/registered during survey");
        assert_eq!(
            queue.iter().filter(|m| matches!(m.msg, SapMsgInner::LmmMleActivateConf(_))).count(),
            0,
            "survey emits no LMM-ACTIVATE (no registration)"
        );
    }

    /// Survey: a candidate that yields no cell within the dwell is recorded as an
    /// empty carrier (no result) and the survey advances (cl. 18.3.4).
    #[test]
    fn test_survey_empty_carrier_advances_on_dwell() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_lmm_prim(&mut queue, scan_req(true)); // start -> F0
        queue = MessageQueue::new();

        // No cell on F0: the dwell heartbeat advances to F1.
        mle.rx_tlmb_prim(&mut queue, scan_dwell_ind());
        assert!(scan_results(&queue).is_empty(), "empty carrier yields no result");
        assert_eq!(tuned_carrier(&queue), Some(SCAN_F1), "advanced past the empty carrier");

        // No cell on F1 either: the survey completes with zero found cells.
        queue = MessageQueue::new();
        mle.rx_tlmb_prim(&mut queue, scan_dwell_ind());
        let complete = scan_complete(&queue).expect("survey completion emitted");
        assert_eq!(complete.found, 0);
        assert_eq!(complete.scanned, 2);
        assert!(!mle.survey, "survey finished");
    }

    /// Survey: a SYNC-only cell (SYSINFO never arrives) is still reported after
    /// the bounded wait, with LA/registration left unknown.
    #[test]
    fn test_survey_reports_sync_only_cell_after_bound() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_lmm_prim(&mut queue, scan_req(true)); // start -> F0
        queue = MessageQueue::new();

        // Repeated SYNC without SYSINFO: bounded by SURVEY_SYSINFO_SYNC_LIMIT.
        for _ in 0..MleMs::SURVEY_SYSINFO_SYNC_LIMIT + 1 {
            mle.rx_tlmb_prim(&mut queue, sync_ind(901, 9999));
        }

        let results = scan_results(&queue);
        assert_eq!(results.len(), 1, "SYNC-only cell reported once");
        assert_eq!(results[0].mcc, 901);
        assert_eq!(results[0].location_area, None, "LA unknown without SYSINFO");
        assert_eq!(results[0].registration_required, None, "reg flag unknown without SYSINFO");
        assert_eq!(tuned_carrier(&queue), Some(SCAN_F1), "advanced after the bound");
    }

    /// Survey: a foreign/disallowed-network cell is still characterised and
    /// reported (a survey observes, it does not filter by allowed-network).
    #[test]
    fn test_survey_reports_disallowed_network_cell() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_lmm_prim(&mut queue, scan_req(true)); // start -> F0
        queue = MessageQueue::new();

        mle.rx_tlmb_prim(&mut queue, sync_ind(238, 6)); // foreign network
        mle.rx_tlmb_prim(&mut queue, sysinfo_ind(7, 0b1));

        let results = scan_results(&queue);
        assert_eq!(results.len(), 1, "disallowed cell still reported by the survey");
        assert_eq!(results[0].mcc, 238);
        assert_eq!(results[0].mnc, 6);
        assert!(mle.serving_cell.is_none(), "survey never adopts the foreign cell");
    }

    /// Survey: `StopCellScan` cancels an in-progress survey and reports a
    /// completion for the carriers visited so far.
    #[test]
    fn test_survey_cancel_reports_completion() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_lmm_prim(&mut queue, scan_req(true)); // start -> F0
        queue = MessageQueue::new();

        mle.rx_lmm_prim(&mut queue, scan_req(false)); // cancel

        assert!(!mle.survey, "survey cancelled");
        assert!(scan_complete(&queue).is_some(), "cancellation reports a completion");
    }

    /// Manual mode: a SYNC for a suitable cell is NOT adopted (auto-camp is
    /// suppressed) until the operator arms a camp.
    #[test]
    fn test_manual_mode_suppresses_auto_camp() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_lmm_prim(&mut queue, selection_mode_req(true));
        assert!(mle.selection_mode_manual, "manual mode set");
        queue = MessageQueue::new();

        mle.rx_tlmb_prim(&mut queue, sync_ind(901, 9999));
        assert!(mle.serving_cell.is_none(), "manual mode does not auto-camp");
        assert!(!mle.activate_confirmed, "no registration in manual mode without a camp");
    }

    /// Register-to-cell: an operator `CampOnCell` on a valid candidate arms a
    /// camp, tunes the carrier, and the next suitable SYNC+SYSINFO adopts the
    /// cell and confirms to MM (cl. 18.3.4.6) even in manual mode.
    #[test]
    fn test_camp_request_adopts_and_confirms() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_lmm_prim(&mut queue, selection_mode_req(true)); // manual
        queue = MessageQueue::new();

        mle.rx_lmm_prim(&mut queue, camp_req(SCAN_F1, true));
        assert!(mle.camp_armed, "camp armed");
        assert!(mle.camp_force_register, "force-register recorded");
        assert_eq!(tuned_carrier(&queue), Some(SCAN_F1), "tuned to the chosen carrier");
        queue = MessageQueue::new();

        // The cell on the camped carrier is adopted despite manual mode.
        mle.rx_tlmb_prim(&mut queue, sync_ind(901, 9999));
        assert!(mle.serving_cell.is_some(), "camped cell adopted");
        mle.rx_tlmb_prim(&mut queue, sysinfo_ind(1, 0b1));

        assert!(mle.activate_confirmed, "cell confirmed to MM");
        assert!(!mle.camp_armed, "camp disarmed after honouring it");
        assert_eq!(
            queue.iter().filter(|m| matches!(m.msg, SapMsgInner::LmmMleActivateConf(_))).count(),
            1,
            "LMM-ACTIVATE confirmation sent to MM"
        );
    }

    /// Register-to-cell: a camp request for a carrier that is not a codeplug
    /// candidate is rejected (no arm, no retune).
    #[test]
    fn test_camp_request_rejects_unknown_carrier() {
        let mut mle = ms_mle_scan();
        let mut queue = MessageQueue::new();

        mle.rx_lmm_prim(&mut queue, camp_req(123_000_000, true));

        assert!(!mle.camp_armed, "camp not armed for an unknown carrier");
        assert_eq!(tuned_carrier(&queue), None, "no retune for an unknown carrier");
    }
}
