//! Update check. The server publishes a small manifest, signed with a key
//! whose public half is baked into the app at build time (BKN_UPDATE_KEY).
//! The app downloads the installer for its platform, checks it against the
//! manifest, and then offers to open it: installing stays the user's click.
//!
//! The signature is what makes this safe. Whoever controls the download host
//! can serve any file, but not one this app will accept. A build without the
//! key, which is every dev build, never looks for updates.
use std::{collections::HashMap, path::PathBuf, time::Duration};

use anyhow::{Context, Result, anyhow, ensure};
use aws_lc_rs::signature::{ED25519, UnparsedPublicKey};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;

/// `latest.json`, as written by scripts/release-manifest.sh.
#[derive(Deserialize)]
struct Manifest {
    version: String,
    /// Keyed by platform: "macos-aarch64", "macos-x86_64", "windows-x86_64".
    files: HashMap<String, File>,
}

#[derive(Deserialize)]
struct File {
    name: String,
    sha256: String,
}

/// A newer version, downloaded and checked, ready to be opened.
pub struct Update {
    pub version: String,
    pub installer: PathBuf,
}

/// "0.10.2" is newer than "0.9.9". A version that isn't dotted numbers is
/// never newer, so a malformed manifest can't start a download.
fn newer(candidate: &str, current: &str) -> bool {
    let parse = |version: &str| version.split('.').map(|n| n.parse::<u64>().ok()).collect::<Option<Vec<_>>>();
    matches!((parse(candidate), parse(current)), (Some(a), Some(b)) if a > b)
}

/// The manifest, provided `signature` is `public_key`'s over these exact bytes.
fn verified(manifest: &[u8], signature: &[u8], public_key: &[u8]) -> Result<Manifest> {
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(manifest, signature)
        .map_err(|_| anyhow!("the update manifest's signature is not valid"))?;
    Ok(serde_json::from_slice(manifest)?)
}

/// The installer this build should fetch, if the manifest offers a newer one.
fn pick<'a>(manifest: &'a Manifest, current: &str, platform: &str) -> Result<Option<&'a File>> {
    let Some(file) = manifest.files.get(platform).filter(|_| newer(&manifest.version, current)) else {
        return Ok(None);
    };
    // The name becomes a URL and a path on disk: a plain file name only.
    let plain = !file.name.is_empty() && !file.name.starts_with('.') && file.name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    ensure!(plain, "the update manifest names an unusable file: {:?}", file.name);
    Ok(Some(file))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn check(public_key: &[u8]) -> Result<Option<Update>> {
    let manifest = crate::api::download("latest.json").await?;
    let signature = crate::api::download("latest.json.sig").await?;
    let manifest = verified(&manifest, &signature, public_key)?;
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let Some(file) = pick(&manifest, env!("CARGO_PKG_VERSION"), &platform)? else {
        return Ok(None);
    };

    let installer = dirs::download_dir().or_else(dirs::cache_dir).context("no folder to download into")?.join(&file.name);
    let expected = file.sha256.to_lowercase();
    // Left there by an earlier run? Then don't fetch it again, but only if it still matches.
    let already = tokio::fs::read(&installer).await.is_ok_and(|bytes| sha256_hex(&bytes) == expected);
    if !already {
        let bytes = crate::api::download(&file.name).await?;
        ensure!(sha256_hex(&bytes) == expected, "the downloaded update does not match its manifest");
        tokio::fs::write(&installer, bytes).await?;
    }
    Ok(Some(Update { version: manifest.version, installer }))
}

/// Looks for an update now and every six hours until one is ready.
pub async fn watch(found: UnboundedSender<Update>) {
    let Some(public_key) = option_env!("BKN_UPDATE_KEY").and_then(|key| STANDARD.decode(key).ok()) else {
        return;
    };
    loop {
        match check(&public_key).await {
            Ok(Some(update)) => {
                tracing::info!(version = %update.version, "update downloaded");
                let _ = found.send(update);
                return;
            }
            Ok(None) => tracing::debug!("no update"),
            Err(error) => tracing::warn!("update check failed: {error:#}"),
        }
        tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
    }
}

/// Hands the installer to the system: mounts the .dmg, or runs the .exe.
pub fn open(installer: &std::path::Path) {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    } else {
        std::process::Command::new("xdg-open")
    };
    if let Err(error) = command.arg(installer).spawn() {
        tracing::error!("could not open the update: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};

    #[test]
    fn only_signed_newer_plain_named_updates() {
        assert!(newer("0.10.2", "0.9.9") && newer("1.0", "0.9.9"));
        assert!(!newer("0.1.0", "0.1.0") && !newer("0.0.9", "0.1.0"));
        assert!(!newer("9.9.9-beta", "0.1.0") && !newer("", "0.1.0"));

        let json = br#"{"version":"0.2.0","files":{"macos-aarch64":{"name":"bkn-proxy-0.2.0-macos-aarch64.dmg","sha256":"AB"}}}"#;
        let key = Ed25519KeyPair::generate().unwrap();
        let signature = key.sign(json);
        let manifest = verified(json, signature.as_ref(), key.public_key().as_ref()).unwrap();

        // Signed by someone else, or changed after signing: refused.
        let other = Ed25519KeyPair::generate().unwrap();
        assert!(verified(json, signature.as_ref(), other.public_key().as_ref()).is_err());
        let mut tampered = json.to_vec();
        tampered[12] = b'9';
        assert!(verified(&tampered, signature.as_ref(), key.public_key().as_ref()).is_err());

        assert_eq!(pick(&manifest, "0.1.0", "macos-aarch64").unwrap().unwrap().name, "bkn-proxy-0.2.0-macos-aarch64.dmg");
        assert!(pick(&manifest, "0.2.0", "macos-aarch64").unwrap().is_none(), "same version");
        assert!(pick(&manifest, "0.1.0", "windows-x86_64").unwrap().is_none(), "no build for this platform");

        let evil: Manifest = serde_json::from_str(r#"{"version":"9.0.0","files":{"x":{"name":"../../.zshrc","sha256":""}}}"#).unwrap();
        assert!(pick(&evil, "0.1.0", "x").is_err(), "a path is not a file name");

        // Made with scripts/release-manifest.sh's openssl commands: the release
        // side and the app must agree on key and signature encoding.
        let openssl_key = STANDARD.decode("hhwDvtRmnRarZ2AZ9Udz1ULwV5ydATKHRYXI6+Px35Y=").unwrap();
        let openssl_signature =
            STANDARD.decode("SdHDzjShJmuKPeypzN+6nfWgSagatjhotxFcPFPj/yob01r/vXEkfZ+v86JUJal8x28t8JVOE2aw384Yv3F/CQ==").unwrap();
        assert_eq!(verified(br#"{"version":"9.9.9","files":{}}"#, &openssl_signature, &openssl_key).unwrap().version, "9.9.9");

        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
