use crate::net_control::ControlEndpoint;
use crate::net_telemetry::channel::TelemetrySink;
use crate::{MessageQueue, TetraEntityTrait};
use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::typed_pdu_fields::Type3FieldGeneric;
use tetra_core::{BitBuffer, Layer2Service, Sap, TdmaTime, TetraAddress, unimplemented_log};
use tetra_saps::lmm::{LmmMleActivateConf, LmmMleUnitdataReq};
use tetra_saps::{SapMsg, SapMsgInner};

use tetra_pdus::mm::enums::location_update_type::LocationUpdateType;
use tetra_pdus::mm::enums::mm_pdu_type_dl::MmPduTypeDl;
use tetra_pdus::mm::enums::reject_cause::RejectCause;
use tetra_pdus::mm::enums::type34_elem_id_ul::MmType34ElemIdUl;
use tetra_pdus::mm::pdus::d_location_update_accept::DLocationUpdateAccept;
use tetra_pdus::mm::pdus::d_location_update_command::DLocationUpdateCommand;
use tetra_pdus::mm::pdus::d_location_update_reject::DLocationUpdateReject;
use tetra_pdus::mm::pdus::u_location_update_demand::ULocationUpdateDemand;
use tetra_pdus::mm::pdus::u_itsi_detach::UItsiDetach;
use tetra_pdus::mm::fields::class_of_ms::ClassOfMs;
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
    /// OUTBOUND external-interface handle (stack -> UI): TNMM indications
    /// (ETSI TS 100 392-2 cl. 15.3) are pushed here in later phases. Wired in
    /// Phase T0; `None` when no telemetry endpoint is configured.
    telemetry: Option<TelemetrySink>,
    /// INBOUND external-interface handle (UI -> stack): TNMM requests
    /// (cl. 15.3) are received here in later phases. Wired in Phase T0; `None`
    /// when no control endpoint is configured.
    control: Option<ControlEndpoint>,
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
    pub fn new(config: SharedConfig, telemetry: Option<TelemetrySink>, control: Option<ControlEndpoint>) -> Self {
        Self {
            config,
            telemetry,
            control,
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

    /// Own Mobile Network Identity (MNI) as the 24-bit value carried in the
    /// address extension information element (ETSI EN 300 392-1 clause 7): the
    /// 10-bit MCC in the high bits followed by the 14-bit MNC. Taken from the
    /// configured home network (`[net_info]`), which is the MNI of the MS ITSI.
    fn own_mni(&self) -> u32 {
        let net = &self.config.config().net;
        ((net.mcc as u32) << 14) | (net.mnc as u32 & 0x3FFF)
    }

    /// Build the "class of MS" element (ETSI TS 100 392-2 cl. 16.10.5, Table
    /// 16.31) describing this MS's radio capabilities. Clause 16.4.3 requires
    /// the U-LOCATION-UPDATE-DEMAND sent in response to a D-LOCATION-UPDATE-
    /// COMMAND to contain this element.
    ///
    /// The bits reflect the *actual* capabilities of this clear-mode voice SDR
    /// implementation — they are not invented capabilities: only `voice` is
    /// asserted, and `e2e_encryption_not_supported` is set (reversed polarity:
    /// true = E2E encryption **not** supported, which is correct — Part 7
    /// security is out of scope). All data / advanced-link / encryption /
    /// authentication / alternative-modulation capabilities are `false` because
    /// the stack does not implement them. `air_interface_version` = 3 is the AI
    /// protocol version in use on this network (directly observed from the
    /// reference radio registering against this same BS); it is an interworking
    /// version field, not a fabricated capability.
    fn build_class_of_ms(&self) -> ClassOfMs {
        ClassOfMs {
            freq_simplex_duplex: false,
            multislot_phase_mod: false,
            concurrent_multicarrier: false,
            voice: true,
            e2e_encryption_not_supported: true,
            circuit_mode_data: false,
            tetra_packet_data: false,
            fast_switching: false,
            dck_encryption: false,
            clch_needed: false,
            concurrent_circuit_mode: false,
            original_advanced_link: false,
            minimum_mode: false,
            carrier_specific_signalling: false,
            authentication: false,
            sck_encryption: false,
            air_interface_version: 3,
            common_scch: false,
            reserved_21: false,
            mac_d_blck: false,
            extended_advanced_link: false,
            d8psk: false,
        }
    }

    /// Build a "group report response" element (ETSI TS 100 392-2 cl. 16.10.27a,
    /// Table 16.59) indicating "group report complete" (length 1 bit, value 0).
    /// Sent in the command-response demand when all reported groups are carried
    /// in that single PDU (cl. 16.4.3).
    fn group_report_complete() -> Type3FieldGeneric {
        Type3FieldGeneric {
            field_id: MmType34ElemIdUl::GroupReportResponse as u64,
            len: 1,
            data: vec![0],
        }
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

    /// Handle a D-LOCATION-UPDATE-COMMAND (ETSI TS 100 392-2 cl. 16.4.3): the
    /// SwMI has demanded that the MS (re)registers ("infrastructure initiated
    /// registration"). On receipt, if the MNI in the address extension matches
    /// the MS's own MNI (or is absent), the MS responds with a
    /// U-LOCATION-UPDATE-DEMAND of type "demand location updating" and starts
    /// T351.
    fn rx_d_location_update_command(&mut self, queue: &mut MessageQueue, mut sdu: BitBuffer) {
        let pdu = match DLocationUpdateCommand::from_bitbuf(&mut sdu) {
            Ok(pdu) => pdu,
            Err(e) => {
                tracing::warn!("Failed parsing DLocationUpdateCommand: {:?} {}", e, sdu.dump_bin());
                return;
            }
        };

        // cl. 16.4.3: "extract the MNI from the address extension information
        // element. If the MNI is the same as the MNI of the MS ITSI (or the MNI
        // is not present), then [proceed]". A command carrying a different MNI is
        // not for us.
        if let Some(mni) = pdu.address_extension {
            if mni != self.own_mni() as u64 {
                tracing::debug!(
                    "MM: <- D-LOCATION-UPDATE-COMMAND for MNI {} != own MNI {}; ignoring",
                    mni,
                    self.own_mni()
                );
                return;
            }
        }

        if self.left_system {
            tracing::debug!("MM: D-LOCATION-UPDATE-COMMAND received but MS left the system (N351); ignoring");
            return;
        }
        if self.reg_state == RegState::Detaching {
            tracing::debug!("MM: D-LOCATION-UPDATE-COMMAND received while detaching; ignoring");
            return;
        }

        // cl. 16.4.3: if the "cell type control" element is present the MS would
        // update its cell-type preference (cl. 16.4.12) and, if the serving cell
        // is no longer permitted, issue an MLE-LINK request to reselect a cell
        // (possibly MLE-CLOSE / out of service). That MLE cell-type plumbing is
        // not implemented; this BS sends no cell-type control, so we log and
        // proceed to register on the current serving cell.
        if pdu.cell_type_control.is_some() {
            tracing::warn!(
                "MM: D-LOCATION-UPDATE-COMMAND carries a cell type control element; \
                 cell-type-preference update / MLE-LINK reselection (cl. 16.4.12) is not \
                 implemented — proceeding with the current serving cell"
            );
        }

        tracing::info!(
            "MM: <- D-LOCATION-UPDATE-COMMAND (infrastructure-initiated registration), \
             group_report_request={}",
            pdu.group_identity_report
        );

        self.attempts = 0;
        self.send_demand_location_update(queue, pdu.group_identity_report);
    }

    /// Build and send the U-LOCATION-UPDATE-DEMAND that answers a
    /// D-LOCATION-UPDATE-COMMAND (ETSI TS 100 392-2 cl. 16.4.3). Distinct from
    /// the plain ITSI-attach demand (`send_location_update_demand`): per
    /// cl. 16.4.3 this demand carries the MNI in the address extension and the
    /// ISSI in the SSI element (the true ITSI), the class of MS element, a
    /// location-update type of "demand location updating", and group handling
    /// that depends on whether the command requested a group report.
    fn send_demand_location_update(&mut self, queue: &mut MessageQueue, group_report_requested: bool) {
        let issi = self.own_issi();
        let groups = self.attach_groups();

        // Group handling (cl. 16.4.3):
        //  - With a group report request: the MS shall regard all group
        //    identities as no longer attached. If it wishes to keep receiving
        //    group signalling it may include the attachments here; the first
        //    attachment uses attach/detach mode "detach all + attach defined"
        //    (which `build_group_identity_location_demand` already sets, value 1)
        //    and report "not report request". As all reported groups fit in this
        //    single PDU, a "group report response = group report complete"
        //    element is included. If there are no groups, only the "group report
        //    complete" element is sent. (Timer T353 would start here; the group
        //    identity response timer is a later MM slice — R3.)
        //  - Without a group report request: the MS may still include an
        //    attachment request for its configured groups (optional), and no
        //    group report response element is sent.
        let (group_identity_location_demand, group_report_response) = if group_report_requested {
            (
                self.build_group_identity_location_demand(),
                Some(Self::group_report_complete()),
            )
        } else {
            (self.build_group_identity_location_demand(), None)
        };

        // cl. 16.4.3: location update type = "demand location updating" (the MS
        // is enabled; "disabled MS updating" would be used if the MS were
        // disabled — enable/disable is EN 300 392-7, out of scope). The demand
        // shall contain the MNI (address extension) and ISSI (SSI element) — the
        // true ITSI — the class of MS element, shall not request to append the
        // LA, and shall not carry LA information (this is not a forward
        // registration). Extended capabilities are omitted (the MS supports none
        // of the listed items beyond CA); no energy saving mode (not activated).
        let pdu = ULocationUpdateDemand {
            location_update_type: LocationUpdateType::DemandLocationUpdating,
            request_to_append_la: false,
            cipher_control: false,
            ciphering_parameters: None,
            class_of_ms: Some(self.build_class_of_ms()),
            energy_saving_mode: None,
            la_information: None,
            ssi: Some(issi as u64),
            address_extension: Some(self.own_mni() as u64),
            group_identity_location_demand,
            group_report_response,
            authentication_uplink: None,
            extended_capabilities: None,
            proprietary: None,
        };

        let mut sdu = BitBuffer::new_autoexpand(4);
        pdu.to_bitbuf(&mut sdu)
            .expect("U-LOCATION-UPDATE-DEMAND (demand location updating) serialization");
        sdu.seek(0);
        tracing::info!(
            "MM: -> U-LOCATION-UPDATE-DEMAND (demand location updating) ISSI {} MNI {} \
             group_report_requested {} attach_groups {:?} sdu {}",
            issi,
            self.own_mni(),
            group_report_requested,
            groups,
            sdu.dump_bin()
        );

        // NOTE: cl. 16.4.3 assigns this PDU priority 6 (or 3 when a group report
        // response is included). The LMM-UNITDATA request primitive carries no
        // PDU-priority parameter in this stack, so the priority cannot be
        // signalled to lower layers — a documented limitation, not an on-air
        // deviation of the PDU contents.
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

    fn rx_lmm_mle_unitdata_ind(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
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
            MmPduTypeDl::DLocationUpdateCommand => {
                self.rx_d_location_update_command(queue, prim.sdu.clone())
            }
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

    /// Drain and handle inbound external-interface commands (UI -> stack).
    ///
    /// Wired in Phase T0 (MS interface enabler): the control endpoint is polled
    /// once per downlink tick, mirroring `MmBs::tick_start`. No MS-side control
    /// commands are defined yet — TNMM requests (ETSI TS 100 392-2 cl. 15.3) are
    /// added in Phase T2 — so any command that arrives now is logged and
    /// dropped rather than acted upon. Unlike the BS placeholder handler this
    /// does not panic: commands originate from an external UI process and an
    /// unexpected variant must not be able to crash the MS stack.
    fn poll_control(&mut self, _queue: &mut MessageQueue) {
        let Some(cep) = &self.control else {
            return;
        };
        while let Some(cmd) = cep.try_recv() {
            tracing::warn!("MM(MS): received control command with no MS handler yet, dropping: {:?}", cmd);
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
        // Drain any inbound external-interface (UI -> stack) commands first.
        // Phase T0 wires the endpoint but defines no MS control commands yet;
        // TNMM requests (ETSI TS 100 392-2 cl. 15.3) are handled here in T2.
        self.poll_control(queue);

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
    use crate::net_control::ControlCommand;
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
        MmMs::new(SharedConfig::from_parts(cfg, None), None, None)
    }

    fn ms_mm_with_groups(groups: &[u32]) -> MmMs {
        let list = groups.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
        let toml = MS_TOML.replace("attach_groups = []", &format!("attach_groups = [{list}]"));
        let cfg = from_toml_str(&toml).expect("valid MS test config");
        MmMs::new(SharedConfig::from_parts(cfg, None), None, None)
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

    /// Own MNI for the test config (mcc=901, mnc=9999): (901<<14)|9999.
    const MS_MNI: u64 = (901 << 14) | 9999;

    /// Build a D-LOCATION-UPDATE-COMMAND (cl. 16.9.2.8) with the given group
    /// report request flag and optional MNI address extension. Minimal: no
    /// ciphering, no cell type control, no proprietary element (as this BS
    /// sends).
    fn build_command(group_report: bool, address_extension: Option<u64>) -> BitBuffer {
        let pdu = DLocationUpdateCommand {
            group_identity_report: group_report,
            cipher_control: false,
            ciphering_parameters: None,
            address_extension,
            cell_type_control: None,
            proprietary: None,
        };
        let mut sdu = BitBuffer::new_autoexpand(8);
        pdu.to_bitbuf(&mut sdu).unwrap();
        sdu
    }

    /// A D-LOCATION-UPDATE-COMMAND (infrastructure-initiated registration,
    /// cl. 16.4.3) makes MM send a U-LOCATION-UPDATE-DEMAND of type "demand
    /// location updating" that contains the MNI (address extension), the ISSI
    /// (SSI element) and the class of MS element, and it must round-trip through
    /// the exact parser the BS uses.
    #[test]
    fn test_command_triggers_demand_location_updating() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();

        deliver_dl(&mut mm, &mut q, build_command(false, None));
        assert_eq!(mm.reg_state, RegState::Registering);
        assert_eq!(mm.attempts, 1);

        let msg = q.pop_front().expect("a demand must be emitted");
        assert!(q.pop_front().is_none(), "exactly one message emitted");
        let SapMsgInner::LmmMleUnitdataReq(mut req) = msg.msg else {
            panic!("expected LmmMleUnitdataReq");
        };
        assert_eq!(req.address.ssi, MS_ISSI);

        req.sdu.seek(0);
        let pdu = ULocationUpdateDemand::from_bitbuf(&mut req.sdu).expect("BS must parse the demand");
        assert_eq!(pdu.location_update_type, LocationUpdateType::DemandLocationUpdating);
        assert!(!pdu.request_to_append_la, "cl. 16.4.3: shall not request append LA");
        assert!(pdu.la_information.is_none(), "cl. 16.4.3: LA information shall be absent");
        assert_eq!(pdu.ssi, Some(MS_ISSI as u64), "cl. 16.4.3: ISSI in SSI element");
        assert_eq!(pdu.address_extension, Some(MS_MNI), "cl. 16.4.3: MNI in address extension");
        let class = pdu.class_of_ms.expect("cl. 16.4.3: class of MS shall be present");
        assert!(class.voice, "voice-capable radio");
        assert!(class.e2e_encryption_not_supported, "no E2E encryption");
        assert!(pdu.group_report_response.is_none(), "no group report requested");
        assert!(pdu.group_identity_location_demand.is_none(), "no groups configured");
        assert_eq!(req.sdu.get_len_remaining(), 0, "demand fully consumed");
    }

    /// A command WITH a group report request makes MM regard groups as
    /// un-attached and, since all reported groups fit in this one PDU, include a
    /// "group report response = group report complete" element (cl. 16.4.3 /
    /// 16.10.27a). Configured groups are (re)attached in the same demand.
    #[test]
    fn test_command_with_group_report_request() {
        let mut mm = ms_mm_with_groups(&[91]);
        let mut q = MessageQueue::new();

        deliver_dl(&mut mm, &mut q, build_command(true, Some(MS_MNI)));
        let msg = q.pop_front().expect("a demand must be emitted");
        let SapMsgInner::LmmMleUnitdataReq(mut req) = msg.msg else {
            panic!("expected LmmMleUnitdataReq");
        };
        req.sdu.seek(0);
        let pdu = ULocationUpdateDemand::from_bitbuf(&mut req.sdu).expect("BS must parse the demand");

        let grr = pdu.group_report_response.expect("group report response present");
        assert_eq!(grr.len, 1, "group report response is 1 bit");
        assert_eq!(grr.data, vec![0], "value 0 = group report complete (Table 16.59)");

        let gild = pdu
            .group_identity_location_demand
            .expect("configured group re-attached in the demand");
        assert_eq!(gild.group_identity_attach_detach_mode, 1);
        let gssis: Vec<u32> = gild
            .group_identity_uplink
            .expect("group entries present")
            .iter()
            .filter_map(|g| g.gssi)
            .collect();
        assert_eq!(gssis, vec![91]);
        assert_eq!(req.sdu.get_len_remaining(), 0, "demand fully consumed");
    }

    /// With a group report request but no configured groups, MM sends only the
    /// "group report complete" element and no group identity location demand
    /// (cl. 16.4.3: "If the MS has no groups to attach, it shall send
    /// U-LOCATION UPDATE DEMAND PDU containing a group report response
    /// information element indicating 'group report complete'").
    #[test]
    fn test_command_group_report_no_groups() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();

        deliver_dl(&mut mm, &mut q, build_command(true, None));
        let msg = q.pop_front().expect("a demand must be emitted");
        let SapMsgInner::LmmMleUnitdataReq(mut req) = msg.msg else {
            panic!("expected LmmMleUnitdataReq");
        };
        req.sdu.seek(0);
        let pdu = ULocationUpdateDemand::from_bitbuf(&mut req.sdu).expect("BS must parse the demand");
        assert!(pdu.group_report_response.is_some(), "group report complete sent");
        assert!(pdu.group_identity_location_demand.is_none(), "no groups to attach");
    }

    /// A command whose MNI address extension does not match the MS's own MNI is
    /// not for this MS and shall be ignored (cl. 16.4.3), leaving MM idle.
    #[test]
    fn test_command_wrong_mni_ignored() {
        let mut mm = ms_mm();
        let mut q = MessageQueue::new();

        deliver_dl(&mut mm, &mut q, build_command(false, Some(MS_MNI ^ 0x1)));
        assert_eq!(mm.reg_state, RegState::Idle, "command for a different MNI ignored");
        assert!(q.pop_front().is_none(), "no demand emitted for a foreign MNI");
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

    // --- Phase T0: MS external-interface wiring (enabler; no TNMM behaviour) ---

    /// Build an MS MM wired to a telemetry sink and a control endpoint, and
    /// return the far ends (telemetry source + command dispatcher) so a test can
    /// act as the external UI process.
    fn ms_mm_wired() -> (MmMs, crate::net_telemetry::TelemetrySource, crate::net_control::CommandDispatcher) {
        use crate::net_control::channel::make_control_link;
        use crate::net_telemetry::telemetry_channel;
        let cfg = from_toml_str(MS_TOML).expect("valid MS test config");
        let (sink, source) = telemetry_channel();
        let (dispatcher, endpoint) = make_control_link();
        let mm = MmMs::new(SharedConfig::from_parts(cfg, None), Some(sink), Some(endpoint));
        (mm, source, dispatcher)
    }

    /// T0: `MmMs::new` accepts and stores the telemetry sink + control endpoint
    /// (mirroring `MmBs`), so `build_ms_stack` can wire the external interface.
    #[test]
    fn test_ms_mm_accepts_telemetry_and_control() {
        let (mm, _source, _dispatcher) = ms_mm_wired();
        assert!(mm.telemetry.is_some(), "telemetry sink wired");
        assert!(mm.control.is_some(), "control endpoint wired");
    }

    /// T0: MM drains the control endpoint each tick. No MS control command is
    /// defined yet, so an inbound command must be consumed and dropped without
    /// panicking or altering registration state (cl. 15.3 requests land in T2).
    #[test]
    fn test_ms_mm_drains_control_without_panic() {
        let (mut mm, _source, dispatcher) = ms_mm_wired();
        let mut q = MessageQueue::new();

        dispatcher.send(ControlCommand::CommandA { handle: 7, parameter: 1 });
        mm.tick_start(&mut q, TdmaTime::default());

        // Command consumed (nothing left for the endpoint to receive), state
        // unchanged, and no uplink PDU produced as a side effect.
        assert_eq!(mm.reg_state, RegState::Idle, "control command must not change reg state");
        assert!(q.pop_front().is_none(), "no PDU emitted for an unhandled control command");
    }
}
