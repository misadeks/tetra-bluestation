use crate::net_control::{ControlCommand, ControlEndpoint, ControlResponse};
use crate::net_telemetry::TelemetrySink;
use crate::{MessageQueue, TetraEntityTrait};
use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{Sap, TdmaTime, unimplemented_log};
use tetra_saps::{SapMsg, SapMsgInner};

use super::components::pc_bs::{ControlRoute, LcmcRoute, PcBs};
use super::subentities::cc_bs::CcBsSubentity;
use super::subentities::sds_bs::SdsBsSubentity;
use super::subentities::ss_bs::SsBsSubentity;

pub struct CmceBs {
    config: SharedConfig,
    telemetry: Option<TelemetrySink>,
    control: Option<ControlEndpoint>,

    pc: PcBs,
    cc: CcBsSubentity,
    sds: SdsBsSubentity,
    ss: SsBsSubentity,
}

impl CmceBs {
    pub fn new(config: SharedConfig, telemetry: Option<TelemetrySink>, control: Option<ControlEndpoint>) -> Self {
        Self {
            config: config.clone(),
            telemetry,
            control,
            pc: PcBs::new(),
            sds: SdsBsSubentity::new(config.clone()),
            cc: CcBsSubentity::new(config.clone()),
            ss: SsBsSubentity::new(),
        }
    }

    pub fn rx_lcmc_mle_unitdata_ind(&mut self, _queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_lcmc_mle_unitdata_ind");

        let Some(route) = self.pc.route_lcmc_unitdata_ind(&mut message) else {
            return;
        };

        match route {
            LcmcRoute::CcRd => {
                self.cc.route_rd_deliver(_queue, message);
            }
            LcmcRoute::SdsStatus => {
                self.sds.route_status_deliver(_queue, message);
            }
            LcmcRoute::SdsRf => {
                self.sds.route_rf_deliver(_queue, message);
            }
            LcmcRoute::SsRe => {
                self.ss.route_re_deliver(_queue, message);
            }
            LcmcRoute::Unsupported(pdu_type) => {
                unimplemented_log!("{:?}", pdu_type);
            }
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

    fn tick_start(&mut self, queue: &mut MessageQueue, ts: TdmaTime) {
        // Propagate tick to subentities
        self.cc.tick_start(queue, ts);

        // Process incoming control commands, if control link is enabled
        if let Some(cep) = &self.control {
            while let Some(cmd) = cep.try_recv() {
                match cmd {
                    ControlCommand::SendSds { handle, .. } => {
                        let success = self.sds.rx_sds_from_control(queue, cmd);
                        let response = ControlResponse::SendSdsResponse { handle, success };
                        cep.respond(response);
                    }
                    _ => {
                        panic!("Unsupported command {:?}", cmd);
                    }
                }
            }
        }
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);
        // tracing::debug!(ts=%message.dltime, "rx_prim: {:?}", message);

        match message.sap {
            Sap::LcmcSap => match message.msg {
                SapMsgInner::LcmcMleUnitdataInd(_) => {
                    self.rx_lcmc_mle_unitdata_ind(queue, message);
                }
                _ => {
                    panic!("Unexpected message on LcmcSap: {:?}", message.msg);
                }
            },
            Sap::Control => match self.pc.route_control(&message) {
                ControlRoute::CcRa => {
                    self.cc.rx_call_control(queue, message);
                }
                ControlRoute::CcSubscriberUpdate => {
                    let SapMsgInner::MmSubscriberUpdate(update) = message.msg else {
                        unreachable!();
                    };
                    self.cc.handle_subscriber_update(queue, update);
                }
                ControlRoute::SdsRc => {
                    self.sds.rx_sds_from_brew(queue, message);
                }
                ControlRoute::Unsupported => {
                    panic!("Unexpected control message: {:?}", message.msg);
                }
            },
            _ => {
                panic!("Unexpected SAP: {:?}", message.sap);
            }
        }
    }
}
