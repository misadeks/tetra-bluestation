use crate::{MessageQueue, TetraEntityTrait};
use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, Layer2Service, Sap, TdmaTime, TetraAddress, unimplemented_log};
use tetra_saps::lmm::{LmmMleActivateConf, LmmMleUnitdataReq};
use tetra_saps::{SapMsg, SapMsgInner};

use tetra_pdus::mm::enums::location_update_type::LocationUpdateType;
use tetra_pdus::mm::enums::mm_pdu_type_dl::MmPduTypeDl;
use tetra_pdus::mm::enums::reject_cause::RejectCause;
use tetra_pdus::mm::pdus::d_location_update_accept::DLocationUpdateAccept;
use tetra_pdus::mm::pdus::d_location_update_reject::DLocationUpdateReject;
use tetra_pdus::mm::pdus::u_location_update_demand::ULocationUpdateDemand;
use tetra_pdus::mm::pdus::u_itsi_detach::UItsiDetach;
use tetra_pdus::mm::fields::group_identity_location_demand::GroupIdentityLocationDemand;
use tetra_pdus::mm::fields::group_identity_uplink::GroupIdentityUplink;

/// Timer T351 — "Registration response time" (ETSI TS 100 392-2 cl. 16.11.1.1):
/// the maximum time MM waits for a response (D-LOCATION-UPDATE-ACCEPT /
/// -PROCEEDING / -REJECT) to a registration request. The spec value is **10 s**.
/// Expressed here in downlink timeslots: one TETRA timeslot lasts 85/6 ≈ 14.167 ms
/// (cl. 9.4.4), so 10 s / 14.167 ms ≈ 706 slots. The MM `tick_start` hook is
/// driven once per received downlink slot, so this is the on-air timer value, not
/// an invented guard. On expiry MM may resend the demand (cl. 16.4.5).
const T351_TIMEOUT_SLOTS: u32 = 706;

/// Implementation cap on how many times MM resends the U-LOCATION-UPDATE-DEMAND
/// after successive T351 expiries before abandoning the attempt and returning to
/// idle to await a fresh cell (re)selection. Clause 16.4.5 permits MM, on T351
/// expiry, to "resend the U-LOCATION UPDATE DEMAND" and notes the MS "may wish to
/// select a new serving cell before further registration attempts"; the spec does
/// not fix a resend count, so this bound is an implementation choice (not an
/// on-air value). It is distinct from N351 below, which counts *rejections*.
const MAX_REGISTRATION_ATTEMPTS: u8 = 5;

/// N351 — "Maximum system rejection count" (ETSI TS 100 392-2 cl. 16.11.2.1):
/// when an MS has received N351 registration rejections of type "system rejection"
/// without a successful registration on a system, it shall leave the system and
/// shall not attempt to register again on that system until after a power cycle.
/// N351 has a range 1..4 with a **default value of 4**.
const N351_MAX_SYSTEM_REJECTIONS: u8 = 4;

/// Number of downlink timeslots to keep the stack running after a U-ITSI DETACH
/// has been queued at shutdown, so the MAC random-access procedure (cl. 23.5.1.4)
/// and the PHY have time to actually transmit the burst before the SDR streams
/// close. One TDMA multiframe (18 frames × 4 slots ≈ 1.02 s) is ~72 slots; this
/// is ~2 multiframes ≈ 2 s, enough for at least one random-access opportunity and
/// a retransmission. De-registration is best-effort (cl. 16.6.1: MM proceeds
/// whether the DLL reports the PDU "successfully or unsuccessfully transmitted"),
/// so this is a bounded drain, not a wait for acknowledgement. It is an
/// implementation guard, not a value transmitted on air.
const DETACH_DRAIN_SLOTS: u32 = 144;

/// Class of usage sent for each group attached at ITSI attach (ETSI TS 100 392-2
/// cl. 16.10.6, Table 16.32; the raw 3-bit field value). Per the standard the
/// class of usage "has meaning only for the user application" — the SwMI simply
/// echoes it back in the accept — so a fixed value is spec-valid. Value 4 matches
/// the reference (Motorola) radio observed attaching against this same BS.
const GROUP_CLASS_OF_USAGE: u8 = 4;

/// MS registration state (ETSI TS 100 392-2 cl. 16.4 location updating /
/// ITSI attach).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegState {
    /// Not registered and not currently attempting to register (no cell
    /// selected yet, or a previous attempt was abandoned).
    Idle,
    /// A U-LOCATION-UPDATE-DEMAND has been sent; awaiting a response
    /// (D-LOCATION-UPDATE-ACCEPT / -REJECT). Retransmits on timeout.
    Registering,
    /// Registration accepted by the SwMI.
    Registered,
    /// De-registration in progress: a U-ITSI DETACH has been sent at shutdown
    /// and the stack is being drained so the burst can be transmitted
    /// (cl. 16.6.1). Terminal — the MS is stopping.
    Detaching,
}

