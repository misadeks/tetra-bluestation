use crate::config::stack_config::SharedConfig;
use crate::common::messagerouter::MessageQueue;
use crate::entities::mm::enums::mm_location_update_type::MmLocationUpdateType;
use crate::entities::mm::fields::group_identity_location_accept::GroupIdentityLocationAccept;
use crate::entities::mm::fields::group_identity_uplink::GroupIdentityUplink;
use crate::saps::lmm::LmmMleUnitdataReq;
use crate::saps::sapmsg::{SapMsg, SapMsgInner};
use crate::common::address::{SsiType, TetraAddress};
use crate::common::bitbuffer::BitBuffer;
use crate::common::tdma_time::TdmaTime;
use crate::common::online;
use crate::common::group_registry;
use crate::entities::mm::components::client_state::MmClientMgr;
use crate::entities::mm::enums::mm_pdu_type_ul::MmPduTypeUl;
use crate::entities::mm::fields::group_identity_attachment::GroupIdentityAttachment;
use crate::entities::mm::fields::group_identity_downlink::GroupIdentityDownlink;
use crate::entities::mm::pdus::d_attach_detach_group_identity_acknowledgement::DAttachDetachGroupIdentityAcknowledgement;
use crate::entities::mm::pdus::d_attach_detach_group_identity::DAttachDetachGroupIdentity;
use crate::entities::mm::pdus::d_location_update_accept::DLocationUpdateAccept;
use crate::entities::mm::pdus::u_attach_detach_group_identity::UAttachDetachGroupIdentity;
use crate::entities::mm::pdus::u_attach_detach_group_identity_acknowledgement::UAttachDetachGroupIdentityAcknowledgement;
use crate::entities::mm::pdus::u_itsi_detach::UItsiDetach;
use crate::entities::mm::pdus::u_location_update_demand::ULocationUpdateDemand;
use crate::entities::TetraEntityTrait;
use crate::common::tetra_common::Sap;
use crate::common::tetra_entities::TetraEntity;
use crate::unimplemented_log;

use crate::common::dgna_job;
use crate::saps::dgna::{DgnaOp, DgnaSetReq};

fn dgna_detachment_uplink_reason(code: u8) -> &'static str {
    match code {
        0 => "unknown group identity",
        1 => "no valid encryption key (end-to-end)",
        2 => "user initiated",
        3 => "capacity exceeded",
        _ => "unknown",
    }
}


pub struct MmBs {
    config: SharedConfig,
    pub client_mgr: MmClientMgr,
}

impl MmBs {
    pub fn new(config: SharedConfig) -> Self {
        Self { config, client_mgr: MmClientMgr::new() }
    }

