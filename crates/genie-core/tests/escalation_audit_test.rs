use genie_core::Message;
use genie_core::llm::escalation_audit::{EscalationAudit, redact_endpoint};

fn msg(role: &str, content: &str) -> Message {
    Message {
        role: role.into(),
        content: content.into(),
    }
}

#[test]
fn redact_endpoint_strips_scheme_path_and_credentials() {
    assert_eq!(
        redact_endpoint("http://127.0.0.1:8180/v1"),
        "127.0.0.1:8180"
    );
    assert_eq!(redact_endpoint("http://localhost/v1/chat"), "localhost");
    // embedded credentials and query string must never survive into the audit trail
    assert_eq!(
        redact_endpoint("https://user:s3cret@proxy.local:9090/v1/chat?key=tok"),
        "proxy.local:9090"
    );
    // no scheme, and empty input
    assert_eq!(redact_endpoint("127.0.0.1:8180"), "127.0.0.1:8180");
    assert_eq!(redact_endpoint("  "), "unknown");
}

#[test]
fn record_counts_what_left_and_redacts_endpoint() {
    let messages = [msg("system", "you are local"), msg("user", "héllo")];
    let audit = EscalationAudit::record(
        "privacy_proxy",
        "http://user:pw@127.0.0.1:8180/v1",
        true,
        &messages,
        3,
    );

    assert_eq!(audit.provider, "privacy_proxy");
    assert_eq!(audit.endpoint, "127.0.0.1:8180"); // credentials + path stripped
    assert!(audit.anonymized);
    assert_eq!(audit.messages, 2);
    assert_eq!(
        audit.outgoing_chars,
        "you are local".chars().count() + "héllo".chars().count()
    );
    assert_eq!(audit.seeded_terms, 3);
}

#[test]
fn audit_record_holds_no_prompt_content() {
    let messages = [msg("user", "my wifi password is hunter2")];
    let audit = EscalationAudit::record(
        "privacy_proxy",
        "http://root:token@10.0.0.5:8180/v1",
        true,
        &messages,
        1,
    );

    // The audit is a metadata-only record: neither prompt content nor the endpoint
    // credentials appear anywhere in its serialized form, so the trail can't leak.
    let json = serde_json::to_string(&audit).expect("serializable");
    assert!(!json.contains("hunter2"));
    assert!(!json.contains("password"));
    assert!(!json.contains("token"));
    assert!(!json.contains("root"));
    assert_eq!(
        audit.outgoing_chars,
        "my wifi password is hunter2".chars().count()
    );
}
