//! External command server (TCP JSON lines).
//!
//! Enabled with environment variable:
//! - `TETRA_CMD_PORT` (e.g. `4444`)
//!
//! Each line is a JSON object ending with `\n`.
//!
//! Commands:
//! - send_text:
//!   {"cmd":"send_text","dst_type":"I","dst":10002,"calling":10001,"link_id":0,"endpoint_id":0,
//!    "text":"HELLO","tcs":"gsm7","mr":1,"drr":false}
//! - send_hex:
//!   {"cmd":"send_hex","dst_type":"G","dst":20001,"calling":10001,"link_id":0,"endpoint_id":0,
//!    "bitlen":64,"hex":"82020100A0B1","mr":1,"drr":false}

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use crossbeam_channel::{Receiver, Sender};
use serde::Deserialize;

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

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd")]
enum Cmd {
    #[serde(rename="send_text")]
    SendText {
        dst_type: String,
        dst: u32,
        calling: u32,
        link_id: Option<u16>,
        endpoint_id: Option<u16>,
        text: String,
        tcs: Option<String>,
        mr: Option<u8>,
        drr: Option<bool>,
    },
    #[serde(rename="send_hex")]
    SendHex {
        dst_type: String,
        dst: u32,
        calling: u32,
        link_id: Option<u16>,
        endpoint_id: Option<u16>,
        bitlen: usize,
        hex: String,
        mr: Option<u8>,
        drr: Option<bool>,
    },
}

pub struct CmdServer {
    rx: Option<Receiver<PendingReq>>,
}

impl CmdServer {
    pub fn new() -> Self {
        let port = std::env::var("TETRA_CMD_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok());
        if port.is_none() {
            return Self { rx: None };
        }
        let (tx, rx) = crossbeam_channel::unbounded::<PendingReq>();
        thread::spawn(move || {
            if let Err(e) = server_thread(port.unwrap(), tx) {
                eprintln!("CmdServer thread exited: {e}");
            }
        });
        Self { rx: Some(rx) }
    }
}

fn server_thread(port: u16, tx: Sender<PendingReq>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let txc = tx.clone();
                thread::spawn(move || {
                    let _ = handle_client(s, txc);
                });
            }
            Err(e) => eprintln!("incoming failed: {e}"),
        }
    }
    Ok(())
}

fn handle_client(stream: TcpStream, tx: Sender<PendingReq>) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Cmd>(trimmed) {
            Ok(cmd) => {
                if let Some(reqs) = cmd_to_reqs(cmd) {
                    let parts = reqs.len();
                    for req in reqs {
                        tx.send(PendingReq { req }).ok();
                    }
                    if parts > 1 {
                        let _ = writer.write_all(format!("{{\"ok\":true,\"parts\":{}}}\n", parts).as_bytes());
                    } else {
                        let _ = writer.write_all(b"{\"ok\":true}\n");
                    }
                } else {
                    let _ = writer.write_all(b"{\"ok\":false,\"err\":\"bad_request\"}\n");
                }
            }
            Err(_) => {
                let _ = writer.write_all(b"{\"ok\":false,\"err\":\"json\"}\n");
            }
        }
    }
    Ok(())
}

fn cmd_to_reqs(cmd: Cmd) -> Option<Vec<TnsdsUnitdataReq>> {
    match cmd {
        Cmd::SendText { dst_type, dst, calling, link_id, endpoint_id, text, tcs, mr, drr } => {
            let ssi_type = if dst_type.eq_ignore_ascii_case("G") { SsiType::Gssi } else { SsiType::Issi };
            let tcs = match tcs.as_deref() {
                Some("ucs2") => TextCodingScheme::Ucs2,
                Some("latin1") => TextCodingScheme::Latin1,
                _ => TextCodingScheme::Gsm7,
            };
            let mr_v = mr.unwrap_or(1);
            let drr_v = drr.unwrap_or(false);

            let payloads = encode_sds_tl_text_multi(&text, tcs, mr_v, drr_v);
            let mut out: Vec<TnsdsUnitdataReq> = Vec::new();

            for (mr_i, payload) in payloads {
                if payload.get_len() > MAX_SDS_TYPE4_BITS {
                    return None;
                }
                out.push(TnsdsUnitdataReq {
                    dst: TetraAddress { encrypted: false, ssi_type, ssi: dst },
                    calling_ssi: calling,
                    link_id: link_id.unwrap_or(0),
                    endpoint_id: endpoint_id.unwrap_or(0),
                    type4_payload: payload,
                    delivery_report_request: drr_v,
                    message_reference: mr_i,
                    trace: None,
                });
            }
            Some(out)
        }
        Cmd::SendHex { dst_type, dst, calling, link_id, endpoint_id, bitlen, hex, mr, drr } => {
            let ssi_type = if dst_type.eq_ignore_ascii_case("G") { SsiType::Gssi } else { SsiType::Issi };
            let mr_v = mr.unwrap_or(1);
            let drr_v = drr.unwrap_or(false);
            let payload = hex_to_bitbuffer(&hex, bitlen)?;
            if payload.get_len() > MAX_SDS_TYPE4_BITS {
                return None;
            }
            Some(vec![TnsdsUnitdataReq {
                dst: TetraAddress { encrypted: false, ssi_type, ssi: dst },
                calling_ssi: calling,
                link_id: link_id.unwrap_or(0),
                endpoint_id: endpoint_id.unwrap_or(0),
                type4_payload: payload,
                delivery_report_request: drr_v,
                message_reference: mr_v,
                trace: None,
            }])
        }
    }
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
    let mut bb = BitBuffer::from_bytes(&bytes);
    bb.set_raw_end(bitlen);
    bb.seek(0);
    Some(bb)
}

impl TetraEntityTrait for CmdServer {
    fn entity(&self) -> TetraEntity {
        TetraEntity::CmdServer
    }

    fn rx_prim(&mut self, _queue: &mut MessageQueue, _message: SapMsg) {}

    fn tick_start(&mut self, queue: &mut MessageQueue, ts: Option<TdmaTime>) {
        let Some(rx) = &self.rx else { return; };
        let dltime = ts.unwrap_or_default();
        while let Ok(p) = rx.try_recv() {
            let msg = SapMsg {
                sap: Sap::TnsdsSap,
                src: TetraEntity::CmdServer,
                dest: TetraEntity::Cmce,
                dltime,
                msg: SapMsgInner::TnsdsUnitdataReq(p.req),
            };
            queue.push_back(msg);
        }
    }
}
