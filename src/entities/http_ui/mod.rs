//! Minimal HTTP UI + API bridge (pure std::net).
//!
//! Enabled with environment variable:
//! - `TETRA_HTTP_PORT` (e.g. `8080`)
//!
//! Binds `0.0.0.0:<port>`.
//!
//! Routes:
//! - `GET /` -> HTML UI
//! - `POST /api/send_text` -> send text (PID 0x82 / SDS-TL)
//! - `POST /api/send_hex`  -> send raw Type-4 payload
//! - `GET /api/online?ttl=300` -> online SSI list (best-effort)
//! - `GET /api/group_online?gssi=20001&ttl=300` -> group-recent SSI list (best-effort)
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use crate::common::address::{SsiType, TetraAddress};
use crate::common::bitbuffer::BitBuffer;
use crate::common::messagerouter::MessageQueue;
use crate::common::online;
use crate::common::groupbook;
use crate::common::sds_job;
use crate::common::dgna_job;
use crate::common::voice_state;
use crate::common::tdma_time::TdmaTime;
use crate::common::tetra_common::Sap;
use crate::common::tetra_entities::TetraEntity;
use crate::common::sds_codec::{encode_sds_tl_text_multi, encode_simple_text_multi, TextCodingScheme, MAX_SDS_TYPE4_BITS};
use crate::entities::TetraEntityTrait;
use crate::saps::sapmsg::{SapMsg, SapMsgInner};
use crate::saps::tnsds::TnsdsUnitdataReq;
use crate::saps::dgna::{DgnaOp, DgnaSetReq};

#[derive(Debug, Clone)]
enum PendingReq {
    Sds(TnsdsUnitdataReq),
    Dgna(DgnaSetReq),
}
#[derive(Debug, Serialize)]
struct ApiResp {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    info: Option<String>,
}

#[derive(Debug, Serialize)]
struct SendTextApiResp {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    info: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    job: Option<sds_job::SdsJob>,
}

#[derive(Debug, Serialize)]
struct DgnaApiResp {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    info: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    job: Option<dgna_job::DgnaJob>,
}

#[derive(Debug, Deserialize)]
struct GroupUpsertReq {
    name: String,
    gssi: u32,
}

#[derive(Debug, Deserialize)]
struct GroupDeleteReq {
    gssi: u32,
}

#[derive(Debug, Deserialize)]
struct VoiceStateReq {
    enabled: bool,
    tch_ts: u8,
    tchan: u8,
}

#[derive(Debug, Deserialize)]
struct GroupSetMembersReq {
    gssi: u32,
    members: Vec<u32>,
}

#[derive(Debug, Deserialize)]
struct GroupMoveMemberReq {
    issi: u32,
    to_gssi: u32,
}

#[derive(Debug, Serialize)]
struct IdListResp {
    ids: Vec<u32>,
}

#[derive(Debug, Serialize)]
struct GroupOnlineEntry {
    gssi: u32,
    ids: Vec<u32>,
}

