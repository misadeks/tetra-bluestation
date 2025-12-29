use crate::config::stack_config::SharedConfig;
use crate::common::messagerouter::MessageQueue;
use crate::saps::lmm::LmmMleUnitdataReq;
use crate::saps::sapmsg::{SapMsg, SapMsgInner};
use crate::common::address::{SsiType, TetraAddress};
use crate::common::bitbuffer::BitBuffer;
use crate::entities::mm::components::client_state::MmClientMgr;
use crate::entities::mm::enums::mm_location_update_accept_type::MmLocationUpdateAcceptType;
use crate::entities::mm::enums::mm_pdu_type_ul::MmPduTypeUl;
use crate::entities::mm::fields::group_identity_attachment::GroupIdentityAttachment;
use crate::entities::mm::fields::group_identity_downlink::GroupIdentityDownlink;
use crate::entities::mm::pdus::d_attach_detach_group_identity_acknowledgement::DAttachDetachGroupIdentityAcknowledgement;
use crate::entities::mm::pdus::d_location_update_accept::DLocationUpdateAccept;
use crate::entities::mm::pdus::u_attach_detach_group_identity::UAttachDetachGroupIdentity;
use crate::entities::mm::pdus::u_itsi_detach::UItsiDetach;
use crate::entities::mm::pdus::u_location_update_demand::ULocationUpdateDemand;
use crate::entities::TetraEntityTrait;
use crate::common::tetra_common::Sap;
use crate::common::tetra_entities::TetraEntity;
use crate::unimplemented_log;

pub struct MmBs {
    config: SharedConfig,
    pub clients: MmClientMgr,
}

impl MmBs {
    pub fn new(config: SharedConfig) -> Self {
        Self { config, clients: MmClientMgr::new() }
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
        let detached_client = self.clients.remove(ssi);
        if detached_client.is_none() {
            tracing::warn!("Received UItsiDetach for unknown client with SSI: {}", ssi);
            // return;
        };
    }
        


    fn rx_u_location_update_demand(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("rx_location_update_demand: {:?}", message);
        let SapMsgInner::LmmMleUnitdataInd(prim) = &mut message.msg else {panic!()};

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

        let ssi = prim.received_address.ssi;

        let location_update_accept_type = match pdu.location_update_type {
            0 => {
                if !self.clients.is_known(ssi) {
                    tracing::debug!("Roaming update for unknown MS {}, registering", ssi);
                    self.clients.register(ssi, true);
                }
                MmLocationUpdateAcceptType::RoamingLocationUpdating
            },
            3 => {
                self.clients.register(ssi, true);
                MmLocationUpdateAcceptType::ItsiAttach
            },
            _ => {
                unimplemented_log!("Location update type {} not implemented", pdu.location_update_type);
                return;
            }
        };
        let pdu_response = DLocationUpdateAccept {
            location_update_accept_type,
            ssi: Some(ssi as u64),
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

        let pdu_len = 4+3+24+1+1+1; // Minimal lenght; may expand beyond this. 
        let mut sdu = BitBuffer::new_autoexpand(pdu_len);
        pdu_response.to_bitbuf(&mut sdu);
        sdu.seek(0);
        tracing::debug!("rx_location_update_demand: -> {:?} sdu {}", pdu_response, sdu.dump_bin());

        let addr = TetraAddress { encrypted: false, ssi_type: SsiType::Ssi, ssi };

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
        
        let ssi = prim.received_address.ssi;
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

        let Some(giu) = pdu.group_identity_uplink else {
            // We want to know when this happens
            unimplemented!("not yet implemented UAttachDetachGroupIdentity PDU without group_identity_uplink field");
            // return;
        };
        
        // Build vec of GroupIdentityDownlink elements from received GroupIdentityUplink elements
        let mut gid = Vec::with_capacity(giu.len());
        for elem in giu {
            let gia = GroupIdentityAttachment {
                group_identity_attachment_lifetime: 3, // re-attach after location update
                class_of_usage: elem.class_of_usage.unwrap_or(0),
            };
            gid.push(GroupIdentityDownlink {
                group_identity_attachment: Some(gia),
                group_identity_detachment_uplink: None,
                gssi: elem.gssi,
                address_extension: elem.address_extension,
                vgssi: elem.vgssi,
            })
        }

        // Build reply PDU
        let pdu_response = DAttachDetachGroupIdentityAcknowledgement {
            group_identity_accept_reject: 0, // Accept
            reserved: false, // TODO FIXME Guessed proper value of reserved field
            proprietary: None,
            group_identity_downlink: Some(gid),
            group_identity_security_related_information: None,
        };

        // Write to PDU
        let mut sdu = BitBuffer::new_autoexpand(32);
        pdu_response.to_bitbuf(&mut sdu).unwrap(); // We want to know when this happens
        sdu.seek(0);
        tracing::debug!("rx_u_attach_detach_group_identity: -> {:?} sdu {}", pdu_response, sdu.dump_bin());

        let addr = TetraAddress { 
            encrypted: false, 
            ssi_type: SsiType::Ssi, 
            ssi 
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

    fn rx_lmm_mle_unitdata_ind(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {

        // unimplemented_log!("rx_lmm_mle_unitdata_ind not implemented for MM component");
        let SapMsgInner::LmmMleUnitdataInd(prim) = &mut message.msg else {panic!()};

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
                unimplemented_log!("UAuthentication not implemented"),
            MmPduTypeUl::UItsiDetach => 
                self.rx_u_itsi_detach(queue, message),
                
            MmPduTypeUl::ULocationUpdateDemand => 
                self.rx_u_location_update_demand(queue, message),
            MmPduTypeUl::UMmStatus =>   
                unimplemented_log!("UMmStatus not implemented"),
            MmPduTypeUl::UCkChangeResult => 
                unimplemented_log!("UCkChangeResult not implemented"),
            MmPduTypeUl::UOtar =>   
                unimplemented_log!("UOtar not implemented"),
            MmPduTypeUl::UInformationProvide => 
                unimplemented_log!("UInformationProvide not implemented"),
            MmPduTypeUl::UAttachDetachGroupIdentity => 
                self.rx_u_attach_detach_group_identity(queue, message),
            MmPduTypeUl::UAttachDetachGroupIdentityAcknowledgement => 
                unimplemented_log!("UAttachDetachGroupIdentityAcknowledgement not implemented"),
            MmPduTypeUl::UTeiProvide => 
                unimplemented_log!("UTeiProvide not implemented"),
            MmPduTypeUl::UDisableStatus => 
                unimplemented_log!("UDisableStatus not implemented"),
            MmPduTypeUl::MmPduFunctionNotSupported => 
                unimplemented_log!("MmPduFunctionNotSupported not implemented"),
        };
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
        
        match message.msg {
            SapMsgInner::LmmMleUnitdataInd(_) => {
                self.rx_lmm_mle_unitdata_ind(queue, message);
            }
            _ => { panic!(); }
        }
    }
}
