use crate::config::stack_config::SharedConfig;
use crate::common::messagerouter::MessageQueue;
use crate::common::ms_route_registry::{self, MsRoute};
use crate::entities::cmce::enums::cmce_pdu_type_ul::CmcePduTypeUl;
use crate::saps::sapmsg::{SapMsg, SapMsgInner};
use crate::common::tetra_entities::TetraEntity;
use crate::entities::TetraEntityTrait;
use crate::common::tetra_common::Sap;
use crate::unimplemented_log;

use super::subentities::cc::CcSubentity;
use super::subentities::sds::SdsSubentity;
use super::subentities::ss::SsSubentity;

pub struct CmceBs {
    config: SharedConfig,
    
    sds: SdsSubentity,
    cc: CcSubentity,
    ss: SsSubentity,
}

impl CmceBs {
    pub fn new(config: SharedConfig) -> Self {
        Self { 
            config,
            sds: SdsSubentity::new(),
            cc: CcSubentity::new(),
            ss: SsSubentity::new(),
         }
    }

    pub fn rx_lcmc_mle_unitdata_ind(&mut self, _queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_lcmc_mle_unitdata_ind");
        
        // Handle the incoming unit data indication
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else { panic!(); };
        let Some(bits) = prim.sdu.peek_bits(5) else {
            tracing::warn!("insufficient bits: {}", prim.sdu.dump_bin());
            return;
        };
        let Ok(pdu_type) = CmcePduTypeUl::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, prim.sdu.dump_bin());
            return;
        };

        match pdu_type {
            CmcePduTypeUl::UAlert =>
                unimplemented_log!("UAlert"),
            CmcePduTypeUl::UConnect =>
                unimplemented_log!("UConnect"),
            CmcePduTypeUl::UDisconnect =>
                unimplemented_log!("UDisconnect"),
            CmcePduTypeUl::UInfo =>
                unimplemented_log!("UInfo"),
            CmcePduTypeUl::URelease =>
                unimplemented_log!("URelease"),
            CmcePduTypeUl::USetup =>
                unimplemented_log!("USetup"),
            CmcePduTypeUl::UStatus =>
                self.sds.route_rf_deliver(_queue, message),
            CmcePduTypeUl::UTxCeased =>
                unimplemented_log!("UTxCeased"),
            CmcePduTypeUl::UTxDemand =>
                unimplemented_log!("UTxDemand"),
            CmcePduTypeUl::UCallRestore =>
                unimplemented_log!("UCallRestore"),
            CmcePduTypeUl::USdsData =>
                self.sds.route_rf_deliver(_queue, message),
            CmcePduTypeUl::UFacility =>
                unimplemented_log!("UFacility"),
            CmcePduTypeUl::CmceFunctionNotSupported =>
                unimplemented_log!("CmceFunctionNotSupported"),
        };
    }
}

impl TetraEntityTrait for CmceBs {

    fn entity(&self) -> TetraEntity {
        TetraEntity::Cmce
    }

    fn set_config(&mut self, config: SharedConfig) {
        self.config = config;
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        
        tracing::debug!("rx_prim: {:?}", message);
        
        // There is only one SAP for CMCE
        assert!(message.sap == Sap::LcmcSap);

        match message.msg {
            SapMsgInner::LcmcMleUnitdataInd(_) => {
                self.rx_lcmc_mle_unitdata_ind(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }
}
