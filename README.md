# bkn-proxy

A desktop app that sends all of a device's traffic through a SOCKS5 proxy.
You sign in, pick one of the connections your admin gave you, and press
Connect. macOS and Windows; Linux builds from source.

Built with Rust, [GPUI](https://www.gpui.rs) and
[tun2proxy](https://github.com/tun2proxy/tun2proxy).

## What it does

- Signs in through the browser: the server's sign-in page handles password,
  2FA code and passkeys, then hands the app a one-time code on 127.0.0.1
  (OAuth's native-app flow with PKCE). See `src/signin.rs`.
- Lives in the menu bar (macOS) or notification area (Windows, Linux): the
  icon shows whether the tunnel is on and switches connections. Closing the
  window keeps the app and the tunnel running; quit from the icon's menu.
  On macOS the app has a Dock icon and a place in Cmd+Tab only while its
  window is open.
- Lists the proxies the admin granted. Changes on the server (a proxy granted,
  revoked or edited) reach the app at once; a revoked proxy drops the tunnel.
- Lets you give a connection your own name. Only you see it, and it follows
  your account to your other devices.
- Routes the whole device, not just one program, and shows speed, totals and
  a graph of the last minute while connected.
- Keeps the connection by itself. A tunnel that goes down is brought back
  (after 1, 2, 4, 8 and 16 seconds, then every 30) until it is up or you
  disconnect, and a system notification says that traffic goes out directly
  meanwhile. Another Wi-Fi network or a wake from sleep does not need a
  reconnect at all: the tunnel moves to the new network within a few seconds.
- Three switches in Settings: Reconnect (on by default), Connect at launch
  (to the connection used last) and Open at login (in the background, with
  no window). The last two are off until you turn them on.
- Two-factor setup shows the key as a QR code for the authenticator app.
- Reports to the server, every five seconds while connected, which proxy it
  uses and how many bytes went through: no addresses, no DNS names. The
  server's backoffice shows who is online and the traffic.
- English and Russian, light and dark; a fixed-size window.
- Looks for a newer version every hour and from a button in Settings, and
  says when there is one: in a system notification and in a ribbon across the
  top of every screen. Update downloads it and shows the progress. On macOS
  it then puts the new app in the old one's place without opening the disk
  image, and Restart runs it; on Windows the installer replaces the app and
  starts it again.

## Run from source

```bash
BKN_API_URL=http://localhost:8080 cargo run
```

`BKN_API_URL` is the server's address. It is read at build time and again at
run time, and is never written in this repository. Without it the app talks
to `http://localhost:8080` (a host name, not an IP: the sign-in page's
passkeys need one).

## Checks

```bash
cargo test
```

To look at every screen without launching the app, render them offscreen to
PNG (macOS). The page the browser shows after sign-in is written next to them
as HTML:

```bash
BKN_SHOTS=/tmp/bkn-shots cargo test
```

## How the tunnel works

A helper with administrator rights, the app's own binary in another mode,
creates a tun device and forwards everything in it to the chosen proxy with
tun2proxy; names are resolved by the proxy, so DNS does not leak. Switching
proxies reuses the running helper, so it is instant. Quitting the app stops
the helper.

How the helper gets its rights:

- macOS asks for the administrator password once. The first Connect copies
  the app to `/Library/PrivilegedHelperTools/kz.bkn.proxy.helper.app`, where
  only root can change it, and registers the executable in that copy with
  launchd (`/Library/LaunchDaemons/kz.bkn.proxy.helper.plist`). The whole app
  and not just the executable: a signed executable is bound to the bundle
  around it, and macOS refuses to run it alone.
  From then on launchd starts the helper whenever the app opens the service's
  socket, and the helper exits when the app closes it. An update asks again
  only when it changes the helper itself (`src/tunnel.rs`, `src/net.rs` or a
  dependency). Only the user who installed the service can open its socket;
  any program of that user can then switch the tunnel without a prompt.
- Windows (UAC) and Linux (`pkexec`) ask at the first Connect of every launch.

To remove the service from a Mac:

```bash
sudo launchctl bootout system/kz.bkn.proxy.helper; sudo rm -rf /Library/LaunchDaemons/kz.bkn.proxy.helper.plist /Library/PrivilegedHelperTools/kz.bkn.proxy.helper.app
```

On macOS and Windows every network change undoes itself if the helper goes
away, even if it is killed (see `src/net.rs`):

- The system's default route is never touched. Two half-routes, 0.0.0.0/1
  and 128.0.0.0/1, point into the tun device; they vanish with it.
- DNS points into the tunnel only through settings that live as long as the
  helper: on macOS keys of its SystemConfiguration session, on Windows the
  tun adapter's own DNS. Saved network settings are never written.
- The tunnel uses 198.51.100.0/30, an address range no real network uses, so
  it can't hide the local network.

Versions 0.1.0 and 0.2.0 could leave DNS pointed at 10.0.0.1 or the default
route changed after a crash. The app checks for that at start and offers
Repair network (also in Settings), and every connect repairs it first.

The host route that keeps the proxy server itself out of the tunnel goes
through the gateway of the network the device is on. While a tunnel is up the
helper reads the routing table every three seconds and moves that route when
the gateway is no longer there, or puts it back when it went away with the
old address (`follow` in `src/net.rs`).

Linux still uses tun2proxy's own setup, which does not undo itself if the
helper is killed and does not follow a change of network.

## Settings on disk

The switches are kept in `prefs.json` next to the saved language, in the
system's configuration folder (`~/Library/Application Support/bkn-proxy` on
macOS). Open at login is not in that file: it is an entry the system reads by
itself, which starts the app with `--background`. On macOS that is
`~/Library/LaunchAgents/kz.bkn.proxy.plist`, on Windows a value under the
user's `Run` key, on Linux `~/.config/autostart/kz.bkn.proxy.desktop`. The
app rewrites the entry at every start, so it follows the app when it is moved.

Windows needs `wintun.dll` next to the executable; the installer puts it there.

## Installing on Windows

`bkn-proxy-windows-x86_64.exe` is an installer (Inno Setup,
`scripts/installer.iss`). It installs for the current user, into
`%LOCALAPPDATA%\Programs\bkn-proxy`, so neither installing nor updating asks
for administrator rights; only connecting does. It adds bkn-proxy to the Start
menu and, unless unticked, to the desktop, and starts the app. Running a newer
installer over a running app stops the app, replaces it and starts it again.
Remove it from Settings → Apps; that also removes the Open at login entry. The
saved sign-in and settings stay.

To build the installer by hand, on Windows with Inno Setup 6 and `wintun/`
unpacked from wintun.net next to `Cargo.toml`:

```
cargo build --release
iscc /DMyAppVersion=0.0.0 scripts\installer.iss
```

## Releases

Pushing a tag such as `v0.2.0` (matching `version` in `Cargo.toml`) runs
`.github/workflows/release.yml`: it builds and notarizes the macOS disk
image (Apple Silicon only), builds the Windows installer, writes the signed
update manifest and publishes a GitHub release. The release notes are that
version's section of `CHANGELOG.md`, so write it before tagging. The secrets
the workflow needs are listed at the top of that file. Windows builds are not
code-signed yet: see Code signing policy below.

To package for macOS by hand:

```bash
BKN_API_URL=https://example.com scripts/package-macos.sh
```

### Updates

A check fetches `latest.json` and `latest.json.sig` from the server's
`/downloads`, and accepts the manifest only if it is signed by the key whose
public half was given to the build as `BKN_UPDATE_KEY`. Nothing else is
downloaded until the user presses Update. Then the app downloads the
installer for its platform and checks it against the manifest's SHA-256.

On macOS it mounts the disk image out of sight, copies the new app next to
the installed one and swaps the two by renaming, so a failure on the way
leaves the installed app as it was; Restart runs the new one. Where that is
not possible (an app folder the user can't write to) the disk image is opened
instead. On Windows the app runs the downloaded installer with only its
progress window; the installer stops the app, replaces it and starts the new
one. A build without `BKN_UPDATE_KEY` never checks for updates.

Before tagging a release that changes how the helper is installed or how the
app is replaced, run the workflow by hand and try its signed disk image: an
unsigned local build does not behave like the signed one there. Creating the key is
described in `scripts/release-manifest.sh`.

## Code signing policy

macOS builds are signed with the maintainer's Developer ID and notarized by
Apple. Windows builds are not signed yet. The project is applying for free
code signing for open source from SignPath Foundation; once it is accepted,
the release workflow signs the app and its installer (see the top of
`.github/workflows/release.yml`), and this applies:

Free code signing provided by [SignPath.io](https://about.signpath.io),
certificate by [SignPath Foundation](https://signpath.org).

- Committers and reviewers: [baken667](https://github.com/baken667)
- Approvers: [baken667](https://github.com/baken667)

Every release is built from this repository by GitHub Actions and signed only
after an approver confirms the request.

## Privacy

This is the project's privacy policy.

The app talks to one server, the one it was built for (`BKN_API_URL`), and to
the proxy you connect to. Nothing goes to anyone else: no analytics, no crash
reports, no third-party services.

What the app sends to the server:

- Your sign-in. You sign in on the server's page in the browser; the app gets
  a session token and keeps it in the system's keychain (Credential Manager on
  Windows).
- The kind of device, as "Mac" or "Windows PC".
- The names you give connections.
- While connected, every five seconds: which connection is in use and how
  many bytes went through it. No addresses, no DNS names, nothing else about
  the traffic.
- A question whether there is a newer version, every hour.

What the server keeps: your account (email address, password hash, two-factor
and passkey settings), your sessions with the kind of device and the IP
address they came from, a log of account actions with the IP address, and one
record per connection: which proxy, when it started and ended, and the byte
counts. The server's administrators see this in its backoffice; they create
the accounts and can delete them, and deleting an account deletes its
sessions and connection records. The server is run by whoever gave you the
account: ask them about anything it holds on you.

While connected, all of the device's traffic goes through the chosen proxy,
whose operator can see it as any network operator can.

On the device the app keeps its settings (`prefs.json`, see Settings on disk)
and the session token. Uninstalling leaves both; signing out removes the
token.

## Languages and design

Texts live in `src/i18n.rs`, one `Strings` value per language: a string
missing from a language does not compile. The interface follows one small
design system (`src/main.rs`, "design"): a flat ground, ink and a label grey;
no cards, borders or shadows. Four colours carry a meaning and always come
with an icon or a word: danger (errors, and actions that take something
away), warning, success and info. The orange of the logo is the brand's and
says nothing.

Icons are [Lucide](https://lucide.dev), which gpui-kit bundles; the ones the
app uses are listed in `icon_assets!` in `src/main.rs`.

The wordmark is set in JetBrains Mono (SIL Open Font License, see
`assets/JetBrainsMono-LICENSE.txt`). `assets/make-icons.py` regenerates the
icon files from `assets/icon.svg`, `assets/make-tray-icons.py` the tray icons.

Linux needs GTK and libayatana-appindicator for the tray icon (on Debian and
Ubuntu: `libgtk-3-dev libayatana-appindicator3-dev`).

## License

bkn-proxy is free software under the GNU General Public License, version 3 or
later: see `LICENSE`. That is not only a choice. tun2proxy, which carries the
tunnel, builds on socks5-impl, which is GPL-3.0-or-later, so the app as it is
distributed is under the GPL either way.

It ships JetBrains Mono (SIL Open Font License 1.1) and Lucide icons (ISC
License); the Windows installer adds `wintun.dll` from wintun.net under its own
license.

To report a security problem, see `SECURITY.md`.