#[derive(Debug, Serialize)]
struct GroupOnlineAllResp {
    groups: Vec<GroupOnlineEntry>,
}
#[derive(Debug, Deserialize)]
struct SendTextReq {
    dst_type: String, // "I" or "G"
    dst: u32,
    calling: u32,
    link_id: Option<u16>,
    endpoint_id: Option<u16>,
    text: String,
    format: Option<String>, // "sds_tl" (PID 0x82) | "simple" (PID 0x02)
    tcs: Option<String>, // "ucs2" | "latin1" | "gsm7"
    mr: Option<u8>,
    drr: Option<bool>,
}
#[derive(Debug, Deserialize)]
struct SendHexReq {
    dst_type: String, // "I" or "G"
    dst: u32,
    calling: u32,
    link_id: Option<u16>,
    endpoint_id: Option<u16>,
    bitlen: usize,
    hex: String,
    mr: Option<u8>,
    drr: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct DgnaApiReq {
    targets: Vec<u32>,
    gssi: u32,
    detach_all_first: Option<bool>,
    lifetime: Option<u8>,
    class_of_usage: Option<u8>,
    ack_req: Option<bool>,
}
pub struct HttpUi {
    rx: Option<Receiver<PendingReq>>,
}
impl HttpUi {
    pub fn new() -> Self {
        let env_port = std::env::var("TETRA_HTTP_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok());

        let port = env_port.unwrap_or(8080);

        match env_port {
            Some(p) => println!("HttpUi: using TETRA_HTTP_PORT={p}"),
            None => println!("HttpUi: TETRA_HTTP_PORT not set/invalid, defaulting to {port}"),
        }
        let (tx, rx) = crossbeam_channel::unbounded::<PendingReq>();
        thread::spawn(move || {
            if let Err(e) = server_thread(port, tx) {
                eprintln!("HttpUi thread exited: {e}");
            }
        });
        Self { rx: Some(rx) }
    }
}
impl TetraEntityTrait for HttpUi {
    fn entity(&self) -> TetraEntity {
        TetraEntity::HttpUi
    }
    fn rx_prim(&mut self, _queue: &mut MessageQueue, _message: SapMsg) {}
    fn tick_start(&mut self, queue: &mut MessageQueue, ts: Option<TdmaTime>) {
        let Some(rx) = &self.rx else { return; };
        let dltime = ts.unwrap_or_default();
        while let Ok(p) = rx.try_recv() {
            match p {
                PendingReq::Sds(req) => {
                    if let Some(tr) = req.trace.clone() {
                        sds_job::update_part_status(tr.job_id, tr.part_index, sds_job::SdsPartStatus::SubmittedToStack);
                    }
                    let msg = SapMsg {
                        sap: Sap::TnsdsSap,
                        src: TetraEntity::HttpUi,
                        dest: TetraEntity::Cmce,
                        dltime,
                        msg: SapMsgInner::TnsdsUnitdataReq(req),
                    };
                    queue.push_back(msg);
                }
                PendingReq::Dgna(req) => {
                    let msg = SapMsg {
                        sap: Sap::LmmSap,
                        src: TetraEntity::HttpUi,
                        dest: TetraEntity::Mm,
                        dltime,
                        msg: SapMsgInner::DgnaSetReq(req),
                    };
                    queue.push_back(msg);
                }
            }
        }
    }
}
fn server_thread(port: u16, tx: Sender<PendingReq>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let txc = tx.clone();
                thread::spawn(move || {
                    let _ = handle_connection(s, txc);
                });
            }
            Err(e) => eprintln!("incoming failed: {e}"),
        }
    }
    Ok(())
}
fn handle_connection(mut stream: TcpStream, tx: Sender<PendingReq>) -> anyhow::Result<()> {
    let mut buf = [0u8; 64 * 1024];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        return Ok(());
    }
    let req = std::str::from_utf8(&buf[..n]).unwrap_or("");
    let (method, path) = parse_request_line(req).unwrap_or(("GET", "/"));
    if method == "GET" && path == "/" {
        return write_response(&mut stream, 200, "text/html; charset=utf-8", INDEX_HTML.as_bytes());
    }
    if method == "GET" && path.starts_with("/api/online") {
        let ttl = query_u64(path, "ttl").unwrap_or(300);
        let list = online::snapshot_online(ttl);
        let body = serde_json::to_vec(&IdListResp { ids: list })?;
        return write_response(&mut stream, 200, "application/json", &body);
    }
    if method == "GET" && path.starts_with("/api/group_online") {
        let ttl = query_u64(path, "ttl").unwrap_or(300);
        let gssi = query_u64(path, "gssi").unwrap_or(0) as u32;
        let list = online::snapshot_group_online(gssi, ttl);
        let body = serde_json::to_vec(&IdListResp { ids: list })?;
        return write_response(&mut stream, 200, "application/json", &body);
    }
    if method == "GET" && path.starts_with("/api/group_online_all") {
        let ttl = query_u64(path, "ttl").unwrap_or(300);
        let groups = online::snapshot_group_online_all(ttl)
            .into_iter()
            .map(|(gssi, ids)| GroupOnlineEntry { gssi, ids })
            .collect();
        let body = serde_json::to_vec(&GroupOnlineAllResp { groups })?;
        return write_response(&mut stream, 200, "application/json", &body);
    }

    if method == "GET" && path == "/api/sds_jobs" {
        let jobs = sds_job::list_jobs();
        let body = serde_json::to_vec(&jobs)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }
    if method == "GET" && path.starts_with("/api/sds_jobs/") {
        let id_s = path.trim_start_matches("/api/sds_jobs/");
        let id = id_s.parse::<u64>().unwrap_or(0);
        let body = serde_json::to_vec(&sds_job::get_job(id))?;
        return write_response(&mut stream, 200, "application/json", &body);
    }

    if method == "GET" && path == "/api/dgna_jobs" {
        let jobs = dgna_job::list_jobs();
        let body = serde_json::to_vec(&jobs)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }
    if method == "GET" && path.starts_with("/api/dgna_jobs/") {
        let id_s = path.trim_start_matches("/api/dgna_jobs/");
        let id = id_s.parse::<u64>().unwrap_or(0);
        let body = serde_json::to_vec(&dgna_job::get_job(id))?;
        return write_response(&mut stream, 200, "application/json", &body);
    }

    if method == "POST" && path == "/api/dgna/attach" {
        let body_s = extract_body(req).unwrap_or("");
        let parsed: Result<DgnaApiReq, _> = serde_json::from_str(body_s);
        let resp = match parsed {
            Ok(r) => match build_dgna_set(DgnaOp::Attach, r) {
                Ok((job, set_req)) => {
                    let _ = tx.send(PendingReq::Dgna(set_req));
                    DgnaApiResp { ok: true, error: None, info: None, job: Some(job) }
                }
                Err(e) => DgnaApiResp { ok: false, error: Some(e), info: None, job: None },
            },
            Err(e) => DgnaApiResp { ok: false, error: Some(format!("invalid json: {e}")), info: None, job: None },
        };
        let body = serde_json::to_vec(&resp)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }
    if method == "POST" && path == "/api/dgna/detach" {
        let body_s = extract_body(req).unwrap_or("");
        let parsed: Result<DgnaApiReq, _> = serde_json::from_str(body_s);
        let resp = match parsed {
            Ok(r) => match build_dgna_set(DgnaOp::Detach, r) {
                Ok((job, set_req)) => {
                    let _ = tx.send(PendingReq::Dgna(set_req));
                    DgnaApiResp { ok: true, error: None, info: None, job: Some(job) }
                }
                Err(e) => DgnaApiResp { ok: false, error: Some(e), info: None, job: None },
            },
            Err(e) => DgnaApiResp { ok: false, error: Some(format!("invalid json: {e}")), info: None, job: None },
        };
        let body = serde_json::to_vec(&resp)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }

    // Convenience: update UI groupbook and then force attach (detach all first)
    if method == "POST" && path == "/api/dgna/transfer" {
        let body_s = extract_body(req).unwrap_or("");
        let parsed: Result<GroupMoveMemberReq, _> = serde_json::from_str(body_s);
        let resp = match parsed {
            Ok(r) => {
                groupbook::move_member(r.issi, r.to_gssi);
                let api_req = DgnaApiReq {
                    targets: vec![r.issi],
                    gssi: r.to_gssi,
                    detach_all_first: Some(true),
                    lifetime: Some(3),
                    class_of_usage: Some(0),
                    ack_req: Some(true),
                };
                match build_dgna_set(DgnaOp::Attach, api_req) {
                    Ok((job, set_req)) => {
                        let _ = tx.send(PendingReq::Dgna(set_req));
                        DgnaApiResp { ok: true, error: None, info: Some("moved in local groupbook + DGNA attach queued".into()), job: Some(job) }
                    }
                    Err(e) => DgnaApiResp { ok: false, error: Some(e), info: None, job: None },
                }
            }
            Err(e) => DgnaApiResp { ok: false, error: Some(format!("invalid json: {e}")), info: None, job: None },
        };
        let body = serde_json::to_vec(&resp)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }

    if method == "GET" && path == "/api/groupbook" {
        let book = groupbook::get();
        let body = serde_json::to_vec(&book)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }
    if method == "POST" && path == "/api/groupbook/upsert" {
        let body_s = extract_body(req).unwrap_or("");
        let parsed: Result<GroupUpsertReq, _> = serde_json::from_str(body_s);
        let resp = match parsed {
            Ok(r) => {
                groupbook::upsert_group(r.name, r.gssi);
                ApiResp { ok: true, error: None, info: None }
            }
            Err(e) => ApiResp { ok: false, error: Some(format!("invalid json: {e}")), info: None },
        };
        let body = serde_json::to_vec(&resp)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }
    if method == "POST" && path == "/api/groupbook/delete" {
        let body_s = extract_body(req).unwrap_or("");
        let parsed: Result<GroupDeleteReq, _> = serde_json::from_str(body_s);
        let resp = match parsed {
            Ok(r) => {
                groupbook::delete_group(r.gssi);
                ApiResp { ok: true, error: None, info: None }
            }
            Err(e) => ApiResp { ok: false, error: Some(format!("invalid json: {e}")), info: None },
        };
        let body = serde_json::to_vec(&resp)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }
    if method == "POST" && path == "/api/groupbook/set_members" {
        let body_s = extract_body(req).unwrap_or("");
        let parsed: Result<GroupSetMembersReq, _> = serde_json::from_str(body_s);
        let resp = match parsed {
            Ok(r) => {
                groupbook::set_members(r.gssi, r.members);
                ApiResp { ok: true, error: None, info: None }
            }
            Err(e) => ApiResp { ok: false, error: Some(format!("invalid json: {e}")), info: None },
        };
        let body = serde_json::to_vec(&resp)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }
    if method == "POST" && path == "/api/groupbook/move_member" {
        let body_s = extract_body(req).unwrap_or("");
        let parsed: Result<GroupMoveMemberReq, _> = serde_json::from_str(body_s);
        let resp = match parsed {
            Ok(r) => {
                groupbook::move_member(r.issi, r.to_gssi);
                ApiResp { ok: true, error: None, info: None }
            }
            Err(e) => ApiResp { ok: false, error: Some(format!("invalid json: {e}")), info: None },
        };
        let body = serde_json::to_vec(&resp)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }


    if method == "GET" && path == "/api/voice/state" {
        let st = voice_state::get();
        let body = serde_json::to_vec(&st)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }
    if method == "POST" && path == "/api/voice/state" {
        let body_s = extract_body(req).unwrap_or("");
        let parsed: Result<VoiceStateReq, _> = serde_json::from_str(body_s);
        let resp = match parsed {
            Ok(r) => {
                let tch_ts = if r.tch_ts < 2 || r.tch_ts > 4 { 2 } else { r.tch_ts };
                let tchan = if r.tchan < 4 { 4 } else { r.tchan };
                voice_state::set(voice_state::VoiceState { enabled: r.enabled, tch_ts, tchan });
                ApiResp { ok: true, error: None, info: Some(format!("voice placeholder updated: enabled={} ts={} tchan={}", r.enabled, tch_ts, tchan)) }
            }
            Err(e) => ApiResp { ok: false, error: Some(format!("invalid json: {e}")), info: None },
        };
        let body = serde_json::to_vec(&resp)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }
    if method == "POST" && path == "/api/send_text" {
        let body = extract_body(req).unwrap_or("");
        let parsed: Result<SendTextReq, _> = serde_json::from_str(body);
        let resp = match parsed {
            Ok(r) => match build_send_text(r) {
                Ok((job, reqs)) => {
                    let parts = reqs.len();
                    for p in reqs {
                        let _ = tx.send(PendingReq::Sds(p));
                    }
                    SendTextApiResp {
                        ok: true,
                        error: None,
                        info: if parts > 1 {
                            Some(format!(
                                "split into {} parts (<= {} bytes payload each)",
                                parts,
                                crate::common::sds_codec::max_text_payload_bytes_configured()
                            ))
                        } else {
                            None
                        },
                        job: Some(job),
                    }
                }
                Err(e) => SendTextApiResp { ok: false, error: Some(e), info: None, job: None },
            },
            Err(e) => SendTextApiResp { ok: false, error: Some(format!("invalid json: {e}")), info: None, job: None },
        };
        let body = serde_json::to_vec(&resp)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }
    if method == "POST" && path == "/api/send_hex" {
        let body = extract_body(req).unwrap_or("");
        let parsed: Result<SendHexReq, _> = serde_json::from_str(body);
        let resp = match parsed {
            Ok(r) => {
                match build_send_hex(r) {
                    Ok(p) => {
                        let _ = tx.send(PendingReq::Sds(p));
                        ApiResp { ok: true, error: None, info: None }
                    }
                    Err(e) => ApiResp { ok: false, error: Some(e), info: None },
                }
            }
            Err(e) => ApiResp { ok: false, error: Some(format!("invalid json: {e}")), info: None },
        };
        let body = serde_json::to_vec(&resp)?;
        return write_response(&mut stream, 200, "application/json", &body);
    }
    write_response(&mut stream, 404, "text/plain; charset=utf-8", b"Not Found")
}
fn write_response(stream: &mut TcpStream, code: u16, ctype: &str, body: &[u8]) -> anyhow::Result<()> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "OK",
    };
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}
fn parse_request_line(req: &str) -> Option<(&str, &str)> {
    let line = req.lines().next()?;
    let mut it = line.split_whitespace();
    let method = it.next()?;
    let path = it.next()?;
    Some((method, path))
}
fn extract_body(req: &str) -> Option<&str> {
    let idx = req.find("\r\n\r\n")?;
    Some(&req[idx + 4..])
}
fn query_u64(path: &str, key: &str) -> Option<u64> {
    let q = path.splitn(2, '?').nth(1)?;
    for kv in q.split('&') {
        let mut it = kv.splitn(2, '=');
        let k = it.next()?;
        let v = it.next().unwrap_or("");
        if k == key {
            return v.parse::<u64>().ok();
        }
    }
    None
}
fn dst_type_to_ssi_type(dst_type: &str) -> Result<SsiType, String> {
    match dst_type.trim().to_ascii_uppercase().as_str() {
        "I" => Ok(SsiType::Issi),
        "G" => Ok(SsiType::Gssi),
        _ => Err("dst_type must be 'I' or 'G'".into()),
    }
}
fn parse_tcs(v: Option<String>) -> TextCodingScheme {
    match v.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
        Some(s) if s == "latin1" => TextCodingScheme::Latin1,
        Some(s) if s == "gsm7" => TextCodingScheme::Gsm7,
        // default to UCS2 for best interoperability with terminals
        _ => TextCodingScheme::Ucs2,
    }
}

