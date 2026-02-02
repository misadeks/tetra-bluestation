//! PEI/AT serial interface (A2).
//!
//! Enabled via environment variables:
//! - `TETRA_PEI_AT_PORT` (e.g. `/dev/ttyUSB0` or `COM3`)
//! - `TETRA_PEI_AT_BAUD` (default 115200)
//!
//! Supported commands (minimal, Motorola-like):
//! - `AT`
//! - `AT+CTCALL=<ssi>` : set calling party SSI
//! - `AT+CTADDR=I|G` : destination type (ISSI/GSSI)
//! - `AT+CTSDS=<area_sel>[,<link_id>,<endpoint_id>]` : routing selectors (B3)
//! - `AT+CMGS=<dst>,<bitlen>` then `>` prompt, send hex payload, terminate with Ctrl+Z (0x1A)
//! - `AT+CMGS=<dst>,<bitlen>,<hex>` inline
//! - `AT+CMGSTEXT=<dst>,<text>` send SDS-TL text (PID=0x82)

use std::io::{Read, Write};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use serialport::SerialPort;

use crate::common::address::{SsiType, TetraAddress};
use crate::common::bitbuffer::BitBuffer;
use crate::common::messagerouter::MessageQueue;
use crate::common::tdma_time::TdmaTime;
use crate::common::tetra_common::Sap;
use crate::common::tetra_entities::TetraEntity;
use crate::entities::TetraEntityTrait;
use crate::saps::sapmsg::{SapMsg, SapMsgInner};
use crate::saps::tnsds::TnsdsUnitdataReq;
use crate::common::sds_codec::{encode_sds_tl_text_multi, TextCodingScheme, MAX_SDS_TYPE4_BITS};

#[derive(Debug, Clone)]
struct PendingReq {
    req: TnsdsUnitdataReq,
}

pub struct PeiAt {
    rx: Option<Receiver<PendingReq>>,
    calling_ssi: u32,
    dst_type: SsiType,
    link_id: u16,
    endpoint_id: u16,
    mr: u8,
}

impl PeiAt {
    pub fn new() -> Self {
        let port_name = std::env::var("TETRA_PEI_AT_PORT").ok();
        if port_name.is_none() {
            return Self {
                rx: None,
                calling_ssi: 0,
                dst_type: SsiType::Issi,
                link_id: 0,
                endpoint_id: 0,
                mr: 1,
            };
        }

        let baud = std::env::var("TETRA_PEI_AT_BAUD")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(115200);

        let (tx, rx) = crossbeam_channel::unbounded::<PendingReq>();
        let port_name = port_name.unwrap();

        std::thread::spawn(move || {
            if let Err(e) = serial_thread(&port_name, baud, tx) {
                eprintln!("PEI/AT thread exited: {e}");
            }
        });

        Self {
            rx: Some(rx),
            calling_ssi: 0,
            dst_type: SsiType::Issi,
            link_id: 0,
            endpoint_id: 0,
            mr: 1,
        }
    }
}

