use super::*;

impl CcBsSubentity {
    pub fn tick_start(&mut self, queue: &mut MessageQueue, dltime: TdmaTime) {
        self.dltime = dltime;

        // ETSI T310 equivalent for active calls.
        self.check_call_timeout_expiry(queue);
        // ETSI T301/T302 equivalent while waiting for call completion.
        self.check_individual_setup_timeout(queue);
        // Check hangtime expiry for active local calls
        self.check_hangtime_expiry(queue);

        if let Some(tasks) = self.circuits.tick_start(dltime) {
            for task in tasks {
                match task {
                    CircuitMgrCmd::SendDSetup(call_id, usage, ts) => {
                        // Get our cached D-SETUP, build a prim and send it down the stack
                        let Some(cached) = self.cached_setups.get_mut(&call_id) else {
                            tracing::debug!(
                                "CMCE: skipping D-SETUP resend for call_id={} (no cached D-SETUP; likely Brew-routed individual call)",
                                call_id
                            );
                            continue;
                        };
                        if !cached.resend {
                            continue;
                        }
                        if let Some(receipt) = cached.tx_receipt.as_ref()
                            && !receipt.is_in_final_state()
                        {
                            tracing::debug!(
                                "CMCE: throttling D-SETUP resend for call_id={} while previous resend is {:?}",
                                call_id,
                                receipt.get_state()
                            );
                            continue;
                        }

                        // Update transmission_grant based on current call state:
                        // During NoActiveSpeaker (nobody transmitting), use NotGranted;
                        // during Transmitting, use GrantedToOtherUser.
                        if let Some(active) = self.active_calls.get(&call_id) {
                            cached.pdu.transmission_grant = if active.is_tx_active() {
                                TransmissionGrant::GrantedToOtherUser
                            } else {
                                TransmissionGrant::NotGranted
                            };
                        }
                        let dest_addr = cached.dest_addr;
                        let (sdu, chan_alloc) = Self::build_d_setup_prim(&cached.pdu, usage, ts, UlDlAssignment::Both);
                        let reporter = TxReporter::new_unacked();
                        let receipt = reporter.clone();
                        cached.tx_receipt = Some(receipt);
                        let prim = Self::build_sapmsg(sdu, Some(chan_alloc), self.dltime, dest_addr, Some(reporter));
                        queue.push_back(prim);
                    }

                    CircuitMgrCmd::SendClose(call_id, circuit) => {
                        tracing::warn!("need to send CLOSE for call id {}", call_id);
                        let ts = circuit.ts;
                        // Safety circuit expiry is not a setup timeout. Do not report it to
                        // handsets as ExpiryOfTimer, which many radios render as "No answer".
                        let disconnect_cause = DisconnectCause::SwmiRequestedDisconnection;

                        // Get our cached D-SETUP, build D-RELEASE and send
                        if let Some(cached) = self.cached_setups.get(&call_id) {
                            let sdu = Self::build_d_release_from_d_setup(&cached.pdu, disconnect_cause);
                            let prim = Self::build_sapmsg(sdu, None, self.dltime, cached.dest_addr, None);
                            queue.push_back(prim);

                            if let Some(ind_call) = self.individual_calls.get(&call_id) {
                                if !ind_call.calling_over_brew {
                                    let sdu_calling = Self::build_d_release_from_d_setup(&cached.pdu, disconnect_cause);
                                    let prim_calling = SapMsg {
                                        sap: Sap::LcmcSap,
                                        src: TetraEntity::Cmce,
                                        dest: TetraEntity::Mle,
                                        msg: SapMsgInner::LcmcMleUnitdataReq(LcmcMleUnitdataReq {
                                            sdu: sdu_calling,
                                            handle: ind_call.calling_handle,
                                            endpoint_id: ind_call.calling_endpoint_id,
                                            link_id: ind_call.calling_link_id,
                                            layer2service: Layer2Service::Todo,
                                            pdu_prio: 0,
                                            layer2_qos: 0,
                                            stealing_permission: false,
                                            stealing_repeats_flag: false,
                                            chan_alloc: None,
                                            main_address: ind_call.calling_addr,
                                            tx_reporter: None,
                                        }),
                                    };
                                    queue.push_back(prim_calling);
                                }
                            }
                        } else {
                            tracing::warn!("No cached D-SETUP for call id {} during timer-close", call_id);
                            if let Some(ind_call) = self.individual_calls.get(&call_id) {
                                if !ind_call.calling_over_brew {
                                    let sdu_calling = Self::build_d_release(call_id, disconnect_cause);
                                    let prim_calling = if ind_call.is_active() {
                                        Self::build_sapmsg_stealing(
                                            sdu_calling,
                                            self.dltime,
                                            ind_call.calling_addr,
                                            ind_call.calling_ts,
                                            Some(ind_call.calling_usage),
                                        )
                                    } else {
                                        Self::build_sapmsg_direct(
                                            sdu_calling,
                                            self.dltime,
                                            ind_call.calling_addr,
                                            ind_call.calling_handle,
                                            ind_call.calling_link_id,
                                            ind_call.calling_endpoint_id,
                                        )
                                    };
                                    queue.push_back(prim_calling);
                                } else if !ind_call.called_over_brew {
                                    let sdu_called = Self::build_d_release(call_id, disconnect_cause);
                                    let prim_called = if ind_call.is_active() {
                                        Self::build_sapmsg_stealing(
                                            sdu_called,
                                            self.dltime,
                                            ind_call.called_addr,
                                            ind_call.called_ts,
                                            Some(ind_call.called_usage),
                                        )
                                    } else if let (Some(handle), Some(link_id), Some(endpoint_id)) =
                                        (ind_call.called_handle, ind_call.called_link_id, ind_call.called_endpoint_id)
                                    {
                                        Self::build_sapmsg_direct(
                                            sdu_called,
                                            self.dltime,
                                            ind_call.called_addr,
                                            handle,
                                            link_id,
                                            endpoint_id,
                                        )
                                    } else {
                                        Self::build_sapmsg(sdu_called, None, self.dltime, ind_call.called_addr, None)
                                    };
                                    queue.push_back(prim_called);
                                }
                            }
                        }

                        if let Some(ind_call) = self.individual_calls.get(&call_id) {
                            if (ind_call.called_over_brew || ind_call.calling_over_brew)
                                && let Some(brew_uuid) = ind_call.brew_uuid
                            {
                                self.notify_network_circuit_release(queue, brew_uuid, disconnect_cause);
                            }
                        }

                        // Clean up call state
                        if let Some(call) = self.active_calls.get_mut(&call_id) {
                            call.begin_release(disconnect_cause);
                        }
                        if let Some(call) = self.individual_calls.get_mut(&call_id) {
                            call.begin_release(disconnect_cause);
                        }
                        self.cached_setups.remove(&call_id);
                        self.active_calls.remove(&call_id);
                        self.individual_calls.remove(&call_id);

                        // Signal UMAC to release the circuit
                        Self::signal_umac_circuit_close(queue, circuit, self.dltime);
                        self.release_timeslot(ts);
                    }
                }
            }
        }
    }