fn parse_format(v: Option<String>) -> &'static str {
    match v.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
        Some(s) if s == "simple" => "simple",
        _ => "sds_tl",
    }
}

fn hex_to_bitbuffer(hex: &str, bitlen: usize) -> Result<BitBuffer, String> {
    let mut s = hex.trim().to_string();
    if s.starts_with("0x") || s.starts_with("0X") {
        s = s[2..].to_string();
    }
    if s.len() % 2 != 0 {
        return Err("hex length must be even".into());
    }
    let bytes = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| "invalid hex"))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    // Allocate enough for provided bitlen (autoexpand) and write whole bytes, then clamp to bitlen.
    let mut bb = BitBuffer::new_autoexpand(bitlen.max(16));
    for b in bytes {
        bb.write_bits(b as u64, 8);
    }
    // Clamp window length to bitlen (may be less than the written full bytes).
    if bitlen < bb.get_len() {
        bb.set_raw_pos(bitlen);
        bb.set_raw_end(bitlen);
    }
    bb.set_raw_pos(0);
    Ok(bb)
}
fn build_send_text(req: SendTextReq) -> Result<(sds_job::SdsJob, Vec<TnsdsUnitdataReq>), String> {
    let ssi_type = dst_type_to_ssi_type(&req.dst_type)?;
    let dst = TetraAddress { encrypted: false, ssi_type, ssi: req.dst };
    let base_mr = req.mr.unwrap_or(1);
    let drr = req.drr.unwrap_or(false);

    let tcs = parse_tcs(req.tcs);
    let fmt = parse_format(req.format.clone());

    let mut out: Vec<TnsdsUnitdataReq> = Vec::new();
    let mut parts_meta: Vec<sds_job::SdsPartInfo> = Vec::new();

    match fmt {
        "simple" => {
            let payloads = encode_simple_text_multi(&req.text, tcs);
            let total = payloads.len().max(1);

            for (i, payload) in payloads.into_iter().enumerate() {
                if payload.get_len() > MAX_SDS_TYPE4_BITS {
                    return Err(format!(
                        "type-4 payload too large ({} bits, max {} bits)",
                        payload.get_len(),
                        MAX_SDS_TYPE4_BITS
                    ));
                }

                let mr = base_mr.wrapping_add(i as u8);

                tracing::info!(
                    "HTTP send_text: part={}/{} dst_type={} dst={} calling={} link_id={} endpoint_id={} format={} tcs={:?} mr={} drr={} payload_bits={} payload_hex={}",
                    i + 1,
                    total,
                    req.dst_type,
                    req.dst,
                    req.calling,
                    req.link_id.unwrap_or(0),
                    req.endpoint_id.unwrap_or(0),
                    fmt,
                    tcs,
                    mr,
                    drr,
                    payload.get_len(),
                    payload.dump_hex()
                );

                parts_meta.push(sds_job::SdsPartInfo {
                    part_index: i,
                    part_total: total,
                    status: sds_job::SdsPartStatus::Queued,
                    mr,
                    payload_bits: payload.get_len(),
                    error: None,
                });

                out.push(TnsdsUnitdataReq {
                    dst,
                    calling_ssi: req.calling,
                    link_id: req.link_id.unwrap_or(0),
                    endpoint_id: req.endpoint_id.unwrap_or(0),
                    type4_payload: payload,
                    delivery_report_request: drr,
                    message_reference: mr,
                    trace: None,
                });
            }
        }
        _ => {
            let payloads = encode_sds_tl_text_multi(&req.text, tcs, base_mr, drr);
            let total = payloads.len().max(1);

            for (i, (mr, payload)) in payloads.into_iter().enumerate() {
                if payload.get_len() > MAX_SDS_TYPE4_BITS {
                    return Err(format!(
                        "type-4 payload too large ({} bits, max {} bits)",
                        payload.get_len(),
                        MAX_SDS_TYPE4_BITS
                    ));
                }

                tracing::info!(
                    "HTTP send_text: part={}/{} dst_type={} dst={} calling={} link_id={} endpoint_id={} format={} tcs={:?} mr={} drr={} payload_bits={} payload_hex={}",
                    i + 1,
                    total,
                    req.dst_type,
                    req.dst,
                    req.calling,
                    req.link_id.unwrap_or(0),
                    req.endpoint_id.unwrap_or(0),
                    fmt,
                    tcs,
                    mr,
                    drr,
                    payload.get_len(),
                    payload.dump_hex()
                );

                parts_meta.push(sds_job::SdsPartInfo {
                    part_index: i,
                    part_total: total,
                    status: sds_job::SdsPartStatus::Queued,
                    mr,
                    payload_bits: payload.get_len(),
                    error: None,
                });

                out.push(TnsdsUnitdataReq {
                    dst,
                    calling_ssi: req.calling,
                    link_id: req.link_id.unwrap_or(0),
                    endpoint_id: req.endpoint_id.unwrap_or(0),
                    type4_payload: payload,
                    delivery_report_request: drr,
                    message_reference: mr,
                    trace: None,
                });
            }
        }
    }

    // Create a job record for UI status tracking.
    let job_id = sds_job::new_job(sds_job::SdsJob {
        job_id: 0,
        created_ms: 0,
        dst_type: req.dst_type.clone(),
        dst: req.dst,
        calling: req.calling,
        format: fmt.to_string(),
        tcs: format!("{:?}", tcs).to_ascii_lowercase(),
        text_preview: req.text.chars().take(96).collect(),
        parts: parts_meta,
    });

    // Attach trace info so downstream entities can update statuses.
    let part_total = out.len();
    for (i, r) in out.iter_mut().enumerate() {
        r.trace = Some(sds_job::SdsTraceMeta { job_id, part_index: i, part_total });
    }

    let job = sds_job::get_job(job_id).unwrap();
    Ok((job, out))
}

