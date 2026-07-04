//! Privacy audit for cloud escalation (issue #570).
//!
//! When a chat turn escalates off-device through the on-device PrivacyProxy gateway,
//! this records a content-free audit of exactly what left the box — a redacted
//! endpoint, message/character counts, and the masking posture — so the local-first
//! guarantee is auditable. The record deliberately holds NO prompt content, so the
//! audit trail itself can never become the leak it exists to catch.

use serde::Serialize;

use crate::llm::Message;

/// A privacy-safe record of one cloud-escalation event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EscalationAudit {
    /// Backend that received the data (e.g. `"privacy_proxy"`).
    pub provider: String,
    /// Endpoint reached, redacted to `host:port` — no scheme, path, query, or credentials.
    pub endpoint: String,
    /// Whether identifier masking is applied before the data leaves (PrivacyProxy always does).
    pub anonymized: bool,
    /// Number of chat turns forwarded off-device.
    pub messages: usize,
    /// Total characters of prompt content that left the device.
    pub outgoing_chars: usize,
    /// Household terms seeded to the proxy so it can mask them.
    pub seeded_terms: usize,
}

impl EscalationAudit {
    /// Build the audit for an escalation of `messages` to `provider` at `base_url`, having
    /// seeded `seeded_terms` household terms for masking. `anonymized` is true when the
    /// transport masks identifiers before forwarding (always so for PrivacyProxy).
    pub fn record(
        provider: &str,
        base_url: &str,
        anonymized: bool,
        messages: &[Message],
        seeded_terms: usize,
    ) -> Self {
        Self {
            provider: provider.to_string(),
            endpoint: redact_endpoint(base_url),
            anonymized,
            messages: messages.len(),
            outgoing_chars: messages.iter().map(|m| m.content.chars().count()).sum(),
            seeded_terms,
        }
    }

    /// Emit the audit to the tracing log as a structured, content-free line.
    pub fn emit(&self) {
        tracing::info!(
            provider = %self.provider,
            endpoint = %self.endpoint,
            anonymized = self.anonymized,
            messages = self.messages,
            outgoing_chars = self.outgoing_chars,
            seeded_terms = self.seeded_terms,
            "ESCALATION: forwarding a chat turn off-device via anonymizing proxy"
        );
    }
}

/// Redact a provider base URL to a bare `host:port` for the audit trail.
///
/// Strips the scheme, any embedded `user:pass@` credentials, and the path/query/fragment,
/// so a misconfigured URL that carries a token can never leak through the audit log.
/// Returns `"unknown"` when no host can be parsed.
pub fn redact_endpoint(base_url: &str) -> String {
    let after_scheme = base_url
        .trim()
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| base_url.trim());
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, hp)| hp)
        .unwrap_or(authority)
        .trim();
    if host_port.is_empty() {
        "unknown".to_string()
    } else {
        host_port.to_string()
    }
}
