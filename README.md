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
- Checks for a newer version, downloads and verifies it, and offers to install.

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
PNG (macOS):

```bash
BKN_SHOTS=/tmp/bkn-shots cargo test shots
```

## How the tunnel works

The first Connect re-launches the app's own binary with administrator rights
as a helper (one password prompt: `osascript` on macOS, UAC on Windows,
`pkexec` on Linux). The helper creates a tun device and forwards everything
in it to the chosen proxy with tun2proxy; names are resolved by the proxy, so
DNS does not leak. Switching proxies reuses the running helper, so it is
instant. Quitting the app stops the helper.

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
design system: a flat ground, ink, a label grey and a single signal colour;
no cards, borders or shadows.

The wordmark is set in JetBrains Mono (SIL Open Font License, see
`assets/JetBrainsMono-LICENSE.txt`). `assets/make-icons.py` regenerates the
icon files from `assets/icon.svg`, `assets/make-tray-icons.py` the tray icons.

Linux needs GTK and libayatana-appindicator for the tray icon (on Debian and
Ubuntu: `libgtk-3-dev libayatana-appindicator3-dev`).