fn build_dgna_set(op: DgnaOp, req: DgnaApiReq) -> Result<(dgna_job::DgnaJob, DgnaSetReq), String> {
    if req.gssi == 0 {
        return Err("gssi must be non-zero".into());
    }
    if req.targets.is_empty() {
        return Err("targets must not be empty".into());
    }
    // Very small sanity cap to prevent accidental flooding.
    if req.targets.len() > 64 {
        return Err("too many targets (max 64)".into());
    }

    let detach_all_first = req.detach_all_first.unwrap_or(false);
    let lifetime = req.lifetime.unwrap_or(3);
    let class_of_usage = req.class_of_usage.unwrap_or(0);
    let ack_req = req.ack_req.unwrap_or(true);

    if ack_req {
        let mut busy = Vec::new();
        for &issi in req.targets.iter() {
            if dgna_job::peek_pending_job_id(issi).is_some() {
                busy.push(issi);
            }
        }
        if !busy.is_empty() {
            let list = busy.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",");
            return Err(format!("DGNA busy for ISSI(s): {} (wait for ACK or set ack_req=false)", list));
        }
    }


    if ack_req {
        let mut busy = Vec::new();
        for &issi in req.targets.iter() {
            if dgna_job::peek_pending_job_id(issi).is_some() {
                busy.push(issi);
            }
        }
        if !busy.is_empty() {
            let list = busy.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",");
            return Err(format!("DGNA busy for ISSI(s): {} (wait for ACK or set ack_req=false)", list));
        }
    }

    let mut targets_meta = Vec::new();
    for &issi in req.targets.iter() {
        targets_meta.push(dgna_job::DgnaTargetInfo {
            issi,
            status: dgna_job::DgnaTargetStatus::Queued,
            error: None,
        });
    }

    let job_id = dgna_job::new_job(dgna_job::DgnaJob {
        job_id: 0,
        created_ms: 0,
        op,
        gssi: req.gssi,
        detach_all_first,
        lifetime,
        class_of_usage,
        ack_req,
        targets: targets_meta,
    });
    let job = dgna_job::get_job(job_id).unwrap();

    let set_req = DgnaSetReq {
        job_id,
        op,
        targets: req.targets,
        gssi: req.gssi,
        detach_all_first,
        lifetime,
        class_of_usage,
        ack_req,
    };

    Ok((job, set_req))
}

