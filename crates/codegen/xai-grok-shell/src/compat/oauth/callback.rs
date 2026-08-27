//! Tiny loopback HTTP helpers for vendor OAuth callbacks.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) async fn read_http_request(
    stream: &mut tokio::net::TcpStream,
) -> std::io::Result<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16_384 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub(super) async fn write_html(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: String,
) -> std::io::Result<()> {
    let reason = if status == 200 { "OK" } else { "Error" };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await
}

pub(super) fn success_html(heading: &str) -> String {
    page_html(
        "Signed in",
        heading,
        "You may close this tab and return to Grok.",
        None,
    )
}

pub(super) fn error_html(heading: &str, details: &str) -> String {
    page_html(
        "Sign-in failed",
        heading,
        "You may close this tab and return to Grok.",
        Some(details),
    )
}

pub(super) fn page_html(
    title: &str,
    heading: &str,
    message: &str,
    details: Option<&str>,
) -> String {
    let details = details
        .filter(|d| !d.is_empty())
        .map(|d| {
            format!(
                "<pre style=\"color:#a1a1aa;white-space:pre-wrap\">{}</pre>",
                html_escape(d)
            )
        })
        .unwrap_or_default();
    format!(
        "<!doctype html><html><head><meta charset=utf-8><title>{}</title></head>\
         <body style=\"font-family:system-ui;background:#09090b;color:#fafafa;display:flex;\
         align-items:center;justify-content:center;min-height:100vh;margin:0\">\
         <main><h1>{}</h1><p>{}</p>{}</main></body></html>",
        html_escape(title),
        html_escape(heading),
        html_escape(message),
        details
    )
}

pub(super) fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
