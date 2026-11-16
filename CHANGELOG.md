# Changelog

What changed in each release of the bkn-proxy desktop app. The section for a
version becomes the notes of its GitHub release, where a line break inside a
paragraph shows as one: keep each entry on a single line.

## 0.2.3 — 2026-11-17

- macOS: closing the window takes the app out of the Dock and the Cmd+Tab list. It stays in the menu bar: Open bkn-proxy there, or opening the app again, brings the window and the Dock icon back.
- A system notification says when a new version has been downloaded; its Install button opens the installer, a click on the notification opens the window.
- Settings has a Check for updates button.

## 0.2.2 — 2026-11-16

- macOS no longer asks for the keychain password at every launch. The saved session now has a keychain entry of its own per server, so a development build and the installed app no longer take turns locking each other out of one shared entry. Sign in once more after updating: the old entry is not carried over.
- A saved session shows nothing of the account until the server has confirmed it. While the server can't be reached the app says "Checking your session…" with the reason and a Sign out button, instead of an empty connection list and account settings.
- Settings ends with the app's version and the commit it was built from.

## 0.2.1 — 2026-11-16

- Fixed: DNS could stay pointed at the tunnel after a disconnect or a crash and leave the device without internet. The app now changes the network only in ways the system undoes by itself when the tunnel goes away.
- Network settings left behind by 0.1.0 and 0.2.0 are found at start (a banner with Repair network, also in Settings) and repaired before every connect.
- The tunnel no longer drops new connections under load: the limit is 4096 connections instead of 200.
- The tunnel's own addresses moved from 10.0.0.x, a common home network range, to 198.51.100.0/30.
- The tray icon reacts to clicks at once; release builds are smaller.

## 0.2.0 — 2026-11-16

- Sign-in goes through the browser, so passwords, 2FA codes and passkeys all work. Settings links to the browser to manage passkeys.
- An icon in the menu bar (macOS) or notification area shows whether the tunnel is on, switches connections, reopens the window and quits.
- Closing the window keeps the app and the tunnel running.
- While connected the app reports its connection and traffic totals to the server, for the admin console.
- The window has a fixed size.

## 0.1.0 — 2026-11-16

First release: sign in, pick a connection, press Connect, and all of the device's traffic goes through that SOCKS5 proxy.

- Connections granted, changed or revoked by the admin show up at once; a revoked one drops the tunnel.
- Personal names for connections, kept per account.
- Speed, totals and a one-minute graph while connected.
- English and Russian, light and dark.
- Signed update check: a newer version is downloaded, verified and installed on your click.