pub struct MmMs {
    config: SharedConfig,
    /// Registration procedure state (cl. 16.4).
    reg_state: RegState,
    /// Timer T351 (registration response time, cl. 16.11.1.1) countdown in
    /// downlink slots. Started when a demand is sent, stopped on ACCEPT/REJECT.
    /// On expiry MM may resend the demand (cl. 16.4.5).
    t351_countdown: u32,
    /// Number of demands sent for the current registration attempt.
    attempts: u8,
    /// Count of registration rejections of type "system rejection" received
    /// without an intervening successful registration (cl. 16.11.2.1, N351).
    /// When it reaches `N351_MAX_SYSTEM_REJECTIONS` the MS leaves the system.
    system_rejection_count: u8,
    /// Set once the MS has left the system after N351 system rejections
    /// (cl. 16.11.2.1): no further registration is attempted on this system
    /// until a power cycle (process restart).
    left_system: bool,
    /// Downlink slots remaining to drain after sending a U-ITSI DETACH at
    /// shutdown, giving the MAC/PHY time to transmit it (cl. 16.6.1).
    detach_countdown: u32,
}

impl MmMs {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            config,
            reg_state: RegState::Idle,
            t351_countdown: 0,
            attempts: 0,
            system_rejection_count: 0,
            left_system: false,
            detach_countdown: 0,
        }
    }

    /// Own Individual Short Subscriber Identity (ISSI) from the MS config
    /// section. This is the MS's own address used as the source of the uplink.
    fn own_issi(&self) -> u32 {
        self.config
            .config()
            .ms
            .as_ref()
            .expect("MS config section required in MS mode")
            .issi
    }

    /// Group identities (GSSIs) this MS is configured to attach to at
    /// registration (ETSI TS 100 392-2 cl. 16.8 group identity attachment).
    fn attach_groups(&self) -> Vec<u32> {
        self.config
            .config()
            .ms
            .as_ref()
            .map(|ms| ms.attach_groups.clone())
            .unwrap_or_default()
    }

    /// Build the group identity location demand element carried inside the
    /// U-LOCATION-UPDATE-DEMAND at ITSI attach (cl. 16.10.24 / 16.8.2), from the
    /// configured `attach_groups`. Returns `None` when no groups are configured
    /// (the element is then omitted, as before).
    ///
    /// The attach/detach mode is set to "detach all currently attached group
    /// identities and attach the group identities defined here" (cl. 16.10.17,
    /// Table 16.49, value 1): at ITSI attach nothing is yet attached, so this
    /// cleanly establishes exactly the configured set. Each group is an
    /// attachment (`class_of_usage` present, `group_identity_detachment_uplink`
    /// absent) carried as a plain GSSI (address type 0). This mirrors the
    /// reference radio, which the BS accepts.
    fn build_group_identity_location_demand(&self) -> Option<GroupIdentityLocationDemand> {
        let groups = self.attach_groups();
        if groups.is_empty() {
            return None;
        }
        Some(GroupIdentityLocationDemand {
            group_identity_attach_detach_mode: 1,
            group_identity_uplink: Some(
                groups
                    .iter()
                    .map(|&gssi| GroupIdentityUplink {
                        class_of_usage: Some(GROUP_CLASS_OF_USAGE),
                        group_identity_detachment_uplink: None,
                        gssi: Some(gssi),
                        address_extension: None,
                        vgssi: None,
                    })
                    .collect(),
            ),
        })
    }

    /// LMM-ACTIVATE confirmation from MLE (cl. 17.3.2): a serving cell has been
    /// selected. Start the registration procedure if the cell requires it.
    fn rx_activate_conf(&mut self, queue: &mut MessageQueue, conf: &LmmMleActivateConf) {
        if self.left_system {
            // Left the system after N351 system rejections (cl. 16.11.2.1); no
            // further registration until a power cycle (process restart).
            tracing::debug!("MM: activate-conf received but MS left the system (N351); ignoring");
            return;
        }
        if !conf.registration_required {
            tracing::info!(
                "MM: serving cell (LA={}) does not require registration; not registering",
                conf.la
            );
            return;
        }
        if self.reg_state == RegState::Registered {
            // Already registered on a cell; a repeated confirmation for the same
            // cell needs no action. (Cell change resets MLE's confirmation.)
            tracing::debug!("MM: activate-conf received while already registered; ignoring");
            return;
        }
        if self.reg_state == RegState::Detaching {
            // Shutting down; do not start a new registration.
            tracing::debug!("MM: activate-conf received while detaching; ignoring");
            return;
        }
        tracing::info!("MM: serving cell selected (LA={}), initiating ITSI attach registration", conf.la);
        self.attempts = 0;
        self.send_location_update_demand(queue);
    }

    /// Build and send a U-LOCATION-UPDATE-DEMAND (ITSI attach, cl. 16.9.3.4) down
    /// to MLE. MLE prepends its protocol discriminator and forwards it to LLC,
    /// from where it reaches the MAC and is transmitted on the uplink via random
    /// access (cl. 23.5.1.4).
    fn send_location_update_demand(&mut self, queue: &mut MessageQueue) {
        let issi = self.own_issi();

        // Attach the configured group identities as part of the ITSI attach
        // (cl. 16.8.2): the SwMI affiliates the MS to these groups and will then
        // forward group-addressed traffic to it. Omitted when none configured.
        let group_identity_location_demand = self.build_group_identity_location_demand();

        // Minimal ITSI-attach demand: no ciphering, no optional elements. The
        // MS identity is carried by the MAC-layer source address, so the ssi
        // element is left absent (cl. 16.9.3.4 note 2 / BS accepts this).
        let pdu = ULocationUpdateDemand {
            location_update_type: LocationUpdateType::ItsiAttach,
            request_to_append_la: false,
            cipher_control: false,
            ciphering_parameters: None,
            class_of_ms: None,
            energy_saving_mode: None,
            la_information: None,
            ssi: None,
            address_extension: None,
            group_identity_location_demand,
            group_report_response: None,
            authentication_uplink: None,
            extended_capabilities: None,
            proprietary: None,
        };

        let mut sdu = BitBuffer::new_autoexpand(4);
        pdu.to_bitbuf(&mut sdu).expect("U-LOCATION-UPDATE-DEMAND serialization");
        sdu.seek(0);
        tracing::info!(
            "MM: -> U-LOCATION-UPDATE-DEMAND (ITSI attach) for ISSI {}, attach_groups {:?} sdu {}",
            issi,
            self.attach_groups(),
            sdu.dump_bin()
        );

        let m = SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mm,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LmmMleUnitdataReq(LmmMleUnitdataReq {
                sdu,
                handle: 0,
                address: TetraAddress::issi(issi),
                layer2service: Layer2Service::Acknowledged,
                stealing_permission: false,
                stealing_repeats_flag: false,
                encryption_flag: false,
                is_null_pdu: false,
                tx_reporter: None,
            }),
        };
        queue.push_back(m);

        self.reg_state = RegState::Registering;
        self.t351_countdown = T351_TIMEOUT_SLOTS;
        self.attempts += 1;
    }

    /// Build and send a U-ITSI DETACH (cl. 16.9.3.3) down to MLE as part of the
    /// de-registration procedure (cl. 16.6.1). Sent with an MLE-UNITDATA request
    /// over acknowledged basic link (TL-DATA, per Figure 16.7); the MS identity
    /// is carried by the MAC-layer source address, so the optional MNI address
    /// extension is omitted (cl. 16.6.1 note: it cannot fully safeguard the
    /// identity anyway as there is no ISSI element in the PDU). De-registration
    /// is best-effort — MM does not wait for a D-MM STATUS response.
    fn send_itsi_detach(&mut self, queue: &mut MessageQueue) {
        let issi = self.own_issi();

        // Minimal detach: no MNI address extension, no proprietary element.
        let pdu = UItsiDetach {
            address_extension: None,
            proprietary: None,
        };

        let mut sdu = BitBuffer::new_autoexpand(4);
        pdu.to_bitbuf(&mut sdu).expect("U-ITSI DETACH serialization");
        sdu.seek(0);
        tracing::info!(
            "MM: -> U-ITSI DETACH (de-registration) for ISSI {} sdu {}",
            issi,
            sdu.dump_bin()
        );

        let m = SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mm,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LmmMleUnitdataReq(LmmMleUnitdataReq {
                sdu,
                handle: 0,
                address: TetraAddress::issi(issi),
                layer2service: Layer2Service::Acknowledged,
                stealing_permission: false,
                stealing_repeats_flag: false,
                encryption_flag: false,
                is_null_pdu: false,
                tx_reporter: None,
            }),
        };
        queue.push_back(m);
    }

    fn rx_lmm_mle_unitdata_ind(&mut self, _queue: &mut MessageQueue, mut message: SapMsg) {
        let SapMsgInner::LmmMleUnitdataInd(prim) = &mut message.msg else {
            panic!()
        };

        let Some(bits) = prim.sdu.peek_bits(4) else {
            tracing::warn!("insufficient bits: {}", prim.sdu.dump_bin());
            return;
        };

        let Ok(pdu_type) = MmPduTypeDl::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, prim.sdu.dump_bin());
            return;
        };

        match pdu_type {
            MmPduTypeDl::DOtar => unimplemented_log!("DOtar"),
            MmPduTypeDl::DAuthentication => unimplemented_log!("DAuthentication"),
            MmPduTypeDl::DCkChangeDemand => unimplemented_log!("DCkChangeDemand"),
            MmPduTypeDl::DDisable => unimplemented_log!("DDisable"),
            MmPduTypeDl::DEnable => unimplemented_log!("DEnable"),
            MmPduTypeDl::DLocationUpdateAccept => self.rx_d_location_update_accept(prim.sdu.clone()),
            MmPduTypeDl::DLocationUpdateCommand => unimplemented_log!("DLocationUpdateCommand"),
            MmPduTypeDl::DLocationUpdateReject => self.rx_d_location_update_reject(prim.sdu.clone()),
            MmPduTypeDl::DLocationUpdateProceeding => unimplemented_log!("DLocationUpdateProceeding"),
            MmPduTypeDl::DAttachDetachGroupIdentity => unimplemented_log!("DAttachDetachGroupIdentity"),
            MmPduTypeDl::DAttachDetachGroupIdentityAcknowledgement => {
                unimplemented_log!("DAttachDetachGroupIdentityAcknowledgement")
            }
            MmPduTypeDl::DMmStatus => unimplemented_log!("DMmStatus"),
            MmPduTypeDl::MmPduFunctionNotSupported => unimplemented_log!("MmPduFunctionNotSupported"),
        };
    }

    /// Handle D-LOCATION-UPDATE-ACCEPT (cl. 16.9.2.7): the SwMI has accepted our
    /// registration. Mark the MS as registered and stop the retry timer.
    fn rx_d_location_update_accept(&mut self, mut sdu: BitBuffer) {
        let pdu = match DLocationUpdateAccept::from_bitbuf(&mut sdu) {
            Ok(pdu) => pdu,
            Err(e) => {
                tracing::warn!("Failed parsing DLocationUpdateAccept: {:?} {}", e, sdu.dump_bin());
                return;
            }
        };

        if self.reg_state == RegState::Registered {
            tracing::debug!("MM: <- D-LOCATION-UPDATE-ACCEPT (duplicate); already registered");
            return;
        }

        tracing::info!(
            "MM: <- D-LOCATION-UPDATE-ACCEPT type={:?} ssi={:?}: registration COMPLETE",
            pdu.location_update_accept_type,
            pdu.ssi
        );

        // Report the outcome of the group identity attachment requested in the
        // demand (cl. 16.8.2 / 16.10.24). The SwMI returns the accepted groups
        // (each carrying a group_identity_attachment) and/or rejected ones. We
        // log the result for observability; RX group filtering continues to use
        // the configured attach_groups (dynamic granted-set tracking is a future
        // refinement).
        if let Some(accept) = &pdu.group_identity_location_accept {
            let attached: Vec<u32> = accept
                .group_identity_downlink
                .as_ref()
                .map(|v| {
                    v.iter()
                        .filter(|g| g.group_identity_attachment.is_some())
                        .filter_map(|g| g.gssi)
                        .collect()
                })
                .unwrap_or_default();
            tracing::info!(
                "MM: group attachment result (accept_reject={}): attached GSSIs {:?}",
                accept.group_identity_accept_reject,
                attached
            );
        } else if !self.attach_groups().is_empty() {
            // cl. 16.4.1.1: "In case the group identity location accept
            // information element is not present in the D-LOCATION UPDATE ACCEPT
            // PDU, the MS shall assume the group attachment/detachment failed.
            // The MS shall treat the failure as equivalent to T353 timer expiry."
            // The ITSI registration itself is still accepted; only the group
            // attachment failed. (T353 handling / re-attach is part of the group
            // identity procedures, cl. 16.8 — a later MM slice.)
            tracing::warn!(
                "MM: registration accepted but no group identity location accept returned; \
                 assuming group attachment failed (cl. 16.4.1.1, equivalent to T353 expiry) \
                 for configured groups {:?}",
                self.attach_groups()
            );
        }

        // Successful registration: stop timer T351 (cl. 16.4.1.1) and clear the
        // N351 system-rejection counter (cl. 16.11.2.1 counts rejections "without
        // a successful registration").
        self.reg_state = RegState::Registered;
        self.t351_countdown = 0;
        self.system_rejection_count = 0;
    }

    /// Handle D-LOCATION-UPDATE-REJECT (cl. 16.9.2.9 / 16.4.1.1): the SwMI
    /// refused our registration. Per cl. 16.4.1.1 the reject is acted upon only
    /// while timer T351 (or T354) is active; MM stops T351/T352, then analyses
    /// the reject cause and either re-tries, abandons the cell (awaiting cell
    /// reselection), or — after N351 "system rejection" results — leaves the
    /// system (cl. 16.11.2.1).
    fn rx_d_location_update_reject(&mut self, mut sdu: BitBuffer) {
        let pdu = match DLocationUpdateReject::from_bitbuf(&mut sdu) {
            Ok(pdu) => pdu,
            Err(e) => {
                tracing::warn!("Failed parsing DLocationUpdateReject: {:?} {}", e, sdu.dump_bin());
                return;
            }
        };

        // cl. 16.4.1.1: the reject is processed only if timer T351 (or T354) is
        // active — i.e. we are awaiting a registration response. A stray reject
        // outside a registration attempt is ignored.
        if self.reg_state != RegState::Registering {
            tracing::debug!(
                "MM: <- D-LOCATION-UPDATE-REJECT with no registration outstanding (T351 not active); ignoring"
            );
            return;
        }

        // Stop timers T351 and T352 (cl. 16.4.1.1).
        self.t351_countdown = 0;

        let cause = RejectCause::try_from(pdu.reject_cause as u64);
        match &cause {
            Ok(c) => tracing::warn!(
                "MM: <- D-LOCATION-UPDATE-REJECT type={:?} reject_cause={} ({})",
                pdu.location_update_type,
                pdu.reject_cause,
                c
            ),
            Err(_) => tracing::warn!(
                "MM: <- D-LOCATION-UPDATE-REJECT type={:?} unknown reject_cause={}",
                pdu.location_update_type,
                pdu.reject_cause
            ),
        }

        // TNMM-REGISTRATION indication ("failure" + reject cause, cl. 16.4.1.1)
        // would be issued to the user application here; there is no user-app
        // consumer of the TNMM-SAP in this stack, so it is logged above.

        let action = cause.map(Self::analyse_reject_cause).unwrap_or(RejectAction::Abandon);
        match action {
            RejectAction::Retry => {
                if self.attempts < MAX_REGISTRATION_ATTEMPTS {
                    tracing::info!(
                        "MM: reject cause permits re-try; resending U-LOCATION-UPDATE-DEMAND (attempt {})",
                        self.attempts + 1
                    );
                    // send_location_update_demand re-arms T351 and Registering.
                    // (caller state already Registering)
                    self.resend_after_reject();
                } else {
                    tracing::warn!(
                        "MM: reject cause permits re-try but resend cap ({}) reached; abandoning",
                        MAX_REGISTRATION_ATTEMPTS
                    );
                    self.reg_state = RegState::Idle;
                }
            }
            RejectAction::Abandon => {
                // The spec issues an MLE-UPDATE request (LA / cell-type / cell
                // rejection) so MLE runs cell reselection (cl. 18.3.4.7). MLE-side
                // reselection is a later slice; MM abandons this attempt and awaits
                // the next LMM-ACTIVATE confirmation.
                tracing::info!("MM: reject cause requires cell reselection / abandon; returning to idle");
                self.reg_state = RegState::Idle;
            }
            RejectAction::SystemRejection => {
                self.system_rejection_count = self.system_rejection_count.saturating_add(1);
                if self.system_rejection_count >= N351_MAX_SYSTEM_REJECTIONS {
                    tracing::error!(
                        "MM: reached N351={} system rejections; leaving the system (cl. 16.11.2.1), \
                         no further registration until power cycle",
                        N351_MAX_SYSTEM_REJECTIONS
                    );
                    self.left_system = true;
                } else {
                    tracing::warn!(
                        "MM: system rejection {}/{}; abandoning attempt, MLE cell reselection pending",
                        self.system_rejection_count,
                        N351_MAX_SYSTEM_REJECTIONS
                    );
                }
                self.reg_state = RegState::Idle;
            }
        }
    }

    /// Resend the U-LOCATION-UPDATE-DEMAND after a recoverable reject cause. Kept
    /// separate from the timeout resend so the log context differs; both re-arm
    /// T351 and increment the attempt count via `send_location_update_demand`.
    fn resend_after_reject(&mut self) {
        // A fresh MessageQueue is not available here; the reject handler is
        // invoked from rx_lmm_mle_unitdata_ind which does not thread the queue
        // down. The demand is instead re-sent from tick_start on the next slot by
        // leaving reg_state = Registering with an expired T351. Setting the
        // countdown to 0 makes tick_start resend immediately (cl. 16.4.5).
        self.reg_state = RegState::Registering;
        self.t351_countdown = 0;
    }

    /// Classify a D-LOCATION-UPDATE-REJECT cause into the action MM must take,
    /// strictly per the reject-cause analysis in cl. 16.4.1.1. Causes not
    /// enumerated there for normal registration (e.g. security/ciphering causes,
    /// which belong to ETSI EN 300 392-7) are treated as `Abandon` and logged;
    /// they are not counted as N351 "system rejections".
    fn analyse_reject_cause(cause: RejectCause) -> RejectAction {
        match cause {
            // "may re-try registration after a suitable time" (else cell rejection).
            RejectCause::Congestion | RejectCause::NetworkFailure => RejectAction::Retry,
            // "the MS shall be allowed at least one registration re-try".
            RejectCause::MandatoryElementError | RejectCause::MessageConsistencyError => {
                RejectAction::Retry
            }
            // "cell type rejection" → MLE-UPDATE → cell reselection.
            RejectCause::UseCaCellNotPermitted | RejectCause::UseDaCellNotPermitted => {
                RejectAction::Abandon
            }
            // "LA rejection" → MLE-UPDATE → cell reselection.
            RejectCause::LaNotAllowed
            | RejectCause::ServiceNotSubscribed
            | RejectCause::RoamingNotSupported => RejectAction::Abandon,
            // "system rejection" type causes (counted against N351). Our MAC
            // header carries the ISSI (no ASSI/(V)ASSI has been issued), so the
            // ITSI/ATSI-unknown ASSI-retry branch does not apply.
            RejectCause::ItsiAtsiUnknown
            | RejectCause::IllegalMs
            | RejectCause::MigrationNotSupported => RejectAction::SystemRejection,
            // "not applicable to normal registration".
            RejectCause::ForwardRegistrationFailure => RejectAction::Abandon,
            // All other causes (LA unknown, and the security/ciphering causes
            // defined in ETSI EN 300 392-7) are outside the scope of the normal
            // registration reject analysis of cl. 16.4.1.1.
            _ => RejectAction::Abandon,
        }
    }
}

