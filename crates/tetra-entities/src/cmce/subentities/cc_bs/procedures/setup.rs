use super::*;

impl CcBsSubentity {
    fn reject_setup_request(
        &mut self,
        queue: &mut MessageQueue,
        message: &SapMsg,
        target: TetraAddress,
        cause: DisconnectCause,
        reason: &str,
    ) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &message.msg else {
            tracing::warn!("CMCE: cannot reject setup on non-LCMC message: {}", reason);
            return;
        };

        let call_id = self.circuits.get_next_call_id();
        tracing::info!(
            "CMCE: rejecting U-SETUP from ISSI {} call_id={} cause={} ({})",
            target.ssi,
            call_id,
            cause,
            reason
        );
        let sdu = Self::build_d_release(call_id, cause);
        let msg = Self::build_sapmsg_direct(sdu, self.dltime, target, prim.handle, prim.link_id, prim.endpoint_id);
        queue.push_back(msg);
    }

    fn setup_collision_cause(&self, calling_issi: u32, called_issi: Option<u32>) -> Option<(u16, IndividualCallState, DisconnectCause)> {
        if let Some((call_id, state)) = self.find_individual_call_by_issi(calling_issi) {
            return Some((call_id, state, DisconnectCause::ConcurrentSetUpNotSupported));
        }

        let called_issi = called_issi?;
        self.find_individual_call_by_issi(called_issi)
            .map(|(call_id, state)| (call_id, state, DisconnectCause::CalledPartyBusy))
    }

    fn abort_individual_setup(
        &mut self,
        queue: &mut MessageQueue,
        call_id: u16,
        target: TetraAddress,
        handle: u32,
        link_id: u32,
        endpoint_id: u32,
        allocated_timeslots: &[u8],
        cause: DisconnectCause,
    ) {
        tracing::info!("CMCE: aborting unsuccessful individual setup call_id={} cause={}", call_id, cause);
        let sdu = Self::build_d_release(call_id, cause);
        queue.push_back(Self::build_sapmsg_direct(sdu, self.dltime, target, handle, link_id, endpoint_id));

        self.cached_setups.remove(&call_id);
        self.individual_calls.remove(&call_id);

        let mut released = Vec::new();
        for &ts in allocated_timeslots {
            if released.contains(&ts) {
                continue;
            }
            released.push(ts);

            if let Ok(circuit) = self.circuits.close_circuit(Direction::Both, ts) {
                Self::signal_umac_circuit_close(queue, circuit, self.dltime);
            }
            self.release_timeslot(ts);
        }
    }

    /// Handle U-SETUP for group calls (non-P2P communication types).
    pub(in crate::cmce::subentities::cc_bs) fn fsm_on_u_setup_group(
        &mut self,
        queue: &mut MessageQueue,
        message: &SapMsg,
        pdu: &USetup,
        calling_party: TetraAddress,
    ) {
        // Get destination GSSI (called party)
        let Some(dest_gssi) = pdu.called_party_ssi else {
            tracing::warn!("U-SETUP without called_party_ssi, ignoring");
            return;
        };
        let dest_gssi = dest_gssi as u32;
        let dest_addr = TetraAddress::new(dest_gssi, SsiType::Gssi);

        if !self.has_listener(dest_gssi) {
            tracing::info!(
                "CMCE: rejecting U-SETUP from issi={} to gssi={} (no listeners)",
                calling_party.ssi,
                dest_gssi
            );
            return;
        }

        // Allocate circuit (DL+UL for group call)
        let circuit = match {
            let mut state = self.config.state_write();
            self.circuits.allocate_circuit_with_allocator(
                Direction::Both,
                pdu.basic_service_information.communication_type,
                pdu.simplex_duplex_selection,
                &mut state.timeslot_alloc,
                TimeslotOwner::Cmce,
            )
        } {
            Ok(circuit) => circuit.clone(),
            Err(e) => {
                tracing::error!("Failed to allocate circuit for U-SETUP: {:?}", e);
                return;
            }
        };

        tracing::info!(
            "rx_u_setup: call from ISSI {} to GSSI {} -> ts={} call_id={} usage={}",
            calling_party.ssi,
            dest_gssi,
            circuit.ts,
            circuit.call_id,
            circuit.usage
        );

        // Signal UMAC to open DL+UL circuits.
        Self::signal_umac_circuit_open(queue, &circuit, self.dltime, None, CircuitDlMediaSource::LocalLoopback);

        // Build channel allocation timeslot mask for this call.
        let mut timeslots = [false; 4];
        timeslots[circuit.ts as usize - 1] = true;

        // Extract UL message routing info for individually-addressed responses.
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &message.msg else {
            panic!()
        };
        let ul_handle = prim.handle;
        let ul_link_id = prim.link_id;
        let ul_endpoint_id = prim.endpoint_id;

        // 1) D-CALL-PROCEEDING to caller.
        self.send_d_call_proceeding(
            queue,
            message,
            pdu,
            circuit.call_id,
            CallTimeoutSetupPhase::T10s,
            pdu.hook_method_selection,
        );

        // 2) D-CONNECT to caller.
        let d_connect = DConnect {
            call_identifier: circuit.call_id,
            call_time_out: CallTimeout::T5m,
            hook_method_selection: pdu.hook_method_selection,
            simplex_duplex_selection: pdu.simplex_duplex_selection,
            transmission_grant: TransmissionGrant::Granted,
            transmission_request_permission: false,
            call_ownership: true,
            call_priority: None,
            basic_service_information: None,
            temporary_address: None,
            notification_indicator: None,
            facility: None,
            proprietary: None,
        };

        tracing::info!("-> {:?}", d_connect);
        let mut connect_sdu = BitBuffer::new_autoexpand(30);
        d_connect.to_bitbuf(&mut connect_sdu).expect("Failed to serialize DConnect");
        connect_sdu.seek(0);

        let connect_msg = SapMsg {
            sap: Sap::LcmcSap,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LcmcMleUnitdataReq(LcmcMleUnitdataReq {
                sdu: connect_sdu,
                handle: ul_handle,
                endpoint_id: ul_endpoint_id,
                link_id: ul_link_id,
                layer2service: Layer2Service::Todo,
                pdu_prio: 0,
                layer2_qos: 0,
                stealing_permission: false,
                stealing_repeats_flag: false,
                chan_alloc: Some(CmceChanAllocReq {
                    usage: Some(circuit.usage),
                    alloc_type: ChanAllocType::Replace,
                    carrier: None,
                    timeslots,
                    ul_dl_assigned: UlDlAssignment::Both,
                }),
                main_address: calling_party,
                tx_reporter: None,
            }),
        };
        queue.push_back(connect_msg);

        // 3) D-SETUP to group.
        let d_setup = DSetup {
            call_identifier: circuit.call_id,
            call_time_out: CallTimeout::T5m,
            hook_method_selection: pdu.hook_method_selection,
            simplex_duplex_selection: pdu.simplex_duplex_selection,
            basic_service_information: pdu.basic_service_information.clone(),
            transmission_grant: TransmissionGrant::GrantedToOtherUser,
            transmission_request_permission: false,
            call_priority: pdu.call_priority,
            notification_indicator: None,
            temporary_address: None,
            calling_party_address_ssi: Some(calling_party.ssi),
            calling_party_extension: None,
            external_subscriber_number: None,
            facility: None,
            dm_ms_address: None,
            proprietary: None,
        };

        self.cached_setups.insert(
            circuit.call_id,
            CachedSetup {
                pdu: d_setup,
                dest_addr,
                resend: true,
                tx_receipt: None,
            },
        );
        let d_setup_ref = &self.cached_setups.get(&circuit.call_id).unwrap().pdu;

        let (setup_sdu, setup_chan_alloc) = Self::build_d_setup_prim(d_setup_ref, circuit.usage, circuit.ts, UlDlAssignment::Both);
        let setup_msg = Self::build_sapmsg(setup_sdu, Some(setup_chan_alloc), self.dltime, dest_addr, None);
        queue.push_back(setup_msg);

        // Track active group call.
        self.active_calls.insert(
            circuit.call_id,
            ActiveCall::new_local(
                calling_party,
                dest_gssi,
                calling_party.ssi,
                circuit.ts,
                circuit.usage,
                self.dltime,
                CallTimeout::T5m,
            ),
        );

        self.notify_floor_granted(
            queue,
            GroupFloorGrant {
                call_id: circuit.call_id,
                source_issi: calling_party.ssi,
                dest_gssi,
                ts: circuit.ts,
            },
            false,
            BrewNotification::IfGroupRoutable(dest_gssi),
        );
    }

    /// Handle U-SETUP for point-to-point (individual) duplex calls.
    pub(in crate::cmce::subentities::cc_bs) fn fsm_on_u_setup_p2p(
        &mut self,
        queue: &mut MessageQueue,
        message: &SapMsg,
        pdu: &USetup,
        calling_party: TetraAddress,
    ) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &message.msg else {
            panic!()
        };

        let is_issi_address = matches!(
            pdu.called_party_type_identifier,
            PartyTypeIdentifier::Ssi | PartyTypeIdentifier::Tsi
        );
        if !is_issi_address && !brew::is_active(&self.config) {
            tracing::warn!(
                "U-SETUP P2P with non-ISSI called_party_type_identifier={} (rejecting, Brew disabled)",
                pdu.called_party_type_identifier
            );
            self.reject_setup_request(
                queue,
                message,
                calling_party,
                DisconnectCause::RequestedServiceNotAvailable,
                "non-ISSI destination requires Brew",
            );
            return;
        }
        if is_issi_address
            && (pdu.called_party_short_number_address.is_some()
                || (pdu.called_party_extension.is_some() && pdu.called_party_type_identifier != PartyTypeIdentifier::Tsi))
        {
            tracing::warn!("U-SETUP P2P with invalid called party fields (short number/extension mismatch), rejecting");
            self.reject_setup_request(
                queue,
                message,
                calling_party,
                DisconnectCause::UnknownTetraIdentity,
                "invalid called party fields",
            );
            return;
        }

        let called_ssi = pdu.called_party_ssi.map(|v| v as u32).unwrap_or(0);
        let has_external_number = pdu.external_subscriber_number.is_some() || pdu.called_party_short_number_address.is_some();
        if called_ssi == 0 && !has_external_number {
            tracing::warn!("U-SETUP P2P without called ISSI/number, ignoring");
            self.reject_setup_request(
                queue,
                message,
                calling_party,
                DisconnectCause::UnknownTetraIdentity,
                "missing called party identity",
            );
            return;
        }

        let called_addr = TetraAddress::new(called_ssi, SsiType::Issi);

        // PBX/phone calls (no concrete local ISSI) always go through Brew.
        if called_ssi == 0 {
            self.fsm_on_u_setup_p2p_over_brew(queue, message, pdu, calling_party, called_addr);
            return;
        }

        if !self.is_locally_registered_issi(called_addr.ssi) {
            tracing::info!(
                "CMCE: called ISSI {} not registered locally (known registry ISSIs={:?}), routing U-SETUP over Brew",
                called_addr.ssi,
                self.known_local_issis()
            );
            self.fsm_on_u_setup_p2p_over_brew(queue, message, pdu, calling_party, called_addr);
            return;
        }

        if let Some((active_call_id, state, cause)) = self.setup_collision_cause(calling_party.ssi, Some(called_addr.ssi)) {
            tracing::info!(
                "CMCE: rejecting U-SETUP P2P from ISSI {} to ISSI {} (collision call_id={} state={:?} cause={})",
                calling_party.ssi,
                called_addr.ssi,
                active_call_id,
                state,
                cause
            );
            self.reject_setup_request(queue, message, calling_party, cause, "individual setup collision");
            return;
        }

        // Allocate circuit(s). Duplex uses two traffic timeslots, one per MS, with cross-routing.
        let (circuit_calling, circuit_called) = {
            let mut state = self.config.state_write();
            let circuit_calling = match self.circuits.allocate_circuit_with_allocator(
                Direction::Both,
                pdu.basic_service_information.communication_type,
                pdu.simplex_duplex_selection,
                &mut state.timeslot_alloc,
                TimeslotOwner::Cmce,
            ) {
                Ok(circuit) => circuit.clone(),
                Err(e) => {
                    tracing::info!(
                        "CMCE: rejecting U-SETUP P2P from ISSI {} to ISSI {}, failed to allocate circuit for U-SETUP P2P, error: {:?}",
                        calling_party.ssi,
                        called_addr.ssi,
                        e
                    );
                    drop(state);
                    self.reject_setup_request(
                        queue,
                        message,
                        calling_party,
                        DisconnectCause::CongestionInInfrastructure,
                        "failed to allocate calling circuit",
                    );
                    return;
                }
            };

            let circuit_called = if pdu.simplex_duplex_selection {
                match self.circuits.allocate_circuit_for_call_with_allocator(
                    circuit_calling.call_id,
                    Direction::Both,
                    pdu.basic_service_information.communication_type,
                    pdu.simplex_duplex_selection,
                    &mut state.timeslot_alloc,
                    TimeslotOwner::Cmce,
                ) {
                    Ok(circuit) => Some(circuit.clone()),
                    Err(e) => {
                        let _ = self.circuits.close_circuit(Direction::Both, circuit_calling.ts);
                        let _ = state.timeslot_alloc.release(TimeslotOwner::Cmce, circuit_calling.ts);
                        tracing::info!(
                            "CMCE: rejecting U-SETUP P2P from ISSI {} to ISSI {}, failed to allocate second circuit for duplex P2P, error {:?}",
                            calling_party.ssi,
                            called_addr.ssi,
                            e
                        );
                        drop(state);
                        self.reject_setup_request(
                            queue,
                            message,
                            calling_party,
                            DisconnectCause::CongestionInInfrastructure,
                            "failed to allocate called circuit",
                        );
                        return;
                    }
                }
            } else {
                None
            };

            (circuit_calling, circuit_called)
        };

        let calling_ts = circuit_calling.ts;
        let calling_usage = circuit_calling.usage;
        let call_id = circuit_calling.call_id;
        let (called_ts, called_usage) = if let Some(called) = &circuit_called {
            (called.ts, called.usage)
        } else {
            (calling_ts, calling_usage)
        };

        tracing::info!(
            "rx_u_setup_p2p: call from ISSI {} to ISSI {} -> call_id={} ts(call)={} usage(call)={} ts(called)={} usage(called)={}",
            calling_party.ssi,
            called_addr.ssi,
            call_id,
            calling_ts,
            calling_usage,
            called_ts,
            called_usage
        );

        // Do not open traffic channel yet. Let called MS respond on MCCH.
        self.send_d_call_proceeding(queue, message, pdu, call_id, CallTimeoutSetupPhase::T60s, pdu.hook_method_selection);

        let d_setup = DSetup {
            call_identifier: call_id,
            call_time_out: CallTimeout::T5m,
            hook_method_selection: pdu.hook_method_selection,
            simplex_duplex_selection: pdu.simplex_duplex_selection,
            basic_service_information: pdu.basic_service_information.clone(),
            transmission_grant: if pdu.simplex_duplex_selection {
                TransmissionGrant::NotGranted
            } else {
                TransmissionGrant::GrantedToOtherUser
            },
            transmission_request_permission: false,
            call_priority: pdu.call_priority,
            notification_indicator: None,
            temporary_address: None,
            calling_party_address_ssi: Some(calling_party.ssi),
            calling_party_extension: None,
            external_subscriber_number: None,
            facility: None,
            dm_ms_address: None,
            proprietary: None,
        };
        tracing::debug!("-> {:?}", d_setup);

        self.cached_setups.insert(
            call_id,
            CachedSetup {
                pdu: d_setup,
                dest_addr: called_addr,
                resend: false,
                tx_receipt: None,
            },
        );

        let d_setup_ref = &self.cached_setups.get(&call_id).unwrap().pdu;
        let mut setup_sdu = BitBuffer::new_autoexpand(80);
        d_setup_ref.to_bitbuf(&mut setup_sdu).expect("Failed to serialize DSetup");
        setup_sdu.seek(0);
        let setup_msg = Self::build_sapmsg(setup_sdu, None, self.dltime, called_addr, None);
        queue.push_back(setup_msg);

        if let Err(err) = self.fsm_individual_create_setup_call(
            call_id,
            IndividualCall {
                calling_addr: calling_party,
                called_addr,
                calling_handle: prim.handle,
                calling_link_id: prim.link_id,
                calling_endpoint_id: prim.endpoint_id,
                called_handle: None,
                called_link_id: None,
                called_endpoint_id: None,
                calling_ts,
                called_ts,
                calling_usage,
                called_usage,
                simplex_duplex: pdu.simplex_duplex_selection,
                state: IndividualCallState::CallSetupPending,
                formal_state: CcFormalState::Idle.after(CcFormalEvent::SetupRequest),
                setup_timer_started: Some(self.dltime),
                setup_timeout: Some(CallTimeoutSetupPhase::T60s),
                active_timer_started: None,
                call_timeout: CallTimeout::T5m,
                called_over_brew: false,
                calling_over_brew: false,
                brew_uuid: None,
                network_call: None,
                connect_request_sent: false,
                floor_holder: None,
                queued_tx_demand: None,
            },
        ) {
            match err {
                IndividualTransitionError::DuplicateCall(_) => {
                    tracing::warn!("CMCE: duplicate call_id={} while creating local P2P setup", call_id);
                    self.abort_individual_setup(
                        queue,
                        call_id,
                        calling_party,
                        prim.handle,
                        prim.link_id,
                        prim.endpoint_id,
                        &[calling_ts, called_ts],
                        DisconnectCause::NoIdleCcEntity,
                    );
                }
                IndividualTransitionError::InvalidTransition { state, .. } => {
                    tracing::warn!("CMCE: local P2P setup call_id={} creation rejected for state {:?}", call_id, state);
                    self.abort_individual_setup(
                        queue,
                        call_id,
                        calling_party,
                        prim.handle,
                        prim.link_id,
                        prim.endpoint_id,
                        &[calling_ts, called_ts],
                        DisconnectCause::IncompatibleTrafficCase,
                    );
                }
                IndividualTransitionError::UnknownCall(_)
                | IndividualTransitionError::MissingBrewUuid(_)
                | IndividualTransitionError::NotBrewOriginated(_)
                | IndividualTransitionError::ConnectRequestAlreadySent(_) => {}
            }
        }
    }

    /// Handle U-SETUP for non-local ISSI, PBX and phone calls via Brew.
    pub(in crate::cmce::subentities::cc_bs) fn fsm_on_u_setup_p2p_over_brew(
        &mut self,
        queue: &mut MessageQueue,
        message: &SapMsg,
        pdu: &USetup,
        calling_party: TetraAddress,
        called_addr: TetraAddress,
    ) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &message.msg else {
            panic!()
        };
        let mut network_call = Self::build_network_circuit_call_from_u_setup(pdu, calling_party.ssi);

        if !brew::is_active(&self.config) {
            tracing::info!(
                "CMCE: rejecting U-SETUP P2P from ISSI {} (Brew disabled, called_ssi={})",
                calling_party.ssi,
                called_addr.ssi
            );
            self.reject_setup_request(
                queue,
                message,
                calling_party,
                DisconnectCause::RequestedServiceNotAvailable,
                "Brew disabled",
            );
            return;
        }

        if let Some((active_call_id, state, cause)) = self.setup_collision_cause(calling_party.ssi, None) {
            tracing::info!(
                "CMCE: rejecting U-SETUP P2P over Brew from ISSI {} (collision call_id={} state={:?} cause={})",
                calling_party.ssi,
                active_call_id,
                state,
                cause
            );
            self.reject_setup_request(queue, message, calling_party, cause, "individual setup collision");
            return;
        }

        if !self.config.state_read().network_connected {
            tracing::info!(
                "CMCE: rejecting U-SETUP over Brew src={} dst={} (backhaul disconnected)",
                calling_party.ssi,
                called_addr.ssi
            );
            self.reject_setup_request(
                queue,
                message,
                calling_party,
                DisconnectCause::RequestedServiceNotAvailable,
                "backhaul disconnected",
            );
            return;
        }

        if !brew::is_brew_issi_routable(&self.config, calling_party.ssi) {
            tracing::info!(
                "CMCE: rejecting U-SETUP P2P over Brew src={} dst={} (source ISSI not routable)",
                calling_party.ssi,
                called_addr.ssi
            );
            self.reject_setup_request(
                queue,
                message,
                calling_party,
                DisconnectCause::CalledPartyNotReachable,
                "source ISSI not Brew-routable",
            );
            return;
        }

        let has_external_called_party = Self::has_external_called_party(pdu, &network_call);
        let destination_routable = network_call.destination == 0 || brew::is_brew_issi_routable(&self.config, network_call.destination);

        if !has_external_called_party && !destination_routable {
            tracing::info!(
                "CMCE: rejecting U-SETUP P2P over Brew src={} dst={} (destination ISSI not routable)",
                calling_party.ssi,
                network_call.destination
            );
            self.reject_setup_request(
                queue,
                message,
                calling_party,
                DisconnectCause::CalledPartyNotReachable,
                "destination ISSI not Brew-routable",
            );
            return;
        }

        if has_external_called_party && !destination_routable && network_call.destination != 0 {
            tracing::debug!(
                "CMCE: overriding non-routable destination SSI {} with 0 for external-number call src={} number='{}'",
                network_call.destination,
                calling_party.ssi,
                network_call.number
            );
            network_call.destination = 0;
        }

        // Allocate one bearer for the local MS.
        let circuit_calling = {
            let mut state = self.config.state_write();
            match self.circuits.allocate_circuit_with_allocator(
                Direction::Both,
                pdu.basic_service_information.communication_type,
                pdu.simplex_duplex_selection,
                &mut state.timeslot_alloc,
                TimeslotOwner::Cmce,
            ) {
                Ok(circuit) => circuit.clone(),
                Err(e) => {
                    tracing::info!(
                        "CMCE: rejecting U-SETUP over Brew src={} dst={} (allocation failed: {:?})",
                        calling_party.ssi,
                        called_addr.ssi,
                        e
                    );
                    drop(state);
                    self.reject_setup_request(
                        queue,
                        message,
                        calling_party,
                        DisconnectCause::CongestionInInfrastructure,
                        "failed to allocate Brew-routed circuit",
                    );
                    return;
                }
            }
        };

        let call_id = circuit_calling.call_id;
        let ts = circuit_calling.ts;
        let usage = circuit_calling.usage;
        let brew_uuid = uuid::Uuid::new_v4();

        tracing::info!(
            "CMCE: forwarding U-SETUP over Brew call_id={} src={} dst={} ts={} duplex={} number='{}' uuid={}",
            call_id,
            calling_party.ssi,
            network_call.destination,
            ts,
            network_call.duplex,
            network_call.number,
            brew_uuid
        );

        self.send_d_call_proceeding(queue, message, pdu, call_id, CallTimeoutSetupPhase::T60s, pdu.hook_method_selection);

        queue.push_back(SapMsg {
            sap: Sap::Control,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Brew,
            msg: SapMsgInner::CmceCallControl(CallControl::NetworkCircuitSetupRequest {
                brew_uuid,
                call: network_call.clone(),
            }),
        });

        if let Err(err) = self.fsm_individual_create_setup_call(
            call_id,
            IndividualCall {
                calling_addr: calling_party,
                called_addr,
                calling_handle: prim.handle,
                calling_link_id: prim.link_id,
                calling_endpoint_id: prim.endpoint_id,
                called_handle: None,
                called_link_id: None,
                called_endpoint_id: None,
                calling_ts: ts,
                called_ts: ts,
                calling_usage: usage,
                called_usage: usage,
                simplex_duplex: pdu.simplex_duplex_selection,
                state: IndividualCallState::CallSetupPending,
                formal_state: CcFormalState::Idle.after(CcFormalEvent::SetupRequest),
                setup_timer_started: Some(self.dltime),
                setup_timeout: Some(CallTimeoutSetupPhase::T60s),
                active_timer_started: None,
                call_timeout: CallTimeout::T5m,
                called_over_brew: true,
                calling_over_brew: false,
                brew_uuid: Some(brew_uuid),
                network_call: Some(network_call),
                connect_request_sent: false,
                floor_holder: None,
                queued_tx_demand: None,
            },
        ) {
            match err {
                IndividualTransitionError::DuplicateCall(_) => {
                    tracing::warn!("CMCE: duplicate call_id={} while creating Brew P2P setup", call_id);
                    self.abort_individual_setup(
                        queue,
                        call_id,
                        calling_party,
                        prim.handle,
                        prim.link_id,
                        prim.endpoint_id,
                        &[ts],
                        DisconnectCause::NoIdleCcEntity,
                    );
                }
                IndividualTransitionError::InvalidTransition { state, .. } => {
                    tracing::warn!("CMCE: Brew P2P setup call_id={} creation rejected for state {:?}", call_id, state);
                    self.abort_individual_setup(
                        queue,
                        call_id,
                        calling_party,
                        prim.handle,
                        prim.link_id,
                        prim.endpoint_id,
                        &[ts],
                        DisconnectCause::IncompatibleTrafficCase,
                    );
                }
                IndividualTransitionError::UnknownCall(_)
                | IndividualTransitionError::MissingBrewUuid(_)
                | IndividualTransitionError::NotBrewOriginated(_)
                | IndividualTransitionError::ConnectRequestAlreadySent(_) => {}
            }
        }
    }
}
