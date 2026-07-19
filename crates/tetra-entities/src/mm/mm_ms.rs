use crate::{MessageQueue, TetraEntityTrait};
use tetra_config::bluestation::SharedConfig;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, Layer2Service, Sap, TdmaTime, TetraAddress, unimplemented_log};
use tetra_saps::lmm::{LmmMleActivateConf, LmmMleUnitdataReq};
use tetra_saps::{SapMsg, SapMsgInner};

use tetra_pdus::mm::enums::location_update_type::LocationUpdateType;
use tetra_pdus::mm::enums::mm_pdu_type_dl::MmPduTypeDl;
use tetra_pdus::mm::pdus::d_location_update_accept::DLocationUpdateAccept;
use tetra_pdus::mm::pdus::d_location_update_reject::DLocationUpdateReject;
use tetra_pdus::mm::pdus::u_location_update_demand::ULocationUpdateDemand;

/// Number of downlink timeslots to wait for a location update response before
/// retransmitting the U-LOCATION-UPDATE-DEMAND. One TDMA multiframe is 18 frames
/// × 4 slots ≈ 1.02 s, so this is roughly 4 s — long enough for the MAC random
/// access attempt (cl. 23.5.1.4, up to Nu retransmissions) plus the BS response
/// to complete. This is an implementation guard around the MM registration
/// procedure (cl. 16.4); it is not a value transmitted on air.
const REGISTRATION_RETRY_SLOTS: u32 = 288;

/// Maximum number of U-LOCATION-UPDATE-DEMAND transmissions for one registration
/// attempt before MM gives up and returns to idle (awaiting a fresh cell
/// selection). Guards against transmitting forever on a cell that never answers.
const MAX_REGISTRATION_ATTEMPTS: u8 = 5;

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
}

pub struct MmMs {
    config: SharedConfig,
    /// Registration procedure state (cl. 16.4).
    reg_state: RegState,
    /// Downlink slots remaining before retransmitting the demand while in
    /// `Registering`.
    retry_countdown: u32,
    /// Number of demands sent for the current registration attempt.
    attempts: u8,
}

impl MmMs {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            config,
            reg_state: RegState::Idle,
            retry_countdown: 0,
            attempts: 0,
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

    /// LMM-ACTIVATE confirmation from MLE (cl. 17.3.2): a serving cell has been
    /// selected. Start the registration procedure if the cell requires it.
    fn rx_activate_conf(&mut self, queue: &mut MessageQueue, conf: &LmmMleActivateConf) {
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
            group_identity_location_demand: None,
            group_report_response: None,
            authentication_uplink: None,
            extended_capabilities: None,
            proprietary: None,
        };

        let mut sdu = BitBuffer::new_autoexpand(4);
        pdu.to_bitbuf(&mut sdu).expect("U-LOCATION-UPDATE-DEMAND serialization");
        sdu.seek(0);
        tracing::info!(
            "MM: -> U-LOCATION-UPDATE-DEMAND (ITSI attach) for ISSI {} sdu {}",
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

        self.reg_state = RegState::Registering;
        self.retry_countdown = REGISTRATION_RETRY_SLOTS;
        self.attempts += 1;
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
        self.reg_state = RegState::Registered;
        self.retry_countdown = 0;
    }

    /// Handle D-LOCATION-UPDATE-REJECT (cl. 16.9.2.9): the SwMI refused our
    /// registration. Log the cause and abandon this attempt; a later cell
    /// (re)selection re-arms the procedure.
    fn rx_d_location_update_reject(&mut self, mut sdu: BitBuffer) {
        let pdu = match DLocationUpdateReject::from_bitbuf(&mut sdu) {
            Ok(pdu) => pdu,
            Err(e) => {
                tracing::warn!("Failed parsing DLocationUpdateReject: {:?} {}", e, sdu.dump_bin());
                return;
            }
        };
        tracing::warn!(
            "MM: <- D-LOCATION-UPDATE-REJECT type={:?} reject_cause={}: registration rejected",
            pdu.location_update_type,
            pdu.reject_cause
        );
        self.reg_state = RegState::Idle;
        self.retry_countdown = 0;
    }
}

impl TetraEntityTrait for MmMs {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Mm
    }

    fn set_config(&mut self, config: SharedConfig) {
        self.config = config;
    }

    fn tick_start(&mut self, queue: &mut MessageQueue, _ts: TdmaTime) {
        // Drive the registration retransmission timer (cl. 16.4). Only active
        // while awaiting a response to a sent demand.
        if self.reg_state != RegState::Registering {
            return;
        }
        if self.retry_countdown > 0 {
            self.retry_countdown -= 1;
            return;
        }
        if self.attempts >= MAX_REGISTRATION_ATTEMPTS {
            tracing::error!(
                "MM: registration failed after {} attempts; abandoning until next cell (re)selection",
                self.attempts
            );
            self.reg_state = RegState::Idle;
            return;
        }
        tracing::warn!(
            "MM: registration response timeout; retransmitting U-LOCATION-UPDATE-DEMAND (attempt {})",
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetra_config::bluestation::from_toml_str;
    use tetra_saps::lmm::LmmMleUnitdataInd;
    use tetra_pdus::mm::pdus::d_location_update_accept::DLocationUpdateAccept;

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
        for _ in 0..=REGISTRATION_RETRY_SLOTS {
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

        // Countdown slots: no retransmit until the tick after it reaches zero.
        for _ in 0..=REGISTRATION_RETRY_SLOTS {
            mm.tick_start(&mut q, TdmaTime::default());
        }
        let msg = q.pop_front().expect("demand retransmitted after timeout");
        assert!(matches!(msg.msg, SapMsgInner::LmmMleUnitdataReq(_)));
        assert_eq!(mm.attempts, 2);
    }
}
