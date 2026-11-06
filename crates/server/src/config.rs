use anyhow::{Result, bail};

pub struct Config {
    pub database_url: String,
    pub bind: String,
    /// `CALZONE_DAV`: the optional CalDAV/CardDAV door (decision 3). On by
    /// default; everything else must work with it off.
    pub dav: bool,
    /// `Secure` on the web session cookie. On by default; turn off only for a
    /// plain-http LAN setup, or browsers will not send the cookie back.
    pub cookie_secure: bool,
    /// This server as clients reach it (`https://calzone.example.com`): the
    /// base of sign-in QR codes and feed links. Default: the request's host.
    pub public_url: Option<String>,
    /// `CALZONE_BEHIND_PROXY`: the right-most `X-Forwarded-For` entry is the
    /// client (what Traefik saw). Declared, never guessed: off, the header is
    /// ignored, because anyone can send one. Used for login throttling.
    pub behind_proxy: bool,
    /// `CALZONE_SECRET_KEY`: 32 bytes, base64. Encrypts two-factor secrets at
    /// rest. Required to serve; losing it means everyone sets two-factor up
    /// again (an admin resets them).
    pub secret_key: Option<[u8; 32]>,
    /// `CALZONE_TWO_FACTOR`: who must use two-factor sign-in.
    pub two_factor: TwoFactorPolicy,
    /// `CALZONE_DEFAULT_TIMEZONE`: for people who have not set theirs yet
    /// (the calendar page sends the browser's). Default UTC.
    pub default_timezone: chrono_tz::Tz,
}

/// Who must sign in with a second factor (decision 32). Anyone may turn it
/// on; these are the people who cannot use the web app without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TwoFactorPolicy {
    /// Nobody is required to (people may still turn it on).
    Optional,
    /// Admins must (the default): they can make accounts and passwords.
    Admins,
    /// Everyone must.
    Everyone,
}

impl TwoFactorPolicy {
    pub fn parse(v: Option<&str>) -> Result<Self> {
        match v.map(str::to_ascii_lowercase).as_deref() {
            None | Some("admins") => Ok(Self::Admins),
            Some("everyone") => Ok(Self::Everyone),
            Some("optional") => Ok(Self::Optional),
            Some(other) => bail!("CALZONE_TWO_FACTOR must be admins, everyone or optional, not {other:?}"),
        }
    }

    pub fn required_for(self, is_admin: bool) -> bool {
        match self {
            Self::Optional => false,
            Self::Admins => is_admin,
            Self::Everyone => true,
        }
    }
}

pub fn secret_key(v: Option<&str>) -> Result<Option<[u8; 32]>> {
    use base64::Engine as _;
    let Some(v) = v else { return Ok(None) };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(v)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(v))
        .map_err(|_| {
            anyhow::anyhow!("CALZONE_SECRET_KEY is not base64 (make one with: head -c 32 /dev/urandom | base64)")
        })?;
    let key: [u8; 32] = bytes.try_into().map_err(|_| {
        anyhow::anyhow!("CALZONE_SECRET_KEY must be 32 bytes (make one with: head -c 32 /dev/urandom | base64)")
    })?;
    Ok(Some(key))
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: var("DATABASE_URL").ok_or_else(|| anyhow::anyhow!("DATABASE_URL is required"))?,
            bind: var("CALZONE_BIND").unwrap_or_else(|| "0.0.0.0:8080".into()),
            dav: switch("CALZONE_DAV", var("CALZONE_DAV").as_deref(), true)?,
            cookie_secure: switch("CALZONE_COOKIE_SECURE", var("CALZONE_COOKIE_SECURE").as_deref(), true)?,
            public_url: public_url(var("CALZONE_PUBLIC_URL").as_deref())?,
            behind_proxy: switch("CALZONE_BEHIND_PROXY", var("CALZONE_BEHIND_PROXY").as_deref(), false)?,
            secret_key: secret_key(var("CALZONE_SECRET_KEY").as_deref())?,
            two_factor: TwoFactorPolicy::parse(var("CALZONE_TWO_FACTOR").as_deref())?,
            default_timezone: match var("CALZONE_DEFAULT_TIMEZONE") {
                None => chrono_tz::UTC,
                Some(v) => {
                    v.parse().map_err(|_| anyhow::anyhow!("CALZONE_DEFAULT_TIMEZONE {v:?} is not an IANA zone"))?
                }
            },
        })
    }
}

/// `on`/`off` style switches. Anything unrecognised is an error, not a
/// guess: a typo must not silently turn a door on or off.
pub fn switch(name: &str, v: Option<&str>, default: bool) -> Result<bool> {
    match v.map(str::to_ascii_lowercase).as_deref() {
        None => Ok(default),
        Some("on" | "true" | "1" | "yes") => Ok(true),
        Some("off" | "false" | "0" | "no") => Ok(false),
        Some(other) => bail!("{name} must be on or off, not {other:?}"),
    }
}

pub fn public_url(v: Option<&str>) -> Result<Option<String>> {
    let Some(v) = v else { return Ok(None) };
    if !(v.starts_with("https://") || v.starts_with("http://")) {
        bail!("CALZONE_PUBLIC_URL must start with https:// (or http:// on a LAN)");
    }
    Ok(Some(v.trim_end_matches('/').to_string()))
}

/// Trimmed; empty means unset.
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_are_explicit() {
        assert!(switch("X", None, true).unwrap());
        assert!(!switch("X", None, false).unwrap());
        assert!(switch("X", Some("ON"), false).unwrap());
        assert!(!switch("X", Some("off"), true).unwrap());
        assert!(switch("X", Some("of"), true).is_err());
    }

    #[test]
    fn public_url_is_a_base_url() {
        assert_eq!(public_url(None).unwrap(), None);
        assert_eq!(public_url(Some("https://c.example.com/")).unwrap().as_deref(), Some("https://c.example.com"));
        assert!(public_url(Some("c.example.com")).is_err());
    }

    #[test]
    fn two_factor_settings() {
        assert_eq!(TwoFactorPolicy::parse(None).unwrap(), TwoFactorPolicy::Admins);
        assert_eq!(TwoFactorPolicy::parse(Some("Everyone")).unwrap(), TwoFactorPolicy::Everyone);
        assert!(TwoFactorPolicy::parse(Some("all")).is_err());
        assert!(TwoFactorPolicy::Admins.required_for(true) && !TwoFactorPolicy::Admins.required_for(false));
        assert!(secret_key(Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")).unwrap().is_some());
        assert!(secret_key(Some("c2hvcnQ=")).is_err());
        assert!(secret_key(None).unwrap().is_none());
    }
}
