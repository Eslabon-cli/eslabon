//! Read-only probes of local model servers: a plain HTTP GET over loopback with a
//! short timeout. Never generates, never touches anything but loopback.

use std::io::Read;
use std::io::Write;
use std::net::TcpStream;
use std::net::ToSocketAddrs;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

pub trait Sondeo {
    /// Body of a successful `GET url`, or `None` when the server is unreachable or
    /// answers with an error.
    fn get(&self, url: &str) -> Option<String>;
}

/// `http://host:port/path` split into parts; only loopback hosts are accepted.
pub fn partes_loopback(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (authority, 80),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(host, "127.0.0.1" | "localhost" | "::1").then(|| {
        (host.to_string(), port, path.to_string())
    })
}

pub fn es_loopback(url: &str) -> bool {
    partes_loopback(url).is_some()
}

pub struct SondeoHttp;

impl Sondeo for SondeoHttp {
    fn get(&self, url: &str) -> Option<String> {
        let (host, port, path) = partes_loopback(url)?;
        // `localhost` comes from the resolver: only connect if it really is loopback.
        let addr = (host.as_str(), port)
            .to_socket_addrs()
            .ok()?
            .find(|addr| addr.ip().is_loopback())?;
        let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT).ok()?;
        stream.set_read_timeout(Some(TIMEOUT)).ok()?;
        stream.set_write_timeout(Some(TIMEOUT)).ok()?;
        write!(
            stream,
            "GET {path} HTTP/1.0\r\nHost: {host}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
        )
        .ok()?;
        let mut raw = Vec::new();
        stream.take(MAX_RESPONSE_BYTES).read_to_end(&mut raw).ok()?;
        let text = String::from_utf8_lossy(&raw);
        let (head, body) = text.split_once("\r\n\r\n")?;
        let status_ok = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .is_some_and(|code| code.starts_with('2'));
        status_ok.then(|| body.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn only_loopback_urls_are_probed() {
        assert_eq!(
            partes_loopback("http://127.0.0.1:11434/v1/models"),
            Some(("127.0.0.1".to_string(), 11434, "/v1/models".to_string()))
        );
        assert_eq!(
            partes_loopback("http://localhost/props"),
            Some(("localhost".to_string(), 80, "/props".to_string()))
        );
        assert_eq!(partes_loopback("https://api.novita.ai/openai/models"), None);
        assert_eq!(partes_loopback("http://10.0.0.5:11434/v1/models"), None);
    }
}
