use super::*;

impl CcMsSubentity {
    /// LCMC-SAP (lower boundary) air-interface ingress route.
    ///
    /// Downlink CMCE call-control PDUs are delivered up from MLE as
    /// `LcmcMleUnitdataInd`; the CMCE protocol control (`cmce_ms`) has already
    /// demultiplexed by message type (cl. 14.8.28) and routed the CC-owned PDU
    /// set here. This peeks the 5-bit PDU type, parses, and dispatches to the
    /// per-PDU `rx_d_*` handlers — mirroring cc_bs's `route_rd_deliver`
    /// (`rx_u_*` on the BS, which receives the uplink instead).
    pub fn route_rd_deliver(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };
        let Some(bits) = prim.sdu.peek_bits(5) else {
            tracing::warn!("insufficient bits: {}", prim.sdu.dump_bin());
            return;
        };
        let Ok(pdu_type) = CmcePduTypeDl::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, prim.sdu.dump_bin());
            return;
        };
        let route = CallRoute {
            main_address: prim.received_tetra_address,
            handle: prim.handle,
            endpoint_id: prim.endpoint_id,
            link_id: prim.link_id,
        };
        macro_rules! parse {
            ($ty:ty, $handler:ident) => {
                match <$ty>::from_bitbuf(&mut prim.sdu) {
                    Ok(pdu) => self.$handler(queue, pdu, route),
                    Err(e) => tracing::warn!("CMCE-MS: failed parsing {:?}: {:?} {}", pdu_type, e, prim.sdu.dump_bin()),
                }
            };
        }
        match pdu_type {
            CmcePduTypeDl::DAlert => parse!(DAlert, rx_d_alert),
            CmcePduTypeDl::DCallProceeding => parse!(DCallProceeding, rx_d_call_proceeding),
            CmcePduTypeDl::DCallRestore => parse!(DCallRestore, rx_d_call_restore),
            CmcePduTypeDl::DConnect => parse!(DConnect, rx_d_connect),
            CmcePduTypeDl::DConnectAcknowledge => parse!(DConnectAcknowledge, rx_d_connect_ack),
            CmcePduTypeDl::DDisconnect => parse!(DDisconnect, rx_d_disconnect),
            CmcePduTypeDl::DInfo => parse!(DInfo, rx_d_info),
            CmcePduTypeDl::DRelease => parse!(DRelease, rx_d_release),
            CmcePduTypeDl::DSetup => parse!(DSetup, rx_d_setup),
            CmcePduTypeDl::DTxCeased => parse!(DTxCeased, rx_d_tx_ceased),
            CmcePduTypeDl::DTxContinue => parse!(DTxContinue, rx_d_tx_continue),
            CmcePduTypeDl::DTxGranted => parse!(DTxGranted, rx_d_tx_granted),
            CmcePduTypeDl::DTxInterrupt => parse!(DTxInterrupt, rx_d_tx_interrupt),
            CmcePduTypeDl::DTxWait => parse!(DTxWait, rx_d_tx_wait),
            _ => panic!(),
        }
    }

    pub(in crate::cmce::subentities::cc_ms) fn rx_d_setup(&mut self, queue: &mut MessageQueue, pdu: DSetup, route: CallRoute) {
        let kind = kind_from_basic_service(&pdu.basic_service_information);
        let is_group = kind != MsCallKind::Individual;

        // ETSI TS 100 392-2 cl. 14.5.2.1.2: "CC shall ignore the D-SETUP PDU if
        // the calling party address is the same as the MS's own address and
        // shall send a CONFIGURE request primitive for lower layer configuration
        // ignoring the channel change." The SwMI periodically re-broadcasts the
        // group D-SETUP for late entry (cl. 14.5.1.1); for the call originator
        // these echoes carry its own calling party address. Ignoring them (no
        // CONFIGURE change is emitted, so the U-plane is left untouched) prevents
        // recreating the call, raising a duplicate TNCC-SETUP indication to the
        // user application, and — critically — applying the echoed transmission
        // grant, which would knock a talking MS off its own granted floor.
        if is_group
            && pdu.calling_party_address_ssi.is_some()
            && pdu.calling_party_address_ssi == self.own_issi
        {
            tracing::debug!(
                call_identifier = pdu.call_identifier,
                calling_party = ?pdu.calling_party_address_ssi,
                "CMCE-MS: ignoring own-address group D-SETUP re-broadcast (cl. 14.5.2.1.2)"
            );
            return;
        }

        // ETSI TS 100 392-2 cl. 14.5.2.1.2: a group addressed D-SETUP for a call
        // the MS already tracks (late-entry re-broadcast from another calling
        // party) makes the MS enter/stay CALL ACTIVE and apply the transmission
        // grant element (U-plane reception), but it is not a new incoming call:
        // the call is updated in place — it is NOT recreated and NO fresh
        // TNCC-SETUP indication is raised. A genuine talker change is surfaced to
        // the user application as a TNCC-TX indication only when the current
        // speaker actually changes.
        if is_group && self.calls.contains_key(&pdu.call_identifier) {
            let prev_speaker = self.calls.get(&pdu.call_identifier).and_then(|c| c.current_speaker_ssi);
            // ETSI TS 100 392-2 cl. 14.5.2.1.2 / 14.5.1.1: the periodic group
            // D-SETUP re-broadcast is a late-entry beacon whose transmission-grant
            // element reflects the floor state at the time the SwMI queued it and
            // can therefore be stale. While THIS MS currently holds the floor
            // (GrantedSelf), real-time floor control is governed solely by
            // D-TX-GRANTED / D-TX-CEASED / D-TX-INTERRUPT (cl. 14.8). Applying the
            // re-broadcast's grant here would revoke our own active transmission —
            // the same hazard the own-address guard above prevents for our own
            // echo, but reached via a re-broadcast that carries a previous talker's
            // calling-party address. So keep the call-active housekeeping but do
            // NOT re-apply the floor grant (or raise a talker-change indication)
            // when we are the current transmitter; a genuine revocation arrives via
            // D-TX-INTERRUPT / D-TX-CEASED.
            let holds_floor_self = self
                .calls
                .get(&pdu.call_identifier)
                .map(|c| c.tx_grant_state == MsTxGrantState::GrantedSelf)
                .unwrap_or(false);
            if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
                call.route = route;
                call.state = MsCcState::CallActive;
                call.transmission_request_allowed = !pdu.transmission_request_permission; // ETSI 14.8.43 Table 14.81: bit 0 = allowed to request
            }
            if holds_floor_self {
                tracing::debug!(
                    call_identifier = pdu.call_identifier,
                    grant = ?pdu.transmission_grant,
                    "CMCE-MS: group D-SETUP re-broadcast ignored for floor (MS holds the floor; cl. 14.5.2.1.2)"
                );
                return;
            }
            self.apply_transmission_grant(queue, pdu.call_identifier, pdu.transmission_grant, pdu.calling_party_address_ssi);
            let new_speaker = self.calls.get(&pdu.call_identifier).and_then(|c| c.current_speaker_ssi);
            tracing::debug!(
                call_identifier = pdu.call_identifier,
                grant = ?pdu.transmission_grant,
                speaker = ?new_speaker,
                permission_not_allowed = pdu.transmission_request_permission,
                "CMCE-MS: group D-SETUP re-broadcast — updating floor in place (cl. 14.5.2.1.2)"
            );
            if new_speaker != prev_speaker {
                self.emit(TelemetryEvent::TnccTxIndication {
                    call_identifier: pdu.call_identifier,
                    indication: tncc::TnccTxIndication {
                        encryption_flag: tncc::EncryptionFlag::ClearEndToEndTransmission,
                        notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                        transmitting_party_ssi: new_speaker,
                        transmitting_party_extension: None,
                        external_subscriber_number: None,
                        transmit_request_permission: tncc::TransmissionRequestPermission::from_bool(pdu.transmission_request_permission),
                        transmission_status: tncc_transmission_status_from_grant(pdu.transmission_grant),
                    },
                });
            }
            return;
        }

        let setup_basic_for_event = pdu.basic_service_information.clone();
        let state = if kind == MsCallKind::Individual {
            MsCcState::MtCallSetup
        } else {
            MsCcState::CallActive
        };
        let mut call = MsCall::new(
            pdu.call_identifier,
            state,
            kind,
            pdu.basic_service_information,
            pdu.simplex_duplex_selection,
            route,
            // ETSI 14.8.43 Table 14.81: transmission_request_permission bit 0 =
            // "allowed to request"; invert into the MS's "allowed" flag.
            !pdu.transmission_request_permission,
        );
        call.current_speaker_ssi = pdu.calling_party_address_ssi;
        // Record the signalling mode dictated by the D-SETUP Hook method
        // selection IE (cl. 14.8.23) so the answer path (cl. 14.5.1.1.1) can
        // choose U-ALERT-then-U-CONNECT (on/off-hook) vs immediate U-CONNECT
        // (direct set-up).
        call.hook_on_off = pdu.hook_method_selection;
        call.start_call_timer(self.dltime, pdu.call_time_out);
        self.calls.insert(pdu.call_identifier, call);
        if let Some(basic) = tncc_basic_from_pdu(&setup_basic_for_event) {
            self.emit(TelemetryEvent::TnccSetupIndication {
                call_identifier: pdu.call_identifier,
                indication: Box::new(tncc::TnccSetupIndication {
                    basic_service_information: basic,
                    call_priority: tncc::CallPriority::from_raw(pdu.call_priority).unwrap_or(tncc::CallPriority::PriorityNotDefined),
                    call_time_out: tncc_call_timeout(pdu.call_time_out),
                    called_party_ssi: route.main_address.ssi,
                    called_party_extension: None,
                    calling_party_ssi: pdu.calling_party_address_ssi,
                    calling_party_extension: pdu.calling_party_extension,
                    external_subscriber_number_calling: None,
                    clir_control: None,
                    hook_method_selection: tncc::HookMethodSelection::from_bool(pdu.hook_method_selection),
                    notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                    simplex_duplex_selection: tncc::SimplexDuplexSelection::from_bool(pdu.simplex_duplex_selection),
                    transmission_grant: tncc_transmission_grant(pdu.transmission_grant),
                    transmission_request_permission: tncc::TransmissionRequestPermission::from_bool(pdu.transmission_request_permission),
                }),
            });
        } else {
            tracing::warn!(
                call_identifier = pdu.call_identifier,
                "CMCE-MS: unsupported TNCC basic service value; suppressing TNCC-SETUP indication"
            );
        }
        if kind != MsCallKind::Individual {
            self.apply_transmission_grant(queue, pdu.call_identifier, pdu.transmission_grant, pdu.calling_party_address_ssi);
        }
    }

    fn rx_d_call_proceeding(&mut self, _queue: &mut MessageQueue, pdu: DCallProceeding, route: CallRoute) {
        let pending = self.pending_originations.pop();
        let basic = pdu
            .basic_service_information
            .clone()
            .or_else(|| pending.as_ref().map(|p| p.basic_service.clone()))
            .unwrap_or_else(default_speech_basic_service);
        let simplex = pending
            .as_ref()
            .map(|p| p.simplex_duplex_selection)
            .unwrap_or(pdu.simplex_duplex_selection);
        let kind = kind_from_basic_service(&basic);
        let call = self
            .calls
            .entry(pdu.call_identifier)
            .or_insert_with(|| MsCall::new(pdu.call_identifier, MsCcState::MoCallSetup, kind, basic, simplex, route, true));
        call.state = MsCcState::MoCallSetup;
        call.route = route;
        call.start_setup_timer(self.dltime, pdu.call_time_out_set_up_phase);
        self.emit(TelemetryEvent::TnccProceedIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccProceedIndication {
                basic_service_information_offered: pdu.basic_service_information.as_ref().and_then(tncc_basic_from_pdu),
                call_status: pdu.call_status.and_then(tncc_call_status),
                hook_method: Some(tncc::HookMethodSelection::from_bool(pdu.hook_method_selection)),
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                simplex_duplex: Some(tncc::SimplexDuplexSelection::from_bool(pdu.simplex_duplex_selection)),
            },
        });
    }

    fn rx_d_alert(&mut self, _queue: &mut MessageQueue, pdu: DAlert, route: CallRoute) {
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            if let Some(basic) = &pdu.basic_service_information {
                call.basic_service = basic.clone();
            }
            if let Ok(timeout) = CallTimeoutSetupPhase::try_from(pdu.call_time_out_set_up_phase as u64) {
                call.start_setup_timer(self.dltime, timeout);
            }
        }
        self.emit(TelemetryEvent::TnccAlertIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccAlertIndication {
                basic_service_information_offered: pdu.basic_service_information.as_ref().and_then(tncc_basic_from_pdu),
                call_queued: Some(if pdu.call_queued {
                    tncc::CallQueued::CallIsQueued
                } else {
                    tncc::CallQueued::CallIsNotQueued
                }),
                call_time_out_set_up_phase: tncc_setup_timeout(
                    CallTimeoutSetupPhase::try_from(pdu.call_time_out_set_up_phase as u64).unwrap_or(CallTimeoutSetupPhase::Predefined),
                ),
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                simplex_duplex: tncc::SimplexDuplexSelection::from_bool(pdu.simplex_duplex_selection),
            },
        });
    }

    fn rx_d_connect(&mut self, queue: &mut MessageQueue, pdu: DConnect, route: CallRoute) {
        let pending = self.pending_originations.pop();
        let basic = pdu
            .basic_service_information
            .clone()
            .or_else(|| pending.as_ref().map(|p| p.basic_service.clone()))
            .unwrap_or_else(default_speech_basic_service);
        let simplex = pending
            .as_ref()
            .map(|p| p.simplex_duplex_selection)
            .unwrap_or(pdu.simplex_duplex_selection);
        let kind = kind_from_basic_service(&basic);
        let call = self.calls.entry(pdu.call_identifier).or_insert_with(|| {
            MsCall::new(
                pdu.call_identifier,
                MsCcState::CallActive,
                kind,
                basic,
                simplex,
                route,
                // ETSI 14.8.43 Table 14.81: bit 0 = allowed to request; invert.
                !pdu.transmission_request_permission,
            )
        });
        call.state = MsCcState::CallActive;
        call.route = route;
        call.transmission_request_allowed = !pdu.transmission_request_permission; // ETSI 14.8.43 Table 14.81: bit 0 = allowed to request
        call.simplex_duplex_selection = pdu.simplex_duplex_selection;
        call.start_call_timer(self.dltime, pdu.call_time_out);
        call.timers.setup_phase_deadline = None;
        let confirm_basic = call.basic_service.clone();
        let _ = call;
        self.apply_transmission_grant(queue, pdu.call_identifier, pdu.transmission_grant, None);
        if let Some(basic) = tncc_basic_from_pdu(&confirm_basic) {
            self.emit(TelemetryEvent::TnccSetupConfirm {
                call_identifier: pdu.call_identifier,
                confirm: Box::new(tncc::TnccSetupConfirm {
                    basic_service_information: basic,
                    call_priority: pdu.call_priority.and_then(|v| tncc::CallPriority::from_raw(v as u8)),
                    call_ownership: if pdu.call_ownership {
                        tncc::CallOwnership::ACallOwner
                    } else {
                        tncc::CallOwnership::NotACallOwner
                    },
                    call_amalgamation: tncc::CallAmalgamation::CallNotAmalgamated,
                    call_time_out: tncc_call_timeout(pdu.call_time_out),
                    hook_method_selection: tncc::HookMethodSelection::from_bool(pdu.hook_method_selection),
                    notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                    simplex_duplex_selection: tncc::SimplexDuplexSelection::from_bool(pdu.simplex_duplex_selection),
                    transmission_grant: tncc_transmission_grant(pdu.transmission_grant),
                    transmission_request_permission: tncc::TransmissionRequestPermission::from_bool(pdu.transmission_request_permission),
                }),
            });
        }
    }

    pub(in crate::cmce::subentities::cc_ms) fn rx_d_connect_ack(&mut self, queue: &mut MessageQueue, pdu: DConnectAcknowledge, route: CallRoute) {
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.state = MsCcState::CallActive;
            call.route = route;
            call.transmission_request_allowed = !pdu.transmission_request_permission; // ETSI 14.8.43 Table 14.81: bit 0 = allowed to request
            call.timers.setup_phase_deadline = None;
            if let Ok(timeout) = CallTimeout::try_from(pdu.call_time_out as u64) {
                call.start_call_timer(self.dltime, timeout);
            }
        }
        if let Ok(grant) = TransmissionGrant::try_from(pdu.transmission_grant as u64) {
            self.apply_transmission_grant(queue, pdu.call_identifier, grant, None);
            if let Some(call) = self.calls.get(&pdu.call_identifier) {
                self.emit(TelemetryEvent::TnccCompleteConfirm {
                    call_identifier: pdu.call_identifier,
                    confirm: tncc::TnccCompleteConfirm {
                        call_time_out: tncc_call_timeout(call.timers.call_timeout),
                        notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                        transmission_grant: tncc_transmission_grant(grant),
                        transmission_request_permission: tncc::TransmissionRequestPermission::from_bool(
                            pdu.transmission_request_permission,
                        ),
                        transmission_status: tncc_transmission_status_from_grant(grant),
                    },
                });
            }
        }
    }

    fn rx_d_tx_granted(&mut self, queue: &mut MessageQueue, pdu: DTxGranted, route: CallRoute) {
        tracing::info!(
            call_identifier = pdu.call_identifier,
            grant = pdu.transmission_grant,
            transmitting_party_ssi = ?pdu.transmitting_party_address_ssi,
            permission_not_allowed = pdu.transmission_request_permission,
            "CMCE-MS: rx D-TX-GRANTED"
        );
        let pending_before = self.calls.get(&pdu.call_identifier).map(|c| c.pending_tx_request).unwrap_or(false);
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::CallActive;
            call.transmission_request_allowed = !pdu.transmission_request_permission; // ETSI 14.8.43 Table 14.81: bit 0 = allowed to request
            call.current_speaker_ssi = pdu.transmitting_party_address_ssi.map(|v| v as u32);
        }
        if let Ok(grant) = TransmissionGrant::try_from(pdu.transmission_grant as u64) {
            let speaker = pdu.transmitting_party_address_ssi.map(|v| v as u32);
            if grant == TransmissionGrant::GrantedToOtherUser && speaker == self.own_issi {
                tracing::warn!(call_identifier = pdu.call_identifier, "CMCE-MS: explicit self grant still required");
                return;
            }
            self.apply_transmission_grant(queue, pdu.call_identifier, grant, speaker);
            if let Some(call) = self.calls.get(&pdu.call_identifier) {
                if pending_before && grant == TransmissionGrant::Granted {
                    self.emit(TelemetryEvent::TnccTxConfirm {
                        call_identifier: pdu.call_identifier,
                        confirm: tncc::TnccTxConfirm {
                            encryption_flag: tncc::EncryptionFlag::from_bool(pdu.encryption_control),
                            transmit_request_permission: tncc::TransmissionRequestPermission::from_bool(
                                pdu.transmission_request_permission,
                            ),
                            transmission_status: tncc_transmission_status_from_grant(grant),
                        },
                    });
                } else {
                    self.emit(TelemetryEvent::TnccTxIndication {
                        call_identifier: pdu.call_identifier,
                        indication: tncc::TnccTxIndication {
                            encryption_flag: tncc::EncryptionFlag::from_bool(pdu.encryption_control),
                            notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                            transmitting_party_ssi: pdu.transmitting_party_address_ssi.map(|v| v as u32).or(call.current_speaker_ssi),
                            transmitting_party_extension: pdu.transmitting_party_extension.map(|v| v as u32),
                            external_subscriber_number: None,
                            transmit_request_permission: tncc::TransmissionRequestPermission::from_bool(
                                pdu.transmission_request_permission,
                            ),
                            transmission_status: tncc_transmission_status_from_grant(grant),
                        },
                    });
                }
            }
        }
    }

    fn rx_d_tx_ceased(&mut self, queue: &mut MessageQueue, pdu: DTxCeased, route: CallRoute) {
        tracing::info!(
            call_identifier = pdu.call_identifier,
            permission_not_allowed = pdu.transmission_request_permission,
            "CMCE-MS: rx D-TX-CEASED"
        );
        let simplex_duplex = if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::CallActive;
            call.tx_grant_state = MsTxGrantState::None;
            call.current_speaker_ssi = None;
            call.transmission_request_allowed = !pdu.transmission_request_permission; // ETSI 14.8.43 Table 14.81: bit 0 = allowed to request
            call.pending_tx_request = false;
            Some(call.simplex_duplex_selection)
        } else {
            None
        };
        if let Some(simplex_duplex) = simplex_duplex {
            self.configure_uplane(queue, pdu.call_identifier, false, false, simplex_duplex);
        }
        self.emit(TelemetryEvent::TnccTxIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccTxIndication {
                encryption_flag: tncc::EncryptionFlag::ClearEndToEndTransmission,
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                transmitting_party_ssi: None,
                transmitting_party_extension: None,
                external_subscriber_number: None,
                transmit_request_permission: tncc::TransmissionRequestPermission::from_bool(pdu.transmission_request_permission),
                transmission_status: tncc::TransmissionStatus::TransmissionCeased,
            },
        });
    }

    fn rx_d_tx_wait(&mut self, queue: &mut MessageQueue, pdu: DTxWait, route: CallRoute) {
        tracing::info!(
            call_identifier = pdu.call_identifier,
            permission_not_allowed = pdu.transmission_request_permission,
            "CMCE-MS: rx D-TX-WAIT"
        );
        let simplex_duplex = if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::Wait;
            call.transmission_request_allowed = !pdu.transmission_request_permission; // ETSI 14.8.43 Table 14.81: bit 0 = allowed to request
            call.tx_grant_state = MsTxGrantState::Waiting;
            call.uplane_before_wait = call.last_uplane.filter(|u| u.switch_u_plane);
            Some(call.simplex_duplex_selection)
        } else {
            None
        };
        if let Some(simplex_duplex) = simplex_duplex {
            self.configure_uplane(queue, pdu.call_identifier, false, false, simplex_duplex);
        }
        self.emit(TelemetryEvent::TnccTxIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccTxIndication {
                encryption_flag: tncc::EncryptionFlag::ClearEndToEndTransmission,
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                transmitting_party_ssi: None,
                transmitting_party_extension: None,
                external_subscriber_number: None,
                transmit_request_permission: tncc::TransmissionRequestPermission::from_bool(pdu.transmission_request_permission),
                transmission_status: tncc::TransmissionStatus::TransmissionWait,
            },
        });
    }

    fn rx_d_tx_continue(&mut self, queue: &mut MessageQueue, pdu: DTxContinue, route: CallRoute) {
        tracing::info!(
            call_identifier = pdu.call_identifier,
            do_continue = pdu.do_continue,
            permission_not_allowed = pdu.transmission_request_permission,
            "CMCE-MS: rx D-TX-CONTINUE"
        );
        let restore = if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::CallActive;
            call.transmission_request_allowed = !pdu.transmission_request_permission; // ETSI 14.8.43 Table 14.81: bit 0 = allowed to request
            let restore = if pdu.do_continue { call.uplane_before_wait.take() } else { None };
            if restore.is_none() {
                call.tx_grant_state = MsTxGrantState::None;
            }
            restore
        } else {
            None
        };
        if let Some(u) = restore {
            self.configure_uplane(queue, pdu.call_identifier, u.switch_u_plane, u.tx_grant, u.simplex_duplex);
            if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
                call.tx_grant_state = if u.tx_grant {
                    MsTxGrantState::GrantedSelf
                } else {
                    MsTxGrantState::GrantedOther
                };
            }
        }
        self.emit(TelemetryEvent::TnccTxIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccTxIndication {
                encryption_flag: tncc::EncryptionFlag::ClearEndToEndTransmission,
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                transmitting_party_ssi: None,
                transmitting_party_extension: None,
                external_subscriber_number: None,
                transmit_request_permission: tncc::TransmissionRequestPermission::from_bool(pdu.transmission_request_permission),
                transmission_status: if pdu.do_continue {
                    tncc::TransmissionStatus::TransmissionGranted
                } else {
                    tncc::TransmissionStatus::TransmissionCeased
                },
            },
        });
    }

    fn rx_d_tx_interrupt(&mut self, queue: &mut MessageQueue, pdu: DTxInterrupt, route: CallRoute) {
        tracing::info!(
            call_identifier = pdu.call_identifier,
            grant = pdu.transmission_grant,
            transmitting_party_ssi = ?pdu.transmitting_party_address_ssi,
            permission_not_allowed = pdu.transmission_request_permission,
            "CMCE-MS: rx D-TX-INTERRUPT"
        );
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::CallActive;
            call.transmission_request_allowed = !pdu.transmission_request_permission; // ETSI 14.8.43 Table 14.81: bit 0 = allowed to request
            call.current_speaker_ssi = pdu.transmitting_party_address_ssi.map(|v| v as u32);
        }
        if let Ok(grant) = TransmissionGrant::try_from(pdu.transmission_grant as u64) {
            if grant == TransmissionGrant::GrantedToOtherUser {
                self.apply_transmission_grant(
                    queue,
                    pdu.call_identifier,
                    grant,
                    pdu.transmitting_party_address_ssi.map(|v| v as u32),
                );
            } else if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
                call.tx_grant_state = MsTxGrantState::Interrupted;
                let simplex_duplex = call.simplex_duplex_selection;
                let _ = call;
                self.configure_uplane(queue, pdu.call_identifier, false, false, simplex_duplex);
            }
        }
        self.emit(TelemetryEvent::TnccTxIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccTxIndication {
                encryption_flag: tncc::EncryptionFlag::ClearEndToEndTransmission,
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                transmitting_party_ssi: pdu.transmitting_party_address_ssi.map(|v| v as u32),
                transmitting_party_extension: pdu.transmitting_party_extension.map(|v| v as u32),
                external_subscriber_number: None,
                transmit_request_permission: tncc::TransmissionRequestPermission::from_bool(pdu.transmission_request_permission),
                transmission_status: tncc::TransmissionStatus::TransmissionInterrupt,
            },
        });
    }

    fn rx_d_disconnect(&mut self, queue: &mut MessageQueue, pdu: DDisconnect, route: CallRoute) {
        self.emit(TelemetryEvent::TnccReleaseIndication {
            call_identifier: pdu.call_identifier,
            indication: tncc::TnccReleaseIndication {
                disconnect_cause: tncc_disconnect_cause(pdu.disconnect_cause),
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
            },
        });
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::Disconnect;
            call.disconnect_cause = Some(pdu.disconnect_cause);
        }
        let _ = self.release_call(queue, pdu.call_identifier, pdu.disconnect_cause);
    }

    fn rx_d_release(&mut self, queue: &mut MessageQueue, pdu: DRelease, route: CallRoute) {
        let was_local_disconnect = self
            .calls
            .get(&pdu.call_identifier)
            .map(|c| c.state == MsCcState::Disconnect || c.state == MsCcState::Release)
            .unwrap_or(false);
        let simplex_duplex = if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            call.state = MsCcState::Release;
            call.disconnect_cause = Some(pdu.disconnect_cause);
            Some(call.simplex_duplex_selection)
        } else {
            None
        };
        if let Some(simplex_duplex) = simplex_duplex {
            self.configure_uplane(queue, pdu.call_identifier, false, false, simplex_duplex);
        }
        if was_local_disconnect {
            self.emit(TelemetryEvent::TnccReleaseConfirm {
                call_identifier: pdu.call_identifier,
                confirm: tncc::TnccReleaseConfirm {
                    disconnect_cause: tncc_disconnect_cause(pdu.disconnect_cause),
                    disconnect_status: tncc::DisconnectStatus::DisconnectionSuccessful,
                    notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                },
            });
        } else {
            self.emit(TelemetryEvent::TnccReleaseIndication {
                call_identifier: pdu.call_identifier,
                indication: tncc::TnccReleaseIndication {
                    disconnect_cause: tncc_disconnect_cause(pdu.disconnect_cause),
                    notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                },
            });
        }
        self.calls.remove(&pdu.call_identifier);
    }

    fn rx_d_info(&mut self, _queue: &mut MessageQueue, pdu: DInfo, route: CallRoute) {
        let mut new_key = None;
        if let Some(call) = self.calls.get_mut(&pdu.call_identifier) {
            call.route = route;
            if let Some(timeout) = pdu
                .call_time_out_set_up_phase_t301_t302_
                .and_then(|v| CallTimeoutSetupPhase::try_from(v).ok())
            {
                call.start_setup_timer(self.dltime, timeout);
            }
            if pdu.reset_call_time_out_timer_t310_ {
                if let Some(timeout) = pdu.call_time_out.and_then(|v| CallTimeout::try_from(v).ok()) {
                    call.start_call_timer(self.dltime, timeout);
                } else if call.timers.call_timeout != CallTimeout::Infinite {
                    call.start_call_timer(self.dltime, call.timers.call_timeout);
                }
            } else if let Some(timeout) = pdu.call_time_out.and_then(|v| CallTimeout::try_from(v).ok()) {
                call.timers.call_timeout = timeout;
            }
            new_key = pdu.new_call_identifier.map(|id| id as u16);
        }
        if let Some(id) = new_key {
            if let Some(mut call) = self.calls.remove(&pdu.call_identifier) {
                call.call_identifier = id;
                self.calls.insert(id, call);
            }
        }
        self.emit(TelemetryEvent::TnccNotifyIndication {
            call_identifier: new_key.unwrap_or(pdu.call_identifier),
            indication: tncc::TnccNotifyIndication {
                call_status: pdu.call_status.and_then(|v| tncc_call_status_raw(v as u8)),
                call_time_out_in_set_up_phase: pdu
                    .call_time_out_set_up_phase_t301_t302_
                    .and_then(|v| CallTimeoutSetupPhase::try_from(v).ok())
                    .map(tncc_setup_timeout),
                call_time_out: pdu.call_time_out.and_then(|v| CallTimeout::try_from(v).ok()).map(tncc_call_timeout),
                call_ownership: pdu.call_ownership.map(|v| {
                    if v == 0 {
                        tncc::CallOwnership::NotACallOwner
                    } else {
                        tncc::CallOwnership::ACallOwner
                    }
                }),
                notification_indicator: pdu.notification_indicator.map(|v| v as u8),
                poll_response_percentage: pdu.poll_response_percentage.map(|v| v as u8),
                poll_response_number: pdu.poll_response_number.map(|v| v as u8),
                poll_response_addresses: None,
                poll_request: Some(pdu.poll_request),
            },
        });
    }

    fn rx_d_call_restore(&mut self, queue: &mut MessageQueue, pdu: DCallRestore, route: CallRoute) {
        let mut key = pdu.call_identifier;
        if let Some(new_id) = pdu.new_call_identifier {
            if let Some(mut call) = self.calls.remove(&pdu.call_identifier) {
                key = new_id as u16;
                call.call_identifier = key;
                self.calls.insert(key, call);
            }
        }
        if let Some(call) = self.calls.get_mut(&key) {
            call.route = route;
            call.state = MsCcState::CallActive;
            if pdu.reset_call_time_out_timer_t310_ {
                if let Some(timeout) = pdu.call_time_out.and_then(|v| CallTimeout::try_from(v).ok()) {
                    call.start_call_timer(self.dltime, timeout);
                } else if call.timers.call_timeout != CallTimeout::Infinite {
                    call.start_call_timer(self.dltime, call.timers.call_timeout);
                }
            }
        }
        if let Ok(grant) = TransmissionGrant::try_from(pdu.transmission_grant as u64) {
            self.apply_transmission_grant(queue, key, grant, None);
        }
    }
}
