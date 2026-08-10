//! Command worker thread — receives commands from a remote server via a
//! pluggable network transport, dispatches them to the appropriate entity
//! through per-entity [`CommandDispatcher`] links, collects
//! [`CommandResponse`]s, and sends them back over the network.
//!
//! - decodes inbound messages as [`Command`]s, interprets how command must be handled, dispatches via per-entity links
//! - collects [`CommandResponse`]s from entities and sends them back

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tetra_core::tetra_entities::TetraEntity;

use crate::{
    net_control::{
        channel::CommandDispatcher,
        codec::ControlCodecJson,
        commands::{ControlCommand, ControlResponse},
    },
    network::transports::NetworkTransport,
};

/// How long to block on transport receive before running a maintenance cycle.
const POLL_TIMEOUT: Duration = Duration::from_millis(500);

/// How long to wait between reconnection attempts when the transport is down.
/// Kept short so the radio reconnects to the UI within ~1 s of it becoming
/// available (the operator UI may start after, or restart independently of, the
/// radio). The first attempt is made immediately when the worker starts.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

pub struct ControlWorker<T: NetworkTransport> {
    dispatchers: HashMap<TetraEntity, CommandDispatcher>,
    transport: T,
    connected: bool,
    last_connect_attempt: Option<Instant>,
    /// Whether the current "down" state has already been logged at WARN. Lets us
    /// warn once on a disconnect but retry quietly (DEBUG) every second after,
    /// so a UI that is simply not up yet does not flood the journal.
    down_logged: bool,
}

impl<T: NetworkTransport> ControlWorker<T> {
    pub fn new(dispatchers: HashMap<TetraEntity, CommandDispatcher>, transport: T) -> Self {
        Self {
            dispatchers,
            transport,
            connected: false,
            last_connect_attempt: None,
            down_logged: false,
        }
    }

    pub fn run(&mut self) {
        tracing::debug!("Control worker started");
        self.try_connect();

        loop {
            if self.connected {
                self.poll_commands();
                self.collect_responses();
            } else {
                // Not connected — sleep briefly to avoid busy-spinning
                std::thread::sleep(POLL_TIMEOUT);
            }

            // Detect fresh disconnection
            if !self.transport.is_connected() && self.connected {
                tracing::warn!("Control transport disconnected");
                self.connected = false;
            }

            // Periodically retry connection when disconnected
            if !self.connected {
                let should_retry = match self.last_connect_attempt {
                    Some(last) => last.elapsed() >= RECONNECT_DELAY,
                    None => true,
                };
                if should_retry {
                    self.try_connect();
                }
            }
        }
    }

    /// Poll the transport for inbound commands, decode them, and dispatch
    /// to the appropriate entity through its [`CommandDispatcher`] link.
    fn poll_commands(&mut self) {
        let msgs = self.transport.receive_reliable();

        for msg in msgs {
            let codec = ControlCodecJson;
            match codec.decode_command(&msg.payload) {
                Ok(command) => {
                    tracing::debug!("command received: {:?}", command);
                    self.dispatch_command(command);
                }
                Err(e) => {
                    tracing::warn!("Command: failed to decode inbound message ({} bytes): {}", msg.payload.len(), e);
                }
            }
        }
    }

    /// Route a command to the correct entity's dispatcher.
    /// Override this mapping as real command variants are added.
    fn dispatch_command(&self, command: ControlCommand) {
        let target = Self::route_control_command(&command);
        match self.dispatchers.get(&target) {
            Some(dispatcher) => {
                tracing::debug!("dispatching command to {:?}", target);
                dispatcher.send(command);
            }
            None => {
                tracing::warn!("no dispatcher registered for {:?}, dropping command", target);
            }
        }
    }

