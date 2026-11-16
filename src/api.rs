//! HTTP client for the bkn-proxy API.
use std::{sync::LazyLock, time::Duration};

use reqwest::{Client, Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use crate::wire::*;
use tokio::sync::mpsc::UnboundedSender;

static HTTP: LazyLock<Client> = LazyLock::new(Client::new);

/// The server's address, e.g. https://example.com. Runtime env wins, then the
/// value baked in at build time, then local dev. It is never written in the
/// source: release builds get it from the build environment.
pub fn base() -> String {
    std::env::var("BKN_API_URL").unwrap_or_else(|_| option_env!("BKN_API_URL").unwrap_or("http://localhost:8080").into())
}

/// The version of the server's API this app speaks.
const API: &str = "/api/v1";

/// A file from the server's /downloads (update manifest, installer).
/// `progress` hears each new whole percent, when the server says how long the file is.
pub async fn download(name: &str, mut progress: impl FnMut(u8)) -> Result<Vec<u8>, reqwest::Error> {
    let mut response = HTTP.get(format!("{}/downloads/{name}", base())).timeout(Duration::from_secs(600)).send().await?.error_for_status()?;
    let total = response.content_length().filter(|total| *total > 0);
    let (mut bytes, mut said) = (Vec::new(), 0);
    while let Some(chunk) = response.chunk().await? {
        bytes.extend_from_slice(&chunk);
        let percent = total.map_or(0, |total| (bytes.len() as u64 * 100 / total).min(100) as u8);
        if percent != said {
            said = percent;
            progress(percent);
        }
    }
    Ok(bytes)
}

/// The API's machine code ("totp_required", "unauthorized", ...) or "offline".
#[derive(Debug)]
pub struct Error(pub String);

pub async fn call<T: DeserializeOwned>(method: Method, path: &str, token: Option<&str>, body: Option<Value>) -> Result<T, Error> {
    let mut req = HTTP.request(method, base() + API + path).timeout(Duration::from_secs(15));
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    if let Some(body) = body {
        req = req.json(&body);
    }
    let offline = |_| Error("offline".into());
    let resp = req.send().await.map_err(offline)?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(offline)?;
    if !status.is_success() {
        let code = serde_json::from_slice::<ApiError>(&bytes).map_or(status.to_string(), |e| e.error);
        return Err(Error(code));
    }
    // 204s have no body; `null` deserializes into `()`.
    serde_json::from_slice(if bytes.is_empty() { b"null" } else { &bytes }).map_err(|e| Error(e.to_string()))
}

/// Everything the main screen shows.
pub async fn snapshot(token: String) -> Result<(Me, Vec<Proxy>), Error> {
    let me = call(Method::GET, "/me", Some(&token), None).await?;
    let proxies = call(Method::GET, "/proxies", Some(&token), None).await?;
    Ok((me, proxies))
}

/// Holds the server's event stream open and pokes `refresh` whenever the
/// server says something changed, and once on every (re)connect so nothing
/// missed while offline is lost. Ends when the receiver is dropped.
pub async fn watch(token: String, refresh: UnboundedSender<()>) {
    loop {
        if refresh.send(()).is_err() {
            return;
        }
        if let Ok(mut resp) = HTTP.get(base() + API + "/events").bearer_auth(&token).send().await {
            if resp.status() == StatusCode::UNAUTHORIZED {
                return; // the refresh above got the same 401 and signs the user out
            }
            // The server sends a keep-alive comment every 15s; 45s of silence = dead connection.
            while let Ok(Ok(Some(chunk))) = tokio::time::timeout(Duration::from_secs(45), resp.chunk()).await {
                if chunk.windows(5).any(|w| w == b"data:") && refresh.send(()).is_err() {
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