    fn rx_u_itsi_detach(&mut self, _queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_u_itsi_detach: {:?}", message);
        let SapMsgInner::LmmMleUnitdataInd(prim) = &mut message.msg else {panic!()};
        
        let _pdu = match UItsiDetach::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing UItsiDetach: {:?} {}", e, prim.sdu.dump_bin());
                return;
            }
        };

        let ssi = prim.received_address.ssi;
        let detached_client = self.client_mgr.remove_client(ssi);
        if detached_client.is_none() {
            tracing::warn!("Received UItsiDetach for unknown client with SSI: {}", ssi);
            // return;
        };
        group_registry::detach_all(ssi);
    }

    fn rx_u_location_update_demand(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_location_update_demand: {:?}", message);
        let SapMsgInner::LmmMleUnitdataInd(prim) = &mut message.msg else {panic!()};

        // Best-effort online tracking: any MM location update implies the SSI is active.
        online::record_seen(prim.received_address.ssi);

        let pdu = match ULocationUpdateDemand::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing ULocationUpdateDemand: {:?} {}", e, prim.sdu.dump_bin());
                return;
            }
        };

        // Check if we can satisfy this request, print unsupported stuff
        if !Self::feature_check_u_location_update_demand(&pdu) {
            tracing::error!("Unsupported features in ULocationUpdateDemand");
            return;
        }

        // Register the client if it's new (do NOT reset existing state on every Location Update)
        let issi = prim.received_address.ssi;
        if self.client_mgr.get_client_by_issi(issi).is_none() {
            match self.client_mgr.register_client(issi, true) {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("Failed registering roaming MS {}: {:?}", issi, e);
                    unimplemented_log!("Handle failed registration of roaming MS");
                    return;
                }
            }
        }

        // Process optional GroupIdentityLocationDemand field
        let gila = if let Some(gild) = pdu.group_identity_location_demand {

            // Best-effort: mark SSI as active in each requested group.
            if let Some(giu) = &gild.group_identity_uplink {
                for g in giu {
                    if let Some(gssi) = g.gssi {
                        online::record_group_seen(gssi as u32, issi);
                    }
                }
            }

            
            // Try to attach to requested groups, then build GroupIdentityLocationAccept element
            let accepted_groups = if let Some(giu) = &gild.group_identity_uplink {
                Some(self.try_attach_detach_groups(issi, &giu))
            } else {
                None
            };
            let gila = GroupIdentityLocationAccept {
                group_identity_accept_reject: 0, // Accept
                group_identity_downlink: accepted_groups,
            };

            Some(gila)
        } else {
            // No GroupIdentityLocationAccept element present
            None
        };

        // Build D-LOCATION UPDATE ACCEPT pdu
        let pdu_response = DLocationUpdateAccept {
            location_update_accept_type: pdu.location_update_type, // Practically identical besides minor migration-related difference
            ssi: Some(issi as u64),
            address_extension: None,
            subscriber_class: None,
            energy_saving_information: None,
            scch_information_and_distribution_on_18th_frame: None,
            new_registered_area: None,
            security_downlink: None,
            group_identity_location_accept: gila,
            default_group_attachment_lifetime: None,
            authentication_downlink: None,
            group_identity_security_related_information: None,
            cell_type_control: None,
            proprietary: None,
        };

        // Convert pdu to bits
        let pdu_len = 4+3+24+1+1+1; // Minimal lenght; may expand beyond this. 
        let mut sdu = BitBuffer::new_autoexpand(pdu_len);
        pdu_response.to_bitbuf(&mut sdu).unwrap(); // we want to know when this happens
        sdu.seek(0);
        tracing::debug!("-> {} sdu {}", pdu_response, sdu.dump_bin());

        // Build and submit response prim
        let addr = TetraAddress { encrypted: false, ssi_type: SsiType::Ssi, ssi: issi };
        let msg = SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mm,
            dest: TetraEntity::Mle,
            dltime: message.dltime,
            msg: SapMsgInner::LmmMleUnitdataReq(LmmMleUnitdataReq{
                sdu,
                handle: 0,
                address: addr,
                layer2service: 0,
                stealing_permission: false,
                stealing_repeats_flag: false, 
                encryption_flag: false,
                is_null_pdu: false,
            })
        };
        queue.push_back(msg);        
    }


    fn rx_u_attach_detach_group_identity(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_u_attach_detach_group_identity: {:?}", message);
        let SapMsgInner::LmmMleUnitdataInd(prim) = &mut message.msg else {panic!()};

        // Best-effort online tracking: any MM location update implies the SSI is active.
        online::record_seen(prim.received_address.ssi);
        
        let issi = prim.received_address.ssi;
        let pdu = match UAttachDetachGroupIdentity::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing UAttachDetachGroupIdentity: {:?} {}", e, prim.sdu.dump_bin());
                return;
            }
        };

        // Check if we can satisfy this request, print unsupported stuff
        if !Self::feature_check_u_attach_detach_group_identity(&pdu) {
            tracing::error!("Unsupported features in UAttachDetachGroupIdentity");
            return;
        }

        // If group_identity_attach_detach_mode == 1, we first detach all groups
        if pdu.group_identity_attach_detach_mode == true {
            match self.client_mgr.client_detach_all_groups(issi) {
                Ok(_) => {},
                Err(e) => {
                    tracing::warn!("Failed detaching all groups for MS {}: {:?}", issi, e);
                    return;
                }
            }
            group_registry::detach_all(issi);
        }

        // Try to attach to requested groups, and retrieve list of accepted GroupIdentityDownlink elements
        // We can unwrap since we did compat check earlier
        let accepted_gid= self.try_attach_detach_groups(issi, &pdu.group_identity_uplink.unwrap());

        // Build reply PDU
        let pdu_response = DAttachDetachGroupIdentityAcknowledgement {
            group_identity_accept_reject: 0, // Accept
            reserved: false, // TODO FIXME Guessed proper value of reserved field
            proprietary: None,
            group_identity_downlink: Some(accepted_gid),
            group_identity_security_related_information: None,
        };

        // Write to PDU
        let mut sdu = BitBuffer::new_autoexpand(32);
        pdu_response.to_bitbuf(&mut sdu).unwrap(); // We want to know when this happens
        sdu.seek(0);
        tracing::debug!("-> {:?} sdu {}", pdu_response, sdu.dump_bin());

        let addr = TetraAddress { 
            encrypted: false, 
            ssi_type: SsiType::Ssi, 
            ssi: issi 
        };
        let msg = SapMsg {
            sap: Sap::LmmSap,
            src: TetraEntity::Mm,
            dest: TetraEntity::Mle,
            dltime: message.dltime,
            msg: SapMsgInner::LmmMleUnitdataReq(LmmMleUnitdataReq{
                sdu,
                handle: 0,
                address: addr,
                layer2service: 0,
                stealing_permission: false,
                stealing_repeats_flag: false, 
                encryption_flag: false,
                is_null_pdu: false,
            })
        };
        queue.push_back(msg);
    }

    fn rx_u_attach_detach_group_identity_ack(&mut self, _queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_u_attach_detach_group_identity_ack: {:?}", message);
        let SapMsgInner::LmmMleUnitdataInd(prim) = &mut message.msg else { panic!() };

        online::record_seen(prim.received_address.ssi);

        let issi = prim.received_address.ssi;
        let pdu = match UAttachDetachGroupIdentityAcknowledgement::from_bitbuf(&mut prim.sdu) {
            Ok(pdu) => {
                tracing::debug!("<- {:?}", pdu);
                pdu
            }
            Err(e) => {
                tracing::warn!("Failed parsing UAttachDetachGroupIdentityAcknowledgement: {:?} {}", e, prim.sdu.dump_bin());
                return;
            }
        };

        // NOTE: In ETSI TETRA AI, this bit indicates whether at least one *attachment* was rejected.
        // Value 0 => all accepted; value 1 => at least one attachment rejected.
        // Therefore it must NOT be interpreted as a boolean "OK" flag.

        // Correlate by source ISSI (one in-flight per ISSI in our current job tracker).
        let pending_job_id = dgna_job::peek_pending_job_id(issi);
        let Some(job_id) = pending_job_id else {
            // Not a tracked request; ignore.
            return;
        };

        let job = match dgna_job::get_job(job_id) {
            Some(j) => j,
            None => {
                // Stale tracker entry; clear it best-effort.
                let _ = dgna_job::mark_ack_from_issi(issi, false, Some("stale DGNA tracker entry".to_string()));
                return;
            }
        };

        // Determine acceptance for the *requested* operation.
        let mut accepted = true;
        let mut reason: Option<String> = None;

        match job.op {
            DgnaOp::Attach => {
                // If acknowledgement_type == 0, all attachments accepted.
                if pdu.group_identity_acknowledgement_type {
                    // At least one attachment rejected; check if our requested group is listed as rejected.
                    accepted = true;
                    if let Some(giu_vec) = &pdu.group_identity_uplink {
                        for giu in giu_vec {
                            if giu.gssi == Some(job.gssi) {
                                if let Some(code) = giu.group_identity_detachment_uplink {
                                    accepted = false;
                                    reason = Some(format!(
                                        "DGNA attach rejected: {} (code={})",
                                        dgna_detachment_uplink_reason(code),
                                        code
                                    ));
                                    break;
                                }
                            }
                        }
                    }
                    if !accepted && reason.is_none() {
                        reason = Some("DGNA attach rejected (no reason provided)".to_string());
                    }
                }
            }
            DgnaOp::Detach => {
                // The acknowledgement_type bit is defined for attachments; for a pure detachment request
                // we treat the presence of an ACK as acceptance unless we later implement a finer-grained
                // detachment reject parsing.
                accepted = true;
            }
        }

        let _ = dgna_job::mark_ack_from_issi(issi, accepted, reason);

        // Best-effort local state update (used by WebUI): only mutate on accepted.
        if accepted {
            // Ensure we have a client entry, but do NOT reset groups for existing clients.
            if self.client_mgr.get_client_by_issi(issi).is_none() {
                let _ = self.client_mgr.register_client(issi, true);
            }

            // Replace/transfer semantics: clear current groups *after* ACK success.
            if job.op == DgnaOp::Attach && job.detach_all_first {
                let _ = self.client_mgr.client_detach_all_groups(issi);
            }

            match job.op {
                DgnaOp::Attach => {
                    let _ = self.client_mgr.client_group_attach(issi, job.gssi, true);
                    online::record_group_seen(job.gssi, issi);
                }
                DgnaOp::Detach => {
                    let _ = self.client_mgr.client_group_attach(issi, job.gssi, false);
                }
            }
        }
    }

    fn rx_dgna_set_req(&mut self, queue: &mut MessageQueue, dltime: TdmaTime, req: DgnaSetReq) {
        tracing::info!(
            "DGNA set req: op={:?} gssi={} targets={:?} detach_all_first={} lifetime={} class_of_usage={} ack_req={}",
            req.op, req.gssi, req.targets, req.detach_all_first, req.lifetime, req.class_of_usage, req.ack_req
        );

        // Clamp to 3-bit fields.
        let lifetime = req.lifetime & 0x07;
        let class_of_usage = req.class_of_usage & 0x07;

        for &issi in req.targets.iter() {
            // Only create a local client entry if we don't already have one.
            // IMPORTANT: do not call register_client() unconditionally here, because it resets the stored group list.
            if self.client_mgr.get_client_by_issi(issi).is_none() {
                let _ = self.client_mgr.register_client(issi, true);
            }

            // Local snapshot updates:
            // - If we request an ACK, defer local group changes until ACK comes back.
            // - If no ACK is requested, apply best-effort local changes immediately.
            if !req.ack_req {
                match req.op {
                    DgnaOp::Attach => {
                        let _ = self.client_mgr.client_group_attach(issi, req.gssi, true);
                        online::record_group_seen(req.gssi, issi);
                    }
                    DgnaOp::Detach => {
                        let _ = self.client_mgr.client_group_attach(issi, req.gssi, false);
                    }
                }
            }

            let gid = match req.op {
                DgnaOp::Attach => GroupIdentityDownlink {
                    group_identity_attachment: Some(GroupIdentityAttachment {
                        group_identity_attachment_lifetime: lifetime,
                        class_of_usage,
                    }),
                    group_identity_detachment_uplink: None,
                    gssi: Some(req.gssi),
                    address_extension: None,
                    vgssi: None,
                },
                DgnaOp::Detach => GroupIdentityDownlink {
                    group_identity_attachment: None,
                    // 2-bit detachment downlink reason.
                    // ETSI defines 3 as "permanent detachment"; use that for a generic DGNA detach.
                    group_identity_detachment_uplink: Some(3),
                    gssi: Some(req.gssi),
                    address_extension: None,
                    vgssi: None,
                },
            };

            let pdu = DAttachDetachGroupIdentity {
                group_identity_report: false,
                group_identity_acknowledgement_request: req.ack_req,
                // If detach_all_first is set for an ATTACH, request "detach all currently active group identities"
                // then attach the provided group identities (TETRA AI semantics).
                group_identity_attach_detach_mode: req.detach_all_first && matches!(req.op, DgnaOp::Attach),
                proprietary: None,
                group_report_response: None,
                group_identity_downlink: Some(vec![gid]),
                group_identity_security_related_information: None,
            };

            let mut sdu = BitBuffer::new_autoexpand(64);
            if let Err(e) = pdu.to_bitbuf(&mut sdu) {
                dgna_job::set_error(req.job_id, issi, format!("pdu encode failed: {:?}", e));
                continue;
            }
            sdu.seek(0);
            tracing::debug!("-> {:?} sdu {}", pdu, sdu.dump_bin());

            let addr = TetraAddress {
                encrypted: false,
                ssi_type: SsiType::Ssi,
                ssi: issi,
            };

            let msg = SapMsg {
                sap: Sap::LmmSap,
                src: TetraEntity::Mm,
                dest: TetraEntity::Mle,
                dltime,
                msg: SapMsgInner::LmmMleUnitdataReq(LmmMleUnitdataReq {
                    sdu,
                    handle: 0,
                    address: addr,
                    layer2service: 0,
                    stealing_permission: false,
                    stealing_repeats_flag: false,
                    encryption_flag: false,
                    is_null_pdu: false,
                }),
            };

            queue.push_back(msg);
            dgna_job::mark_submitted(req.job_id, issi, req.ack_req);
        }
    }

    fn rx_lmm_mle_unitdata_ind(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {

        // unimplemented_log!("rx_lmm_mle_unitdata_ind for MM component");
        let SapMsgInner::LmmMleUnitdataInd(prim) = &mut message.msg else {panic!()};

        // Best-effort online tracking: any MM location update implies the SSI is active.
        online::record_seen(prim.received_address.ssi);

        let Some(bits) = prim.sdu.peek_bits(4) else {
            tracing::warn!("insufficient bits: {}", prim.sdu.dump_bin());
            return;
        };

        let Ok(pdu_type) = MmPduTypeUl::try_from(bits) else {
            tracing::warn!("invalid pdu type: {} in {}", bits, prim.sdu.dump_bin());
            return;
        };

        match pdu_type {
            MmPduTypeUl::UAuthentication => 
                unimplemented_log!("UAuthentication"),
            MmPduTypeUl::UItsiDetach => 
                self.rx_u_itsi_detach(queue, message),
            MmPduTypeUl::ULocationUpdateDemand => 
                self.rx_u_location_update_demand(queue, message),
            MmPduTypeUl::UMmStatus =>   
                unimplemented_log!("UMmStatus"),
            MmPduTypeUl::UCkChangeResult => 
                unimplemented_log!("UCkChangeResult"),
            MmPduTypeUl::UOtar =>   
                unimplemented_log!("UOtar"),
            MmPduTypeUl::UInformationProvide => 
                unimplemented_log!("UInformationProvide"),
            MmPduTypeUl::UAttachDetachGroupIdentity => 
                self.rx_u_attach_detach_group_identity(queue, message),
            MmPduTypeUl::UAttachDetachGroupIdentityAcknowledgement => 
                self.rx_u_attach_detach_group_identity_ack(queue, message),
            MmPduTypeUl::UTeiProvide => 
                unimplemented_log!("UTeiProvide"),
            MmPduTypeUl::UDisableStatus => 
                unimplemented_log!("UDisableStatus"),
            MmPduTypeUl::MmPduFunctionNotSupported => 
                unimplemented_log!("MmPduFunctionNotSupported"),
        };
    }

    fn try_attach_detach_groups(&mut self, issi: u32, giu_vec: &Vec<GroupIdentityUplink>) -> Vec<GroupIdentityDownlink> {
        let mut accepted_groups = Vec::new();
        for giu in giu_vec.iter() {
            if giu.gssi.is_none() || giu.vgssi.is_some() || giu.address_extension.is_some() {
                unimplemented_log!("Only support GroupIdentityUplink with address_type 0");
                continue;
            }

            let gssi = giu.gssi.unwrap(); // can't fail
            let do_attach = giu.group_identity_detachment_uplink.is_none();
            match self.client_mgr.client_group_attach(issi, gssi, do_attach) {
                Ok(_) => {
                    if do_attach {
                        group_registry::attach(issi, gssi);
                        // We have added the client to this group. Add an entry to the downlink response
                        let gid = GroupIdentityDownlink {
                            group_identity_attachment: Some(GroupIdentityAttachment {
                                group_identity_attachment_lifetime: 3, // re-attach after location update
                                class_of_usage: giu.class_of_usage.unwrap_or(0),
                            }),
                            group_identity_detachment_uplink: None,
                            gssi: Some(gssi),
                            address_extension: None,
                            vgssi: None
                        };
                        accepted_groups.push(gid);
                    } else {
                        group_registry::detach(issi, gssi);
                        let gid = GroupIdentityDownlink {
                            group_identity_attachment: None,
                            group_identity_detachment_uplink: giu.group_identity_detachment_uplink,
                            gssi: Some(gssi),
                            address_extension: None,
                            vgssi: None
                        };
                        accepted_groups.push(gid);
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed updating group attach for MS {} to group {}: {:?}", issi, gssi, e);
                }
            }
        }
        accepted_groups
    }

    fn feature_check_u_location_update_demand(pdu: &ULocationUpdateDemand) -> bool {
        let mut supported = true;
        if pdu.location_update_type != MmLocationUpdateType::RoamingLocationUpdating && pdu.location_update_type != MmLocationUpdateType::ItsiAttach {
            unimplemented_log!("Unsupported {}", pdu.location_update_type);
            supported = false;
        }
        if pdu.request_to_append_la == true {
            unimplemented_log!("Unsupported request_to_append_la == true");
            supported = false;
        }
        if pdu.cipher_control == true {
            unimplemented_log!("Unsupported cipher_control == true");
            supported = false;
        }
        if pdu.ciphering_parameters.is_some() {
            unimplemented_log!("Unsupported ciphering_parameters present");
            supported = false;
        }
        // pub class_of_ms: Option<u64>, currently not parsed nor interpreted
        if pdu.energy_saving_mode.is_some() {
            unimplemented_log!("Unsupported energy_saving_mode present");
        }
        if pdu.la_information.is_some() {
            unimplemented_log!("Unsupported la_information present");
        }
        if pdu.ssi.is_some() {
            unimplemented_log!("Unsupported ssi present");
        }
        if pdu.address_extension.is_some() {
            unimplemented_log!("Unsupported address_extension present");
        }
        // pub group_identity_location_demand: Option<GroupIdentityLocationDemand>, kind of supported
        if pdu.group_report_response.is_some() {
            unimplemented_log!("Unsupported group_report_response present");
        }
        if pdu.authentication_uplink.is_some() {
            unimplemented_log!("Unsupported authentication_uplink present");
        }
        if pdu.extended_capabilities.is_some() {
            unimplemented_log!("Unsupported extended_capabilities present");
        }
        if pdu.proprietary.is_some() {
            unimplemented_log!("Unsupported proprietary present");
        }

        supported
    }


    fn feature_check_u_attach_detach_group_identity(pdu: &UAttachDetachGroupIdentity) -> bool {
        let mut supported = true;
        if pdu.group_identity_report == true {
            unimplemented_log!("Unsupported group_identity_report == true");
        }
        if pdu.group_identity_uplink.is_none() {
            unimplemented_log!("Missing group_identity_uplink");
            supported = false;
        }
        if pdu.group_report_response.is_some() {
            unimplemented_log!("Unsupported group_report_response present");
        }
        if pdu.proprietary.is_some() {
            unimplemented_log!("Unsupported proprietary present");
        }

        supported
    }
}



impl TetraEntityTrait for MmBs {

    fn entity(&self) -> TetraEntity {
        TetraEntity::Mm
    }

    fn set_config(&mut self, config: SharedConfig) {
        self.config = config;
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        
        tracing::debug!("rx_prim: {:?}", message);
        
        // There is only one SAP for MM
        assert!(message.sap == Sap::LmmSap);
        
        match message {
            SapMsg { msg: SapMsgInner::LmmMleUnitdataInd(_), .. } => {
                self.rx_lmm_mle_unitdata_ind(queue, message);
            }
            SapMsg { msg: SapMsgInner::DgnaSetReq(req), dltime, .. } => {
                self.rx_dgna_set_req(queue, dltime, req);
            }
            _ => { panic!(); }
        }
    }
}
