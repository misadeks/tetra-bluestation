use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
#[cfg(target_os = "linux")]
use std::os::raw::c_short;
#[cfg(target_os = "linux")]
use std::os::unix::io::FromRawFd;
#[cfg(target_os = "linux")]
use std::process::Command;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossbeam_channel::{Receiver, Sender};

use crate::common::address::{SsiType, TetraAddress};
use crate::common::bitbuffer::BitBuffer;
use crate::common::messagerouter::MessageQueue;
use crate::common::tetra_common::{Sap, Todo};
use crate::common::tetra_entities::TetraEntity;
use crate::config::stack_config::SharedConfig;
use crate::entities::TetraEntityTrait;
use crate::saps::ltpd::{LtpdMleUnitdataInd, LtpdMleUnitdataReq};
use crate::saps::sapmsg::{SapMsg, SapMsgInner};
use crate::saps::tma::TmaUnitdataReq;

const PDP_HDR_LEN: usize = 8;
const DEFAULT_PDP_IP: &str = "10.0.0.2";
const DEFAULT_TUN_NAME: &str = "tetra0";
const DEFAULT_TUN_MTU: usize = 1500;
const DEFAULT_TUN_ADDR: &str = "10.0.0.1/24";

#[cfg(target_os = "linux")]
const TUNSETIFF: libc::c_ulong = 0x400454ca;
#[cfg(target_os = "linux")]
const IFF_TUN: c_short = 0x0001;
#[cfg(target_os = "linux")]
const IFF_NO_PI: c_short = 0x1000;

#[derive(Debug, Clone)]
struct PdpDlPacket {
    payload: Vec<u8>,
    dst_ssi: Option<u32>,
    endpoint_id: Option<u16>,
    link_id: Option<u16>,
}

#[derive(Debug, Clone)]
struct PdpUlPacket {
    payload: Vec<u8>,
    src_ssi: u32,
    endpoint_id: u16,
    link_id: u16,
}

#[derive(Debug, Clone, Copy)]
struct RouteInfo {
    endpoint_id: u16,
    link_id: u16,
}

#[derive(Debug, Clone, Copy)]
struct PdpContext {
    nsapi: u8,
    ip: Option<Ipv4Addr>,
}

#[derive(Debug, Clone, Copy)]
struct PdpTimers {
    ready: u8,
    standby: u8,
    response_wait: u8,
}

#[derive(Debug, Default, Clone)]
struct SndcpCounters {
    ul_unitdata_rx: u64,
    ul_reassembled_ok: u64,
    ul_written_to_tun: u64,
    ul_drop_not_ip: u64,
    dl_from_tun_rx: u64,
    dl_route_ok: u64,
    dl_route_miss: u64,
    dl_bytes_sent: u64,
}

