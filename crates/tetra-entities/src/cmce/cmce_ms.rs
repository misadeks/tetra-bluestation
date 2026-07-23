use crate::net_control::{ControlCommand, ControlEndpoint, ControlResponse};
use crate::net_telemetry::channel::TelemetrySink;
use crate::{MessageQueue, TetraEntityTrait};
use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{Sap, TdmaTime};
use tetra_saps::{SapMsg, SapMsgInner};

use tetra_pdus::cmce::enums::cmce_pdu_type_dl::CmcePduTypeDl;

use super::subentities::cc_ms::CcMsSubentity;
use super::subentities::sds_ms::SdsMsSubentity;
use super::subentities::ss_ms::SsMsSubentity;

pub struct CmceMs {
    config: SharedConfig,

    sds: SdsMsSubentity,
    cc: CcMsSubentity,
    ss: SsMsSubentity,
    telemetry: Option<TelemetrySink>,
    control: Option<ControlEndpoint>,
}

impl CmceMs {
    pub fn new(config: SharedConfig, telemetry: Option<TelemetrySink>, control: Option<ControlEndpoint>) -> Self {
        Self {
            config: config.clone(),
            sds: SdsMsSubentity::new(),
            cc: CcMsSubentity::new_with_config(config.clone(), telemetry.clone()),
            ss: SsMsSubentity::new(),
            telemetry,
            control,
        }
    }

    pub fn rx_unitdata_ind(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_unitdata_ind");

        // Handle the incoming unit data indication
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

        match pdu_type {
            CmcePduTypeDl::DSdsData | CmcePduTypeDl::DStatus => {
                self.sds.route_rf_deliver(queue, message);
            }
            CmcePduTypeDl::DFacility => {
                self.ss.route_re_deliver(queue, message);
            }
            CmcePduTypeDl::DAlert
            | CmcePduTypeDl::DCallProceeding
            | CmcePduTypeDl::DCallRestore
            | CmcePduTypeDl::DConnect
            | CmcePduTypeDl::DConnectAcknowledge
            | CmcePduTypeDl::DDisconnect
            | CmcePduTypeDl::DInfo
            | CmcePduTypeDl::DRelease
            | CmcePduTypeDl::DSetup
            | CmcePduTypeDl::DTxCeased
            | CmcePduTypeDl::DTxContinue
            | CmcePduTypeDl::DTxGranted
            | CmcePduTypeDl::DTxInterrupt
            | CmcePduTypeDl::DTxWait => {
                self.cc.route_rd_deliver(queue, message);
            }
            _ => {
                panic!();
            }
        }
    }

    fn poll_control(&mut self, queue: &mut MessageQueue) {
        let mut commands = Vec::new();
        if let Some(cep) = &self.control {
            while let Some(cmd) = cep.try_recv() {
                commands.push(cmd);
            }
        }
        for cmd in commands {
            self.handle_control_command(queue, cmd);
        }
    }

    fn respond(&self, response: ControlResponse) {
        if let Some(cep) = &self.control {
            cep.respond(response);
        }
    }

    fn tncc_ack(&self, handle: u32, accepted: bool, detail: Option<String>) {
        self.respond(ControlResponse::TnccAck { handle, accepted, detail });
    }

    fn handle_control_command(&mut self, queue: &mut MessageQueue, cmd: ControlCommand) {
        match cmd {
            ControlCommand::TnccSetup { handle, request } => match self.cc.handle_tncc_setup_request(queue, &request) {
                Ok(()) => self.tncc_ack(handle, true, None),
                Err(detail) => self.tncc_ack(handle, false, Some(detail)),
            },
            ControlCommand::TnccSetupResponse {
                handle,
                call_identifier,
                response,
            } => {
                let accepted = self.cc.handle_tncc_setup_response(queue, call_identifier, &response);
                self.tncc_ack(
                    handle,
                    accepted,
                    if accepted {
                        None
                    } else {
                        Some("unknown call identifier".to_string())
                    },
                );
            }
            ControlCommand::TnccComplete {
                handle,
                call_identifier,
                request,
            } => {
                let accepted = self.cc.handle_tncc_complete(queue, call_identifier, &request);
                self.tncc_ack(
                    handle,
                    accepted,
                    if accepted {
                        None
                    } else {
                        Some("unknown call identifier".to_string())
                    },
                );
            }
            ControlCommand::TnccTx {
                handle,
                call_identifier,
                request,
            } => {
                let accepted = self.cc.handle_tncc_tx_request(queue, call_identifier, request);
                self.tncc_ack(
                    handle,
                    accepted,
                    if accepted {
                        None
                    } else {
                        Some("TX request rejected by CC state".to_string())
                    },
                );
            }
            ControlCommand::TnccRelease {
                handle,
                call_identifier,
                request,
            } => match self.cc.handle_tncc_release_request(queue, call_identifier, request) {
                Ok(()) => self.tncc_ack(handle, true, None),
                Err(detail) => self.tncc_ack(handle, false, Some(detail)),
            },
            other => tracing::warn!("CMCE(MS): received non-TNCC control command, dropping: {:?}", other),
        }
    }
}

impl TetraEntityTrait for CmceMs {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Cmce
    }

    fn set_config(&mut self, config: SharedConfig) {
        self.cc.set_config(config.clone());
        self.config = config;
    }

    fn tick_start(&mut self, queue: &mut MessageQueue, ts: TdmaTime) {
        self.poll_control(queue);
        self.cc.tick_start(queue, ts);
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);
        // tracing::debug!(ts=%message.dltime, "rx_prim: {:?}", message);

        match message.sap {
            // C-plane: MLE control/unitdata over the LCMC-SAP (cl. 17.3.3).
            Sap::LcmcSap => self.rx_lcmc_prim(queue, message),
            // U-plane: decoded downlink circuit-mode (TCH/S) traffic relayed up
            // from the MAC over the TMD-SAP (cl. 23). Delivered into the active
            // call's U-plane receive path.
            Sap::TmdSap => match message.msg {
                SapMsgInner::TmdCircuitDataInd(ind) => {
                    self.cc.rx_downlink_traffic(ind.ts, &ind.data);
                }
                _ => panic!("CMCE-MS: unexpected message on TMD-SAP: {:?}", message.msg),
            },
            _ => panic!("CMCE-MS: unexpected SAP {:?}", message.sap),
        }
    }
}

impl CmceMs {
    fn rx_lcmc_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        match message.msg {
            SapMsgInner::LcmcMleUnitdataInd(_) => {
                self.rx_unitdata_ind(queue, message);
            }
            SapMsgInner::LcmcMleBreakInd(_) => {
                // MLE-BREAK (cl. 17.3.3): communication resources temporarily
                // unavailable. CC-MS switches U-plane off and enters restoration
                // handling per cl. 14.5.1.4.2 e / cl. 14.5.2.2.4.
                tracing::warn!("CMCE: MLE-BREAK — communication resources unavailable");
                self.cc.handle_break(queue);
            }
            SapMsgInner::LcmcMleReopenInd(_) => {
                // MLE-REOPEN (cl. 17.3.3): cl. 14.5.2.2.4 treats this as
                // unsuccessful call restoration for active group calls.
                tracing::info!("CMCE: MLE-REOPEN — communication resources available");
                self.cc.handle_reopen(queue);
            }
            _ => {
                panic!();
            }
        }
    }
}
