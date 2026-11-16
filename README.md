# bkn-proxy

A desktop app that sends all of a device's traffic through a SOCKS5 proxy.
You sign in, pick one of the connections your admin gave you, and press
Connect. macOS and Windows; Linux builds from source.

Built with Rust, [GPUI](https://www.gpui.rs) and
[tun2proxy](https://github.com/tun2proxy/tun2proxy).

## What it does

- Signs in to a bkn-proxy server with email, password and optional 2FA.
- Lists the proxies the admin granted. Changes on the server (a proxy granted,
  revoked or edited) reach the app at once; a revoked proxy drops the tunnel.
- Lets you give a connection your own name. Only you see it, and it follows
  your account to your other devices.
- Routes the whole device, not just one program, and shows speed, totals and
  a graph of the last minute while connected.
- English and Russian, light and dark.
- Checks for a newer version, downloads and verifies it, and offers to install.

## Run from source

```bash
BKN_API_URL=http://127.0.0.1:8080 cargo run
```

`BKN_API_URL` is the server's address. It is read at build time and again at
run time, and is never written in this repository. Without it the app talks
to `http://127.0.0.1:8080`.

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
`pkexec` on Linux). The helper creates a TUN device, routes all traffic into
it and forwards it to the chosen proxy; names are resolved by the proxy, so
DNS does not leak. Switching proxies reuses the running helper, so it is
instant. Quitting the app stops the helper and restores the routes. See
`src/tunnel.rs`.

Windows needs `wintun.dll` next to the executable; the release build ships it.

## Releases

Pushing a tag such as `v0.2.0` (matching `version` in `Cargo.toml`) runs
`.github/workflows/release.yml`: it builds and notarizes the macOS disk
images, builds the Windows archive, writes the signed update manifest and
publishes a GitHub release. The secrets it needs are listed at the top of
that file. Windows builds are not code-signed yet.

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
icon files from `assets/icon.svg`.
