use crate::common::address::TetraAddress;
use crate::common::bitbuffer::BitBuffer;
use crate::common::messagerouter::MessageQueue;
use crate::common::tdma_time::TdmaTime;
use crate::common::tetra_common::Sap;
use crate::common::tetra_entities::TetraEntity;
use crate::config::stack_config::SharedConfig;
use crate::entities::TetraEntityTrait;
use crate::entities::sndcp::enums::sn_pdu_type::SnPduType;
use crate::entities::sndcp::pdus::d_activate_pdp_context_accept;
use crate::entities::sndcp::pdus::d_activate_pdp_context_accept::SnActivatePdpContextAccept;
use crate::entities::sndcp::pdus::sn_pdu::{
    parse_data_transmit_response_after_type, parse_sn_pdu_header,
};
use crate::entities::sndcp::pdus::u_activate_pdp_context_demand::SnActivatePdpContextDemand;
use crate::saps::ltpd::LtpdMleUnitdataReq;
use crate::saps::sapmsg::{SapMsg, SapMsgInner};
use crate::saps::sn::SnNsapiAllocInd;
use crate::unimplemented_log;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdpContextState {
    Pending,
    Active,
}

#[derive(Debug, Clone)]
pub struct PdpContext {
    pub nsapi: u8,
    pub ssi: u32, // or whatever type your TetraAddress.ssi is
    pub atid: u8,
    pub ipv4: Option<[u8; 4]>,
    pub ms_type: u8,
    pub pcomp: u8,
    pub endpoint_id: u16, // bearer endpoint_id (LTPD)
    pub link_id: u8,      // bearer link_id (LTPD)
    pub state: PdpContextState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PdpKey {
    ssi: u32,
    nsapi: u8,
}

pub struct Sndcp {
    // config: Option<SharedConfig>,
    config: SharedConfig,
    pdp: HashMap<PdpKey, PdpContext>,
}

impl Sndcp {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            config,
            pdp: HashMap::new(),
        }
    }

    fn upsert_pdp_context_active(&mut self, ctx: PdpContext) {
        let key = PdpKey {
            ssi: ctx.ssi,
            nsapi: ctx.nsapi,
        };
        self.pdp.insert(key, ctx);
    }

    pub fn get_pdp_context(&self, ssi: u32, nsapi: u8) -> Option<&PdpContext> {
        self.pdp.get(&PdpKey { ssi, nsapi })
    }
    fn send_activate_context_accept(
        queue: &mut MessageQueue,
        dltime: TdmaTime,
        to_addr: TetraAddress,
        endpoint_id: u16,
        link_id: u8,
        nsapi: u8,
        demanded_dynamic: bool,
    ) {
        // Pick an address for now (demo). Replace with pool/PDN later.
        let ipv4 = [10, 0, 0, 2];

        // Minimal sensible defaults (tweak later):
        let ready = 4; // example coding
        let standby = 2; // example coding
        let response_wait = 7; // example coding
        let mtu = 4; // 1500 coding (your coding)
        let pdu_priority_max = 7;
        let pcomp_negotiation = 0u8;

        // This is the OPTIONAL IE from ETSI Table 372 (16 bits).
        // This is NOT the bearer endpoint_id/link_id.
        let sndcp_network_endpoint_id_ie: Option<u16> = Some(1280);

        // In your accept PDU, type_id_in_accept controls whether IPv4 is present.
        let type_id_in_accept = if demanded_dynamic { 2 } else { 1 };

        let pdu = SnActivatePdpContextAccept {
            nsapi,
            pdu_priority_max,
            ready,
            standby,
            response_wait,
            mtu,
            type_id_in_accept,
            ipv4_addr: Some(ipv4),
            pcomp_negotiation,
            sndcp_network_endpoint_id: sndcp_network_endpoint_id_ie,
        };

        // ---- Serialize into a scratch buffer, then shrink to exact used length ----
        let mut tmp = BitBuffer::new(256);
        if let Err(e) = pdu.to_bitbuf(&mut tmp) {
            tracing::warn!("Failed to serialize ActivatePdpContextAccept: {:?}", e);
            return;
        }

        // `tmp.get_pos()` = number of bits written (your BitBuffer uses cursor as write position)
        let used_bits = tmp.get_pos();

        // Pad to octet boundary (ETSI-style packing is octet oriented; this makes SDU length sane)
        let pad = (8 - (used_bits % 8)) % 8;
        if pad != 0 {
            tmp.write_bits(0, pad as u32 as usize);
        }
        let used_padded = used_bits + pad;

        // Create exact-sized SDU and copy only what we used
        tmp.seek(0);
        let mut sn_pdu = BitBuffer::new(used_padded);
        sn_pdu.copy_bits(&mut tmp, used_padded);
        sn_pdu.seek(0);

        // Roundtrip parse (debug)
        let mut rt = sn_pdu.clone();
        rt.seek(0);
        let parsed = SnActivatePdpContextAccept::from_bitbuf(&mut rt);
        tracing::info!("ACCEPT roundtrip parse: {:?}", parsed);

        // Fix misleading log: show bearer endpoint/link vs IE network endpoint id
        tracing::info!(
            "SNDCP TX: ActivatePdpContextAccept nsapi={} to {:?} (ltpd_endpoint_id={}, ltpd_link_id={}, sndcp_net_endpoint_ie={:?}, bits={})",
            nsapi,
            to_addr,
            endpoint_id,
            link_id,
            sndcp_network_endpoint_id_ie,
            used_padded
        );

        let req = LtpdMleUnitdataReq {
            sdu: sn_pdu,
            address: to_addr,
            handle: 0,
            layer2service: 0,
            unacked_bl_repetitions: 0,
            pdu_prio: 0,

            // USE THE ARGS (don’t hardcode 0/0)
            endpoint_id: endpoint_id as i32,
            link_id: link_id as i32,

            stealing_permission: false,
            stealing_repeats_flag: false,
            channel_advice_flag: false,
            data_class_info: 0,
            data_prio: 0,
            mle_data_prio_flag: false,
            packet_data_flag: true,
            scheduled_data_status: 0,
            max_schedule_interval: 0,
            fcs_flag: false,
        };

        queue.push_back(SapMsg {
            sap: Sap::TlpdSap,
            src: TetraEntity::Sndcp,
            dest: TetraEntity::Mle,
            dltime,
            msg: SapMsgInner::LtpdMleUnitdataReq(req),
        });
    }
    fn send_nsapi_alloc_ind(
        queue: &mut MessageQueue,
        dltime: TdmaTime,
        to: TetraAddress,
        demand: &SnActivatePdpContextDemand, // your parsed demand type
        endpoint_id: u16,
        link_id: u8,
    ) {
        let ind = SnNsapiAllocInd {
            address: to,
            nsapi: demand.nsapi,
            ipv4: demand.ipv4_addr,
            atid: demand.atid,
            pcomp: demand.pcomp_negotiation,
            endpoint_id,
            link_id,
        };

        queue.push_back(SapMsg {
            sap: Sap::SnSap,
            src: TetraEntity::Sndcp,
            dest: TetraEntity::SnSap, // <- replace with your “SN-SAP user” entity
            dltime,
            msg: SapMsgInner::SnNsapiAllocInd(ind),
        });
    }
}

