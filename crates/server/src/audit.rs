//! One fixed-format log line per refused sign-in, for CrowdSec or fail2ban
//! (decision 34):
//!
//! ```text
//! 2026-11-06T10:00:00.000000Z  WARN calzone::auth: auth_failure service=web reason=wrong ip=203.0.113.7
//! ```
//!
//! `service` is `web` (passphrase), `code` (two-factor) or `dav` (device
//! password); `reason` is `wrong` or `throttled` (refused unchecked because
//! the address or account is over its budget). The address is the one
//! login throttling uses (`CALZONE_BEHIND_PROXY`). Nothing else goes in:
//! never the name that was tried (people type passphrases into the name
//! field), never a credential. `deploy/crowdsec/` parses exactly this.

use std::net::IpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Web,
    Code,
    Dav,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Wrong,
    Throttled,
}

/// The message text (also what the tests and the CrowdSec parser pin).
pub fn line(service: Service, reason: Reason, ip: Option<IpAddr>) -> String {
    let service = match service {
        Service::Web => "web",
        Service::Code => "code",
        Service::Dav => "dav",
    };
    let reason = match reason {
        Reason::Wrong => "wrong",
        Reason::Throttled => "throttled",
    };
    let ip = ip.map(|i| i.to_string()).unwrap_or_else(|| "unknown".into());
    format!("auth_failure service={service} reason={reason} ip={ip}")
}

pub fn auth_failure(service: Service, reason: Reason, ip: Option<IpAddr>) {
    tracing::warn!(target: "calzone::auth", "{}", line(service, reason, ip));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_is_fixed() {
        assert_eq!(
            line(Service::Web, Reason::Wrong, "203.0.113.7".parse().ok()),
            "auth_failure service=web reason=wrong ip=203.0.113.7"
        );
        assert_eq!(
            line(Service::Dav, Reason::Throttled, "2001:db8::1".parse().ok()),
            "auth_failure service=dav reason=throttled ip=2001:db8::1"
        );
        assert_eq!(line(Service::Code, Reason::Wrong, None), "auth_failure service=code reason=wrong ip=unknown");
    }
}
