use crate::{MessageQueue, TetraEntityTrait};
use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, Sap, unimplemented_log};
use tetra_saps::lcmc::{LcmcMleBreakInd, LcmcMleReopenInd, LcmcMleUnitdataInd};
use tetra_saps::lmm::{LmmMleActivateConf, LmmMleRssiInd, LmmMleUnitdataInd};
use tetra_saps::ltpd::LtpdMleUnitdataInd;
use tetra_saps::tla::TlaTlDataReqBl;
use tetra_saps::tlmc::{TlmcConfigureReq, TlmcValidAddress};
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
}

impl MleMs {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            config,
            serving_cell: None,
            activate_confirmed: false,
            out_of_service: false,
            serving_cell_rssi_dbfs: None,
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
                tracing::warn!("TM-UNITDATA for MM?"); // todo fixme find if ever used
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
                tracing::warn!("TM-UNITDATA for CMCE?"); // todo fixme find if ever used
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

    /// Radio-style cell suitability — allowed network (ETSI TS 100 392-2
    /// cl. 18.3.4 initial cell selection). The cell's network identity is its
    /// D-MLE-SYNC MCC/MNC (cl. 18.4.2.1); it is usable only when programmed as
    /// allowed (codeplug allowed-network list, **[impl policy]**; empty list =>
    /// home network only). The home network from `[net_info]` is always allowed.
    fn network_allowed(&self, mcc: u16, mnc: u16) -> bool {
        let cfg = self.config.config();
        cfg.codeplug.is_network_allowed(mcc, mnc, cfg.net.mcc, cfg.net.mnc)
    }

    /// Radio-style cell suitability — subscriber-class permission (ETSI
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
            _ => {
                panic!();
            }
        }
    }

    /// Handle the MS-internal serving-cell downlink monitoring indication from
    /// the PHY (ETSI TS 100 392-2 cl. 18.3.4.5.3 / 18.3.4.7).
    ///
    /// On a declared downlink failure the MLE declares the cell out of service,
    /// issues MLE-BREAK to the upper layers (CMCE), and re-arms cell selection
    /// so that when the downlink recovers the cell is re-selected and the
    /// LMM-ACTIVATE confirmation re-fired to MM — which then re-evaluates
    /// registration against cl. 18.3.4.7.1a (same cell/LA => no re-registration,
    /// NOTE 2; a changed LA/network => a location update). On recovery the MLE
    /// issues MLE-REOPEN.
    fn rx_tlmb_monitor_ind(&mut self, queue: &mut MessageQueue, downlink_available: bool) {
        match (downlink_available, self.out_of_service) {
            (false, false) => {
                // Serving-cell radio link failure: declare out of service.
                self.out_of_service = true;
                tracing::warn!(
                    "MLE: serving-cell downlink failure — declaring out of service, \
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
                tracing::info!("MLE: serving-cell downlink recovered — MLE-REOPEN (cl. 18.3.4.7)");
                queue.push_back(SapMsg {
                    sap: Sap::LcmcSap,
                    src: TetraEntity::Mle,
                    dest: TetraEntity::Cmce,
                    msg: SapMsgInner::LcmcMleReopenInd(LcmcMleReopenInd {}),
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
                     (cell class bitmap {:#06x}) — cell unsuitable, not confirming (cl. 18.4.2.2)",
                    mcc,
                    mnc,
                    pdu.subscriber_class,
                );
                return;
            }
            self.activate_confirmed = true;
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

        // Cell suitability (ETSI cl. 18.3.4): only camp on a cell whose network
        // is allowed. The network identity is the D-MLE-SYNC MCC/MNC
        // (cl. 18.4.2.1). This was previously a warning-only TODO ("proper
        // allowed-network handling is out of Phase 2 scope"); enforcing it is
        // the Phase-D radio-style behaviour — a radio does not camp on a cell
        // outside its programmed allowed-network set. A disallowed cell is
        // simply not selected: an already-camped (allowed) cell is retained,
        // and while scanning this rejection lets the scan advance to the next
        // candidate.
        if newly_selected && !self.network_allowed(pdu.mcc, pdu.mnc) {
            tracing::warn!(
                "MLE: cell MCC/MNC {}/{} is not an allowed network — not selecting (cl. 18.3.4)",
                pdu.mcc,
                pdu.mnc
            );
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
            _ => panic!(),
        }
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

    /// Suitability (cl. 18.3.4): a SYNC for the home network is selected — the
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
    /// rejected — no cell is selected and the MAC is not configured.
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
    /// NOT admit our class leaves the cell unconfirmed — no registration.
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
}