impl TetraEntityTrait for Sndcp {
    fn entity(&self) -> TetraEntity {
        TetraEntity::Sndcp
    }

    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg) {
        tracing::debug!("rx_prim: {:?}", message);

        assert!(message.sap == Sap::TlpdSap);

        match message.msg {
            SapMsgInner::LtpdMleUnitdataInd(mut ind) => {
                // Move SDU out (start-of-payload is current pos; do NOT seek(0) on src)
                let mut src = ind.sdu;
                let payload_bits = src.get_len_remaining();

                // Normalize payload to a fresh buffer starting at bit 0
                let mut payload = BitBuffer::new(payload_bits);
                payload.copy_bits(&mut src, payload_bits);
                payload.seek(0);

                // Parse header (consumes 4 bits for type)
                if let Some(hdr) = parse_sn_pdu_header(&mut payload) {
                    match hdr.pdu_type {
                        // Type 0: Activate PDP Context Demand/Accept
                        SnPduType::ActivatePdpContextDemandAccept => {
                            payload.seek(0);
                            let demand = match SnActivatePdpContextDemand::from_bitbuf(&mut payload)
                            {
                                Ok(d) => d,
                                Err(e) => {
                                    tracing::warn!(
                                        "ActivatePdpContextDemand parse failed: {:?}",
                                        e
                                    );
                                    return;
                                }
                            };

                            tracing::info!(
                                "SNDCP RX: ActivatePdpContextDemand {:?} from {:?} (ltpd_endpoint_id={}, ltpd_link_id={})",
                                demand,
                                ind.received_tetra_address,
                                ind.endpoint_id,
                                ind.link_id
                            );

                            // --- Step 3: Insert into SwMI PDP contexts (store state) ---
                            let ctx = PdpContext {
                                nsapi: demand.nsapi,
                                ssi: ind.received_tetra_address.ssi,
                                atid: demand.atid,
                                ipv4: demand.ipv4_addr,
                                ms_type: demand.packet_data_ms_type,
                                pcomp: demand.pcomp_negotiation,
                                endpoint_id: ind.endpoint_id as u16,
                                link_id: ind.link_id as u8,
                                state: PdpContextState::Active,
                            };
                            self.upsert_pdp_context_active(ctx);

                            // --- Step 3: Send ACCEPT (you already do this) ---
                            let demanded_dynamic = false;
                            Self::send_activate_context_accept(
                                queue,
                                message.dltime,
                                ind.received_tetra_address,
                                ind.endpoint_id as u16,
                                ind.link_id as u8,
                                demand.nsapi,
                                demanded_dynamic,
                            );

                            // --- Step 4: Notify SwMI SN-SAP user ---
                            Self::send_nsapi_alloc_ind(
                                queue,
                                message.dltime,
                                ind.received_tetra_address,
                                &demand,
                                ind.endpoint_id as u16,
                                ind.link_id as u8,
                            );

                            return;
                        }

                        SnPduType::DataTransmitResponse => {
                            if let Some(rsp) = parse_data_transmit_response_after_type(&mut payload)
                            {
                                tracing::info!(
                                    "SN-DATA TRANSMIT RESPONSE: nsapi={}, {:?}, reject_cause={:?}, endpoint_id={:?}",
                                    rsp.nsapi,
                                    rsp.accept_reject,
                                    rsp.reject_cause,
                                    rsp.network_endpoint_id
                                );
                            } else {
                                tracing::warn!("SN-DATA TRANSMIT RESPONSE: parse failed");
                            }
                        }

                        other => {
                            tracing::info!("SNDCP RX: SN-PDU={:?}", other);
                        }
                    }
                } else {
                    tracing::warn!(
                        "SNDCP RX: too short for SN-PDU header ({} bits)",
                        payload_bits
                    );
                }
            }

            other => {
                unimplemented_log!("SNDCP: unhandled primitive: {:?}", other);
            }
        }
    }
}