    /// Release active calls when their configured call timeout expires.
    pub(super) fn check_call_timeout_expiry(&mut self, queue: &mut MessageQueue) {
        let expired_group_calls: Vec<u16> = self
            .active_calls
            .iter()
            .filter_map(|(&call_id, call)| call.call_timeout_expired(self.dltime).then_some(call_id))
            .collect();

        for call_id in expired_group_calls {
            tracing::info!("Call timeout expired for group call_id={}, releasing", call_id);
            self.release_group_call(queue, call_id, DisconnectCause::SwmiRequestedDisconnection);
        }

        let expired_individual_calls: Vec<u16> = self
            .individual_calls
            .iter()
            .filter_map(|(&call_id, call)| call.active_timeout_expired(self.dltime).then_some(call_id))
            .collect();

        for call_id in expired_individual_calls {
            tracing::info!("Call timeout expired for individual call_id={}, releasing", call_id);
            self.release_individual_call(queue, call_id, DisconnectCause::SwmiRequestedDisconnection);
        }
    }

    /// Release individual setup attempts that exceed setup timeout.
    pub(super) fn check_individual_setup_timeout(&mut self, queue: &mut MessageQueue) {
        let expired_setup_calls: Vec<u16> = self
            .individual_calls
            .iter()
            .filter_map(|(&call_id, call)| call.setup_timeout_expired(self.dltime).then_some(call_id))
            .collect();

        for call_id in expired_setup_calls {
            tracing::info!("Setup timeout expired for individual call_id={}, releasing", call_id);
            self.release_individual_call(queue, call_id, DisconnectCause::ExpiryOfTimer);
        }
    }