    /// Determine which entity should handle a given command.
    /// Placeholder routing — will be extended as real commands are defined.
    fn route_control_command(command: &ControlCommand) -> TetraEntity {
        match command {
            ControlCommand::SendSds { .. } => TetraEntity::Cmce,
            ControlCommand::CommandA { .. } => TetraEntity::Mm,
            ControlCommand::TestCmdB { .. } => TetraEntity::Cmce,
            // TNMM-SAP requests (cl. 15.3) are handled by Mobility Management.
            ControlCommand::TnmmRegistration { .. }
            | ControlCommand::TnmmDeregistration { .. }
            | ControlCommand::TnmmAttachDetachGroupIdentity { .. }
            | ControlCommand::TnmmStatus { .. }
            | ControlCommand::TnmmEnergySaving { .. } => TetraEntity::Mm,
            // TNCC-SAP requests (cl. 11.3) are handled by CMCE call control.
            ControlCommand::TnccSetup { .. }
            | ControlCommand::TnccSetupResponse { .. }
            | ControlCommand::TnccComplete { .. }
            | ControlCommand::TnccTx { .. }
            | ControlCommand::TnccDtmf { .. }
            | ControlCommand::TnccRelease { .. }
            // U-plane uplink speech (cl. 14.5.1.4) is owned by CMCE CC-MS.
            | ControlCommand::MsUplinkSpeech { .. }
            // TNSDS-SAP requests (cl. 13.3) are handled by CMCE SDS.
            | ControlCommand::TnsdsUnitdata { .. }
            | ControlCommand::TnsdsStatus { .. }
            | ControlCommand::TnsdsSendMessage { .. }
            | ControlCommand::TnsdsSendReport { .. }
            | ControlCommand::TnsdsCancel { .. } => TetraEntity::Cmce,
            // Management / provisioning (Plane B, non-standard) is served by MM,
            // the single writer of MS runtime state and config-apply.
            ControlCommand::Management(_) => TetraEntity::Mm,
        }
    }

    /// Drain pending responses from all entity dispatchers and send them
    /// back to the command server.
    fn collect_responses(&mut self) {
        let responses: Vec<ControlResponse> = self.dispatchers.values().flat_map(|d| d.try_recv_responses()).collect();

        for response in &responses {
            tracing::debug!("response collected: {:?}", response);
            self.send_response(response);
        }
    }

    fn send_response(&mut self, response: &ControlResponse) {
        if !self.connected {
            return;
        }

        let codec = ControlCodecJson;
        let payload = codec.encode_response(response);
        if let Err(e) = self.transport.send_reliable(&payload) {
            tracing::warn!("Control transport send failed: {}, will reconnect", e);
            self.connected = false;
            self.try_connect();
        }
    }

