//! Trusted resolution of the inbound HTTP request origin (issue #232).
//!
//! The `X-Genie-Origin` header decides per-origin tool ACLs, actuation ACLs,
//! rate limits, audit attribution, and NLU confidence thresholds — but it is
//! client-supplied, so it cannot, on its own, be trusted as a security
//! principal. [`OriginResolver`] turns the *claimed* origin into the *trusted*
//! origin by checking what the request actually proves:
//!
//!   * The untrusted `api` baseline (also the value for a missing or
//!     unrecognized header) needs no proof and is never elevated.
//!   * A loopback peer is inside the documented single-host trust boundary
//!     (`doc/household-security.md`); its header is honored unless strict mode
//!     ([`OriginAuthConfig::require_token`]) is on or a token is configured for
//!     that origin.
//!   * An `X-Genie-Origin-Token` matching the secret configured for the
//!     claimed origin authenticates the claim from any peer.
//!
//! Anything else is downgraded to [`RequestOrigin::Api`], the least-privileged
//! origin, and the rejected claim is logged.

use std::collections::HashMap;
use std::net::IpAddr;

use genie_common::config::OriginAuthConfig;

use crate::tools::RequestOrigin;

/// Resolves the trusted [`RequestOrigin`] for an inbound HTTP request.
#[derive(Debug, Clone, Default)]
pub struct OriginResolver {
    tokens: HashMap<RequestOrigin, String>,
    require_token: bool,
}

impl OriginResolver {
    /// Build a resolver from the `[core.origin_auth]` config, resolving tokens
    /// from config values and `GENIE_ORIGIN_TOKEN_<ORIGIN>` env vars.
    pub fn from_config(cfg: &OriginAuthConfig) -> Self {
        let mut tokens = HashMap::new();
        for (name, secret) in cfg.resolved_tokens() {
            let origin = RequestOrigin::from_header(&name);
            // Only real, claimable channels can hold a token. `unknown`,
            // `api`, and `confirmation` are never assumed from the wire, so a
            // token for them would be dead config; flag it rather than create
            // a false sense of protection.
            if matches!(
                origin,
                RequestOrigin::Unknown | RequestOrigin::Api | RequestOrigin::Confirmation
            ) {
                tracing::warn!(
                    origin = %name,
                    "ignoring origin_auth token for an origin that is never assumed from the wire"
                );
                continue;
            }
            tokens.insert(origin, secret);
        }
        Self {
            tokens,
            require_token: cfg.require_token,
        }
    }

    /// Register a token minted in-process for a first-party adapter (e.g. the
    /// Telegram channel's loopback credential). Overrides any configured value;
    /// a blank token is ignored.
    pub fn insert_token(&mut self, origin: RequestOrigin, token: impl Into<String>) {
        let token = token.into();
        if !token.trim().is_empty() {
            self.tokens.insert(origin, token.trim().to_string());
        }
    }

    /// Resolve the trusted origin for a request from `peer`, claiming
    /// `claimed_header` and presenting `token_header`.
    ///
    /// `peer` is `None` only when the peer address could not be determined; it
    /// is then treated as untrusted (non-loopback).
    pub fn resolve(
        &self,
        peer: Option<IpAddr>,
        claimed_header: Option<&str>,
        token_header: Option<&str>,
    ) -> RequestOrigin {
        let claimed = claimed_header
            .map(RequestOrigin::from_header)
            .unwrap_or(RequestOrigin::Unknown);

        // Baseline: a missing/unknown header, an explicit `api`, or the
        // never-on-the-wire `confirmation` pseudo-origin all resolve to the
        // untrusted `api` floor. No header can sink below it and none needs
        // proof to reach it.
        let claimed = match claimed {
            RequestOrigin::Unknown | RequestOrigin::Api | RequestOrigin::Confirmation => {
                return RequestOrigin::Api;
            }
            other => other,
        };

        // A configured token is authoritative: once an operator (or adapter)
        // sets a secret for an origin, that origin must always present it —
        // even over loopback — so one local process cannot assume another's
        // channel just by being on the same host.
        if let Some(expected) = self.tokens.get(&claimed) {
            let presented = token_header.map(str::trim).unwrap_or("");
            if !presented.is_empty() && constant_time_eq(presented.as_bytes(), expected.as_bytes())
            {
                return claimed;
            }
            tracing::warn!(
                origin = %claimed.as_policy_key(),
                "rejected origin claim: missing or invalid X-Genie-Origin-Token; downgraded to api"
            );
            return RequestOrigin::Api;
        }

        // No token configured for this origin. Honor the header only from a
        // loopback peer (the single-host trust boundary) and only when strict
        // mode is off.
        let loopback = peer.map(ip_is_loopback).unwrap_or(false);
        if loopback && !self.require_token {
            return claimed;
        }

        tracing::warn!(
            origin = %claimed.as_policy_key(),
            loopback,
            require_token = self.require_token,
            "rejected unauthenticated origin claim; downgraded to api"
        );
        RequestOrigin::Api
    }
}

/// True for IPv4/IPv6 loopback, including IPv4-mapped IPv6 loopback
/// (`::ffff:127.0.0.1`), which a dual-stack listener may report.
fn ip_is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6
                    .to_ipv4_mapped()
                    .map(|mapped| mapped.is_loopback())
                    .unwrap_or(false)
        }
    }
}

