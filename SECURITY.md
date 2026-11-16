# Security

bkn-proxy routes a whole device's traffic and, to do that, runs a helper with
administrator rights. Reports about either are welcome.

## Reporting a vulnerability

Please report privately, not in a public issue: on the repository's
**Security** tab choose **Report a vulnerability**. Say which version and
system you used and how to reproduce the problem. You will get an answer
within a week.

Only the latest release is supported: fixes ship as a new version, which the
app offers as an update.

## What matters most

- The helper (`src/tunnel.rs`, `src/net.rs`): it runs as root. On macOS it is
  a launchd service that takes commands over a socket only the user who
  installed it can open.
- Updates (`src/update.rs`): an installer is accepted only with a manifest
  signed by the key built into the app.
- Sign-in (`src/signin.rs`): the browser hands the app a one-time code on
  127.0.0.1, bound to this sign-in with PKCE.
- The session token is kept in the system keychain. Proxy URLs can carry
  passwords and are never logged.
