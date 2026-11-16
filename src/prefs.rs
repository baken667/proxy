//! What the user chose in Settings, and the app's place among the programs
//! the system starts at login.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Kept as JSON next to the saved language. A missing or unreadable file is
/// the defaults, and so is a field this version does not find in it.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Prefs {
    /// A tunnel that drops is brought back without asking. On by default:
    /// while it is down the device's traffic goes out directly.
    pub reconnect: bool,
    /// Starting the app connects to the connection used last. Off by
    /// default: an app that was only opened should not reroute the device.
    pub connect_at_start: bool,
    /// The connection used last and the server it is from: an id is only
    /// good for the server that gave it.
    pub last: Option<(String, i64)>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { reconnect: true, connect_at_start: false, last: None }
    }
}

impl Prefs {
    pub fn load() -> Prefs {
        let saved = file().and_then(|path| std::fs::read_to_string(path).ok());
        saved.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    /// Best effort, like the language: an unsaved choice is the default next time.
    pub fn save(&self) {
        // Tests run the real screens; they must not write the user's settings.
        if cfg!(test) {
            return;
        }
        if let (Some(path), Ok(text)) = (file(), serde_json::to_string(self)) {
            let _ = path.parent().map(std::fs::create_dir_all);
            let _ = std::fs::write(path, text);
        }
    }
}

fn file() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("bkn-proxy").join("prefs.json"))
}

/// Starting with the system: an entry the system itself reads at login, so
/// nothing of ours has to run to make it happen. The entry is the setting;
/// `Prefs` knows nothing of it. Off until the user turns it on.
pub mod autostart {
    #[cfg(windows)]
    use std::io::Error;
    #[cfg(not(windows))]
    use std::io::ErrorKind;
    use std::io::Result;

    /// What the entry adds to the command line: start in the tray, no window.
    pub const FLAG: &str = "--background";

    /// A debug build gets an entry of its own, as it does a helper: `cargo
    /// run` must never repoint the installed app's.
    const NAME: &str = if cfg!(debug_assertions) { "kz.bkn.proxy.dev" } else { "kz.bkn.proxy" };

    /// macOS: a launch agent, which launchd runs at login. Linux: an XDG
    /// autostart entry. The file and what goes into it.
    // ponytail: a launch agent shows under "Allow in the Background" in
    // System Settings, not under "Open at Login". SMAppService puts it there,
    // at the price of Objective-C bindings; switch if that distinction matters.
    #[cfg(not(windows))]
    pub(super) fn entry() -> Option<(std::path::PathBuf, String)> {
        let exe = std::env::current_exe().ok()?;
        let exe = exe.to_str()?;
        #[cfg(target_os = "macos")]
        {
            let xml = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            let plist = format!(
                concat!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>"#,
                    r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#,
                    r#"<plist version="1.0"><dict>"#,
                    "<key>Label</key><string>{name}</string>",
                    "<key>ProgramArguments</key><array><string>{exe}</string><string>{flag}</string></array>",
                    "<key>RunAtLoad</key><true/>",
                    // Not a background job to throttle: it is the app.
                    "<key>ProcessType</key><string>Interactive</string>",
                    // System Settings names the entry after the app, not after the file.
                    "<key>AssociatedBundleIdentifiers</key><string>kz.bkn.proxy</string>",
                    "</dict></plist>\n"
                ),
                name = NAME,
                exe = xml(exe),
                flag = FLAG,
            );
            Some((dirs::home_dir()?.join("Library/LaunchAgents").join(format!("{NAME}.plist")), plist))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let desktop = format!("[Desktop Entry]\nType=Application\nName=bkn-proxy\nExec=\"{exe}\" {FLAG}\n");
            Some((dirs::config_dir()?.join("autostart").join(format!("{NAME}.desktop")), desktop))
        }
    }

    #[cfg(not(windows))]
    pub fn enabled() -> bool {
        entry().is_some_and(|(path, _)| path.exists())
    }

    /// Writes the entry for where the app is now, or removes it.
    #[cfg(not(windows))]
    pub fn set(on: bool) -> Result<()> {
        if cfg!(test) {
            return Ok(()); // as with `Prefs::save`
        }
        let (path, content) = entry().ok_or(ErrorKind::NotFound)?;
        if on {
            path.parent().map(std::fs::create_dir_all).transpose()?;
            return std::fs::write(path, content);
        }
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Windows: a value under the user's Run key.
    #[cfg(windows)]
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

    #[cfg(windows)]
    pub fn enabled() -> bool {
        crate::net::command("reg").args(["query", KEY, "/v", NAME]).output().is_ok_and(|out| out.status.success())
    }

    #[cfg(windows)]
    pub fn set(on: bool) -> Result<()> {
        if cfg!(test) {
            return Ok(());
        }
        let mut reg = crate::net::command("reg");
        if on {
            let exe = std::env::current_exe()?;
            reg.args(["add", KEY, "/v", NAME, "/t", "REG_SZ", "/d", &format!("\"{}\" {FLAG}", exe.display()), "/f"]);
        } else if enabled() {
            reg.args(["delete", KEY, "/v", NAME, "/f"]);
        } else {
            return Ok(());
        }
        let out = reg.output()?;
        if out.status.success() { Ok(()) } else { Err(Error::other(String::from_utf8_lossy(&out.stderr).trim().to_string())) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_older_files() {
        let prefs = Prefs::default();
        assert!(prefs.reconnect && !prefs.connect_at_start && prefs.last.is_none());
        // A file from before a field existed, and one that is not ours at all.
        let older: Prefs = serde_json::from_str(r#"{"connect_at_start":true}"#).unwrap();
        assert!(older.reconnect && older.connect_at_start);
        assert!(serde_json::from_str::<Prefs>("not json").is_err());
        let prefs = Prefs { last: Some(("https://example.com".into(), 7)), ..prefs };
        assert_eq!(serde_json::from_str::<Prefs>(&serde_json::to_string(&prefs).unwrap()).unwrap(), prefs);
    }

    /// launchd's own tool reads the login entry, and it starts this
    /// executable in the background.
    #[cfg(target_os = "macos")]
    #[test]
    fn login_entry_is_well_formed() {
        let (path, plist) = autostart::entry().unwrap();
        assert!(path.ends_with("Library/LaunchAgents/kz.bkn.proxy.dev.plist"), "a debug build has an entry of its own: {path:?}");
        let exe = std::env::current_exe().unwrap();
        assert!(plist.contains(&format!("<string>{}</string><string>--background</string>", exe.display())));
        let file = std::env::temp_dir().join(format!("bkn-proxy-test-login-{}.plist", std::process::id()));
        std::fs::write(&file, plist).unwrap();
        let lint = std::process::Command::new("plutil").arg("-lint").arg(&file).output().unwrap();
        let _ = std::fs::remove_file(&file);
        assert!(lint.status.success(), "{}", String::from_utf8_lossy(&lint.stdout));
    }
}