/// Length-checked, content-constant-time byte comparison. Avoids leaking how
/// many leading bytes of a guessed token are correct via response timing. The
/// length itself is not treated as secret (standard for bearer tokens).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn loopback_v4() -> Option<IpAddr> {
        Some(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    fn lan_v4() -> Option<IpAddr> {
        Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)))
    }

    fn cfg(require_token: bool, tokens: &[(&str, &str)]) -> OriginAuthConfig {
        OriginAuthConfig {
            require_token,
            tokens: tokens
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn missing_header_is_api_floor() {
        let r = OriginResolver::default();
        assert_eq!(r.resolve(loopback_v4(), None, None), RequestOrigin::Api);
        assert_eq!(r.resolve(lan_v4(), None, None), RequestOrigin::Api);
    }

    #[test]
    fn unknown_or_confirmation_header_collapses_to_api() {
        let r = OriginResolver::default();
        assert_eq!(
            r.resolve(loopback_v4(), Some("bogus"), None),
            RequestOrigin::Api
        );
        // `confirmation` is an internal pseudo-origin, never claimable.
        assert_eq!(
            r.resolve(loopback_v4(), Some("confirmation"), None),
            RequestOrigin::Api
        );
    }

    #[test]
    fn loopback_honors_unauthenticated_privileged_header_by_default() {
        let r = OriginResolver::default();
        assert_eq!(
            r.resolve(loopback_v4(), Some("dashboard"), None),
            RequestOrigin::Dashboard
        );
        assert_eq!(
            r.resolve(loopback_v4(), Some("voice"), None),
            RequestOrigin::Voice
        );
    }

    #[test]
    fn non_loopback_cannot_forge_privileged_origin_without_token() {
        let r = OriginResolver::default();
        // The core fix: a LAN peer claiming `voice` is downgraded.
        assert_eq!(r.resolve(lan_v4(), Some("voice"), None), RequestOrigin::Api);
        assert_eq!(
            r.resolve(lan_v4(), Some("dashboard"), None),
            RequestOrigin::Api
        );
    }

    #[test]
    fn unknown_peer_is_treated_as_untrusted() {
        let r = OriginResolver::default();
        assert_eq!(r.resolve(None, Some("voice"), None), RequestOrigin::Api);
    }

    #[test]
    fn matching_token_authenticates_from_any_peer() {
        let r = OriginResolver::from_config(&cfg(false, &[("telegram", "s3cret")]));
        assert_eq!(
            r.resolve(lan_v4(), Some("telegram"), Some("s3cret")),
            RequestOrigin::Telegram
        );
        assert_eq!(
            r.resolve(loopback_v4(), Some("telegram"), Some("s3cret")),
            RequestOrigin::Telegram
        );
    }

    #[test]
    fn wrong_or_missing_token_downgrades_even_on_loopback() {
        // Configuring a token makes it mandatory for that origin everywhere —
        // loopback trust no longer applies to it.
        let r = OriginResolver::from_config(&cfg(false, &[("telegram", "s3cret")]));
        assert_eq!(
            r.resolve(loopback_v4(), Some("telegram"), Some("wrong")),
            RequestOrigin::Api
        );
        assert_eq!(
            r.resolve(loopback_v4(), Some("telegram"), None),
            RequestOrigin::Api
        );
        // A different, un-tokened origin still gets loopback trust.
        assert_eq!(
            r.resolve(loopback_v4(), Some("dashboard"), None),
            RequestOrigin::Dashboard
        );
    }

    #[test]
    fn require_token_strips_loopback_trust() {
        let r = OriginResolver::from_config(&cfg(true, &[("dashboard", "ui-token")]));
        // Strict mode: no token, no privileged origin, even on loopback.
        assert_eq!(
            r.resolve(loopback_v4(), Some("voice"), None),
            RequestOrigin::Api
        );
        // …but a correct token still works.
        assert_eq!(
            r.resolve(loopback_v4(), Some("dashboard"), Some("ui-token")),
            RequestOrigin::Dashboard
        );
        // api floor is always reachable.
        assert_eq!(
            r.resolve(loopback_v4(), Some("api"), None),
            RequestOrigin::Api
        );
    }

    #[test]
    fn insert_token_overrides_and_enforces() {
        let mut r = OriginResolver::default();
        r.insert_token(RequestOrigin::Telegram, "minted");
        assert_eq!(
            r.resolve(loopback_v4(), Some("telegram"), Some("minted")),
            RequestOrigin::Telegram
        );
        // Now that telegram has a token, the bare header no longer suffices.
        assert_eq!(
            r.resolve(loopback_v4(), Some("telegram"), None),
            RequestOrigin::Api
        );
        // Blank tokens are ignored (no accidental lockout/elevation).
        r.insert_token(RequestOrigin::Voice, "   ");
        assert_eq!(
            r.resolve(loopback_v4(), Some("voice"), None),
            RequestOrigin::Voice
        );
    }

    #[test]
    fn ipv4_mapped_ipv6_loopback_is_loopback() {
        let mapped = IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped());
        assert!(ip_is_loopback(mapped));
        assert!(ip_is_loopback(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!ip_is_loopback(IpAddr::V6(Ipv6Addr::new(
            0x2001, 0xdb8, 0, 0, 0, 0, 0, 1
        ))));
    }

    #[test]
    fn constant_time_eq_matches_semantics() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }
}
