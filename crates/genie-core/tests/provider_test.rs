//! Runtime `Provider` seam (issue #567): the local model wrapped as the default
//! provider delegates completions unchanged and is usable behind `&dyn Provider`.

use genie_core::llm::{LlmClient, LocalProvider, Message, Provider};

fn user(text: &str) -> Message {
    Message {
        role: "user".into(),
        content: text.into(),
    }
}

#[tokio::test]
async fn local_provider_delegates_completion_to_the_wrapped_client() {
    let llm = LlmClient::mock(["scripted reply"]);
    let provider = LocalProvider::new(&llm);

    let out = provider
        .complete(&[user("hi")], Some(64), None)
        .await
        .unwrap();

    assert_eq!(out, "scripted reply");
    assert_eq!(provider.provider_name(), "mock");
}

#[tokio::test]
async fn local_provider_is_usable_as_a_trait_object() {
    let llm = LlmClient::mock(["dyn reply"]);
    let local = LocalProvider::new(&llm);
    let provider: &dyn Provider = &local;

    let out = provider.complete(&[user("hi")], None, None).await.unwrap();

    assert_eq!(out, "dyn reply");
}