#[derive(Debug, Clone)]
struct ReassemblyState {
    buf: Vec<u8>,
    last_update: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum SnPduType {
    ActivatePdpContext = 0,
    DeactivatePdpContextAccept = 1,
    DeactivatePdpContextDemand = 2,
    ActivatePdpContextReject = 3,
    Unitdata = 4,
    Data = 5,
    DataTransmitRequest = 6,
    DataTransmitResponse = 7,
    EndOfData = 8,
    Reconnect = 9,
    Page = 10,
    NotSupported = 11,
    DataPriority = 12,
    ModifyPdpContext = 13,
}

impl TryFrom<u64> for SnPduType {
    type Error = ();
    fn try_from(v: u64) -> Result<Self, Self::Error> {
        match v {
            0 => Ok(SnPduType::ActivatePdpContext),
            1 => Ok(SnPduType::DeactivatePdpContextAccept),
            2 => Ok(SnPduType::DeactivatePdpContextDemand),
            3 => Ok(SnPduType::ActivatePdpContextReject),
            4 => Ok(SnPduType::Unitdata),
            5 => Ok(SnPduType::Data),
            6 => Ok(SnPduType::DataTransmitRequest),
            7 => Ok(SnPduType::DataTransmitResponse),
            8 => Ok(SnPduType::EndOfData),
            9 => Ok(SnPduType::Reconnect),
            10 => Ok(SnPduType::Page),
            11 => Ok(SnPduType::NotSupported),
            12 => Ok(SnPduType::DataPriority),
            13 => Ok(SnPduType::ModifyPdpContext),
            _ => Err(()),
        }
    }
}

#[derive(Debug)]
struct ActivatePdpDemand {
    version: u8,
    nsapi: u8,
    addr_type: u8,
    requested_ip: Option<Ipv4Addr>,
    pcomp: u8,
}

pub struct Sndcp {
    config: SharedConfig,
    dl_rxs: Vec<Receiver<PdpDlPacket>>,
    ul_txs: Vec<Sender<PdpUlPacket>>,
    framed: bool,
    sndcp_dump: bool,
    sndcp_stats: bool,
    sndcp_find_ip: bool,
    default_route: Option<RouteInfo>,
    default_ssi: Option<u32>,
    default_nsapi: u8,
    default_ip: Option<Ipv4Addr>,
    timers: PdpTimers,
    routes: HashMap<u32, RouteInfo>,
    pdp_contexts: HashMap<(u32, u8), PdpContext>,
    last_nsapi: HashMap<u32, u8>,
    ip_routes: HashMap<Ipv4Addr, u32>,
    counters: SndcpCounters,
    last_stats: Option<Instant>,
    reassembly: HashMap<(u32, u8), ReassemblyState>,
}

impl Sndcp {
    pub fn new(config: SharedConfig) -> Self {
        let listen = std::env::var("TETRA_PDP_LISTEN").ok();
        let peer = std::env::var("TETRA_PDP_PEER")
            .ok()
            .and_then(|s| s.parse::<SocketAddr>().ok());
        let framed = env_flag("TETRA_PDP_FRAMED");
        let sndcp_dump = env_flag("TETRA_SNDCP_DUMP");
        let sndcp_stats = env_flag("TETRA_SNDCP_STATS");
        let sndcp_find_ip = env_flag_default("TETRA_SNDCP_FIND_IP", true);
        let tun_name = Some(DEFAULT_TUN_NAME.to_string());
        let tun_mtu = Some(DEFAULT_TUN_MTU);

        let default_ssi = std::env::var("TETRA_PDP_DEFAULT_SSI")
            .ok()
            .and_then(|s| s.parse::<u32>().ok());

        let default_route = default_ssi.map(|_| RouteInfo {
            endpoint_id: std::env::var("TETRA_PDP_DEFAULT_ENDPOINT_ID")
                .ok()
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(0),
            link_id: std::env::var("TETRA_PDP_DEFAULT_LINK_ID")
                .ok()
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(0),
        });

        let default_nsapi = std::env::var("TETRA_PDP_NSAPI")
            .ok()
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(0);

        let default_ip = std::env::var("TETRA_PDP_IP")
            .ok()
            .and_then(|s| Ipv4Addr::from_str(&s).ok())
            .or_else(|| Ipv4Addr::from_str(DEFAULT_PDP_IP).ok());

        let timers = PdpTimers {
            ready: env_u8("TETRA_PDP_READY_TIMER").unwrap_or(0),
            standby: env_u8("TETRA_PDP_STANDBY_TIMER").unwrap_or(4),
            response_wait: env_u8("TETRA_PDP_RESPONSE_WAIT_TIMER").unwrap_or(0),
        };

        let mut dl_rxs: Vec<Receiver<PdpDlPacket>> = Vec::new();
        let mut ul_txs: Vec<Sender<PdpUlPacket>> = Vec::new();

        if let Some(listen_addr) = listen {
            let (rx, tx) = spawn_pdp_udp(&listen_addr, peer, framed);
            dl_rxs.push(rx);
            ul_txs.push(tx);
        }

        if let Some(name) = tun_name {
            #[cfg(target_os = "linux")]
            {
                match spawn_pdp_tun(&name, tun_mtu) {
                    Ok((rx, tx)) => {
                        dl_rxs.push(rx);
                        ul_txs.push(tx);
                        tracing::info!("SNDCP TUN enabled on {}", name);
                    }
                    Err(e) => {
                        tracing::warn!("SNDCP TUN failed to start: {}", e);
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                tracing::warn!("SNDCP TUN not supported on this OS");
            }
        }

        Self {
            config,
            dl_rxs,
            ul_txs,
            framed,
            sndcp_dump,
            sndcp_stats,
            sndcp_find_ip,
            default_route,
            default_ssi,
            default_nsapi,
            default_ip,
            timers,
            routes: HashMap::new(),
            pdp_contexts: HashMap::new(),
            last_nsapi: HashMap::new(),
            ip_routes: HashMap::new(),
            counters: SndcpCounters::default(),
            last_stats: None,
            reassembly: HashMap::new(),
        }
    }

    fn tx_sndcp_pdu(
        &self,
        queue: &mut MessageQueue,
        dltime: crate::common::tdma_time::TdmaTime,
        dst: TetraAddress,
        link_id: u16,
        endpoint_id: u16,
        data_class_info: Todo,
        pdu: BitBuffer,
    ) {
        let sapmsg = SapMsg {
            sap: Sap::TlpdSap,
            src: TetraEntity::Sndcp,
            dest: TetraEntity::Mle,
            dltime,
            msg: SapMsgInner::LtpdMleUnitdataReq(LtpdMleUnitdataReq {
                sdu: pdu,
                address: dst,
                handle: 0,
                layer2service: 0,
                unacked_bl_repetitions: 0,
                pdu_prio: 0,
                endpoint_id: endpoint_id as Todo,
                link_id: link_id as Todo,
                stealing_permission: false,
                stealing_repeats_flag: false,
                channel_advice_flag: false,
                data_class_info,
                data_prio: 0,
                mle_data_prio_flag: false,
                packet_data_flag: true,
                scheduled_data_status: 0,
                max_schedule_interval: 0,
                fcs_flag: false,
            }),
        };
        queue.push_back(sapmsg);
    }

    fn enqueue_packet_channel_alloc(
        &self,
        queue: &mut MessageQueue,
        dltime: crate::common::tdma_time::TdmaTime,
        dst: TetraAddress,
        endpoint_id: u16,
    ) {
        tracing::debug!(
            "SNDCP: scheduling packet channel alloc for ssi={}",
            dst.ssi
        );
        let sapmsg = SapMsg {
            sap: Sap::TmaSap,
            src: TetraEntity::Sndcp,
            dest: TetraEntity::Umac,
            dltime,
            msg: SapMsgInner::TmaUnitdataReq(TmaUnitdataReq {
                req_handle: 0,
                pdu: BitBuffer::new(0),
                main_address: dst,
                scrambling_code: self.config.config().scrambling_code(),
                endpoint_id: endpoint_id as Todo,
                stealing_permission: false,
                subscriber_class: 0,
                air_interface_encryption: None,
                stealing_repeats_flag: None,
                data_category: Some(0),
            }),
        };
        queue.push_back(sapmsg);
    }

    fn handle_unitdata_ind(
        &mut self,
        queue: &mut MessageQueue,
        dltime: crate::common::tdma_time::TdmaTime,
        prim: LtpdMleUnitdataInd,
    ) {
        let src_ssi = prim.received_tetra_address.ssi;
        let endpoint_id = todo_to_u16(prim.endpoint_id);
        let link_id = todo_to_u16(prim.link_id);

        if src_ssi != 0 {
            self.routes.insert(src_ssi, RouteInfo { endpoint_id, link_id });
        }

        let mut sdu = prim.sdu;
        let total_bits = sdu.get_len_remaining();
        let total_bytes = (total_bits + 7) / 8;
        let pdu_type = match read_bits(&mut sdu, 4, "sn_pdu_type").and_then(|v| SnPduType::try_from(v).ok()) {
            Some(t) => t,
            None => {
                tracing::warn!("SNDCP RX: invalid SN PDU type");
                return;
            }
        };

        match pdu_type {
            SnPduType::Unitdata => {
                let (nsapi, pcomp, dcomp) = match parse_unitdata_header(&mut sdu) {
                    Some(v) => v,
                    None => return,
                };
                if pcomp != 0 || dcomp != 0 {
                    tracing::warn!(
                        "SNDCP RX: unsupported compression pcomp={} dcomp={}",
                        pcomp,
                        dcomp
                    );
                    return;
                }

                if src_ssi != 0 {
                    self.last_nsapi.insert(src_ssi, nsapi);
                }

                let payload = match bitbuffer_to_bytes_from_pos(sdu) {
                    Some(p) => p,
                    None => {
                        tracing::warn!("SNDCP RX: N-PDU not byte-aligned");
                        return;
                    }
                };

                self.counters.ul_unitdata_rx = self.counters.ul_unitdata_rx.saturating_add(1);
                tracing::info!(
                    "SNDCP RX UNITDATA: src_ssi={} nsapi={} endpoint_id={} link_id={} total_bits={} total_bytes={} payload_bytes={}",
                    src_ssi,
                    nsapi,
                    endpoint_id,
                    link_id,
                    total_bits,
                    total_bytes,
                    payload.len()
                );
                self.handle_ul_payload(payload, src_ssi, nsapi, endpoint_id, link_id);
            }
            SnPduType::Data => {
                let (nsapi, pcomp, dcomp) = match parse_unitdata_header(&mut sdu) {
                    Some(v) => v,
                    None => return,
                };
                if pcomp != 0 || dcomp != 0 {
                    tracing::warn!(
                        "SNDCP RX DATA: unsupported compression pcomp={} dcomp={}",
                        pcomp,
                        dcomp
                    );
                    return;
                }
                let payload = match bitbuffer_to_bytes_from_pos(sdu) {
                    Some(p) => p,
                    None => {
                        tracing::warn!("SNDCP RX DATA: N-PDU not byte-aligned");
                        return;
                    }
                };
                let key = (src_ssi, nsapi);
                let entry = self.reassembly.entry(key).or_insert_with(|| ReassemblyState {
                    buf: Vec::new(),
                    last_update: Instant::now(),
                });
                entry.buf.extend_from_slice(&payload);
                entry.last_update = Instant::now();
                self.counters.ul_unitdata_rx = self.counters.ul_unitdata_rx.saturating_add(1);
                tracing::info!(
                    "SNDCP RX DATA: src_ssi={} nsapi={} fragment_bytes={} reassembly_bytes={}",
                    src_ssi,
                    nsapi,
                    payload.len(),
                    entry.buf.len()
                );
            }
            SnPduType::EndOfData => {
                let (nsapi, pcomp, dcomp) = match parse_unitdata_header(&mut sdu) {
                    Some(v) => v,
                    None => return,
                };
                if pcomp != 0 || dcomp != 0 {
                    tracing::warn!(
                        "SNDCP RX END OF DATA: unsupported compression pcomp={} dcomp={}",
                        pcomp,
                        dcomp
                    );
                    return;
                }
                let payload = match bitbuffer_to_bytes_from_pos(sdu) {
                    Some(p) => p,
                    None => {
                        tracing::warn!("SNDCP RX END OF DATA: N-PDU not byte-aligned");
                        return;
                    }
                };
                let key = (src_ssi, nsapi);
                let entry = self.reassembly.entry(key).or_insert_with(|| ReassemblyState {
                    buf: Vec::new(),
                    last_update: Instant::now(),
                });
                entry.buf.extend_from_slice(&payload);
                let complete = std::mem::take(&mut entry.buf);
                self.reassembly.remove(&key);
                self.counters.ul_unitdata_rx = self.counters.ul_unitdata_rx.saturating_add(1);
                tracing::info!(
                    "SNDCP RX END OF DATA: src_ssi={} nsapi={} reassembled_bytes={}",
                    src_ssi,
                    nsapi,
                    complete.len()
                );
                self.handle_ul_payload(complete, src_ssi, nsapi, endpoint_id, link_id);
            }
            SnPduType::ActivatePdpContext => {
                let demand = match parse_activate_pdp_demand(&mut sdu) {
                    Some(d) => d,
                    None => return,
                };

                let (accept, cause, assigned_ip) = self.evaluate_pdp_demand(&demand);

                if accept {
                    let ctx = PdpContext { nsapi: demand.nsapi, ip: assigned_ip };
                    self.pdp_contexts.insert((src_ssi, demand.nsapi), ctx);
                    self.last_nsapi.insert(src_ssi, demand.nsapi);
                    if let Some(ip) = assigned_ip {
                        self.ip_routes.insert(ip, src_ssi);
                    }
                    let tia = match demand.addr_type {
                        0 => 1, // IPv4 static
                        1 => 2, // IPv4 dynamic
                        _ => 0,
                    };
                    let pdu = build_activate_pdp_accept(
                        demand.version,
                        demand.nsapi,
                        tia,
                        assigned_ip,
                        self.timers,
                        7,
                    );
                    tracing::info!(
                        "SNDCP RX ACTIVATE PDP: accept src_ssi={} nsapi={} ip={:?}",
                        src_ssi,
                        demand.nsapi,
                        assigned_ip
                    );
                    self.tx_sndcp_pdu(
                        queue,
                        dltime,
                        prim.received_tetra_address,
                        link_id,
                        endpoint_id,
                        1,
                        pdu,
                    );
                    self.enqueue_packet_channel_alloc(queue, dltime, prim.received_tetra_address, endpoint_id);
                } else {
                    let pdu = build_activate_pdp_reject(demand.nsapi, cause);
                    tracing::warn!(
                        "SNDCP RX ACTIVATE PDP: reject src_ssi={} nsapi={} cause={}",
                        src_ssi,
                        demand.nsapi,
                        cause
                    );
                    self.tx_sndcp_pdu(
                        queue,
                        dltime,
                        prim.received_tetra_address,
                        link_id,
                        endpoint_id,
                        1,
                        pdu,
                    );
                }
            }
            SnPduType::DeactivatePdpContextDemand => {
                let nsapi = read_bits(&mut sdu, 4, "nsapi").unwrap_or(0) as u8;
                self.pdp_contexts.remove(&(src_ssi, nsapi));
                self.ip_routes.retain(|_, v| *v != src_ssi);
                tracing::info!(
                    "SNDCP RX DEACTIVATE PDP: src_ssi={} nsapi={}",
                    src_ssi,
                    nsapi
                );
                let pdu = build_deactivate_pdp_accept(nsapi);
                self.tx_sndcp_pdu(
                    queue,
                    dltime,
                    prim.received_tetra_address,
                    link_id,
                    endpoint_id,
                    1,
                    pdu,
                );
            }
            SnPduType::DataTransmitRequest => {
                let nsapi = read_bits(&mut sdu, 4, "nsapi").unwrap_or(0) as u8;
                tracing::debug!(
                    "SNDCP RX DATA TRANSMIT REQ: src_ssi={} nsapi={}",
                    src_ssi,
                    nsapi
                );
                let pdu = build_data_transmit_response(nsapi, true, None);
                self.tx_sndcp_pdu(
                    queue,
                    dltime,
                    prim.received_tetra_address,
                    link_id,
                    endpoint_id,
                    1,
                    pdu,
                );
            }
            _ => {
                tracing::warn!("SNDCP RX: unsupported PDU type {:?}", pdu_type);
            }
        }
    }

    fn evaluate_pdp_demand(&self, demand: &ActivatePdpDemand) -> (bool, u8, Option<Ipv4Addr>) {
        // Accept only IPv4 (static or dynamic) and no compression.
        if demand.addr_type == 2 {
            return (false, 13, None); // IPv6 not supported
        }
        if demand.addr_type == 3 || demand.addr_type == 4 {
            return (false, 18, None); // Mobile IPv4 not supported
        }
        if demand.addr_type == 5 {
            return (false, 26, None); // Secondary PDP contexts not supported
        }
        if demand.pcomp != 0 {
            return (false, 2, None); // Feature not supported
        }

        let ip = if demand.addr_type == 0 {
            demand.requested_ip.or(self.default_ip)
        } else {
            self.default_ip
        };
        if ip.is_none() {
            return (false, 3, None); // Requested resources not available
        }
        (true, 0, ip)
    }

    fn handle_dl_packet(&mut self, queue: &mut MessageQueue, dltime: crate::common::tdma_time::TdmaTime, pkt: PdpDlPacket) {
        self.counters.dl_from_tun_rx = self.counters.dl_from_tun_rx.saturating_add(1);
        if let Some(summary) = describe_ip_payload(&pkt.payload) {
            tracing::info!("SNDCP DL FROM TUN: {}", summary);
        } else if !pkt.payload.is_empty() {
            tracing::debug!(
                "SNDCP DL FROM TUN: non-IP payload len={} hex={}",
                pkt.payload.len(),
                hex_preview(&pkt.payload, 32)
            );
        }
        let (dst_ssi, endpoint_id, link_id) = match self.resolve_route(&pkt) {
            Some(v) => v,
            None => {
                self.counters.dl_route_miss = self.counters.dl_route_miss.saturating_add(1);
                self.log_route_miss(&pkt);
                return;
            }
        };
        self.counters.dl_route_ok = self.counters.dl_route_ok.saturating_add(1);

        let payload = match normalize_ipv4_payload(pkt.payload) {
            Some(p) => p,
            None => {
                tracing::warn!("SNDCP DL: dropped invalid IPv4 payload");
                return;
            }
        };

        let nsapi = self.last_nsapi.get(&dst_ssi).copied().unwrap_or(self.default_nsapi);
        let pdu = build_sn_unitdata(nsapi, &payload);

        tracing::info!(
            "SNDCP DL UNITDATA: dst_ssi={} nsapi={} payload_bytes={}",
            dst_ssi,
            nsapi,
            payload.len()
        );
        self.counters.dl_bytes_sent = self.counters.dl_bytes_sent.saturating_add(payload.len() as u64);

        let dst = TetraAddress {
            encrypted: false,
            ssi_type: SsiType::Issi,
            ssi: dst_ssi,
        };
        self.tx_sndcp_pdu(queue, dltime, dst, link_id, endpoint_id, 0, pdu);
    }

    fn handle_ul_payload(
        &mut self,
        mut payload: Vec<u8>,
        src_ssi: u32,
        nsapi: u8,
        endpoint_id: u16,
        link_id: u16,
    ) {
        self.counters.ul_reassembled_ok = self.counters.ul_reassembled_ok.saturating_add(1);
        let mut ip_version = payload.first().map(|b| b >> 4).unwrap_or(0);
        if ip_version == 4 {
            tracing::info!(
                "SNDCP UL IPv4: nsapi={} {}",
                nsapi,
                describe_ipv4(&payload).unwrap_or_else(|| "invalid IPv4".to_string())
            );
        } else if ip_version == 6 {
            tracing::info!(
                "SNDCP UL IPv6: nsapi={} len={}",
                nsapi,
                payload.len()
            );
        } else {
            tracing::warn!(
                "SNDCP UL: nsapi={} payload not IP (first_nibble=0x{:x}) len={} head={}",
                nsapi,
                ip_version,
                payload.len(),
                hex_preview(&payload, 16)
            );
        }

        if ip_version != 4 && ip_version != 6 && self.sndcp_find_ip {
            if let Some((offset, total_len)) = find_ipv4_packet(&payload) {
                tracing::warn!(
                    "SNDCP UL: found IPv4 header at offset {} (total_len={}), stripping {} bytes",
                    offset,
                    total_len,
                    offset
                );
                payload = payload[offset..offset + total_len].to_vec();
                ip_version = 4;
            }
        }

        if ip_version != 4 && ip_version != 6 {
            self.counters.ul_drop_not_ip = self.counters.ul_drop_not_ip.saturating_add(1);
            return;
        }

        if self.sndcp_dump {
            tracing::info!("SNDCP UL payload (hex): {}", hex_preview(&payload, 64));
        }

        let pkt = PdpUlPacket {
            payload,
            src_ssi,
            endpoint_id,
            link_id,
        };
        self.counters.ul_written_to_tun = self.counters.ul_written_to_tun.saturating_add(1);
        for tx in &self.ul_txs {
            let _ = tx.send(pkt.clone());
        }
    }

    fn log_route_miss(&self, pkt: &PdpDlPacket) {
        let dst_ip = ipv4_dst(&pkt.payload);
        let maybe_ssi = pkt.dst_ssi.or_else(|| dst_ip.and_then(|ip| self.ip_routes.get(&ip).copied()));
        let last_nsapi = maybe_ssi.and_then(|ssi| self.last_nsapi.get(&ssi).copied());
        tracing::warn!(
            "SNDCP DL: no route available dst_ip={:?} dst_ssi={:?} default_ssi={:?} last_nsapi={:?}",
            dst_ip,
            pkt.dst_ssi,
            self.default_ssi,
            last_nsapi
        );
        tracing::debug!(
            "SNDCP DL routes: routes={:?} ip_routes={:?} last_nsapi={:?} pdp_contexts={:?}",
            self.routes,
            self.ip_routes,
            self.last_nsapi,
            self.pdp_contexts
        );
    }

    fn log_stats_if_needed(&mut self) {
        if !self.sndcp_stats {
            return;
        }
        let now = Instant::now();
        let log_now = self
            .last_stats
            .map(|t| now.duration_since(t) >= Duration::from_secs(5))
            .unwrap_or(true);
        if log_now {
            self.last_stats = Some(now);
            tracing::info!(
                "SNDCP STATS ul_rx={} ul_reasm_ok={} ul_written={} ul_drop_not_ip={} dl_rx={} dl_route_ok={} dl_route_miss={} dl_bytes_sent={}",
                self.counters.ul_unitdata_rx,
                self.counters.ul_reassembled_ok,
                self.counters.ul_written_to_tun,
                self.counters.ul_drop_not_ip,
                self.counters.dl_from_tun_rx,
                self.counters.dl_route_ok,
                self.counters.dl_route_miss,
                self.counters.dl_bytes_sent
            );
        }
    }

    fn prune_reassembly(&mut self) {
        let now = Instant::now();
        self.reassembly
            .retain(|_, v| now.duration_since(v.last_update) < Duration::from_secs(30));
    }

    fn resolve_route(&self, pkt: &PdpDlPacket) -> Option<(u32, u16, u16)> {
        let mut ssi = pkt.dst_ssi;
        if ssi.is_none() {
            if let Some(ip) = ipv4_dst(&pkt.payload) {
                if let Some(mapped) = self.ip_routes.get(&ip) {
                    ssi = Some(*mapped);
                }
            }
        }
        let ssi = ssi
            .or(self.default_ssi)
            .or_else(|| self.routes.keys().next().copied())
            .or_else(|| self.last_nsapi.keys().next().copied())
            .unwrap_or(0);
        if ssi == 0 {
            return None;
        }

        let route = self.routes.get(&ssi).copied().or(self.default_route);
        let endpoint_id = pkt.endpoint_id.or(route.map(|r| r.endpoint_id)).unwrap_or(0);
        let link_id = pkt.link_id.or(route.map(|r| r.link_id)).unwrap_or(0);
        Some((ssi, endpoint_id, link_id))
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
            SapMsgInner::LtpdMleUnitdataInd(prim) => {
                self.handle_unitdata_ind(queue, message.dltime, prim);
            }
            _ => {
                tracing::warn!("SNDCP RX: unsupported primitive");
            }
        }
    }

    fn tick_start(&mut self, queue: &mut MessageQueue, ts: Option<crate::common::tdma_time::TdmaTime>) {
        let dltime = ts.unwrap_or_default();
        self.log_stats_if_needed();
        self.prune_reassembly();
        for idx in 0..self.dl_rxs.len() {
            loop {
                let pkt = self.dl_rxs[idx].try_recv();
                match pkt {
                    Ok(pkt) => self.handle_dl_packet(queue, dltime, pkt),
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => break,
                }
            }
        }
    }
}

fn spawn_pdp_udp(listen_addr: &str, peer: Option<SocketAddr>, framed: bool) -> (Receiver<PdpDlPacket>, Sender<PdpUlPacket>) {
    let (dl_tx, dl_rx) = crossbeam_channel::unbounded::<PdpDlPacket>();
    let (ul_tx, ul_rx) = crossbeam_channel::unbounded::<PdpUlPacket>();

    let socket = UdpSocket::bind(listen_addr).expect("SNDCP: failed to bind UDP socket");
    let send_socket = socket.try_clone().expect("SNDCP: failed to clone UDP socket");
    let last_peer = Arc::new(Mutex::new(peer));

    {
        let last_peer = Arc::clone(&last_peer);
        thread::spawn(move || {
            let mut buf = vec![0u8; 4096];
            loop {
                match socket.recv_from(&mut buf) {
                    Ok((n, src)) => {
                        if peer.is_none() {
                            if let Ok(mut lp) = last_peer.lock() {
                                *lp = Some(src);
                            }
                        }
                        let payload = &buf[..n];
                        let pkt = if framed {
                            parse_framed_dl(payload)
                        } else {
                            Some(PdpDlPacket {
                                payload: payload.to_vec(),
                                dst_ssi: None,
                                endpoint_id: None,
                                link_id: None,
                            })
                        };
                        if let Some(p) = pkt {
                            let _ = dl_tx.send(p);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("SNDCP UDP recv error: {}", e);
                    }
                }
            }
        });
    }

    {
        let last_peer = Arc::clone(&last_peer);
        thread::spawn(move || {
            while let Ok(pkt) = ul_rx.recv() {
                let dst = peer.or_else(|| last_peer.lock().ok().and_then(|v| *v));
                let Some(dst) = dst else {
                    tracing::warn!("SNDCP UDP: no peer to send uplink");
                    continue;
                };
                let out = if framed {
                    encode_framed_ul(&pkt)
                } else {
                    pkt.payload.clone()
                };
                if let Err(e) = send_socket.send_to(&out, dst) {
                    tracing::warn!("SNDCP UDP send error: {}", e);
                }
            }
        });
    }

    (dl_rx, ul_tx)
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct IfReq {
    ifr_name: [libc::c_char; libc::IFNAMSIZ],
    ifr_flags: c_short,
}

#[cfg(target_os = "linux")]
fn create_tun(name: &str) -> Result<(File, File)> {
    let fd = unsafe { libc::open(b"/dev/net/tun\0".as_ptr() as *const libc::c_char, libc::O_RDWR) };
    if fd < 0 {
        return Err(anyhow!("failed to open /dev/net/tun"));
    }

    let mut ifr = IfReq {
        ifr_name: [0; libc::IFNAMSIZ],
        ifr_flags: IFF_TUN | IFF_NO_PI,
    };

    for (i, b) in name.as_bytes().iter().take(libc::IFNAMSIZ - 1).enumerate() {
        ifr.ifr_name[i] = *b as libc::c_char;
    }

    let res = unsafe { libc::ioctl(fd, TUNSETIFF, &ifr) };
    if res < 0 {
        return Err(anyhow!("ioctl(TUNSETIFF) failed"));
    }

    let file = unsafe { File::from_raw_fd(fd) };
    let read_file = file.try_clone().map_err(|e| anyhow!("tun clone failed: {e}"))?;
    Ok((read_file, file))
}

#[cfg(target_os = "linux")]
fn spawn_pdp_tun(name: &str, mtu: Option<usize>) -> Result<(Receiver<PdpDlPacket>, Sender<PdpUlPacket>)> {
    let (dl_tx, dl_rx) = crossbeam_channel::unbounded::<PdpDlPacket>();
    let (ul_tx, ul_rx) = crossbeam_channel::unbounded::<PdpUlPacket>();

    let (mut read_file, mut write_file) = create_tun(name)?;
    configure_tun_interface(name, mtu);
    let read_mtu = mtu.unwrap_or(2000).max(256);

    thread::spawn(move || {
        let mut buf = vec![0u8; read_mtu];
        loop {
            match read_file.read(&mut buf) {
                Ok(n) if n > 0 => {
                    let pkt = PdpDlPacket {
                        payload: buf[..n].to_vec(),
                        dst_ssi: None,
                        endpoint_id: None,
                        link_id: None,
                    };
                    let _ = dl_tx.send(pkt);
                }
                Ok(_) => continue,
                Err(e) => {
                    tracing::warn!("SNDCP TUN read error: {}", e);
                    break;
                }
            }
        }
    });

    thread::spawn(move || {
        while let Ok(pkt) = ul_rx.recv() {
            if let Err(e) = write_file.write_all(&pkt.payload) {
                tracing::warn!("SNDCP TUN write error: {}", e);
                break;
            }
        }
    });

    Ok((dl_rx, ul_tx))
}

#[cfg(target_os = "linux")]
fn configure_tun_interface(name: &str, mtu: Option<usize>) {
    let addr = std::env::var("TETRA_PDP_TUN_ADDR").ok();
    if matches!(addr.as_deref(), Some("0") | Some("none") | Some("NONE") | Some("disable") | Some("DISABLE")) {
        return;
    }
    let addr = addr.unwrap_or_else(|| DEFAULT_TUN_ADDR.to_string());

    let mut cmd = Command::new("ip");
    cmd.args(["addr", "replace", &addr, "dev", name]);
    match cmd.status() {
        Ok(status) if !status.success() => {
            tracing::warn!("SNDCP TUN: ip addr replace failed for {} on {}", addr, name);
        }
        Err(e) => {
            tracing::warn!("SNDCP TUN: failed to set addr {} on {}: {}", addr, name, e);
        }
        _ => {}
    }

    let mut cmd = Command::new("ip");
    cmd.args(["link", "set", "dev", name]);
    if let Some(mtu) = mtu {
        cmd.args(["mtu", &mtu.to_string()]);
    }
    cmd.arg("up");
    match cmd.status() {
        Ok(status) if !status.success() => {
            tracing::warn!("SNDCP TUN: ip link set failed for {}", name);
        }
        Err(e) => {
            tracing::warn!("SNDCP TUN: failed to bring {} up: {}", name, e);
        }
        _ => {}
    }
}

fn parse_framed_dl(buf: &[u8]) -> Option<PdpDlPacket> {
    if buf.len() < PDP_HDR_LEN {
        return None;
    }
    let ssi = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let endpoint_id = u16::from_be_bytes([buf[4], buf[5]]);
    let link_id = u16::from_be_bytes([buf[6], buf[7]]);
    let payload = buf[PDP_HDR_LEN..].to_vec();
    Some(PdpDlPacket {
        payload,
        dst_ssi: if ssi == 0 { None } else { Some(ssi) },
        endpoint_id: if endpoint_id == 0 { None } else { Some(endpoint_id) },
        link_id: if link_id == 0 { None } else { Some(link_id) },
    })
}

fn encode_framed_ul(pkt: &PdpUlPacket) -> Vec<u8> {
    let mut out = Vec::with_capacity(PDP_HDR_LEN + pkt.payload.len());
    out.extend_from_slice(&pkt.src_ssi.to_be_bytes());
    out.extend_from_slice(&pkt.endpoint_id.to_be_bytes());
    out.extend_from_slice(&pkt.link_id.to_be_bytes());
    out.extend_from_slice(&pkt.payload);
    out
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn env_flag_default(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(default)
}

fn env_u8(name: &str) -> Option<u8> {
    std::env::var(name).ok().and_then(|s| s.parse::<u8>().ok())
}

fn todo_to_u16(v: Todo) -> u16 {
    if v < 0 { 0 } else { v as u16 }
}

fn read_bits(bb: &mut BitBuffer, bits: usize, field: &'static str) -> Option<u64> {
    bb.read_bits(bits).or_else(|| {
        tracing::warn!("SNDCP decode: insufficient bits for {}", field);
        None
    })
}

fn ipv4_dst(payload: &[u8]) -> Option<Ipv4Addr> {
    if payload.len() < 20 {
        return None;
    }
    let v = payload[0] >> 4;
    if v != 4 {
        return None;
    }
    let ihl = (payload[0] & 0x0F) as usize * 4;
    if payload.len() < ihl || payload.len() < 20 {
        return None;
    }
    Some(Ipv4Addr::new(payload[16], payload[17], payload[18], payload[19]))
}

fn normalize_ipv4_payload(payload: Vec<u8>) -> Option<Vec<u8>> {
    if payload.is_empty() {
        return Some(payload);
    }
    let v = payload[0] >> 4;
    if v != 4 {
        return Some(payload);
    }
    if payload.len() < 20 {
        return None;
    }
    let ihl = (payload[0] & 0x0F) as usize * 4;
    if ihl < 20 || payload.len() < ihl {
        return None;
    }
    let total_len = u16::from_be_bytes([payload[2], payload[3]]) as usize;
    if total_len < ihl {
        return None;
    }
    if total_len > payload.len() {
        return None;
    }
    if total_len == payload.len() {
        return Some(payload);
    }
    let mut trimmed = payload;
    trimmed.truncate(total_len);
    Some(trimmed)
}

fn describe_ipv4(payload: &[u8]) -> Option<String> {
    if payload.len() < 20 {
        return None;
    }
    let v = payload[0] >> 4;
    if v != 4 {
        return None;
    }
    let ihl = (payload[0] & 0x0F) as usize * 4;
    if ihl < 20 || payload.len() < ihl {
        return None;
    }
    let total_len = u16::from_be_bytes([payload[2], payload[3]]) as usize;
    let proto = payload[9];
    let src = Ipv4Addr::new(payload[12], payload[13], payload[14], payload[15]);
    let dst = Ipv4Addr::new(payload[16], payload[17], payload[18], payload[19]);
    let mut summary = format!("{} -> {} proto={} len={}", src, dst, proto, total_len);
    if proto == 17 && payload.len() >= ihl + 4 {
        let sp = u16::from_be_bytes([payload[ihl], payload[ihl + 1]]);
        let dp = u16::from_be_bytes([payload[ihl + 2], payload[ihl + 3]]);
        summary.push_str(&format!(" udp {}->{}", sp, dp));
    }
    Some(summary)
}

fn describe_ip_payload(payload: &[u8]) -> Option<String> {
    if let Some(v4) = describe_ipv4(payload) {
        return Some(v4);
    }
    if payload.len() >= 40 && (payload[0] >> 4) == 6 {
        let payload_len = u16::from_be_bytes([payload[4], payload[5]]) as usize;
        let next_header = payload[6];
        let src = format!(
            "{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}",
            payload[8], payload[9], payload[10], payload[11], payload[12], payload[13], payload[14], payload[15],
            payload[16], payload[17], payload[18], payload[19], payload[20], payload[21], payload[22], payload[23]
        );
        let dst = format!(
            "{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}",
            payload[24], payload[25], payload[26], payload[27], payload[28], payload[29], payload[30], payload[31],
            payload[32], payload[33], payload[34], payload[35], payload[36], payload[37], payload[38], payload[39]
        );
        return Some(format!(
            "{} -> {} next_header={} payload_len={}",
            src, dst, next_header, payload_len
        ));
    }
    None
}

fn hex_preview(data: &[u8], max: usize) -> String {
    let mut s = String::new();
    for (i, b) in data.iter().take(max).enumerate() {
        if i > 0 {
            s.push(' ');
        }
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{:02x}", b));
    }
    if data.len() > max {
        s.push_str(" ...");
    }
    s
}

fn bitbuffer_to_bytes_from_pos(mut bb: BitBuffer) -> Option<Vec<u8>> {
    let bits = bb.get_len_remaining();
    if bits % 8 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bits / 8);
    while bb.get_len_remaining() >= 8 {
        out.push(bb.read_bits(8)? as u8);
    }
    Some(out)
}

fn parse_unitdata_header(bb: &mut BitBuffer) -> Option<(u8, u8, u8)> {
    let nsapi = read_bits(bb, 4, "nsapi")? as u8;
    let pcomp = read_bits(bb, 4, "pcomp")? as u8;
    let dcomp = read_bits(bb, 4, "dcomp")? as u8;
    Some((nsapi, pcomp, dcomp))
}

fn parse_activate_pdp_demand(bb: &mut BitBuffer) -> Option<ActivatePdpDemand> {
    let version = read_bits(bb, 4, "sndcp_version")? as u8;
    let nsapi = read_bits(bb, 4, "nsapi")? as u8;
    let addr_type = read_bits(bb, 3, "addr_type_identifier")? as u8;
    let requested_ip = if addr_type == 0 {
        let ip = read_bits(bb, 32, "ipv4_address")? as u32;
        Some(Ipv4Addr::from(ip))
    } else {
        None
    };

    if addr_type == 5 {
        let _ = read_bits(bb, 4, "secondary_nsapi");
    }

    let _packet_data_ms_type = read_bits(bb, 4, "packet_data_ms_type").unwrap_or(0) as u8;
    let pcomp = read_bits(bb, 8, "pcomp_negotiation").unwrap_or(0) as u8;

    Some(ActivatePdpDemand {
        version,
        nsapi,
        addr_type,
        requested_ip,
        pcomp,
    })
}

fn find_ipv4_packet(payload: &[u8]) -> Option<(usize, usize)> {
    let max_off = 32usize.min(payload.len());
    for offset in 0..=max_off {
        if payload.len() < offset + 20 {
            break;
        }
        let b0 = payload[offset];
        if (b0 >> 4) != 4 {
            continue;
        }
        let ihl = (b0 & 0x0F) as usize * 4;
        if ihl < 20 || ihl > 60 {
            continue;
        }
        if payload.len() < offset + ihl {
            continue;
        }
        let total_len = u16::from_be_bytes([payload[offset + 2], payload[offset + 3]]) as usize;
        if total_len < ihl {
            continue;
        }
        if payload.len() < offset + total_len {
            continue;
        }
        return Some((offset, total_len));
    }
    None
}

fn build_sn_unitdata(nsapi: u8, payload: &[u8]) -> BitBuffer {
    let mut bb = BitBuffer::new(16 + payload.len() * 8);
    bb.write_bits(SnPduType::Unitdata as u64, 4);
    bb.write_bits((nsapi & 0xF) as u64, 4);
    bb.write_bits(0, 4); // PCOMP
    bb.write_bits(0, 4); // DCOMP
    for b in payload {
        bb.write_bits(*b as u64, 8);
    }
    bb.seek(0);
    bb
}

fn build_activate_pdp_accept(
    version: u8,
    nsapi: u8,
    tia: u8,
    ip: Option<Ipv4Addr>,
    timers: PdpTimers,
    pdu_prio_max: u8,
) -> BitBuffer {
    let mut bb = BitBuffer::new_autoexpand(256);
    bb.write_bits(SnPduType::ActivatePdpContext as u64, 4);
    bb.write_bits((version & 0xF) as u64, 4);
    bb.write_bits((nsapi & 0xF) as u64, 4);
    bb.write_bits((pdu_prio_max & 0x7) as u64, 3);
    bb.write_bits((timers.ready & 0xF) as u64, 4);
    bb.write_bits((timers.standby & 0xF) as u64, 4);
    bb.write_bits((timers.response_wait & 0xF) as u64, 4);
    bb.write_bits((tia & 0x7) as u64, 3);
    if matches!(tia, 1 | 2) {
        let ip_u32 = u32::from(ip.unwrap_or(Ipv4Addr::from_str(DEFAULT_PDP_IP).unwrap()));
        bb.write_bits(ip_u32 as u64, 32);
    }
    bb.write_bits(0, 8); // PCOMP negotiation
    bb.seek(0);
    bb
}

fn build_activate_pdp_reject(nsapi: u8, cause: u8) -> BitBuffer {
    let mut bb = BitBuffer::new(16);
    bb.write_bits(SnPduType::ActivatePdpContextReject as u64, 4);
    bb.write_bits((nsapi & 0xF) as u64, 4);
    bb.write_bits(cause as u64, 8);
    bb.seek(0);
    bb
}

fn build_deactivate_pdp_accept(nsapi: u8) -> BitBuffer {
    let mut bb = BitBuffer::new(8);
    bb.write_bits(SnPduType::DeactivatePdpContextAccept as u64, 4);
    bb.write_bits((nsapi & 0xF) as u64, 4);
    bb.seek(0);
    bb
}

fn build_data_transmit_response(nsapi: u8, accept: bool, cause: Option<u8>) -> BitBuffer {
    let mut bb = BitBuffer::new_autoexpand(32);
    bb.write_bits(SnPduType::DataTransmitResponse as u64, 4);
    bb.write_bits((nsapi & 0xF) as u64, 4);
    bb.write_bits(if accept { 1 } else { 0 }, 1);
    if !accept {
        bb.write_bits(cause.unwrap_or(0) as u64, 8);
    }
    bb.seek(0);
    bb
}
