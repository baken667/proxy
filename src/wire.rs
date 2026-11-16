//! The JSON the server's API (v1) sends and expects, as far as this app uses it.
use serde::Deserialize;

#[derive(Deserialize)]
pub struct LoginResponse {
    pub token: String,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Proxy {
    pub id: i64,
    /// The user's own name for it if they set one, else the admin's name.
    pub name: String,
    /// socks5://[user:pass@]host:port
    pub url: String,
}

#[derive(Deserialize, Clone)]
pub struct Me {
    pub email: String,
    pub totp_enabled: bool,
}

#[derive(Deserialize)]
pub struct TotpSetup {
    pub secret: String,
    pub otpauth_url: String,
}

/// Error body of every non-2xx response. `error` is a stable machine code,
/// e.g. "totp_required", "invalid_credentials", "locked".
#[derive(Deserialize, Debug)]
pub struct ApiError {
    pub error: String,
}