    fn try_connect(&mut self) {
        self.last_connect_attempt = Some(Instant::now());
        match self.transport.connect() {
            Ok(()) => {
                tracing::info!("Control transport connected");
                self.connected = true;
                self.down_logged = false;
            }
            Err(e) => {
                // Warn once per down-period, then retry quietly so a UI that is
                // not up yet does not flood the journal every second.
                if !self.down_logged {
                    tracing::warn!("Control transport connection failed: {}; retrying every {:?} until the UI is available", e, RECONNECT_DELAY);
                    self.down_logged = true;
                } else {
                    tracing::debug!("Control transport still unavailable: {}", e);
                }
                self.connected = false;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use tetra_core::debug::setup_logging_verbose;

    use super::*;
    use crate::net_control::channel::make_control_link;
    use crate::network::transports::mock::MockTransport;

    #[test]
    fn test_route_command_a_to_mm() {
        let target = ControlWorker::<MockTransport>::route_control_command(&ControlCommand::CommandA { handle: 1, parameter: 1 });
        assert_eq!(target, TetraEntity::Mm);
    }

    #[test]
    fn test_route_command_b_to_cmce() {
        let target = ControlWorker::<MockTransport>::route_control_command(&ControlCommand::TestCmdB {
            handle: 2,
            source_ssi: 12345,
            is_group: false,
            payload: vec![],
        });
        assert_eq!(target, TetraEntity::Cmce);
    }

    /// All TNMM-SAP requests (cl. 15.3) are routed to Mobility Management.
    #[test]
    fn test_route_tnmm_requests_to_mm() {
        use crate::tnmm::{
            RegistrationType, TnmmDeregistrationRequest, TnmmEnergySavingRequest, TnmmRegistrationRequest, TnmmStatusRequest,
        };
        let reg = ControlCommand::TnmmRegistration {
            handle: 1,
            request: Box::new(TnmmRegistrationRequest {
                registration_type: RegistrationType::RegistrationToIndicatedCell,
                required_cell_type_list: None,
                preferred_cell_type_list: None,
                preferred_la_list: None,
                preferred_mcc_list: None,
                preferred_mnc_list: None,
                issi: 1,
                mcc_of_issi: 1,
                mnc_of_issi: 1,
                energy_economy_mode: None,
                group_identity_request: None,
                group_identity_attach_detach_mode: None,
            }),
        };
        assert_eq!(ControlWorker::<MockTransport>::route_control_command(&reg), TetraEntity::Mm);

        let dereg = ControlCommand::TnmmDeregistration {
            handle: 2,
            request: TnmmDeregistrationRequest {
                issi: None,
                mcc: None,
                mnc: None,
            },
        };
        assert_eq!(ControlWorker::<MockTransport>::route_control_command(&dereg), TetraEntity::Mm);

        let status = ControlCommand::TnmmStatus {
            handle: 3,
            request: TnmmStatusRequest {
                direct_mode: None,
                dual_watch: None,
                energy_economy_mode: None,
            },
        };
        assert_eq!(ControlWorker::<MockTransport>::route_control_command(&status), TetraEntity::Mm);

        let energy = ControlCommand::TnmmEnergySaving {
            handle: 4,
            request: TnmmEnergySavingRequest {
                energy_economy_mode: crate::tnmm::EnergyEconomyMode::StayAlive,
            },
        };
        assert_eq!(ControlWorker::<MockTransport>::route_control_command(&energy), TetraEntity::Mm);
    }

    #[test]
    fn test_route_tncc_requests_to_cmce() {
        use tetra_saps::tncc as t;
        let basic = t::TnccBasicServiceInformation {
            circuit_mode_service: t::CircuitModeService::SpeechService,
            communication_type: t::CommunicationType::PointToMultipoint,
            data_service: None,
            data_call_capacity: None,
            encryption_flag: t::EncryptionFlag::ClearEndToEndTransmission,
            speech_service: Some(t::SpeechService::TetraEncodedOneTimeslotSpeech),
        };
        let setup = ControlCommand::TnccSetup {
            handle: 1,
            request: Box::new(t::TnccSetupRequest {
                access_priority: None,
                area_selection: None,
                basic_service_information: basic,
                call_priority: t::CallPriority::PriorityNotDefined,
                called_party_type_identifier: t::CalledPartyTypeIdentifier::Ssi,
                called_party_sna: None,
                called_party_ssi: Some(91),
                called_party_extension: None,
                external_subscriber_number_called: None,
                clir_control: None,
                hook_method_selection: t::HookMethodSelection::NoHookSignallingDirectThroughConnect,
                request_to_transmit_send_data: t::RequestToTransmitSendData::RequestToTransmitSendData,
                simplex_duplex_selection: t::SimplexDuplexSelection::SimplexOperation,
                traffic_stealing: None,
            }),
        };
        assert_eq!(ControlWorker::<MockTransport>::route_control_command(&setup), TetraEntity::Cmce);
    }

    /// Management commands (Plane B, non-standard) are routed to Mobility
    /// Management, the single writer of MS runtime state.
    #[test]
    fn test_route_management_to_mm() {
        use crate::management::ManagementCommand;
        let cmd = ControlCommand::Management(ManagementCommand::GetState { handle: 9 });
        assert_eq!(ControlWorker::<MockTransport>::route_control_command(&cmd), TetraEntity::Mm);
    }

    #[test]
    fn test_worker_dispatches_command_and_collects_response() {
        setup_logging_verbose();

        // Set up per-entity links
        let (mm_dispatcher, mm_endpoint) = make_control_link();
        let mut dispatchers = HashMap::new();
        dispatchers.insert(TetraEntity::Mm, mm_dispatcher);

        // Pre-load a CommandA (routed to Mm) into the mock transport
        let codec = ControlCodecJson;
        let cmd = ControlCommand::CommandA { handle: 1, parameter: 99 };
        let payload = codec.encode_command(&cmd);

        let mut mock = MockTransport::new();
        mock.push_inbound(payload);

        let handle = std::thread::spawn(move || {
            let mut worker = ControlWorker::new(dispatchers, mock);
            worker.try_connect();
            worker.poll_commands();

            // Simulate entity processing: endpoint receives command, sends response
            let received = mm_endpoint.try_recv().expect("Mm should receive CommandA");
            assert!(matches!(received, ControlCommand::CommandA { handle: 1, parameter: 99 }));
            mm_endpoint.respond(ControlResponse::CommandAResponse { handle: 1, result: 99 });

            // Worker collects responses and sends them back over the transport
            worker.collect_responses();

            // Verify the response was sent through the transport
            assert_eq!(worker.transport.sent_payloads().len(), 1);
            let sent = &worker.transport.sent_payloads()[0];
            let decoded = codec.decode_response(sent).unwrap();
            assert!(matches!(decoded, ControlResponse::CommandAResponse { handle: 1, result: 99 }));
        });

        handle.join().expect("command worker panicked");
    }

    #[test]
    fn test_worker_drops_command_without_dispatcher() {
        setup_logging_verbose();

        // No dispatchers registered — command should be dropped with a warning
        let dispatchers = HashMap::new();

        let codec = ControlCodecJson;
        let cmd = ControlCommand::CommandA { handle: 1, parameter: 1 };
        let payload = codec.encode_command(&cmd);

        let mut mock = MockTransport::new();
        mock.push_inbound(payload);

        let handle = std::thread::spawn(move || {
            let mut worker = ControlWorker::new(dispatchers, mock);
            worker.try_connect();
            worker.poll_commands(); // should log warning and not panic
        });

        handle.join().expect("command worker panicked");
    }
}
