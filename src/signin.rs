//! Sign-in through the browser, so every way of signing in the server offers
//! (password, 2FA code, passkeys) works here without the app knowing them.
//! OAuth's pattern for native apps (RFC 8252, with PKCE):
//! 1. Pick a secret `verifier`, listen on 127.0.0.1 and open the server's
//!    /login page with the port, SHA-256(verifier) and a random `state`.
//! 2. After sign-in the page sends the browser back to 127.0.0.1:PORT with a
//!    one-time code and the same `state`.
//! 3. Trade the code plus the verifier for this device's own session token.
//! A code that leaks on the way is useless without the verifier.
use std::{net::Ipv4Addr, time::Duration};

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Method;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

use crate::{api, i18n::Strings, wire::LoginResponse};

fn random(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(buf.as_mut_slice());
    URL_SAFE_NO_PAD.encode(buf)
}

/// "Mac", "Windows PC": how this device is listed in the backoffice.
pub fn device_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "Mac",
        "windows" => "Windows PC",
        "linux" => "Linux",
        other => other,
    }
}

/// Opens the browser, waits for the user to sign in (up to ten minutes) and
/// returns this device's session token.
pub async fn sign_in(s: &'static Strings) -> Result<String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = listener.local_addr()?.port();
    let verifier = random(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random(16);
    crate::update::open_external(&format!("{}/login?app={port}&challenge={challenge}&state={state}", api::base()));

    let code = tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            let (stream, _) = listener.accept().await?;
            if let Some(code) = answer(stream, &state, s).await {
                return anyhow::Ok(code);
            }
        }
    })
    .await
    .context("sign-in timed out")??;

    let body = json!({ "code": code, "verifier": verifier, "device": device_name() });
    let response: LoginResponse = api::call(Method::POST, "/auth/app/token", None, Some(body)).await.map_err(|e| anyhow::anyhow!(e.0))?;
    Ok(response.token)
}

/// Reads one request from the browser. Returns the code when it is the
/// callback this sign-in is waiting for; anything else (a favicon request, a
/// stale tab) gets a short answer and is ignored.
async fn answer(mut stream: TcpStream, state: &str, s: &Strings) -> Option<String> {
    let mut buf = vec![0u8; 8192];
    let mut len = 0;
    while !buf[..len].windows(4).any(|w| w == b"\r\n\r\n") && len < buf.len() {
        match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf[len..])).await {
            Ok(Ok(n)) if n > 0 => len += n,
            _ => return None,
        }
    }
    let request = String::from_utf8_lossy(&buf[..len]);
    let target = request.lines().next()?.strip_prefix("GET ")?.split(' ').next()?;
    let url = url::Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    let query = |key: &str| url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());
    let found = url.path() == "/" && query("state").as_deref() == Some(state);
    let code = query("code").filter(|_| found);
    // Anything but the callback itself gets a bare 404: nobody looks at it.
    let (status, page) = if code.is_some() { ("200 OK", page(s)) } else { ("404 Not Found", String::new()) };
    let response = format!("HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{page}", page.len());
    let _ = stream.write_all(response.as_bytes()).await;
    code
}

/// What the browser tab shows once the app has its code: the app's own look
/// (main.rs, "design"), light or dark with the system. The colours and the
/// logo's geometry are those of `palette` and `mark_svg`; the page stands
/// alone, so the wordmark's typeface travels inside it.
fn page(s: &Strings) -> String {
    format!(
        concat!(
            r#"<!doctype html><meta charset=utf-8><meta name=viewport content="width=device-width,initial-scale=1"><title>bkn-proxy</title><style>"#,
            r#"@font-face{{font-family:"JetBrains Mono";font-weight:500;src:url(data:font/ttf;base64,{font})}}"#,
            ":root{{color-scheme:light dark;--ground:#e8e8e6;--ink:#191918;--ink-2:#5c5c59;--signal:#b53604;--success:#17633a}}",
            "@media(prefers-color-scheme:dark){{:root{{--ground:#1e1e1d;--ink:#f1f1ee;--ink-2:#a2a29d;--signal:#ff6a2b;--success:#5fd08a}}}}",
            r#"body{{margin:0;min-height:100vh;display:grid;place-items:center;background:var(--ground);color:var(--ink);font:15px/1.4 system-ui,-apple-system,"Segoe UI",sans-serif}}"#,
            "main{{width:min(380px,100vw - 40px);padding-bottom:10vh}}",
            ".brand{{display:flex;align-items:center;gap:12px;margin-bottom:18px}}",
            r#".brand span{{font:500 24px/1.2 "JetBrains Mono",ui-monospace,monospace}}.brand i{{font-style:normal;color:var(--signal)}}"#,
            "h1{{display:flex;align-items:center;gap:8px;margin:0 0 4px;font-size:17px;line-height:1.3;color:var(--success)}}",
            "p{{margin:0;font-size:13px;color:var(--ink-2)}}</style>",
            r#"<main><div class=brand><svg width=56 height=56 viewBox="0 0 64 64" aria-hidden=true><path d="M4 20 H24 L40 44 H45" fill=none stroke=currentColor stroke-width=9 stroke-linejoin=round /><circle cx=55 cy=44 r=6.5 fill="var(--signal)"/></svg><span>bkn<i>-</i>proxy</span></div>"#,
            // Lucide's circle-check, as in the app.
            r#"<h1><svg width=18 height=18 viewBox="0 0 24 24" fill=none stroke=currentColor stroke-width=2 stroke-linecap=round stroke-linejoin=round aria-hidden=true><circle cx=12 cy=12 r=10 /><path d="m16 9-5.5 5.5L8 12"/></svg>{title}</h1>"#,
            "<p>{hint}</p></main>"
        ),
        font = base64::engine::general_purpose::STANDARD.encode(crate::FONT),
        title = s.signed_in_browser,
        hint = s.signed_in_browser_hint,
    )
}

/// For tests: the parts of the callback the app checks.
#[cfg(test)]
pub async fn callback_for_test(target: &str, state: &str) -> Option<String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.ok()?;
    let port = listener.local_addr().ok()?.port();
    let request = format!("GET {target} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n\r\n");
    let client = tokio::spawn(async move {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        reply
    });
    let (stream, _) = listener.accept().await.ok()?;
    let code = answer(stream, state, crate::i18n::Lang::En.strings()).await;
    let reply = client.await.ok()?;
    if code.is_some() {
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_the_expected_callback_carries_a_code() {
        assert_eq!(callback_for_test("/?code=abc&state=s3cret-state", "s3cret-state").await.as_deref(), Some("abc"));
        assert_eq!(callback_for_test("/?code=abc&state=other", "s3cret-state").await, None, "wrong state");
        assert_eq!(callback_for_test("/favicon.ico", "s3cret-state").await, None);
        assert_eq!(callback_for_test("/evil?code=abc&state=s3cret-state", "s3cret-state").await, None, "wrong path");
    }

    /// The page is whole, and `BKN_SHOTS=dir cargo test` leaves it there to look at.
    #[test]
    fn signed_in_page() {
        for lang in crate::i18n::Lang::ALL {
            let page = page(lang.strings());
            assert!(page.contains(lang.strings().signed_in_browser) && page.ends_with("</main>"));
            if let Ok(dir) = std::env::var("BKN_SHOTS") {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(format!("{dir}/signed-in-{}.html", lang.code()), page).unwrap();
            }
        }
    }

    #[test]
    fn pkce_values() {
        let verifier = random(32);
        assert_eq!(verifier.len(), 43);
        assert_eq!(URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())).len(), 43, "the server expects 43 characters");
    }
}
