//! Updates, in three steps the user can tell apart:
//!
//! 1. `check` reads a small manifest the server publishes, signed with a key
//!    whose public half is baked into the app at build time (BKN_UPDATE_KEY).
//!    It downloads nothing else, so it answers at once: there is a newer
//!    version, or there isn't.
//! 2. `install`, on the user's click, downloads the installer for this
//!    platform and checks it against the manifest.
//! 3. On macOS it then puts the new app in the place of the running one,
//!    without opening the disk image; a restart finishes the update. Where
//!    that can't be done (Windows, Linux, an app folder this user can't write
//!    to) the installer is handed to the system instead, as before.
//!
//! The signature is what makes this safe. Whoever controls the download host
//! can serve any file, but not one this app will accept. A build without the
//! key, which is every dev build, never looks for updates.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

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

/// A newer version the signed manifest offers this platform.
#[derive(Clone, Debug)]
pub struct Offer {
    pub version: String,
    /// The installer's file name under /downloads, and what its bytes must hash to.
    pub name: String,
    pub sha256: String,
}

/// What `install` is doing, for the interface to say.
pub enum Progress {
    /// Percent of the installer received.
    Downloading(u8),
    Installing,
}

/// How `install` ended.
#[derive(Debug)]
pub enum Outcome {
    /// The new app is in place; it runs after a restart.
    Installed,
    /// It could not be put in place here: the installer, for the system to open.
    Installer(PathBuf),
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

/// The key updates are signed with; None in a build made without one.
fn public_key() -> Option<Vec<u8>> {
    option_env!("BKN_UPDATE_KEY").and_then(|key| STANDARD.decode(key).ok())
}

/// Is there a newer version? Reads the manifest and nothing else.
pub async fn check() -> Result<Option<Offer>> {
    let public_key = public_key().context("this build has no update key")?;
    let manifest = crate::api::download("latest.json", |_| {}).await?;
    let signature = crate::api::download("latest.json.sig", |_| {}).await?;
    let manifest = verified(&manifest, &signature, &public_key)?;
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let file = pick(&manifest, env!("CARGO_PKG_VERSION"), &platform)?;
    Ok(file.map(|file| Offer { version: manifest.version.clone(), name: file.name.clone(), sha256: file.sha256.to_lowercase() }))
}

/// Downloads the offered installer, checks it and installs it. The steps are
/// reported to `progress` as they begin.
pub async fn install(offer: Offer, progress: UnboundedSender<Progress>) -> Result<Outcome> {
    let installer = dirs::download_dir().or_else(dirs::cache_dir).context("no folder to download into")?.join(&offer.name);
    // Left there by an earlier run? Then don't fetch it again, but only if it still matches.
    let already = tokio::fs::read(&installer).await.is_ok_and(|bytes| sha256_hex(&bytes) == offer.sha256);
    if !already {
        let _ = progress.send(Progress::Downloading(0));
        let bytes = crate::api::download(&offer.name, |percent| drop(progress.send(Progress::Downloading(percent)))).await?;
        ensure!(sha256_hex(&bytes) == offer.sha256, "the downloaded update does not match its manifest");
        tokio::fs::write(&installer, bytes).await?;
    }
    let _ = progress.send(Progress::Installing);
    let disk_image = installer.clone();
    match tokio::task::spawn_blocking(move || in_place(&disk_image)).await? {
        Ok(true) => {
            let _ = tokio::fs::remove_file(&installer).await; // it has done its work
            Ok(Outcome::Installed)
        }
        Ok(false) => Ok(Outcome::Installer(installer)),
        Err(error) => {
            tracing::warn!("could not update in place, handing the installer to the system: {error:#}");
            Ok(Outcome::Installer(installer))
        }
    }
}

/// Puts the app from the downloaded disk image in the place of the one that
/// is running. False when this is not an installed app (`cargo run`).
#[cfg(target_os = "macos")]
fn in_place(disk_image: &Path) -> Result<bool> {
    let exe = std::env::current_exe()?;
    match bundle_of(&exe) {
        Some(bundle) => replace_bundle(disk_image, bundle).map(|()| true),
        None => Ok(false),
    }
}

// ponytail: Windows and Linux still get the downloaded archive opened for
// them. In place there means swapping a running .exe; do it when those
// platforms are worth the testing.
#[cfg(not(target_os = "macos"))]
fn in_place(_: &Path) -> Result<bool> {
    Ok(false)
}

/// `/Applications/bkn-proxy.app` for `/Applications/bkn-proxy.app/Contents/MacOS/bkn-proxy`.
#[cfg(target_os = "macos")]
pub fn bundle_of(exe: &Path) -> Option<&Path> {
    exe.ancestors().nth(3).filter(|bundle| bundle.extension().is_some_and(|extension| extension == "app"))
}

/// The app's name inside the disk image (scripts/package-macos.sh).
#[cfg(target_os = "macos")]
const APP_IN_IMAGE: &str = "bkn-proxy.app";

/// Copies the new app next to the old one, then swaps the two by renaming.
/// Until the swap nothing of the installed app has changed, so a failure on
/// the way leaves it as it was. The running process keeps its own files: a
/// renamed or deleted bundle stays open for whoever has it open.
#[cfg(target_os = "macos")]
fn replace_bundle(disk_image: &Path, bundle: &Path) -> Result<()> {
    let run = |program: &str, args: &[&std::ffi::OsStr]| -> Result<()> {
        let out = std::process::Command::new(program).args(args).output().with_context(|| format!("can't run {program}"))?;
        ensure!(out.status.success(), "{program}: {}", String::from_utf8_lossy(&out.stderr).trim());
        Ok(())
    };
    let folder = bundle.parent().context("the app has no folder")?;
    let (new, old) = (folder.join(".bkn-proxy-update.app"), folder.join(".bkn-proxy-old.app"));
    let mount = std::env::temp_dir().join(format!("bkn-proxy-update-{}", std::process::id()));
    std::fs::create_dir_all(&mount)?;
    // Mounted out of sight: no Finder window, no icon on the desktop.
    run("hdiutil", &["attach".as_ref(), "-nobrowse".as_ref(), "-readonly".as_ref(), "-mountpoint".as_ref(), mount.as_ref(), disk_image.as_ref()])?;
    let copied = (|| {
        let _ = std::fs::remove_dir_all(&new); // left by an attempt that was cut short
        // ditto is what copies an app whole: links, permissions and the signature's extended attributes.
        run("ditto", &[mount.join(APP_IN_IMAGE).as_ref(), new.as_ref()])
    })();
    let _ = run("hdiutil", &["detach".as_ref(), mount.as_ref(), "-force".as_ref()]);
    let _ = std::fs::remove_dir(&mount);
    copied?;

    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(bundle, &old).context("can't move the installed app aside")?;
    if let Err(error) = std::fs::rename(&new, bundle) {
        let _ = std::fs::rename(&old, bundle); // put the old one back
        return Err(error).context("can't move the new app into place");
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// Looks for an update now and every hour until there is one. A check is two
/// small files, so an hour costs nothing and a release reaches a running app
/// within one.
pub async fn watch(found: UnboundedSender<Offer>) {
    if public_key().is_none() {
        return;
    }
    loop {
        match check().await {
            Ok(Some(offer)) => {
                tracing::info!(version = %offer.version, "update available");
                let _ = found.send(offer);
                return;
            }
            Ok(None) => tracing::debug!("no update"),
            Err(error) => tracing::warn!("update check failed: {error:#}"),
        }
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

/// Hands the installer to the system: mounts the .dmg, or opens the archive.
pub fn open(installer: &Path) {
    open_external(installer.as_os_str());
}

/// Opens a file or a URL with whatever the system uses for it.
pub fn open_external(target: impl AsRef<std::ffi::OsStr>) {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    } else {
        std::process::Command::new("xdg-open")
    };
    if let Err(error) = command.arg(target.as_ref()).spawn() {
        tracing::error!("could not open {:?}: {error}", target.as_ref());
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

    /// A real disk image, mounted and copied by the system's own tools: the
    /// app in it takes the place of the installed one, and nothing is left
    /// lying next to it.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_app_is_replaced_in_place() {
        assert_eq!(bundle_of(Path::new("/Applications/bkn-proxy.app/Contents/MacOS/bkn-proxy")), Some(Path::new("/Applications/bkn-proxy.app")));
        assert_eq!(bundle_of(Path::new("/code/target/debug/bkn-proxy")), None, "cargo run is not an installed app");

        let dir = std::env::temp_dir().join(format!("bkn-proxy-test-update-{}", std::process::id()));
        let app = |root: &Path, name: &str, version: &str| {
            std::fs::create_dir_all(root.join(name).join("Contents/MacOS")).unwrap();
            std::fs::write(root.join(name).join("Contents/MacOS/bkn-proxy"), version).unwrap();
        };
        app(&dir.join("image"), APP_IN_IMAGE, "new");
        app(&dir.join("Applications"), "Renamed by the user.app", "old");
        let disk_image = dir.join("update.dmg");
        let made = std::process::Command::new("hdiutil")
            .args(["create", "-quiet", "-volname", "bkn-proxy-test", "-format", "UDZO", "-srcfolder"])
            .args([dir.join("image"), disk_image.clone()])
            .output()
            .unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));

        let installed = dir.join("Applications/Renamed by the user.app");
        replace_bundle(&disk_image, &installed).unwrap();
        assert_eq!(std::fs::read_to_string(installed.join("Contents/MacOS/bkn-proxy")).unwrap(), "new");
        let left: Vec<_> = std::fs::read_dir(dir.join("Applications")).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(left, ["Renamed by the user.app"], "no copy and no old app left behind");

        // Not a disk image: the installed app is untouched.
        std::fs::write(&disk_image, "not an image").unwrap();
        assert!(replace_bundle(&disk_image, &installed).is_err());
        assert_eq!(std::fs::read_to_string(installed.join("Contents/MacOS/bkn-proxy")).unwrap(), "new");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