/// Action MM takes after analysing a D-LOCATION-UPDATE-REJECT cause
/// (ETSI TS 100 392-2 cl. 16.4.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RejectAction {
    /// Recoverable cause — MM may re-try the registration.
    Retry,
    /// Cause requires abandoning the cell and awaiting cell reselection
    /// (the spec's LA / cell-type / cell rejection MLE-UPDATE results).
    Abandon,
    /// A registration result of type "system rejection" — counts toward N351;
    /// once N351 is reached the MS leaves the system (cl. 16.11.2.1).
    SystemRejection,
}

impl TetraEntityTrait for MmMs {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Mm
    }

    fn set_config(&mut self, config: SharedConfig) {
        self.config = config;
    }

    fn tick_start(&mut self, queue: &mut MessageQueue, _ts: TdmaTime) {
        // While detaching at shutdown, count down the bounded drain that gives
        // the MAC/PHY time to transmit the U-ITSI DETACH (cl. 16.6.1).
        if self.reg_state == RegState::Detaching {
            self.detach_countdown = self.detach_countdown.saturating_sub(1);
            return;
        }

        // Drive timer T351 (registration response time, cl. 16.11.1.1). Only
        // active while awaiting a response to a sent demand. On expiry MM resends
        // the U-LOCATION-UPDATE-DEMAND (cl. 16.4.5 permits resending), bounded by
        // an implementation resend cap, after which it returns to idle to await a
        // fresh cell (re)selection.
        if self.reg_state != RegState::Registering {
            return;
        }
        if self.t351_countdown > 0 {
            self.t351_countdown -= 1;
            return;
        }
        if self.attempts >= MAX_REGISTRATION_ATTEMPTS {
            tracing::error!(
                "MM: registration failed after {} attempts (T351 expiries); abandoning until next cell (re)selection",
                self.attempts
            );
            self.reg_state = RegState::Idle;
            return;
        }
        tracing::warn!(
            "MM: T351 expired with no registration response; resending U-LOCATION-UPDATE-DEMAND (attempt {})",
            self.attempts + 1
        );
        self.send_location_update_demand(queue);
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);

        // There is only one SAP for MM
        assert!(message.sap == Sap::LmmSap);

        match &message.msg {
            SapMsgInner::LmmMleUnitdataInd(_) => {
                self.rx_lmm_mle_unitdata_ind(queue, message);
            }
            SapMsgInner::LmmMleActivateConf(conf) => {
                let conf = conf.clone();
                self.rx_activate_conf(queue, &conf);
            }
            _ => {
                panic!();
            }
        }
    }

    /// Shutdown hook (cl. 16.6.1): if the MS is registered, emit a U-ITSI DETACH
    /// and ask the router to keep running so it can be transmitted. Returns
    /// `true` only when a detach was actually initiated.
    fn begin_deregistration(&mut self, queue: &mut MessageQueue) -> bool {
        if self.reg_state != RegState::Registered {
            tracing::info!("MM: shutdown while not registered; no de-registration needed");
            return false;
        }
        tracing::info!("MM: shutdown while registered; sending U-ITSI DETACH (de-registration)");
        self.send_itsi_detach(queue);
        self.reg_state = RegState::Detaching;
        self.detach_countdown = DETACH_DRAIN_SLOTS;
        true
    }

    fn deregistration_pending(&self) -> bool {
        self.reg_state == RegState::Detaching && self.detach_countdown > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetra_config::bluestation::from_toml_str;
    use tetra_saps::lmm::LmmMleUnitdataInd;
    use tetra_pdus::mm::pdus::d_location_update_accept::DLocationUpdateAccept;
    use tetra_pdus::mm::pdus::u_itsi_detach::UItsiDetach;

    const MS_ISSI: u32 = 1000001;

    const MS_TOML: &str = r#"
config_version = "0.6"
stack_mode = "Ms"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
tx_freq = 439825000
rx_freq = 430425000
ppm_err = 0
device = "driver=sx"
sample_rate = 600000
rx_antenna = "RX"
tx_antenna = "TX"
rx_gain_lna = 48.0
rx_gain_pga = 8.0
tx_gain_dac = 0.0
tx_gain_mixer = 0.0

[net_info]
mcc = 901
mnc = 9999

[cell_info]
freq_band = 4
main_carrier = 1593
duplex_spacing = 7
custom_duplex_spacing = 9400000
freq_offset = 0
reverse_operation = false
location_area = 1
colour_code = 1

[ms]
issi = 1000001
subscriber_class = 1
attach_groups = []
"#;

    fn ms_mm() -> MmMs {
        let cfg = from_toml_str(MS_TOML).expect("valid MS test config");
        MmMs::new(SharedConfig::from_parts(cfg, None))
    }

    fn ms_mm_with_groups(groups: &[u32]) -> MmMs {
        let list = groups.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
        let toml = MS_TOML.replace("attach_groups = []", &format!("attach_groups = [{list}]"));
        let cfg = from_toml_str(&toml).expect("valid MS test config");
        MmMs::new(SharedConfig::from_parts(cfg, None))
    }

    fn activate_conf(registration_required: bool) -> LmmMleActivateConf {
        LmmMleActivateConf {
            registration_required,
            la: 1,
            cell_type: 0,
        }
    }

    /// Deliver a downlink MM PDU (already positioned at the MM PDU type) to MM as
    /// an LMM-UNITDATA indication, mirroring what MLE forwards.
    fn deliver_dl(mm: &mut MmMs, queue: &mut MessageQueue, mut sdu: BitBuffer) {
        sdu.seek(0);
        let msg = SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mle,
            dest: TetraEntity::Mm,
            msg: SapMsgInner::LmmMleUnitdataInd(LmmMleUnitdataInd {
                sdu,
                handle: 0,
                received_address: TetraAddress::issi(MS_ISSI),
            }),
        };
        mm.rx_prim(queue, msg);
    }

    fn build_accept() -> BitBuffer {
        let pdu = DLocationUpdateAccept {
            location_update_accept_type: LocationUpdateType::ItsiAttach,
            ssi: Some(MS_ISSI as u64),
            address_extension: None,
            subscriber_class: None,
            energy_saving_information: None,
            scch_information_and_distribution_on_18th_frame: None,
            new_registered_area: None,
            security_downlink: None,
            group_identity_location_accept: None,
            default_group_attachment_lifetime: None,
            authentication_downlink: None,
            group_identity_security_related_information: None,
            cell_type_control: None,
            proprietary: None,
        };
        let mut sdu = BitBuffer::new_autoexpand(8);
        pdu.to_bitbuf(&mut sdu).unwrap();
        sdu
    }

    /// The demand MM emits on cell selection must be a valid, BS-acceptable
    /// U-LOCATION-UPDATE-DEMAND (ITSI attach, no ciphering, no optional fields):
    /// it round-trips through the exact parser the BS uses (cl. 16.9.3.4).
    #[test]
    fn test_registration_demand_on_activate_conf() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();

        mm.rx_activate_conf(&mut q, &activate_conf(true));
        assert_eq!(mm.reg_state, RegState::Registering);
        assert_eq!(mm.attempts, 1);

        let msg = q.pop_front().expect("a demand must be emitted");
        assert!(q.pop_front().is_none(), "exactly one message emitted");
        assert_eq!(msg.sap, Sap::LmmSap);
        assert_eq!(msg.dest, TetraEntity::Mle);
        let SapMsgInner::LmmMleUnitdataReq(mut req) = msg.msg else {
            panic!("expected LmmMleUnitdataReq");
        };
        assert_eq!(req.address.ssi, MS_ISSI);

        // The BS parses exactly this on the uplink; it must decode cleanly.
        req.sdu.seek(0);
        let pdu = ULocationUpdateDemand::from_bitbuf(&mut req.sdu).expect("BS must parse the demand");
        assert_eq!(pdu.location_update_type, LocationUpdateType::ItsiAttach);
        assert!(!pdu.request_to_append_la);
        assert!(!pdu.cipher_control);
        assert!(pdu.ciphering_parameters.is_none());
        assert!(pdu.class_of_ms.is_none());
        assert!(pdu.group_identity_location_demand.is_none());
        assert_eq!(req.sdu.get_len_remaining(), 0, "demand fully consumed");
    }

    /// With groups configured, the ITSI-attach demand must carry a group identity
    /// location demand element (cl. 16.8.2 / 16.10.24) attaching exactly those
    /// GSSIs — mode 1 ("detach all + attach these"), each an attachment with a
    /// class of usage — and it must round-trip through the BS parser.
    #[test]
    fn test_registration_demand_carries_group_attachment() {
        let mut mm = ms_mm_with_groups(&[91, 220]);
        let mut q = MessageQueue::new();

        mm.rx_activate_conf(&mut q, &activate_conf(true));

        let msg = q.pop_front().expect("a demand must be emitted");
        let SapMsgInner::LmmMleUnitdataReq(mut req) = msg.msg else {
            panic!("expected LmmMleUnitdataReq");
        };
        req.sdu.seek(0);
        let pdu = ULocationUpdateDemand::from_bitbuf(&mut req.sdu).expect("BS must parse the demand");

        let gild = pdu
            .group_identity_location_demand
            .expect("group identity location demand present");
        assert_eq!(gild.group_identity_attach_detach_mode, 1);
        let ul = gild.group_identity_uplink.expect("group entries present");
        assert_eq!(ul.len(), 2);
        let gssis: Vec<u32> = ul.iter().filter_map(|g| g.gssi).collect();
        assert_eq!(gssis, vec![91, 220]);
        assert!(ul.iter().all(|g| g.class_of_usage == Some(GROUP_CLASS_OF_USAGE)));
        assert!(ul.iter().all(|g| g.group_identity_detachment_uplink.is_none()));
        assert_eq!(req.sdu.get_len_remaining(), 0, "demand fully consumed");
    }

    /// If the serving cell does not require registration, MM stays idle.
    #[test]
    fn test_no_registration_when_not_required() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(false));
        assert_eq!(mm.reg_state, RegState::Idle);
        assert!(q.pop_front().is_none(), "no demand when registration not required");
    }

    /// A D-LOCATION-UPDATE-ACCEPT completes the registration and stops retries.
    #[test]
    fn test_registration_accept_completes() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();

        deliver_dl(&mut mm, &mut q, build_accept());
        assert_eq!(mm.reg_state, RegState::Registered);

        // No further retransmission even after the retry window elapses.
        for _ in 0..=T351_TIMEOUT_SLOTS {
            mm.tick_start(&mut q, TdmaTime::default());
        }
        assert!(q.pop_front().is_none(), "registered MS must not retransmit");
    }

    /// With no response, MM retransmits the demand after the retry window.
    #[test]
    fn test_registration_retransmits_on_timeout() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        assert_eq!(mm.attempts, 1);

        // Countdown slots: no retransmit until the tick after T351 reaches zero.
        for _ in 0..=T351_TIMEOUT_SLOTS {
            mm.tick_start(&mut q, TdmaTime::default());
        }
        let msg = q.pop_front().expect("demand retransmitted after timeout");
        assert!(matches!(msg.msg, SapMsgInner::LmmMleUnitdataReq(_)));
        assert_eq!(mm.attempts, 2);
    }

    /// Build a D-LOCATION-UPDATE-REJECT with the given raw reject cause.
    fn build_reject(cause: u8) -> BitBuffer {
        let pdu = DLocationUpdateReject {
            location_update_type: LocationUpdateType::ItsiAttach,
            reject_cause: cause,
            cipher_control: false,
            ciphering_parameters: None,
            address_extension: None,
            cell_type_control: None,
            proprietary: None,
        };
        let mut sdu = BitBuffer::new_autoexpand(8);
        pdu.to_bitbuf(&mut sdu).unwrap();
        sdu
    }

    /// A recoverable reject cause (Congestion) makes MM resend the demand
    /// (cl. 16.4.1.1: "may re-try registration"); attempt count increases.
    #[test]
    fn test_reject_recoverable_cause_retries() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        assert_eq!(mm.attempts, 1);

        deliver_dl(&mut mm, &mut q, build_reject(RejectCause::Congestion as u8));
        // Reject-driven resend is deferred to the next tick (T351 set to 0).
        assert_eq!(mm.reg_state, RegState::Registering);
        mm.tick_start(&mut q, TdmaTime::default());
        let msg = q.pop_front().expect("demand resent after recoverable reject");
        assert!(matches!(msg.msg, SapMsgInner::LmmMleUnitdataReq(_)));
        assert_eq!(mm.attempts, 2);
    }

    /// An LA-rejection cause (LA not allowed) abandons the attempt → Idle,
    /// awaiting a fresh cell (re)selection (cl. 16.4.1.1).
    #[test]
    fn test_reject_la_cause_abandons() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();

        deliver_dl(&mut mm, &mut q, build_reject(RejectCause::LaNotAllowed as u8));
        assert_eq!(mm.reg_state, RegState::Idle);
        assert!(!mm.left_system);
        // No resend on the next tick.
        mm.tick_start(&mut q, TdmaTime::default());
        assert!(q.pop_front().is_none());
    }

    /// After N351 "system rejection" causes (Illegal MS) without a successful
    /// registration, the MS leaves the system and ignores further activate-confs
    /// (cl. 16.11.2.1).
    #[test]
    fn test_reject_system_rejection_leaves_after_n351() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        for i in 1..=N351_MAX_SYSTEM_REJECTIONS {
            mm.rx_activate_conf(&mut q, &activate_conf(true));
            let _ = q.pop_front();
            deliver_dl(&mut mm, &mut q, build_reject(RejectCause::IllegalMs as u8));
            assert_eq!(mm.system_rejection_count, i);
        }
        assert!(mm.left_system, "MS must leave the system after N351 system rejections");
        // Further cell selection must not trigger a new registration.
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        assert_eq!(mm.reg_state, RegState::Idle);
        assert!(q.pop_front().is_none(), "no registration after leaving the system");
    }

    /// A successful registration clears the accumulated system-rejection count
    /// (cl. 16.11.2.1 counts rejections "without a successful registration").
    #[test]
    fn test_accept_resets_system_rejection_count() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        deliver_dl(&mut mm, &mut q, build_reject(RejectCause::IllegalMs as u8));
        assert_eq!(mm.system_rejection_count, 1);

        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        deliver_dl(&mut mm, &mut q, build_accept());
        assert_eq!(mm.reg_state, RegState::Registered);
        assert_eq!(mm.system_rejection_count, 0);
    }

    /// On shutdown while registered, MM emits a valid, BS-parseable U-ITSI DETACH
    /// (cl. 16.6.1 / 16.9.3.3) and enters the bounded detach drain.
    #[test]
    fn test_detach_on_shutdown_when_registered() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();

        // Get to the Registered state.
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        deliver_dl(&mut mm, &mut q, build_accept());
        assert_eq!(mm.reg_state, RegState::Registered);

        // Shutdown: a detach must be initiated.
        assert!(mm.begin_deregistration(&mut q), "detach must be initiated when registered");
        assert_eq!(mm.reg_state, RegState::Detaching);
        assert!(mm.deregistration_pending());

        let msg = q.pop_front().expect("a U-ITSI DETACH must be emitted");
        assert!(q.pop_front().is_none(), "exactly one message emitted");
        assert_eq!(msg.sap, Sap::LmmSap);
        assert_eq!(msg.dest, TetraEntity::Mle);
        let SapMsgInner::LmmMleUnitdataReq(mut req) = msg.msg else {
            panic!("expected LmmMleUnitdataReq");
        };
        assert_eq!(req.address.ssi, MS_ISSI);
        assert_eq!(req.layer2service, Layer2Service::Acknowledged);

        // The BS parses exactly this on the uplink; it must decode cleanly as a
        // minimal detach (no MNI, no proprietary).
        req.sdu.seek(0);
        let pdu = UItsiDetach::from_bitbuf(&mut req.sdu).expect("BS must parse the detach");
        assert!(pdu.address_extension.is_none());
        assert!(pdu.proprietary.is_none());
        assert_eq!(req.sdu.get_len_remaining(), 0, "detach fully consumed");
    }

    /// The detach drain counts down over `DETACH_DRAIN_SLOTS` ticks, then reports
    /// no longer pending so the router can stop.
    #[test]
    fn test_detach_drain_bounded() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        mm.rx_activate_conf(&mut q, &activate_conf(true));
        let _ = q.pop_front();
        deliver_dl(&mut mm, &mut q, build_accept());
        assert!(mm.begin_deregistration(&mut q));
        let _ = q.pop_front();

        // Pending for the full drain window, then clears.
        for _ in 0..DETACH_DRAIN_SLOTS {
            assert!(mm.deregistration_pending(), "still draining");
            mm.tick_start(&mut q, TdmaTime::default());
        }
        assert!(!mm.deregistration_pending(), "drain complete");

        // No accidental uplinks emitted during the drain.
        assert!(q.pop_front().is_none(), "detach drain emits nothing further");
    }

    /// On shutdown while not registered, MM does not attempt a detach.
    #[test]
    fn test_no_detach_when_not_registered() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();
        assert!(!mm.begin_deregistration(&mut q), "no detach when idle");
        assert_eq!(mm.reg_state, RegState::Idle);
        assert!(!mm.deregistration_pending());
        assert!(q.pop_front().is_none(), "no PDU emitted when not registered");
    }
}
