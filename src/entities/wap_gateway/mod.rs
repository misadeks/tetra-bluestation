//! Minimal HTTP gateway for WAP 2.0 (HTTP over IP).
//!
//! The HTTP proxy is disabled by default and WSP/UDP is forwarded to Kannel.
//! This is a basic HTTP proxy with a local test page at `/`.
//! It does not support HTTPS CONNECT or chunked transfer encoding.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::thread;
use std::time::Duration;

use crate::common::messagerouter::MessageQueue;
use crate::common::tdma_time::TdmaTime;
use crate::common::tetra_entities::TetraEntity;
use crate::entities::TetraEntityTrait;
use crate::saps::sapmsg::SapMsg;

pub struct WapGateway {
    enabled: bool,
}

impl WapGateway {
    pub fn new() -> Self {
        const PORT_HTTP: u16 = 8081;
        const PORT_WSP_LISTEN: u16 = 9201;
        const PORT_WSP_CO_LISTEN: u16 = 9200;
        const PORT_WSP_FORWARD: u16 = 9202;
        const WSP_LISTEN_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);
        const WSP_FORWARD_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);
        // Hardcoded Kannel upstream on the Pi for now.
        // Kannel WAPBox should listen on PORT_WSP_FORWARD (see kannel config).
        let kannel_udp = Some(SocketAddr::from((WSP_FORWARD_IP, PORT_WSP_FORWARD)));
        let disable_http = true;

        if let Some(up) = kannel_udp {
            println!(
                "WapGateway (WSP/UDP): configured listen {WSP_LISTEN_IP}:{PORT_WSP_LISTEN} forward {up}"
            );
        }

        if !disable_http {
            thread::spawn(move || {
                if let Err(e) = run_gateway(PORT_HTTP) {
                    eprintln!("WapGateway thread exited: {e}");
                }
            });
        } else {
            println!("WapGateway: HTTP server disabled (port {PORT_HTTP})");
        }
        thread::spawn(move || {
            if let Err(e) = run_wsp_gateway(WSP_LISTEN_IP, PORT_WSP_LISTEN, kannel_udp, "WSP/UDP") {
                eprintln!("WapGateway WSP thread exited: {e}");
            }
        });
        thread::spawn(move || {
            if let Err(e) = run_wsp_gateway(WSP_LISTEN_IP, PORT_WSP_CO_LISTEN, kannel_udp, "WSP/CO") {
                eprintln!("WapGateway WSP thread exited: {e}");
            }
        });

        Self { enabled: true }
    }
}

impl TetraEntityTrait for WapGateway {
    fn entity(&self) -> TetraEntity {
        TetraEntity::WapGateway
    }
    fn rx_prim(&mut self, _queue: &mut MessageQueue, _message: SapMsg) {}
    fn tick_start(&mut self, _queue: &mut MessageQueue, _ts: Option<TdmaTime>) {}
}

fn run_gateway(port: u16) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    println!("WapGateway: listening on 0.0.0.0:{port}");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                thread::spawn(move || {
                    let _ = handle_client(s);
                });
            }
            Err(e) => eprintln!("WapGateway accept failed: {e}"),
        }
    }
    Ok(())
}

fn run_wsp_gateway(
    listen_ip: Ipv4Addr,
    port: u16,
    upstream: Option<SocketAddr>,
    label: &str,
) -> anyhow::Result<()> {
    let sock = bind_wsp_socket(listen_ip, port)?;
    println!("WapGateway ({label}): listening on {listen_ip}:{port}");
    if let Some(upstream_addr) = upstream {
        println!("WapGateway ({label}): forwarding to {upstream_addr}");
    }
    let upstream_sock = if upstream.is_some() {
        let s = UdpSocket::bind(("0.0.0.0", 0))?;
        s.set_read_timeout(Some(Duration::from_secs(2)))?;
        Some(s)
    } else {
        None
    };
    let mut buf = [0u8; 2048];
    loop {
        let (n, src) = sock.recv_from(&mut buf)?;
        if n < 2 {
            continue;
        }
        let data = &buf[..n];
        eprintln!(
            "WapGateway {label} RX {} bytes from {} -> {}:{}: {}",
            n,
            src,
            listen_ip,
            port,
            hex_preview(data, 48)
        );
        if let (Some(up_addr), Some(up_sock)) = (upstream, upstream_sock.as_ref()) {
            eprintln!(
                "WapGateway {label} forward {} bytes from {} -> {}",
                n,
                src,
                up_addr
            );
            let _ = up_sock.send_to(data, up_addr);
            match up_sock.recv_from(&mut buf) {
                Ok((m, _)) => {
                    eprintln!("WapGateway {label} RX {} bytes from upstream -> {}", m, src);
                    let _ = sock.send_to(&buf[..m], src);
                }
                Err(e) => {
                    eprintln!("WapGateway {label} forward timeout/error: {e}");
                }
            }
        } else {
            if let Some(resp) = handle_wsp_datagram(data) {
                let _ = sock.send_to(&resp, src);
            }
        }
    }
}

