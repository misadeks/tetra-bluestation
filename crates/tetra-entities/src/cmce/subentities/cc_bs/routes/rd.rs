use super::*;

impl CcBsSubentity {
    pub fn route_rd_deliver(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("route_rd_deliver");

        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            tracing::warn!("CMCE CC ingress received non-LCMC unitdata indication: {:?}", message.msg);
            return;
        };
        let Some(bits) = prim.sdu.peek_bits(5) else {
            tracing::warn!("insufficient bits: {}", prim.sdu.dump_bin());
            return;
        };
        let Ok(pdu_type) = CmcePduTypeUl::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, prim.sdu.dump_bin());
            return;
        };

        match pdu_type {
            CmcePduTypeUl::USetup => self.rx_u_setup(queue, message),
            CmcePduTypeUl::UTxCeased => self.rx_u_tx_ceased(queue, message),
            CmcePduTypeUl::UTxDemand => self.rx_u_tx_demand(queue, message),
            CmcePduTypeUl::URelease => self.rx_u_release(queue, message),
            CmcePduTypeUl::UDisconnect => self.rx_u_disconnect(queue, message),
            CmcePduTypeUl::UAlert => self.rx_u_alert(queue, message),
            CmcePduTypeUl::UConnect => self.rx_u_connect(queue, message),
            CmcePduTypeUl::UInfo => self.rx_u_info(queue, message),
            CmcePduTypeUl::UCallRestore => self.rx_u_call_restore(queue, message),
            CmcePduTypeUl::UStatus => {
                tracing::warn!("CMCE CC received U-STATUS on rd route; PC should route it to SDS");
            }
            _ => {
                tracing::warn!("CMCE CC ingress received unsupported UL PDU type {}", pdu_type);
            }
        }
    }

    pub(super) fn rx_u_setup(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_u_setup: {:?}", message);
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            tracing::warn!("CMCE CC rx_u_setup received non-LCMC unitdata indication");
            return;
        };
        let calling_party = prim.received_tetra_address;

        let pdu = match USetup::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- U-SETUP {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing U-SETUP: {:?} {}", e, prim.sdu.dump_bin());
                return;
            }
        };

        self.fsm_on_u_setup(queue, &message, &pdu, calling_party);
    }

    pub(super) fn rx_u_tx_ceased(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            tracing::warn!("CMCE CC rx_u_tx_ceased received non-LCMC unitdata indication");
            return;
        };

        let sender = prim.received_tetra_address;
        let pdu = match UTxCeased::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- U-TX CEASED {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing U-TX CEASED: {:?}", e);
                return;
            }
        };

        self.fsm_on_u_tx_ceased(queue, sender, pdu);
    }

    pub(super) fn rx_u_tx_demand(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            tracing::warn!("CMCE CC rx_u_tx_demand received non-LCMC unitdata indication");
            return;
        };

        let requesting_party = prim.received_tetra_address;
        let pdu = match UTxDemand::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- U-TX DEMAND {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing U-TX DEMAND: {:?}", e);
                return;
            }
        };

        self.fsm_on_u_tx_demand(queue, requesting_party, pdu);
    }

    pub(super) fn rx_u_release(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            tracing::warn!("CMCE CC rx_u_release received non-LCMC unitdata indication");
            return;
        };

        let sender = prim.received_tetra_address;
        let handle = prim.handle;
        let link_id = prim.link_id;
        let endpoint_id = prim.endpoint_id;
        let pdu = match URelease::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- U-RELEASE {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing U-RELEASE: {:?}", e);
                return;
            }
        };

        self.fsm_on_u_release(queue, sender, handle, link_id, endpoint_id, pdu);
    }

    pub(super) fn rx_u_disconnect(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            tracing::warn!("CMCE CC rx_u_disconnect received non-LCMC unitdata indication");
            return;
        };

        let sender = prim.received_tetra_address;
        let ul_handle = prim.handle;
        let ul_link_id = prim.link_id;
        let ul_endpoint_id = prim.endpoint_id;

        let pdu = match UDisconnect::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- U-DISCONNECT {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing U-DISCONNECT: {:?}", e);
                return;
            }
        };

        self.fsm_on_u_disconnect(queue, sender, ul_handle, ul_link_id, ul_endpoint_id, pdu);
    }

    pub(super) fn rx_u_alert(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            tracing::warn!("CMCE CC rx_u_alert received non-LCMC unitdata indication");
            return;
        };

        let pdu = match UAlert::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- U-ALERT {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing U-ALERT: {:?}", e);
                return;
            }
        };

        self.fsm_on_u_alert(queue, prim.received_tetra_address, prim.handle, prim.link_id, prim.endpoint_id, pdu);
    }

    /// Handle U-CONNECT for an individual call.
    pub(super) fn rx_u_connect(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            tracing::warn!("CMCE CC rx_u_connect received non-LCMC unitdata indication");
            return;
        };

        let pdu = match UConnect::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- U-CONNECT {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing U-CONNECT: {:?}", e);
                return;
            }
        };

        self.fsm_on_u_connect(queue, prim.received_tetra_address, prim.handle, prim.link_id, prim.endpoint_id, pdu);
    }

    pub(super) fn rx_u_info(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            tracing::warn!("CMCE CC rx_u_info received non-LCMC unitdata indication");
            return;
        };

        let sender = prim.received_tetra_address;
        let handle = prim.handle;
        let link_id = prim.link_id;
        let endpoint_id = prim.endpoint_id;

        let pdu = match UInfo::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- U-INFO {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing U-INFO: {:?}", e);
                return;
            }
        };

        self.fsm_on_u_info(queue, sender, handle, link_id, endpoint_id, pdu);
    }

    pub(super) fn rx_u_call_restore(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            tracing::warn!("CMCE CC rx_u_call_restore received non-LCMC unitdata indication");
            return;
        };

        let sender = prim.received_tetra_address;
        let handle = prim.handle;
        let link_id = prim.link_id;
        let endpoint_id = prim.endpoint_id;

        let pdu = match UCallRestore::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- U-CALL RESTORE {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing U-CALL RESTORE: {:?}", e);
                return;
            }
        };

        self.fsm_on_u_call_restore(queue, sender, handle, link_id, endpoint_id, pdu);
    }
}