fn serial_thread(port_name: &str, baud: u32, tx: Sender<PendingReq>) -> anyhow::Result<()> {
    let mut port = serialport::new(port_name, baud)
        .timeout(Duration::from_millis(50))
        .open()?;

    let mut buf = [0u8; 256];
    let mut line = Vec::<u8>::new();

    // CMGS data mode
    let mut cmgs_mode: Option<(u32, usize, Vec<u8>, SsiType, u16, u16, u32, u8)> = None;
    // (dst, bitlen, hex_bytes, dst_type, link_id, endpoint_id, calling_ssi, mr)

    let mut calling_ssi: u32 = 0;
    let mut dst_type: SsiType = SsiType::Issi;
    let mut link_id: u16 = 0;
    let mut endpoint_id: u16 = 0;
    let mut mr: u8 = 1;

    loop {
        match port.read(&mut buf) {
            Ok(n) if n > 0 => {
                for &b in &buf[..n] {
                    if let Some((dst, bitlen, hex_bytes, dt, li, ei, cs, mrv)) = cmgs_mode.as_mut() {
                        if b == 0x1A {
                            // Ctrl+Z ends payload
                            let hex_str = String::from_utf8_lossy(hex_bytes).to_string();
                            if let Some(payload) = hex_to_bitbuffer(&hex_str, *bitlen) {
                                if payload.get_len() > MAX_SDS_TYPE4_BITS {
                                    let _ = port.write_all(b"\r\nERROR\r\n");
                                    *hex_bytes = Vec::new();
                                    cmgs_mode = None;
                                    continue;
                                }
                                let req = TnsdsUnitdataReq {
                                    dst: TetraAddress { encrypted: false, ssi_type: *dt, ssi: *dst },
                                    calling_ssi: *cs,
                                    link_id: *li,
                                    endpoint_id: *ei,
                                    type4_payload: payload,
                                    delivery_report_request: false,
                                    message_reference: *mrv,
                                    trace: None,
                                };
                                tx.send(PendingReq { req }).ok();
                                *hex_bytes = Vec::new();
                                let _ = port.write_all(b"\r\nOK\r\n");
                            } else {
                                let _ = port.write_all(b"\r\nERROR\r\n");
                            }
                            cmgs_mode = None;
                            continue;
                        }
                        // accept hex chars and whitespace
                        if b.is_ascii_hexdigit() || b == b' ' || b == b'\r' || b == b'\n' || b == b'\t' {
                            hex_bytes.push(b);
                        }
                        continue;
                    }

                    if b == b'\n' {
                        let cmd = String::from_utf8_lossy(&line).trim().trim_end_matches('\r').to_string();
                        line.clear();
                        if cmd.is_empty() {
                            continue;
                        }
                        handle_at_command(
                            &mut port,
                            &cmd,
                            &mut calling_ssi,
                            &mut dst_type,
                            &mut link_id,
                            &mut endpoint_id,
                            &mut mr,
                            &mut cmgs_mode,
                            &tx,
                        );
                    } else {
                        line.push(b);
                    }
                }
            }
            _ => {}
        }
    }
}