fn bind_wsp_socket(listen_ip: Ipv4Addr, port: u16) -> anyhow::Result<UdpSocket> {
    let addr = SocketAddr::from((listen_ip, port));
    match UdpSocket::bind(addr) {
        Ok(sock) => return Ok(sock),
        Err(e) if e.kind() == ErrorKind::AddrNotAvailable => {
            eprintln!("WapGateway WSP bind {addr} failed (addr not available); retrying...");
            for _ in 0..20 {
                thread::sleep(Duration::from_millis(250));
                match UdpSocket::bind(addr) {
                    Ok(sock) => return Ok(sock),
                    Err(e) if e.kind() == ErrorKind::AddrNotAvailable => continue,
                    Err(e) => return Err(e.into()),
                }
            }
            eprintln!("WapGateway WSP bind {addr} still failing; falling back to 0.0.0.0:{port}");
            return Ok(UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))?);
        }
        Err(e) if e.kind() == ErrorKind::AddrInUse => {
            return Err(anyhow::anyhow!(
                "bind {addr} failed: {e}. Port in use; ensure Kannel does NOT bind {port} on 0.0.0.0."
            ));
        }
        Err(e) => return Err(e.into()),
    }
}

fn handle_wsp_datagram(data: &[u8]) -> Option<Vec<u8>> {
    let tid = data[0];
    let pdu = data[1];

    match pdu {
        0x40 => {
            // GET (connectionless WSP)
            let (uri_len, used) = decode_uintvar(&data[2..])?;
            let uri_len = uri_len as usize;
            if data.len() < 2 + used + uri_len {
                return None;
            }
            let uri_bytes = &data[2 + used..2 + used + uri_len];
            let uri = String::from_utf8_lossy(uri_bytes).to_string();
            let body = build_wml_body(&uri);
            Some(build_wsp_reply(tid, &body))
        }
        _ => {
            // Reply with Not Implemented
            Some(build_wsp_error(tid, 0x61, "Not implemented"))
        }
    }
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

fn build_wml_body(uri: &str) -> Vec<u8> {
    let body = format!(
        "<?xml version=\"1.0\"?>\
<!DOCTYPE wml PUBLIC \"-//WAPFORUM//DTD WML 1.1//EN\" \
\"http://www.wapforum.org/DTD/wml_1.1.xml\">\
<wml><card id=\"home\" title=\"TETRA\"><p>OK</p><p>{}</p></card></wml>",
        uri
    );
    body.into_bytes()
}

fn build_wsp_reply(tid: u8, body: &[u8]) -> Vec<u8> {
    // Reply PDU: TID, Type=Reply(0x04), Status=200(0x20), HeadersLen, ContentType, Headers, Data
    let mut out = Vec::new();
    out.push(tid);
    out.push(0x04);
    out.push(0x20);

    // ContentType = Value-length + text-string("text/vnd.wap.wml")
    let content_type = "text/vnd.wap.wml";
    let mut ct = Vec::new();
    let text_len = content_type.len() + 1; // include null terminator
    ct.push(encode_value_length(text_len));
    ct.extend_from_slice(content_type.as_bytes());
    ct.push(0x00);

    let headers_len = ct.len();
    out.extend_from_slice(&encode_uintvar(headers_len as u32));
    out.extend_from_slice(&ct);
    out.extend_from_slice(body);
    out
}

fn build_wsp_error(tid: u8, status: u8, msg: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(tid);
    out.push(0x04);
    out.push(status);

    let content_type = "text/plain";
    let mut ct = Vec::new();
    let text_len = content_type.len() + 1;
    ct.push(encode_value_length(text_len));
    ct.extend_from_slice(content_type.as_bytes());
    ct.push(0x00);

    let headers_len = ct.len();
    out.extend_from_slice(&encode_uintvar(headers_len as u32));
    out.extend_from_slice(&ct);
    out.extend_from_slice(msg.as_bytes());
    out
}

fn decode_uintvar(buf: &[u8]) -> Option<(u32, usize)> {
    let mut value: u32 = 0;
    for (i, &b) in buf.iter().take(5).enumerate() {
        value = (value << 7) | (u32::from(b & 0x7F));
        if (b & 0x80) == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

fn encode_uintvar(mut value: u32) -> Vec<u8> {
    let mut buf = [0u8; 5];
    let mut i = 4;
    buf[i] = (value & 0x7F) as u8;
    value >>= 7;
    while value > 0 && i > 0 {
        i -= 1;
        buf[i] = ((value & 0x7F) as u8) | 0x80;
        value >>= 7;
    }
    buf[i..].to_vec()
}

fn encode_value_length(len: usize) -> u8 {
    if len <= 30 {
        len as u8
    } else {
        // Fallback: 31 indicates that a uintvar follows; for now cap
        31
    }
}

fn handle_client(mut stream: TcpStream) -> anyhow::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;

    let req = read_request(&mut stream)?;
    if req.method.eq_ignore_ascii_case("CONNECT") {
        write_simple_response(
            &mut stream,
            501,
            "Not Implemented",
            "CONNECT not supported",
        )?;
        return Ok(());
    }

    let (host, port, path) = match parse_target(&req) {
        Some(t) => t,
        None => {
            write_simple_response(&mut stream, 400, "Bad Request", "Invalid request")?;
            return Ok(());
        }
    };

    if path == "/" || path == "/index.html" {
        let body = "<html><body><h2>TETRA WAP Gateway</h2><p>Gateway is up.</p></body></html>";
        write_simple_response(&mut stream, 200, "OK", body)?;
        return Ok(());
    }

    forward_request(&mut stream, &req, &host, port, &path)
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    uri: String,
    version: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> anyhow::Result<HttpRequest> {
    let mut buf = Vec::<u8>::new();
    let mut tmp = [0u8; 1024];
    let max = 64 * 1024;
    loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > max {
            return Err(anyhow::anyhow!("header too large"));
        }
    }

    let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(buf.len());
    let header_bytes = &buf[..header_end];
    let mut lines = header_bytes.split(|&b| b == b'\n');
    let first = lines.next().ok_or_else(|| anyhow::anyhow!("empty request"))?;
    let first = String::from_utf8_lossy(first).trim().to_string();
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let uri = parts.next().unwrap_or("").to_string();
    let version = parts.next().unwrap_or("HTTP/1.0").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        let line = String::from_utf8_lossy(line).trim().trim_end_matches('\r').to_string();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_string(), v.trim().to_string());
        }
    }

    let mut body = Vec::new();
    let content_len = headers
        .get("Content-Length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let already = buf.len().saturating_sub(header_end + 4);
    if already > 0 {
        body.extend_from_slice(&buf[header_end + 4..]);
    }
    while body.len() < content_len {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }

    Ok(HttpRequest { method, uri, version, headers, body })
}