    /// Check if any active calls in NoActiveSpeaker (hangtime) have expired and release them.
    pub(super) fn check_hangtime_expiry(&mut self, queue: &mut MessageQueue) {
        // NoActiveSpeaker (hangtime): 5 multiframes = ~5 seconds
        const HANGTIME_FRAMES: i32 = 5 * 18 * 4;

        let expired: Vec<u16> = self
            .active_calls
            .iter()
            .filter_map(|(&call_id, call)| match call.state() {
                GroupCallState::NoActiveSpeaker { since } if since.age(self.dltime) > HANGTIME_FRAMES => Some(call_id),
                _ => None,
            })
            .collect();

        for call_id in expired {
            tracing::info!("Hangtime expired for call_id={}, releasing", call_id);
            self.release_group_call(queue, call_id, DisconnectCause::SwmiRequestedDisconnection);
        }
    }

    /// Handle UL inactivity timeout from UMAC: a radio disappeared mid-transmission.
    /// Force the group floor to released and enter hangtime.
    pub(super) fn handle_ul_inactivity_timeout(&mut self, queue: &mut MessageQueue, ts: u8) {
        let call_id = self
            .active_calls
            .iter()
            .find(|(_, call)| call.ts == ts && call.is_tx_active())
            .map(|(call_id, _)| *call_id);

        let Some(call_id) = call_id else {
            let individual_floor = self.individual_calls.iter().find_map(|(&call_id, call)| {
                if !call.is_active() || !call.is_simplex() {
                    return None;
                }

                match call.floor_holder {
                    Some(issi) if issi == call.calling_addr.ssi && call.calling_ts == ts => Some((call_id, call.calling_addr)),
                    Some(issi) if issi == call.called_addr.ssi && call.called_ts == ts => Some((call_id, call.called_addr)),
                    _ => None,
                }
            });

            if let Some((call_id, sender)) = individual_floor {
                tracing::warn!(
                    "UL inactivity timeout on ts={}, forcing simplex individual TX ceased for call_id={}",
                    ts,
                    call_id
                );
                self.fsm_on_u_tx_ceased(
                    queue,
                    sender,
                    UTxCeased {
                        call_identifier: call_id,
                        facility: None,
                        dm_ms_address: None,
                        proprietary: None,
                    },
                );
                return;
            }

            tracing::debug!("UL inactivity timeout on ts={} but no active transmitting call found", ts);
            return;
        };

        let Some(call) = self.active_calls.get_mut(&call_id) else {
            return;
        };

        tracing::warn!("UL inactivity timeout on ts={}, forcing TX ceased for call_id={}", ts, call_id);
        let dest_gssi = call.dest_gssi;
        call.enter_hangtime(self.dltime);

        self.send_d_tx_ceased_facch(queue, call_id, dest_gssi, ts);

        self.notify_floor_released(
            queue,
            CallTimeslot { call_id, ts },
            true,
            BrewNotification::IfGroupRoutable(dest_gssi),
        );
    }
}
