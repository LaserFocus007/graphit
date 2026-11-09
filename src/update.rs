//! Hinweis auf neue Versionen: fragt höchstens einmal am Tag GitHub nach dem neuesten Release.
//! Ohne Netz oder bei einem Fehler passiert nichts.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gtk::prelude::*;
use gtk::{gio, glib};
use serde_json::Value;

use crate::window::Win;

const HOST: &str = "api.github.com";
/// Feste Nummer des Repos: Sie gilt auch, wenn Konto oder Repo umbenannt werden.
const PATH: &str = "/repositories/1451272686/releases/latest";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
const DAY: u64 = 86_400;
const START_DELAY: u32 = 5;
const MAX_RESPONSE: usize = 512 * 1024;

thread_local! {
    static STARTED: Cell<bool> = const { Cell::new(false) };
}

/// Startet die Prüfung für dieses Fenster, einmal je Programmlauf.
pub fn start(win: &Rc<Win>) {
    // "Version": tut für einen Test so, als wäre diese Version installiert
    let pretend = std::env::var("GRAPHIT_UPDATE").ok();
    let test_run = std::env::vars_os().any(|(k, _)| k.to_string_lossy().starts_with("GRAPHIT_"));
    if STARTED.replace(true) || (pretend.is_none() && (test_run || !due())) || !crate::settings::Settings::load().update_check {
        return;
    }
    let win = win.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future_seconds(START_DELAY).await;
        let Some((tag, url)) = fetch().await.and_then(|raw| parse_response(&raw)) else { return };
        remember_check();
        let installed = pretend.as_deref().unwrap_or(CURRENT);
        let offer = is_newer(&tag, installed) && url.starts_with("https://github.com/");
        if std::env::var("GRAPHIT_UPDATE").is_ok() {
            eprintln!("UPDATE neuestes {tag} installiert {installed} Hinweis {offer} {url}");
        }
        if offer {
            win.offer_update(tag.trim_start_matches('v'), &url);
        }
    });
}

fn stamp_file() -> std::path::PathBuf {
    glib::user_cache_dir().join("graphit/update-stand")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs()
}

fn due() -> bool {
    let last = std::fs::read_to_string(stamp_file()).ok().and_then(|s| s.trim().parse::<u64>().ok());
    last.is_none_or(|t| now().saturating_sub(t) >= DAY)
}

fn remember_check() {
    let file = stamp_file();
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(file, now().to_string());
}

async fn fetch() -> Option<Vec<u8>> {
    let client = gio::SocketClient::new();
    client.set_tls(true);
    client.set_timeout(10);
    let connection = client.connect_to_host_future(HOST, 443).await.ok()?;
    let request = format!("GET {PATH} HTTP/1.1\r\nHost: {HOST}\r\nUser-Agent: Graphit\r\nAccept: application/vnd.github+json\r\nConnection: close\r\n\r\n");
    connection.output_stream().write_all_future(request.into_bytes(), glib::Priority::DEFAULT_IDLE).await.ok()?;
    let input = connection.input_stream();
    let mut raw = Vec::new();
    // Server beenden TLS oft ohne Abschied, ein Lesefehler am Ende ist daher normal
    while let Ok(chunk) = input.read_bytes_future(16 * 1024, glib::Priority::DEFAULT_IDLE).await {
        if chunk.is_empty() {
            break;
        }
        raw.extend_from_slice(&chunk);
        if raw.len() > MAX_RESPONSE {
            return None;
        }
    }
    Some(raw)
}

/// Liest Tag und Seite des Releases aus einer rohen HTTP-Antwort.
fn parse_response(raw: &[u8]) -> Option<(String, String)> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    if head.lines().next()?.split(' ').nth(1)? != "200" {
        return None;
    }
    let body = &raw[split + 4..];
    let json: Value = if head.contains("transfer-encoding: chunked") { serde_json::from_slice(&dechunk(body)?).ok()? } else { serde_json::from_slice(body).ok()? };
    Some((json["tag_name"].as_str()?.to_string(), json["html_url"].as_str()?.to_string()))
}

fn dechunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let end = body.windows(2).position(|w| w == b"\r\n")?;
        let size = std::str::from_utf8(&body[..end]).ok()?.split(';').next()?.trim();
        let size = usize::from_str_radix(size, 16).ok()?;
        if size == 0 {
            return Some(out);
        }
        let start = end + 2;
        out.extend_from_slice(body.get(start..start + size)?);
        body = body.get(start + size + 2..)?;
    }
}

fn version_parts(v: &str) -> Option<Vec<u32>> {
    v.trim_start_matches('v').split('.').map(|p| p.parse().ok()).collect()
}

fn is_newer(tag: &str, installed: &str) -> bool {
    match (version_parts(tag), version_parts(installed)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_numbers_not_text() {
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(!is_newer("v0.1.1", "0.1.1"));
        assert!(!is_newer("v0.1.0", "0.1.1"));
        assert!(!is_newer("kaputt", "0.1.1"));
    }

    #[test]
    fn reads_plain_and_chunked_answers() {
        let body = r#"{"tag_name":"v1.2.3","html_url":"https://github.com/a/b/releases/tag/v1.2.3"}"#;
        let plain = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{body}");
        let (first, rest) = body.split_at(10);
        let chunked = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{first}\r\n{:x}\r\n{rest}\r\n0\r\n\r\n", first.len(), rest.len());
        for answer in [plain, chunked] {
            let (tag, url) = parse_response(answer.as_bytes()).expect("Antwort lesbar");
            assert_eq!(tag, "v1.2.3");
            assert!(url.ends_with("/v1.2.3"));
        }
    }

    #[test]
    fn ignores_errors() {
        assert!(parse_response(b"HTTP/1.1 404 Not Found\r\n\r\n{}").is_none());
        assert!(parse_response(b"Unsinn").is_none());
    }
}
