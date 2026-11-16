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
- Reports to the server, every five seconds while connected, which proxy it
  uses and how many bytes went through: no addresses, no DNS names. The
  server's backoffice shows who is online and the traffic.
- English and Russian, light and dark; a fixed-size window.
- Checks for a newer version every hour and from a button in Settings,
  downloads and verifies it, then says so in a system notification and in a
  ribbon across the top of every screen, with an Install button.

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
  the binary to `/Library/PrivilegedHelperTools/kz.bkn.proxy.helper` and
  registers it with launchd (`/Library/LaunchDaemons/kz.bkn.proxy.helper.plist`).
  From then on launchd starts the helper whenever the app opens the service's
  socket, and the helper exits when the app closes it. An update asks again
  only when it changes the helper itself (`src/tunnel.rs`, `src/net.rs` or a
  dependency). Only the user who installed the service can open its socket;
  any program of that user can then switch the tunnel without a prompt.
- Windows (UAC) and Linux (`pkexec`) ask at the first Connect of every launch.

To remove the service from a Mac:

```bash
sudo launchctl bootout system/kz.bkn.proxy.helper; sudo rm /Library/LaunchDaemons/kz.bkn.proxy.helper.plist /Library/PrivilegedHelperTools/kz.bkn.proxy.helper
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

Linux still uses tun2proxy's own setup, which does not undo itself if the
helper is killed.

Windows needs `wintun.dll` next to the executable; the release build ships it.

## Releases

Pushing a tag such as `v0.2.0` (matching `version` in `Cargo.toml`) runs
`.github/workflows/release.yml`: it builds and notarizes the macOS disk
image (Apple Silicon only), builds the Windows archive, writes the signed update manifest and
publishes a GitHub release. The release notes are that version's section of
`CHANGELOG.md`, so write it before tagging. The secrets the workflow needs
are listed at the top of that file. Windows builds are not code-signed yet.

To package for macOS by hand:

```bash
BKN_API_URL=https://example.com scripts/package-macos.sh
```

### Updates

The app fetches `latest.json` and `latest.json.sig` from the server's
`/downloads`, and accepts the manifest only if it is signed by the key whose
public half was given to the build as `BKN_UPDATE_KEY`. It then downloads the
installer for its platform and checks it against the manifest's SHA-256. A
build without `BKN_UPDATE_KEY` never checks for updates. Creating the key is
described in `scripts/release-manifest.sh`.

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
License); the Windows archive adds `wintun.dll` from wintun.net under its own
license.

To report a security problem, see `SECURITY.md`.
