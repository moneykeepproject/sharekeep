//! HTTPS to the household service, trusting only the hub's own certificate authority.
//! Copied from MoneyKeep's capture server (`wifi.rs`, its check client).

use rustls::pki_types::{CertificateDer, ServerName};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

/// A request over HTTPS at `addr`, trusting only `ca_der`, for `host`: the status
/// code and body.
pub fn https_call(
    addr: SocketAddr,
    host: &str,
    ca_der: &[u8],
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>), String> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca_der.to_vec()))
        .map_err(|e| format!("the CA certificate cannot be used: {e}"))?;
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| e.to_string())?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let name = ServerName::try_from(host.to_owned()).map_err(|e| e.to_string())?;
    let mut conn =
        rustls::ClientConnection::new(Arc::new(config), name).map_err(|e| e.to_string())?;
    let mut tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|e| format!("cannot connect to {addr}: {e}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    let mut bytes = req.into_bytes();
    bytes.extend_from_slice(body.unwrap_or(&[]));
    tls.write_all(&bytes).map_err(|e| e.to_string())?;
    let mut answer = Vec::new();
    match tls.read_to_end(&mut answer) {
        Ok(_) => {}
        Err(e) if !answer.is_empty() && e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        Err(e) => return Err(e.to_string()),
    }
    parse_answer(&answer)
}

/// A plain-HTTP request: the hub's certificate helper on the home network (public
/// certificates only) and the hub computer's own loopback admin port. Never for anything
/// secret across the network.
pub fn http_call(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>), String> {
    let mut tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|e| format!("cannot connect to {addr}: {e}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    let mut bytes = req.into_bytes();
    bytes.extend_from_slice(body.unwrap_or(&[]));
    tcp.write_all(&bytes).map_err(|e| e.to_string())?;
    let mut answer = Vec::new();
    tcp.read_to_end(&mut answer).map_err(|e| e.to_string())?;
    parse_answer(&answer)
}

/// The status and body of an HTTP/1.1 answer (`Connection: close`; chunked bodies
/// are joined).
pub fn parse_answer(answer: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = answer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("no HTTP answer")?;
    let head = String::from_utf8_lossy(answer.get(..split).unwrap_or(&[])).into_owned();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or("no HTTP status")?;
    let body = answer.get(split + 4..).unwrap_or(&[]).to_vec();
    let chunked = head.lines().any(|l| {
        l.to_ascii_lowercase()
            .starts_with("transfer-encoding: chunked")
    });
    if !chunked {
        return Ok((status, body));
    }
    let mut out = Vec::new();
    let mut rest: &[u8] = &body;
    while let Some(eol) = rest.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(
            String::from_utf8_lossy(rest.get(..eol).unwrap_or(&[])).trim(),
            16,
        )
        .unwrap_or(0);
        if size == 0 {
            break;
        }
        out.extend_from_slice(rest.get(eol + 2..eol + 2 + size).unwrap_or(&[]));
        rest = rest.get(eol + 4 + size..).unwrap_or(&[]);
    }
    Ok((status, out))
}
