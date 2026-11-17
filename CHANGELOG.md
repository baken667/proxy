# Changelog

What changed in each release of the bkn-proxy desktop app. The section for a
version becomes the notes of its GitHub release, where a line break inside a
paragraph shows as one: keep each entry on a single line.

## 0.5.0 — 2026-11-17

- Fixed: signing in on Windows. The browser opened a page saying the sign-in link was incomplete, because Windows cut the link at its first `&`.
- Windows has an installer instead of an archive to unpack. It installs for the current user without asking for administrator rights, adds bkn-proxy to the Start menu and, if you leave the box ticked, to the desktop, and starts the app. Its language follows Windows: English or Russian. To remove the app, use Settings → Apps.
- Updating on Windows is one click: Update downloads the installer and runs it, and the installer stops the app, replaces it and starts the new version. A tunnel that was up goes down for that moment.
- Coming from the 0.4.0 archive: Update downloads the installer and opens it; press Install. The folder the archive was unpacked into is no longer used and can be deleted.
- Windows builds are still not code-signed, so SmartScreen warns when the installer is first run: choose "More info", then "Run anyway". The project is applying for free signing for open source; the README says how releases will be signed and what the app sends to the server.

## 0.4.0 — 2026-11-17

- The connection comes back by itself. A tunnel that goes down is reconnected: after 1, 2, 4, 8 and 16 seconds, then every 30, until it is up or you press Disconnect. A system notification says that traffic goes out directly meanwhile. Reconnect is a switch in Settings and is on.
- Another Wi-Fi network, a cable or a wake from sleep no longer leaves the device without internet until a reconnect: within a few seconds the tunnel moves to the new network. macOS and Windows. The first connect after this update asks for the administrator password once more on macOS, as the tunnel helper changed.
- Open at login: a switch in Settings. The app starts in the background, in the menu bar or notification area, with no window. Off until you turn it on.
- Connect at launch: a switch in Settings. Starting the app connects to the connection used last. Off until you turn it on; on Windows it brings the administrator prompt at start.
- Setting up two-factor authentication shows a QR code to scan with the authenticator app. The key and the setup link can still be copied.
- Fixed: the line saying that a proxy was taken away by the admin disappeared as soon as it was shown.

## 0.3.1 — 2026-11-17

- Fixed: on macOS 0.3.0 could not connect at all. It asked for the administrator password and then said "The tunnel helper did not start": macOS refuses to run a signed app's executable outside its app, and that is how the helper was installed. The helper is now a copy of the whole app. The first connect after this update asks for the password once more.
- Updates work the way they do in Zed. Check only asks whether there is a newer version and answers at once. Update downloads it with a visible progress and, on macOS, installs it in place without opening the disk image; Restart then runs the new version. Every step is said in the ribbon on top and in Settings, and system notifications carry the same buttons. Windows still gets the downloaded archive opened.
- The sign-in page and the account page open in the app's language, not the browser's. They have a language switch of their own, as does the download site.

## 0.3.0 — 2026-11-17

- macOS asks for the administrator password once instead of at the first connect of every launch. The first connect installs the tunnel helper as a system service; after that it starts without a prompt, also after the app or the Mac restarts. An update asks again only when it changes the helper itself. Windows still asks once per launch.
- A more compact interface. Settings fits on one screen: every setting is a row with an icon and its button, and the password and two-factor fields open under their row only when asked for. Icons are Lucide.
- Colours with a meaning: red for errors and for what takes something away (Sign out, Disconnect, turning two-factor off), amber for warnings, green for Connected and for what went well, blue for news. Each comes with an icon or a word.
- A downloaded update shows as a ribbon across the top of every screen, with an Install button. The app looks for a new version every hour instead of every six.
- The page the browser shows after sign-in has the app's logo, typeface and colours, light and dark.
- The app is licensed under the GPL, version 3 or later; SECURITY.md says how to report a vulnerability.

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