fn build_send_hex(req: SendHexReq) -> Result<TnsdsUnitdataReq, String> {
    let ssi_type = dst_type_to_ssi_type(&req.dst_type)?;
    let dst = TetraAddress { encrypted: false, ssi_type, ssi: req.dst };
    let mr = req.mr.unwrap_or(1);
    let drr = req.drr.unwrap_or(false);

    let payload = hex_to_bitbuffer(&req.hex, req.bitlen)?;
    if payload.get_len() > MAX_SDS_TYPE4_BITS {
        return Err(format!("type-4 payload too large ({} bits, max {} bits)", payload.get_len(), MAX_SDS_TYPE4_BITS));
    }

    tracing::info!("HTTP send_hex: dst_type={} dst={} calling={} link_id={} endpoint_id={} bitlen={} mr={} drr={} payload_bits={} payload_hex={}",
        req.dst_type, req.dst, req.calling, req.link_id.unwrap_or(0), req.endpoint_id.unwrap_or(0), req.bitlen,
        mr, drr, payload.get_len(), payload.dump_hex());

    Ok(TnsdsUnitdataReq {
        dst,
        calling_ssi: req.calling,
        link_id: req.link_id.unwrap_or(0),
        endpoint_id: req.endpoint_id.unwrap_or(0),
        type4_payload: payload,
        delivery_report_request: drr,
        message_reference: mr,
        trace: None,
    })
}
const INDEX_HTML: &str = r#"<!doctype html>
<html lang="zh-Hant">
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width,initial-scale=1"/>
  <title>TETRA SDS Web Sender</title>
  <style>
    body { font-family: system-ui, sans-serif; margin: 24px; max-width: 980px; }
    input, select, textarea, button { font-size: 14px; padding: 8px; }
    textarea { width: 100%; min-height: 120px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
    .row { display: flex; gap: 12px; flex-wrap: wrap; margin-bottom: 12px; }
    .row > div { flex: 1; min-width: 210px; }
    .card { border: 1px solid #ddd; border-radius: 12px; padding: 16px; margin-bottom: 16px; }
    .ok { color: #0a7; }
    .err { color: #c22; }
    .hint { color: #555; font-size: 13px; line-height: 1.4; }
    code { background:#f6f6f6; padding: 2px 4px; border-radius: 6px; }
  </style>
</head>
<body>
  <div class="row" style="align-items:center;justify-content:space-between">
    <h1 id="t_title">TETRA SDS Sender</h1>
    <div>
      <label id="t_lang" for="langSel">Language</label>
      <select id="langSel" onchange="applyLang()">
        <option value="zh">繁體中文</option>
        <option value="en">English</option>
      </select>
    </div>
  </div>

  <p class="hint" id="t_intro">
    本頁面用於從基地台發送 SDS。<b>單發</b>使用 ISSI；<b>組發</b>使用 GSSI（群組廣播一次：能收到就收到，不維護收件人清單）。
    <br/>建議文字先選 <b>UCS2</b>（相容性最高）；若你知道終端支援 GSM7 才改 GSM7。
  </p>

  <div class="card">
    <h2 id="t_send_text">Send Text</h2>

    <div class="row">
      <div>
        <label id="t_mode" for="dstTypeText">Mode</label><br/>
        <select id="dstTypeText">
          <option value="I" selected>單發 (ISSI)</option>
          <option value="G">組發 (GSSI)</option>
        </select>
      </div>
      <div>
        <label id="t_dst" for="dstText">Destination ID (ISSI/GSSI)</label><br/>
        <input id="dstText" value="10002"/>
      </div>
      <div>
        <label id="t_calling" for="callingText">Sender SSI (Calling)</label><br/>
        <input id="callingText" value="10001"/>
      </div>
      <div>
        <label id="t_format" for="fmtText">Text Format</label><br/>
        <select id="fmtText">
          <option value="sds_tl" selected>PID 0x82 (SDS‑TL)</option>
          <option value="simple">PID 0x02 (Simple Text)</option>
        </select>
      </div>
    </div>

    <div class="row">
      <div>
        <label id="t_tcs" for="tcsText">Text Encoding (TCS)</label><br/>
        <select id="tcsText">
          <option value="ucs2" selected>UCS2 (UTF‑16BE)</option>
          <option value="latin1">Latin‑1 (8‑bit)</option>
          <option value="gsm7">GSM7 (7‑bit packed)</option>
        </select>
      </div>
      <div>
        <label id="t_mr" for="mrText">Message Ref (MR)</label><br/>
        <input id="mrText" value="1"/>
      </div>
      <div>
        <label id="t_drr" for="drrText">Delivery Report (DRR)</label><br/>
        <select id="drrText">
          <option value="false" selected>false</option>
          <option value="true">true</option>
        </select>
      </div>
      <div>
        <label id="t_link" for="linkIdText">link_id</label><br/>
        <input id="linkIdText" value="0"/>
      </div>
      <div>
        <label id="t_ep" for="endpointIdText">endpoint_id</label><br/>
        <input id="endpointIdText" value="0"/>
      </div>
    </div>

    <div>
      <label id="t_body" for="textBody">Text</label><br/>
      <textarea id="textBody">HELLO</textarea>
    </div>
    <button onclick="sendText()" id="t_btn_send_text">Send Text</button>
    <div id="respText"></div>
  </div>

  <div class="card">
    <h2 id="t_jobs">SDS Send Status</h2>
    <p class="hint" id="t_jobs_hint">
      每次送出會建立一個 Job，並把長文自動切段；你可以在這裡看到每段的狀態（queued/submitted/cmce...）。
    </p>
    <div class="row">
      <div style="align-self:flex-end;"><button onclick="refreshJobs()" id="t_jobs_refresh">Refresh</button></div>
    </div>
    <textarea id="jobsList" readonly></textarea>
  </div>

  <div class="card">
    <h2 id="t_groupbook">Custom Group List</h2>
    <p class="hint" id="t_groupbook_hint">
      這是 WebUI 內的「自訂群組通訊錄/分組清單」，用於顯示與快速選擇（不是 DGNA；不會改變終端實際附著群組）。
    </p>
    <div class="row">
      <div>
        <label id="t_gb_name" for="gbName">Group Name</label><br/>
        <input id="gbName" placeholder="e.g. Rail Dispatch"/>
      </div>
      <div>
        <label id="t_gb_gssi" for="gbGssi">GSSI</label><br/>
        <input id="gbGssi" placeholder="60031"/>
      </div>
      <div style="align-self:flex-end;">
        <button onclick="upsertGroup()" id="t_gb_save">Save Group</button>
      </div>
    </div>
    <div class="row">
      <div>
        <label id="t_gb_members" for="gbMembers">Members (ISSI list, separated by space/comma/newline)</label><br/>
        <textarea id="gbMembers" placeholder="10001\n10002\n10003"></textarea>
      </div>
      <div>
        <label id="t_gb_apply_to" for="gbMembersGssi">Apply To GSSI</label><br/>
        <input id="gbMembersGssi" placeholder="60031"/>
        <div style="height:8px"></div>
        <button onclick="setMembers()" id="t_gb_set_members">Set Members</button>
        <div style="height:8px"></div>
        <button onclick="deleteGroup()" id="t_gb_delete">Delete Group</button>
      </div>
    </div>
    <div class="row">
      <div>
        <label id="t_gb_move_issi" for="gbMoveIssi">Move ISSI</label><br/>
        <input id="gbMoveIssi" placeholder="10002"/>
      </div>
      <div>
        <label id="t_gb_move_to" for="gbMoveTo">To GSSI</label><br/>
        <input id="gbMoveTo" placeholder="60031"/>
      </div>
      <div style="align-self:flex-end;">
        <button onclick="moveMember()" id="t_gb_move">Move</button>
        <button onclick="refreshGroupBook()" id="t_gb_refresh">Refresh</button>
      </div>
    </div>
    <textarea id="groupBookList" readonly></textarea>
  </div>

  <div class="card">
    <h2 id="t_dgna">DGNA Control (Force group attachment)</h2>
    <p class="hint" id="t_dgna_hint">
      DGNA 會對終端下發 <b>D-ATTACH/DETACH GROUP IDENTITY</b>（網路端強制附著/脫離群組）。
      <br/>此頁面以 Motorola MTP3150/3250/3550/6750、MXP600/660、MXM600 為主要目標；其他終端相容性後續再擴。
    </p>

    <div class="row">
      <div>
        <label id="t_dgna_targets" for="dgnaTargets">Target ISSI list</label><br/>
        <textarea id="dgnaTargets" placeholder="10001\n10002"></textarea>
      </div>
      <div>
        <label id="t_dgna_gssi" for="dgnaGssi">Target GSSI</label><br/>
        <input id="dgnaGssi" placeholder="60031"/>
        <div class="hint" style="margin-top:6px;" id="t_dgna_pick">Pick from Groupbook</div>
        <select id="dgnaGssiSel" onchange="document.getElementById('dgnaGssi').value=this.value"></select>
        <div style="height:8px"></div>
        <label id="t_dgna_detach_all" for="dgnaDetachAll">Replace existing groups (detach all first)</label><br/>
        <select id="dgnaDetachAll">
          <option value="false" selected>false</option>
          <option value="true">true</option>
        </select>
        <div style="height:8px"></div>
        <label id="t_dgna_ack" for="dgnaAck">Ack request</label><br/>
        <select id="dgnaAck">
          <option value="true" selected>true</option>
          <option value="false">false</option>
        </select>
      </div>
      <div>
        <label id="t_dgna_lifetime" for="dgnaLifetime">Attachment lifetime</label><br/>
        <input id="dgnaLifetime" value="3"/>
        <div style="height:8px"></div>
        <label id="t_dgna_class" for="dgnaClass">Class of usage</label><br/>
        <input id="dgnaClass" value="0"/>
        <div style="height:8px"></div>
        <label id="t_dgna_move" for="dgnaMoveIssi">Transfer single ISSI (update Groupbook + Replace)</label><br/>
        <input id="dgnaMoveIssi" placeholder="10002"/>
      </div>
    </div>

    <div class="row">
      <div style="align-self:flex-end;">
        <button onclick="dgnaAttach(false)" id="t_dgna_btn_attach">Attach (Add)</button>
        <button onclick="dgnaAttach(true)" id="t_dgna_btn_replace">Attach (Replace)</button>
        <button onclick="dgnaDetach()" id="t_dgna_btn_detach">Detach</button>
        <button onclick="dgnaTransfer()" id="t_dgna_btn_transfer">Transfer</button>
      </div>
      <div style="align-self:flex-end;">
        <button onclick="refreshDgnaJobs()" id="t_dgna_refresh">Refresh</button>
      </div>
    </div>
    <div id="respDgna"></div>
    <textarea id="dgnaJobsList" readonly></textarea>
  </div>



  <div class="card">
    <h2 id="t_voice">Voice MVP (Traffic Slot Placeholder)</h2>
    <p class="hint" id="t_voice_hint">
      This is a temporary placeholder to iterate toward single-site voice later.

      When enabled, the BS will advertise one timeslot as <b>Traffic</b> in AACH and transmit two STCH blocks (dummy/idle content).

      It does not carry real audio yet.
    </p>
    <div class="row">
      <div>
        <label id="t_voice_enable" for="voiceEnable">Enable</label><br/>
        <select id="voiceEnable">
          <option value="false" selected>false</option>
          <option value="true">true</option>
        </select>
      </div>
      <div>
        <label id="t_voice_ts" for="voiceTs">Traffic timeslot</label><br/>
        <input id="voiceTs" value="2"/>
      </div>
      <div>
        <label id="t_voice_tchan" for="voiceTchan">Traffic channel number (usage marker)</label><br/>
        <input id="voiceTchan" value="4"/>
      </div>
      <div style="align-self:flex-end;">
        <button onclick="applyVoice()" id="t_voice_apply">Apply</button>
        <button onclick="refreshVoice()" id="t_voice_refresh">Refresh</button>
      </div>
    </div>
    <div id="respVoice"></div>
    <textarea id="voiceState" readonly></textarea>
  </div>

  <div class="card">
    <h2 id="t_send_hex">Send Raw Hex</h2>
    <p class="hint" id="t_hex_hint">
      直接送 Type‑4 payload（含 PID）。例如：<code>82 02 MR ...</code>。這個模式最接近終端互通測試。
    </p>
    <div class="row">
      <div><label id="t_mode2" for="dstTypeHex">Mode</label><br/>
        <select id="dstTypeHex">
          <option value="I">單發 (ISSI)</option>
          <option value="G" selected>組發 (GSSI)</option>
        </select></div>
      <div><label id="t_dst2" for="dstHex">Destination ID (ISSI/GSSI)</label><br/><input id="dstHex" value="60031"/></label></div>
      <div><label id="t_calling2" for="callingHex">Sender SSI (Calling)</label><br/><input id="callingHex" value="10001"/></label></div>
      <div><label id="t_bitlen" for="bitlenHex">Bit length</label><br/><input id="bitlenHex" value="64"/></label></div>
    </div>
    <div class="row">
      <div><label id="t_link2" for="linkIdHex">link_id</label><br/><input id="linkIdHex" value="0"/></label></div>
      <div><label id="t_ep2" for="endpointIdHex">endpoint_id</label><br/><input id="endpointIdHex" value="0"/></label></div>
      <div><label id="t_mr2" for="mrHex">MR</label><br/><input id="mrHex" value="1"/></label></div>
      <div><label id="t_drr2" for="drrHex">DRR</label><br/>
        <select id="drrHex">
          <option value="false" selected>false</option>
          <option value="true">true</option>
        </select></label></div>
    </div>
    <div><label id="t_hex" for="hexBody">Hex Payload</label><br/><textarea id="hexBody">82020100A0B1C2D3</textarea></label></div>
    <button onclick="sendHex()" id="t_btn_send_hex">Send Hex</button>
    <div id="respHex"></div>
  </div>

  <div class="card">
    <h2 id="t_online">Online IDs</h2>
    <p class="hint" id="t_online_hint">
      Online = 最近 N 秒內在空中看到活動（SDS/MM/Location Update…）。<br/>
      Group Online 是「近期對各 GSSI 有互動 / 附著」的近似清單（不宣稱=完整成員表）。
    </p>
    <div class="row">
      <div><label id="t_ttl" for="ttl">TTL seconds</label><br/><input id="ttl" value="300"/></label></div>
      <div style="align-self:flex-end;"><button onclick="refreshOnline()" id="t_refresh">Refresh</button></div>
    </div>
    <div class="row">
      <div>
        <h3 id="t_all_online">All Online SSI</h3>
        <textarea id="onlineList" readonly></textarea>
      </div>
      <div>
        <h3 id="t_group_online">Group Online by GSSI (approx)</h3>
        <textarea id="groupOnlineList" readonly></textarea>
      </div>
    </div>
  </div>

<script>
const I18N = {
  zh: {
    t_title: "TETRA SDS Sender",
    t_lang: "語言",
    t_intro: "本頁面用於從基地台發送 SDS。單發使用 ISSI；組發使用 GSSI（群組廣播一次：能收到就收到，不維護收件人清單）。\\n建議文字先選 UCS2（相容性最高）；若你知道終端支援 GSM7 才改 GSM7。",
    t_send_text: "發送文字",
    t_mode: "模式",
    t_dst: "目的 ID（ISSI / GSSI）",
    t_calling: "發送者 SSI（Calling）",
    t_format: "文字格式",
    t_tcs: "文字編碼（TCS）",
    t_mr: "訊息參考（MR）",
    t_drr: "送達回報（DRR）",
    t_link: "link_id（可先填 0）",
    t_ep: "endpoint_id（可先填 0）",
    t_body: "內容",
    t_btn_send_text: "送出文字",
    t_jobs: "SDS 送出狀態",
    t_jobs_hint: "每次送出會建立一個 Job，長文會自動切段並加上 (1/3) 標頭。這裡顯示每段狀態（queued/submitted/cmce...）。",
    t_jobs_refresh: "更新",
    t_groupbook: "自訂群組清單",
    t_groupbook_hint: "WebUI 內的通訊錄/分組清單（不是 DGNA，不會改變終端實際附著群組）。",
    t_gb_name: "群組名稱",
    t_gb_gssi: "GSSI",
    t_gb_save: "儲存群組",
    t_gb_members: "成員清單（ISSI，用空白/逗號/換行分隔）",
    t_gb_apply_to: "套用到 GSSI",
    t_gb_set_members: "設定成員",
    t_gb_delete: "刪除群組",
    t_gb_move_issi: "轉移 ISSI",
    t_gb_move_to: "目的 GSSI",
    t_gb_move: "轉移",
    t_gb_refresh: "更新",
    t_send_hex: "發送 RAW HEX",
    t_hex_hint: "直接送 Type‑4 payload（含 PID）。例如：82 02 MR ...。此模式最接近終端互通測試。",
    t_mode2: "模式",
    t_dst2: "目的 ID（ISSI / GSSI）",
    t_calling2: "發送者 SSI（Calling）",
    t_bitlen: "位元長度（bitlen）",
    t_link2: "link_id",
    t_ep2: "endpoint_id",
    t_mr2: "MR",
    t_drr2: "DRR",
    t_hex: "HEX 內容",
    t_btn_send_hex: "送出 HEX",
    t_online: "線上 ID",
    t_online_hint: "Online = 最近 N 秒內在空中看到活動（SDS/MM/Location Update…）。\\nGroup Online 是「近期對各 GSSI 有互動 / 附著」的近似清單（不宣稱=完整成員表）。",
    t_ttl: "TTL 秒數",
    t_gssi: "目前 GSSI",
    t_refresh: "更新",
    t_all_online: "全部在線 SSI",
    t_group_online: "群組在線（依 GSSI，近似）",

    t_dgna: "DGNA 控制（強制附著群組）",
    t_dgna_hint: "DGNA 會對終端下發 D-ATTACH/DETACH GROUP IDENTITY（網路端強制附著/脫離群組）。",
    t_dgna_targets: "目標 ISSI 清單",
    t_dgna_gssi: "目標 GSSI",
    t_dgna_pick: "從 Groupbook 選擇",
    t_dgna_detach_all: "取代既有群組（先 detach all）",
    t_dgna_ack: "要求回覆（ACK request）",
    t_dgna_lifetime: "附著期限（lifetime）",
    t_dgna_class: "使用類別（class of usage）",
    t_dgna_move: "單一 ISSI 轉移（同步 Groupbook + Replace）",
    t_dgna_btn_attach: "附著（新增）",
    t_dgna_btn_replace: "附著（取代）",
    t_dgna_btn_detach: "脫離",
    t_dgna_btn_transfer: "轉移",
    t_dgna_refresh: "更新",

    t_voice: "語音 MVP（流量時槽占位）",
    t_voice_hint: "此功能僅為單站語音的占位/實驗：啟用後基地台會在 AACH 宣告某個 timeslot 為 Traffic，並在該時槽送出兩個 STCH 區塊（idle/dummy）。目前不承載真實語音。",
    t_voice_enable: "啟用",
    t_voice_ts: "Traffic timeslot",
    t_voice_tchan: "Traffic channel number（usage marker）",
    t_voice_apply: "套用",
    t_voice_refresh: "更新"
  },
  en: {
    t_title: "TETRA SDS Sender",
    t_lang: "Language",
    t_intro: "This page sends SDS from the base station. Unicast uses ISSI; Group uses GSSI (one-shot broadcast; no recipient list).\\nFor best interoperability, start with UCS2; switch to GSM7 only if your terminals support it.",
    t_send_text: "Send Text",
    t_mode: "Mode",
    t_dst: "Destination ID (ISSI / GSSI)",
    t_calling: "Sender SSI (Calling)",
    t_format: "Text Format",
    t_tcs: "Text Encoding (TCS)",
    t_mr: "Message Reference (MR)",
    t_drr: "Delivery Report (DRR)",
    t_link: "link_id (usually 0)",
    t_ep: "endpoint_id (usually 0)",
    t_body: "Text",
    t_btn_send_text: "Send Text",
    t_jobs: "SDS Send Status",
    t_jobs_hint: "Each send creates a job. Long texts are auto-split and prefixed like (1/3). This panel shows per-part status (queued/submitted/cmce...).",
    t_jobs_refresh: "Refresh",
    t_groupbook: "Custom Group List",
    t_groupbook_hint: "Local contact/group book for the WebUI (not DGNA; it does not change terminal group attachment).",
    t_gb_name: "Group Name",
    t_gb_gssi: "GSSI",
    t_gb_save: "Save Group",
    t_gb_members: "Members (ISSI list, separated by space/comma/newline)",
    t_gb_apply_to: "Apply To GSSI",
    t_gb_set_members: "Set Members",
    t_gb_delete: "Delete Group",
    t_gb_move_issi: "Move ISSI",
    t_gb_move_to: "To GSSI",
    t_gb_move: "Move",
    t_gb_refresh: "Refresh",
    t_send_hex: "Send Raw Hex",
    t_hex_hint: "Send Type‑4 payload directly (includes PID). Closest to terminal interoperability testing.",
    t_mode2: "Mode",
    t_dst2: "Destination ID (ISSI / GSSI)",
    t_calling2: "Sender SSI (Calling)",
    t_bitlen: "Bit length",
    t_link2: "link_id",
    t_ep2: "endpoint_id",
    t_mr2: "MR",
    t_drr2: "DRR",
    t_hex: "Hex Payload",
    t_btn_send_hex: "Send Hex",
    t_online: "Online IDs",
    t_online_hint: "Online = seen on air within TTL seconds (SDS/MM/Location Update...).\\nGroup Online is an approximation based on recent interactions/attachments for each GSSI.",
    t_ttl: "TTL seconds",
    t_gssi: "Current GSSI",
    t_refresh: "Refresh",
    t_all_online: "All Online SSI",
    t_group_online: "Group Online by GSSI (approx)",

    t_dgna: "DGNA Control (Force group attachment)",
    t_dgna_hint: "DGNA sends D-ATTACH/DETACH GROUP IDENTITY to force terminals to attach/detach a group.",
    t_dgna_targets: "Target ISSI list",
    t_dgna_gssi: "Target GSSI",
    t_dgna_pick: "Pick from Groupbook",
    t_dgna_detach_all: "Replace existing groups (detach all first)",
    t_dgna_ack: "Ack request",
    t_dgna_lifetime: "Attachment lifetime",
    t_dgna_class: "Class of usage",
    t_dgna_move: "Transfer single ISSI (update Groupbook + Replace)",
    t_dgna_btn_attach: "Attach (Add)",
    t_dgna_btn_replace: "Attach (Replace)",
    t_dgna_btn_detach: "Detach",
    t_dgna_btn_transfer: "Transfer",
    t_dgna_refresh: "Refresh",

    t_voice: "Voice MVP (Traffic Slot Placeholder)",
    t_voice_hint: "Temporary placeholder toward single-site voice. When enabled, the BS advertises one timeslot as Traffic (AACH) and transmits two STCH blocks (dummy/idle). No real audio yet.",
    t_voice_enable: "Enable",
    t_voice_ts: "Traffic timeslot",
    t_voice_tchan: "Traffic channel number (usage marker)",
    t_voice_apply: "Apply",
    t_voice_refresh: "Refresh"
  }
};

function applyLang() {
  const lang = document.getElementById("langSel").value;
  const dict = I18N[lang] || I18N.zh;
  for (const k of Object.keys(dict)) {
    const el = document.getElementById(k);
    if (!el) continue;
    el.textContent = dict[k];
  }
}
applyLang();

async function postJson(url, body) {
  const r = await fetch(url, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
  const j = await r.json().catch(() => ({ ok: false, error: "invalid json response" }));
  return { status: r.status, json: j };
}
function setResp(el, r) {
  const ok = r.json && r.json.ok;
  if (!ok) {
    el.innerHTML = `<p class="err">ERROR: ${r.json && r.json.error ? r.json.error : r.status}</p>`;
    return;
  }
  const job = r.json && r.json.job;
  if (job && job.parts) {
    const header = `<p class="ok">OK</p>` + (r.json.info ? `<p class="hint">${r.json.info}</p>` : "");
    const lines = [];
    lines.push(`JOB #${job.job_id}  dst=${job.dst_type}:${job.dst}  calling=${job.calling}  fmt=${job.format}  tcs=${job.tcs}`);
    for (const p of job.parts) {
      const idx = (p.part_index + 1);
      const tot = p.part_total;
      lines.push(`  (${idx}/${tot}) mr=${p.mr}  status=${p.status}  bits=${p.payload_bits}`);
    }
    el.innerHTML = header + `<pre style="white-space:pre-wrap;background:#f6f6f6;padding:10px;border-radius:10px;">${lines.join("\n")}</pre>`;
  } else {
    el.innerHTML = ok ? `<p class="ok">OK</p>` : `<p class="err">ERROR: ${r.json && r.json.error ? r.json.error : r.status}</p>`;
  }
}

async function refreshJobs() {
  const r = await fetch('/api/sds_jobs');
  const jobs = await r.json().catch(() => []);
  const lines = [];
  for (const job of (jobs || [])) {
    lines.push(`JOB #${job.job_id}  dst=${job.dst_type}:${job.dst}  calling=${job.calling}  fmt=${job.format}  tcs=${job.tcs}`);
    if (job.text_preview) lines.push(`  preview: ${job.text_preview}`);
    for (const p of (job.parts || [])) {
      const idx = p.part_index + 1;
      const tot = p.part_total;
      const err = p.error ? `  err=${p.error}` : '';
      lines.push(`    (${idx}/${tot}) mr=${p.mr}  status=${p.status}  bits=${p.payload_bits}${err}`);
    }
    lines.push('');
  }
  document.getElementById('jobsList').value = lines.join('\n');
}

async function refreshGroupBook() {
  const r = await fetch('/api/groupbook');
  const book = await r.json().catch(() => ({groups:[]}));
  const groups = (book && book.groups) ? book.groups : [];
  const lines = [];
  for (const g of groups) {
    const ids = (g.members || []).join(' / ');
    lines.push(`${g.name || '(unnamed)'}  (${g.gssi})：${ids}`);
  }
  document.getElementById('groupBookList').value = lines.join('\n');

  // Populate DGNA GSSI picker
  const sel = document.getElementById('dgnaGssiSel');
  if (sel) {
    const prev = sel.value;
    sel.innerHTML = '';
    const opt0 = document.createElement('option');
    opt0.value = '';
    opt0.textContent = '(manual input)';
    sel.appendChild(opt0);
    for (const g of groups) {
      const opt = document.createElement('option');
      opt.value = String(g.gssi);
      opt.textContent = `${g.name || '(unnamed)'} (${g.gssi})`;
      sel.appendChild(opt);
    }
    // Try restore previous selection
    sel.value = prev;
  }
}

async function upsertGroup() {
  const body = { name: document.getElementById('gbName').value || '', gssi: Number(document.getElementById('gbGssi').value) };
  const r = await postJson('/api/groupbook/upsert', body);
  setResp(document.getElementById('respText'), r); // reuse
  await refreshGroupBook();
}

async function setMembers() {
  const gssi = Number(document.getElementById('gbMembersGssi').value);
  const members = parseIdList(document.getElementById('gbMembers').value);
  const r = await postJson('/api/groupbook/set_members', { gssi, members });
  setResp(document.getElementById('respText'), r);
  await refreshGroupBook();
}

async function deleteGroup() {
  const gssi = Number(document.getElementById('gbMembersGssi').value);
  const r = await postJson('/api/groupbook/delete', { gssi });
  setResp(document.getElementById('respText'), r);
  await refreshGroupBook();
}

async function moveMember() {
  const issi = Number(document.getElementById('gbMoveIssi').value);
  const to_gssi = Number(document.getElementById('gbMoveTo').value);
  const r = await postJson('/api/groupbook/move_member', { issi, to_gssi });
  setResp(document.getElementById('respText'), r);
  await refreshGroupBook();
}

function parseIdList(s) {
  return (s || "")
    .split(/[\s,\n\r\t]+/)
    .map(x => x.trim())
    .filter(x => x.length > 0)
    .map(x => Number(x))
    .filter(x => Number.isFinite(x));
}

async function dgnaAttach(forceReplace) {
  const targets = parseIdList(document.getElementById("dgnaTargets").value);
  const gssi = Number(document.getElementById("dgnaGssi").value);
  const detach_all_first = forceReplace ? true : (document.getElementById("dgnaDetachAll").value === "true");
  const ack_req = document.getElementById("dgnaAck").value === "true";
  const lifetime = Number(document.getElementById("dgnaLifetime").value);
  const class_of_usage = Number(document.getElementById("dgnaClass").value);
  const body = { targets, gssi, detach_all_first, ack_req, lifetime, class_of_usage };
  setResp(document.getElementById("respDgna"), await postJson("/api/dgna/attach", body));
  await refreshDgnaJobs();
}

async function dgnaDetach() {
  const targets = parseIdList(document.getElementById("dgnaTargets").value);
  const gssi = Number(document.getElementById("dgnaGssi").value);
  const ack_req = document.getElementById("dgnaAck").value === "true";
  const lifetime = Number(document.getElementById("dgnaLifetime").value);
  const class_of_usage = Number(document.getElementById("dgnaClass").value);
  const body = { targets, gssi, detach_all_first: false, ack_req, lifetime, class_of_usage };
  setResp(document.getElementById("respDgna"), await postJson("/api/dgna/detach", body));
  await refreshDgnaJobs();
}

async function dgnaTransfer() {
  const issi = Number(document.getElementById("dgnaMoveIssi").value);
  const to_gssi = Number(document.getElementById("dgnaGssi").value);
  setResp(document.getElementById("respDgna"), await postJson("/api/dgna/transfer", { issi, to_gssi }));
  await refreshGroupBook();
  await refreshDgnaJobs();
}

async function refreshDgnaJobs() {
  const r = await fetch("/api/dgna_jobs");
  const jobs = await r.json().catch(() => []);
  const lines = (Array.isArray(jobs) ? jobs : [])
    .slice(0, 50)
    .map(j => {
      const tgt = (j.targets || []).map(t => `${t.issi}:${t.status}${t.error ? `(${t.error})` : ""}`).join(" | ");
      const op = j.op || "";
      return `#${j.job_id} ${op} gssi=${j.gssi} replace=${j.detach_all_first} ack=${j.ack_req} :: ${tgt}`;
    });
  document.getElementById("dgnaJobsList").value = lines.join("\n");
}

async function sendText() {
  const body = {
    dst_type: document.getElementById("dstTypeText").value,
    dst: Number(document.getElementById("dstText").value),
    calling: Number(document.getElementById("callingText").value),
    link_id: Number(document.getElementById("linkIdText").value),
    endpoint_id: Number(document.getElementById("endpointIdText").value),
    format: document.getElementById("fmtText").value,
    text: document.getElementById("textBody").value,
    tcs: document.getElementById("tcsText").value,
    mr: Number(document.getElementById("mrText").value),
    drr: document.getElementById("drrText").value === "true"
  };
  setResp(document.getElementById("respText"), await postJson("/api/send_text", body));
  await refreshJobs();
}

async function sendHex() {
  const body = {
    dst_type: document.getElementById("dstTypeHex").value,
    dst: Number(document.getElementById("dstHex").value),
    calling: Number(document.getElementById("callingHex").value),
    link_id: Number(document.getElementById("linkIdHex").value),
    endpoint_id: Number(document.getElementById("endpointIdHex").value),
    bitlen: Number(document.getElementById("bitlenHex").value),
    hex: document.getElementById("hexBody").value,
    mr: Number(document.getElementById("mrHex").value),
    drr: document.getElementById("drrHex").value === "true"
  };
  setResp(document.getElementById("respHex"), await postJson("/api/send_hex", body));
}

async function refreshOnline() {
  const ttl = Number(document.getElementById("ttl").value);
  const r1 = await fetch(`/api/online?ttl=${ttl}`);
  const j1 = await r1.json().catch(() => ({ ids:[] }));
  const ids1 = Array.isArray(j1) ? j1 : (j1.ids || []);
  document.getElementById("onlineList").value = ids1.join("\n");

  const r2 = await fetch(`/api/group_online_all?ttl=${ttl}`);
  const j2 = await r2.json().catch(() => ({ groups:[] }));
  const groups = j2.groups || [];
  const lines = groups
    .filter(g => (g.ids || []).length > 0)
    .map(g => `${g.gssi}：${(g.ids || []).join(" / ")}`);
  document.getElementById("groupOnlineList").value = lines.join("\n");
}



async function refreshVoice() {
  const r = await fetch('/api/voice/state');
  const st = await r.json().catch(() => ({}));
  const enabled = !!st.enabled;
  const ts = Number(st.tch_ts || 2);
  const tchan = Number(st.tchan || 4);
  document.getElementById('voiceEnable').value = enabled ? 'true' : 'false';
  document.getElementById('voiceTs').value = String(ts);
  document.getElementById('voiceTchan').value = String(tchan);
  const lines = [];
  lines.push(`enabled=${enabled}`);
  lines.push(`tch_ts=${ts}`);
  lines.push(`tchan=${tchan}`);
  document.getElementById('voiceState').value = lines.join("\n");
}

async function applyVoice() {
  const body = {
    enabled: document.getElementById('voiceEnable').value === 'true',
    tch_ts: Number(document.getElementById('voiceTs').value),
    tchan: Number(document.getElementById('voiceTchan').value)
  };
  setResp(document.getElementById('respVoice'), await postJson('/api/voice/state', body));
  await refreshVoice();
}

// auto refresh
refreshOnline();
refreshJobs();
refreshGroupBook();
refreshDgnaJobs();
refreshVoice();
setInterval(refreshOnline, 2000);
setInterval(refreshJobs, 1000);
setInterval(refreshGroupBook, 5000);
setInterval(refreshDgnaJobs, 1000);
setInterval(refreshVoice, 1000);
</script>
</body>
</html>
"#;