fn handle_at_command(
    port: &mut Box<dyn SerialPort>,
    cmd: &str,
    calling_ssi: &mut u32,
    dst_type: &mut SsiType,
    link_id: &mut u16,
    endpoint_id: &mut u16,
    mr: &mut u8,
    cmgs_mode: &mut Option<(u32, usize, Vec<u8>, SsiType, u16, u16, u32, u8)>,
    tx: &Sender<PendingReq>,
) {
    if cmd.eq_ignore_ascii_case("AT") {
        let _ = port.write_all(b"OK\r\n");
        return;
    }
    if let Some(v) = cmd.strip_prefix("AT+CTCALL=") {
        if let Ok(ssi) = v.trim().parse::<u32>() {
            *calling_ssi = ssi;
            let _ = port.write_all(b"OK\r\n");
        } else {
            let _ = port.write_all(b"ERROR\r\n");
        }
        return;
    }
    if let Some(v) = cmd.strip_prefix("AT+CTADDR=") {
        *dst_type = match v.trim() {
            "I" | "i" => SsiType::Issi,
            "G" | "g" => SsiType::Gssi,
            _ => SsiType::Issi,
        };
        let _ = port.write_all(b"OK\r\n");
        return;
    }
    if let Some(v) = cmd.strip_prefix("AT+CTSDS=") {
        // area_sel ignored for now; store link/endpoint for routing selectors
        let parts: Vec<&str> = v.split(',').map(|s| s.trim()).collect();
        if parts.len() >= 2 {
            *link_id = parts[1].parse::<u16>().unwrap_or(0);
        }
        if parts.len() >= 3 {
            *endpoint_id = parts[2].parse::<u16>().unwrap_or(0);
        }
        let _ = port.write_all(b"OK\r\n");
        return;
    }
    if let Some(v) = cmd.strip_prefix("AT+CMGSTEXT=") {
        let mut it = v.splitn(2, ',');
        let dst = it.next().and_then(|s| s.trim().parse::<u32>().ok());
        let text = it.next().map(|s| s.trim()).unwrap_or("");
        if let Some(dst) = dst {
            let base_mr = *mr;
            let payloads = encode_sds_tl_text_multi(text, TextCodingScheme::Gsm7, base_mr, false);
            let parts = payloads.len();
            for (mr_i, payload) in payloads {
                if payload.get_len() > MAX_SDS_TYPE4_BITS {
                    let _ = port.write_all(b"ERROR\r\n");
                    return;
                }
                let req = TnsdsUnitdataReq {
                    dst: TetraAddress { encrypted: false, ssi_type: *dst_type, ssi: dst },
                    calling_ssi: *calling_ssi,
                    link_id: *link_id,
                    endpoint_id: *endpoint_id,
                    type4_payload: payload,
                    delivery_report_request: false,
                    message_reference: mr_i,
                    trace: None,
                };
                tx.send(PendingReq { req }).ok();
            }
            *mr = base_mr.wrapping_add(parts as u8);
            let _ = port.write_all(b"OK\r\n");
        } else {
            let _ = port.write_all(b"ERROR\r\n");
        }
        return;
    }
    if let Some(v) = cmd.strip_prefix("AT+CMGS=") {
        let parts: Vec<&str> = v.split(',').map(|s| s.trim()).collect();
        if parts.len() < 2 {
            let _ = port.write_all(b"ERROR\r\n");
            return;
        }
        let dst = parts[0].parse::<u32>().ok();
        let bitlen = parts[1].parse::<usize>().ok();
        if dst.is_none() || bitlen.is_none() {
            let _ = port.write_all(b"ERROR\r\n");
            return;
        }
        let dst = dst.unwrap();
        let bitlen = bitlen.unwrap();
        if parts.len() >= 3 {
            let hex = parts[2..].join(",");
            if let Some(payload) = hex_to_bitbuffer(&hex, bitlen) {
                if payload.get_len() > MAX_SDS_TYPE4_BITS {
                    let _ = port.write_all(b"ERROR\r\n");
                    return;
                }
                let req = TnsdsUnitdataReq {
                    dst: TetraAddress { encrypted: false, ssi_type: *dst_type, ssi: dst },
                    calling_ssi: *calling_ssi,
                    link_id: *link_id,
                    endpoint_id: *endpoint_id,
                    type4_payload: payload,
                    delivery_report_request: false,
                    message_reference: *mr,
                    trace: None,
                };
                tx.send(PendingReq { req }).ok();
                let _ = port.write_all(b"OK\r\n");
            } else {
                let _ = port.write_all(b"ERROR\r\n");
            }
            return;
        }

        // prompt mode
        *cmgs_mode = Some((dst, bitlen, Vec::new(), *dst_type, *link_id, *endpoint_id, *calling_ssi, *mr));
        let _ = port.write_all(b">\r\n");
        return;
    }

    let _ = port.write_all(b"ERROR\r\n");
}

fn hex_to_bitbuffer(hex: &str, bitlen: usize) -> Option<BitBuffer> {
    let cleaned: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if cleaned.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::new();
    for i in (0..cleaned.len()).step_by(2) {
        let b = u8::from_str_radix(&cleaned[i..i + 2], 16).ok()?;
        bytes.push(b);
    }

    if bitlen > bytes.len() * 8 {
        return None;
    }

    // BitBuffer::from_bytes creates a window based on bytes*8; then clamp by bitlen.
    let mut bb = BitBuffer::from_bytes(&bytes);
    bb.set_raw_end(bitlen);
    bb.seek(0);
    Some(bb)
}

impl TetraEntityTrait for PeiAt {
    fn entity(&self) -> TetraEntity {
        TetraEntity::PeiAt
    }

    fn rx_prim(&mut self, _queue: &mut MessageQueue, _message: SapMsg) {
        // no inbound primitives expected
    }

    fn tick_start(&mut self, queue: &mut MessageQueue, ts: Option<TdmaTime>) {
        let Some(rx) = &self.rx else { return; };
        let dltime = ts.unwrap_or_default();
        while let Ok(p) = rx.try_recv() {
            let msg = SapMsg {
                sap: Sap::TnsdsSap,
                src: TetraEntity::PeiAt,
                dest: TetraEntity::Cmce,
                dltime,
                msg: SapMsgInner::TnsdsUnitdataReq(p.req),
            };
            queue.push_back(msg);
        }
    }
}
