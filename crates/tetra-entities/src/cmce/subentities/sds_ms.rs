use tetra_pdus::cmce::{
    enums::cmce_pdu_type_dl::CmcePduTypeDl,
    pdus::{d_sds_data::DSdsData, d_status::DStatus},
};
use tetra_saps::{SapMsg, SapMsgInner};

use crate::MessageQueue;

/// Clause 13 Short Data Service CMCE sub-entity
pub struct SdsMsSubentity {}

impl SdsMsSubentity {
    /// Create a new instance of the SdsSubentity
    pub fn new() -> Self {
        SdsMsSubentity {}
    }

    pub fn rx_sds_data(&mut self, _queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_sds_data");

        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            panic!();
        };
        let pdu = match DSdsData::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("Received DSdsData: {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing DSdsData: {:?} {}", e, prim.sdu.dump_bin());
                return;
            }
        };

        // ETSI TS 100 392-2 cl. 14.7.1.10 delivers CMCE SDS user data to the
        // MS. Phase 1 has no TNSDS/UI SAP, so the handoff is intentionally the
        // internal log/state seam; Phase 3 will expose it without changing the
        // wire decode path. SDS-TL interpretation, when user data is Type 4, is
        // defined in cl. 29 and remains above this CMCE receive decode.
        tracing::info!(
            calling_party = ?pdu.calling_party_address_ssi,
            data = ?pdu.user_defined_data,
            "CMCE-MS: received D-SDS-DATA"
        );
    }

    pub fn rx_status(&mut self, _queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_status");

        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            panic!();
        };
        let pdu = match DStatus::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => pdu,
            Err(e) => {
                tracing::warn!("Failed parsing DStatus: {:?} {}", e, prim.sdu.dump_bin());
                return;
            }
        };

        // ETSI TS 100 392-2 cl. 14.7.1.11 / cl. 14.8.34. Full TNSDS exposure is
        // Phase 3; Phase 1 decodes and records the receive event internally.
        tracing::info!(
            calling_party = ?pdu.calling_party_address_ssi,
            status = ?pdu.pre_coded_status,
            "CMCE-MS: received D-STATUS"
        );
    }

    /// Poor man's rx_prim, as this is a subcomponent and not governed by the MessageRouter
    /// If need be, we can deviate from the standard's subentity ranking and make this a full-fledged component
    /// See Figure 14.2: Block view of CMCE-MS
    pub fn route_rf_deliver(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("route_rf_deliver");

        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            panic!();
        };
        let Some(bits) = prim.sdu.peek_bits(5) else {
            tracing::warn!("insufficient bits: {}", prim.sdu.dump_bin());
            return;
        };

        let Ok(pdu_type) = CmcePduTypeDl::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, prim.sdu.dump_bin());
            return;
        };

        // TODO FIXME: Besides these PDUs, we can also receive several signals (BUSY ind, CLOSE ind, etc)
        match pdu_type {
            CmcePduTypeDl::DSdsData => {
                self.rx_sds_data(queue, message);
            }
            CmcePduTypeDl::DStatus => {
                self.rx_status(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }
}