fn parse_target(req: &HttpRequest) -> Option<(String, u16, String)> {
    let uri = req.uri.as_str();
    if let Some(rest) = uri.strip_prefix("http://") {
        return parse_host_path(rest, 80);
    }
    if let Some(rest) = uri.strip_prefix("https://") {
        return parse_host_path(rest, 443);
    }
    if uri.starts_with('/') {
        let host = req.headers.get("Host")?.to_string();
        let (host, port) = split_host_port(&host, 80);
        return Some((host, port, uri.to_string()));
    }
    None
}

fn parse_host_path(rest: &str, default_port: u16) -> Option<(String, u16, String)> {
    let mut parts = rest.splitn(2, '/');
    let host_port = parts.next().unwrap_or("");
    let path = format!("/{}", parts.next().unwrap_or(""));
    let (host, port) = split_host_port(host_port, default_port);
    Some((host, port, path))
}

fn split_host_port(host_port: &str, default_port: u16) -> (String, u16) {
    if let Some((h, p)) = host_port.split_once(':') {
        if let Ok(port) = p.parse::<u16>() {
            return (h.to_string(), port);
        }
    }
    (host_port.to_string(), default_port)
}

fn forward_request(
    client: &mut TcpStream,
    req: &HttpRequest,
    host: &str,
    port: u16,
    path: &str,
) -> anyhow::Result<()> {
    let mut upstream = TcpStream::connect((host, port))?;
    upstream.set_read_timeout(Some(Duration::from_secs(15)))?;

    let mut out = String::new();
    out.push_str(&format!("{} {} HTTP/1.0\r\n", req.method, path));
    out.push_str(&format!("Host: {}\r\n", host));
    out.push_str("Connection: close\r\n");
    for (k, v) in &req.headers {
        let kl = k.to_ascii_lowercase();
        if kl == "host" || kl == "proxy-connection" || kl == "connection" {
            continue;
        }
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");

    upstream.write_all(out.as_bytes())?;
    if !req.body.is_empty() {
        upstream.write_all(&req.body)?;
    }

    let mut buf = [0u8; 4096];
    loop {
        let n = upstream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        client.write_all(&buf[..n])?;
    }
    Ok(())
}

fn write_simple_response(stream: &mut TcpStream, code: u16, status: &str, body: &str) -> anyhow::Result<()> {
    let body_bytes = body.as_bytes();
    let resp = format!(
        "HTTP/1.0 {} {}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        code,
        status,
        body_bytes.len()
    );
    stream.write_all(resp.as_bytes())?;
    stream.write_all(body_bytes)?;
    Ok(())
}
