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

/// Maximum valid 24-bit SSI (ETSI TS 100 392-2 cl. 7). A called/destination SSI
/// outside `1..=MAX_SSI` cannot be encoded into a Called party SSI element
/// (cl. 14.8.28) — attempting it panics the PDU serialiser — so such a value is
/// refused to the TN with a negative ack instead of crashing the stack.
const MAX_SSI: u32 = 0xFF_FFFF;

/// True when `ssi` cannot be carried as a Called party SSI (0 or > 24 bits).
fn ssi_out_of_range(ssi: u32) -> bool {
    ssi == 0 || ssi > MAX_SSI
}

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
            sds: SdsMsSubentity::new(config.clone(), telemetry.clone()),
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

    fn tnsds_ack(&self, handle: u32, accepted: bool, detail: Option<String>) {
        self.respond(ControlResponse::TnsdsAck { handle, accepted, detail });
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
            ControlCommand::TnccDtmf {
                handle,
                call_identifier,
                request,
            } => match self.cc.handle_tncc_dtmf(queue, call_identifier, &request) {
                Ok(()) => self.tncc_ack(handle, true, None),
                Err(detail) => self.tncc_ack(handle, false, Some(detail)),
            },
            // U-plane uplink speech (cl. 14.5.1.4): buffer the frame for the MAC
            // transmit scheduler. Fire-and-forget — no control response (the
            // frame rate makes per-frame acks impractical).
            ControlCommand::MsUplinkSpeech {
                call_identifier,
                frame_bits,
                data,
            } => {
                self.cc.push_uplink_speech(call_identifier, frame_bits, &data);
            }
            // TNSDS-UNITDATA request (Table 13.3, cl. 13.3.2.3): send SDS user
            // data uplink as a U-SDS-DATA PDU (cl. 14.7.2.8).
            ControlCommand::TnsdsUnitdata { handle, request } => {
                if ssi_out_of_range(request.called_party_ssi) {
                    self.tnsds_ack(
                        handle,
                        false,
                        Some(format!("called party SSI {} is out of range (cl. 7: a 24-bit SSI is 1..=16777215)", request.called_party_ssi)),
                    );
                } else {
                    self.sds.send_u_sds_data(
                        queue,
                        request.called_party_ssi,
                        request.called_party_is_group,
                        request.user_data,
                    );
                    self.tnsds_ack(handle, true, None);
                }
            }
            // TNSDS-STATUS request (Table 13.1, cl. 13.3.2.1): send a pre-coded
            // status uplink as a U-STATUS PDU (cl. 14.7.2.7).
            ControlCommand::TnsdsStatus { handle, request } => {
                if ssi_out_of_range(request.called_party_ssi) {
                    self.tnsds_ack(
                        handle,
                        false,
                        Some(format!("called party SSI {} is out of range (cl. 7: a 24-bit SSI is 1..=16777215)", request.called_party_ssi)),
                    );
                } else {
                    self.sds.send_u_status(
                        queue,
                        request.called_party_ssi,
                        request.called_party_is_group,
                        request.status_number,
                    );
                    self.tnsds_ack(handle, true, None);
                }
            }
            // TNSDS-UNITDATA request carrying an SDS-TL SDS-TRANSFER (cl. 29.4.2.4):
            // text/user message with a message reference + delivery reporting.
            ControlCommand::TnsdsSendMessage { handle, request } => {
                if ssi_out_of_range(request.called_party_ssi) {
                    self.tnsds_ack(
                        handle,
                        false,
                        Some(format!("called party SSI {} is out of range (cl. 7: a 24-bit SSI is 1..=16777215)", request.called_party_ssi)),
                    );
                } else {
                    self.sds.send_message(queue, &request);
                    self.tnsds_ack(handle, true, None);
                }
            }
            // TNSDS-REPORT request (Table 13.2, cl. 13.3.2.2): send an SDS-TL
            // delivery/read report (SDS-REPORT) for a received message.
            ControlCommand::TnsdsSendReport { handle, request } => {
                if ssi_out_of_range(request.called_party_ssi) {
                    self.tnsds_ack(
                        handle,
                        false,
                        Some(format!("report destination SSI {} is out of range (cl. 7: a 24-bit SSI is 1..=16777215)", request.called_party_ssi)),
                    );
                } else {
                    self.sds.send_report(queue, &request);
                    self.tnsds_ack(handle, true, None);
                }
            }
            // TNSDS-CANCEL: stop tracking a locally-outstanding SDS-TL message.
            ControlCommand::TnsdsCancel { handle, request } => {
                let found = self.sds.cancel(&request);
                self.tnsds_ack(handle, found, if found { None } else { Some("no outstanding message with that reference".to_string()) });
            }
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
        // Supply the uplink U-plane speech source while this MS holds the floor
        // (cl. 14.5.1.4). CC-MS owns the U-plane both directions; the MAC clocks
        // these frames out on the granted slot (cl. 23).
        self.cc.drive_uplink_source(queue);
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
                    self.cc.rx_downlink_traffic(ind.ts, ind.bfi, ind.usage_marker, ind.owner_ssi, &ind.data);
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
