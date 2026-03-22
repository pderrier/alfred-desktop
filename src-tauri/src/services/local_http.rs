use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    time::Duration,
};

use anyhow::{anyhow, Result};

pub fn parse_http_url(raw: &str) -> Result<(String, u16, String)> {
    let trimmed = raw.trim();
    let without_scheme = trimmed
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("unsupported_url_scheme"))?;
    let (host_port, path) = match without_scheme.split_once('/') {
        Some((left, right)) => (left, format!("/{}", right)),
        None => (without_scheme, "/".to_string()),
    };
    let (host, port) = match host_port.split_once(':') {
        Some((host, port_raw)) => {
            let port = port_raw
                .parse::<u16>()
                .map_err(|error| anyhow!("invalid_url_port:{error}"))?;
            (host.to_string(), port)
        }
        None => (host_port.to_string(), 80),
    };
    if host.trim().is_empty() {
        return Err(anyhow!("invalid_url_host"));
    }
    Ok((host, port, path))
}

pub fn request_http_json(
    method: &str,
    host: &str,
    port: u16,
    path: &str,
    body: Option<&str>,
    timeout_ms: Option<u64>,
) -> Result<serde_json::Value> {
    let address = resolve_socket_addr(host, port)?;
    let mut stream = if let Some(ms) = timeout_ms {
        let timeout = Duration::from_millis(ms);
        let stream =
            TcpStream::connect_timeout(&address, timeout).map_err(|error| anyhow!("connect:{error}"))?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| anyhow!("read_timeout:{error}"))?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|error| anyhow!("write_timeout:{error}"))?;
        stream
    } else {
        TcpStream::connect(address).map_err(|error| anyhow!("connect:{error}"))?
    };
    let payload = body.unwrap_or("");
    let request = if payload.is_empty() {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
        )
    } else {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            payload.as_bytes().len(),
            payload
        )
    };
    stream
        .write_all(request.as_bytes())
        .map_err(|error| anyhow!("write:{error}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|error| anyhow!("read:{error}"))?;
    let response = String::from_utf8_lossy(&raw);
    let (head, body) = split_http_response(&response)?;
    let status_line = head.lines().next().unwrap_or_default();
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    let decoded_body = decode_http_response_body(head, body)?;
    let parsed = parse_http_json_body(&decoded_body)?;
    if status_code != 200 {
        let upstream_code = parsed
            .get("error_code")
            .and_then(|v| v.as_str())
            .unwrap_or("http_status");
        let upstream_message = parsed
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or(upstream_code);
        return Err(anyhow!("{upstream_code}:{upstream_message}"));
    }
    Ok(parsed)
}

pub(crate) fn parse_http_json_body(raw: &str) -> Result<serde_json::Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("http_invalid_response:empty_body"));
    }
    serde_json::from_str::<serde_json::Value>(trimmed)
        .map_err(|error| anyhow!("http_invalid_response:invalid_json:{error}"))
}

pub fn resolve_socket_addr(host: &str, port: u16) -> Result<SocketAddr> {
    format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|error| anyhow!("invalid_socket_addr:{error}"))?
        .next()
        .ok_or_else(|| anyhow!("invalid_socket_addr:no_resolved_address"))
}

fn split_http_response<'a>(response: &'a str) -> Result<(&'a str, &'a str)> {
    response
        .split_once("\r\n\r\n")
        .or_else(|| response.split_once("\n\n"))
        .ok_or_else(|| anyhow!("invalid_http_response"))
}

pub(crate) fn decode_http_response_body(head: &str, body: &str) -> Result<String> {
    let transfer_encoding = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("transfer-encoding") {
                Some(value.trim().to_ascii_lowercase())
            } else {
                None
            }
        })
        .unwrap_or_default();
    if transfer_encoding.contains("chunked") {
        return decode_chunked_http_body(body);
    }
    Ok(body.to_string())
}

fn decode_chunked_http_body(body: &str) -> Result<String> {
    let mut remaining = body;
    let mut decoded = String::new();
    loop {
        let Some((size_line, rest)) = remaining.split_once("\r\n").or_else(|| remaining.split_once('\n')) else {
            return Err(anyhow!("invalid_chunked_response"));
        };
        let chunk_size_hex = size_line.split(';').next().unwrap_or_default().trim();
        let chunk_size = usize::from_str_radix(chunk_size_hex, 16)
            .map_err(|error| anyhow!("invalid_chunk_size:{error}"))?;
        remaining = rest;
        if chunk_size == 0 {
            return Ok(decoded);
        }
        if remaining.len() < chunk_size {
            return Err(anyhow!("invalid_chunked_response"));
        }
        decoded.push_str(&remaining[..chunk_size]);
        remaining = &remaining[chunk_size..];
        if let Some(stripped) = remaining.strip_prefix("\r\n") {
            remaining = stripped;
        } else if let Some(stripped) = remaining.strip_prefix('\n') {
            remaining = stripped;
        } else {
            return Err(anyhow!("invalid_chunked_response"));
        }
    }
}
