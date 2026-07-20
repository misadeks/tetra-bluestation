use crate::{MessageQueue, TetraEntityTrait};
use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, Sap, unimplemented_log};
use tetra_saps::lcmc::LcmcMleUnitdataInd;
use tetra_saps::lmm::{LmmMleActivateConf, LmmMleUnitdataInd};
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
}

impl MleMs {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            config,
            serving_cell: None,
            activate_confirmed: false,
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

    fn rx_tlmb_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::trace!("rx_tlmb_prim");
        match message.msg {
            SapMsgInner::TlmbSysinfoInd(_) => {
                self.rx_tlmb_tl_sysinfo_ind(queue, message);
            }
            SapMsgInner::TlmbSyncInd(_) => {
                self.rx_tlmb_tl_sync_ind(queue, message);
            }
            _ => {
                panic!();
            }
        }
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

        let confirm = match self.serving_cell.as_mut() {
            Some(cell) => {
                let changed = cell.location_area != Some(pdu.location_area);
                cell.location_area = Some(pdu.location_area);
                cell.subscriber_class = Some(pdu.subscriber_class);
                cell.registration_required = Some(registration_required);
                let (mcc, mnc) = (cell.mcc, cell.mnc);
                if changed {
                    tracing::info!(
                        "MLE: serving cell SYSINFO adopted: LA={} subscriber_class={:#x} registration_required={}",
                        pdu.location_area,
                        pdu.subscriber_class,
                        registration_required,
                    );
                }
                // Confirm cell selection to MM exactly once per selected cell,
                // now that both the SYNC identity and the SYSINFO parameters are
                // known (cl. 18.3.4.6 completes selection; the confirmation is the
                // LMM-ACTIVATE confirm primitive, cl. 17.3.2).
                //
                // Additionally re-confirm whenever the location area of the
                // already-selected cell changes (cl. 16.4.1.0 / 18.3.4.7.1a
                // cond. 2): an LA change may require a roaming location update, so
                // MM must be re-notified to re-evaluate against its registered
                // area. Repeated identical SYSINFO (no LA change) is suppressed.
                if !self.activate_confirmed || changed {
                    Some((mcc, mnc, pdu.location_area, registration_required))
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

        if let Some((mcc, mnc, la, registration_required)) = confirm {
            self.activate_confirmed = true;
            self.send_mle_activate_conf(queue, mcc, mnc, la, registration_required);
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
    ) {
        tracing::info!(
            "MLE: cell selection complete, confirming to MM (MCC={}, MNC={}, LA={}, registration_required={})",
            mcc,
            mnc,
            la,
            registration_required
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

        // Adopt the cell identity, preserving any SYSINFO learned for the same cell.
        let (location_area, subscriber_class, registration_required) = if newly_selected {
            (None, None, None)
        } else {
            let c = self.serving_cell.as_ref().unwrap();
            (c.location_area, c.subscriber_class, c.registration_required)
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

        // Log if the cell does not match the configured home network. We do not
        // reject it here: proper allowed-network handling is out of Phase 2 scope
        // and must not be invented.
        let cfg = self.config.config();
        if pdu.mcc != cfg.net.mcc || pdu.mnc != cfg.net.mnc {
            tracing::warn!(
                "MLE: serving cell MCC/MNC {}/{} differs from configured {}/{}",
                pdu.mcc,
                pdu.mnc,
                cfg.net.mcc,
                cfg.net.mnc
            );
        }

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
            _ => panic!(),
        }
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
