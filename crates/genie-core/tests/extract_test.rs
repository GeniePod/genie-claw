use genie_core::Memory;
use genie_core::memory::extract::{ExtractedFact, extract_and_store, extract_facts};

fn facts_key(facts: &[ExtractedFact]) -> String {
    facts
        .iter()
        .map(|f| format!("{}:{}", f.category, f.content))
        .collect::<Vec<_>>()
        .join("|")
}

/// Fixed corpus captured from `main` @ 72b0b28 — guards byte-identical output after
/// identity/preference early-outs and favorite/remember allocation trims (#495).
#[test]
fn extract_facts_corpus_regression() {
    const CORPUS: &[(&str, &str)] = &[
        ("My name is Jared", "identity:User's name is Jared"),
        ("Call me Alex", "identity:User's name is Alex"),
        ("I'm 25 years old", "identity:User is 25 years old"),
        ("I work at TrioSpace", "identity:User works at triospace"),
        (
            "I'm a software engineer",
            "identity:User is a software engineer",
        ),
        ("I live in Denver", "identity:User lives in denver"),
        ("I love spicy food", "preference:User likes spicy food"),
        (
            "I hate cold weather",
            "preference:User dislikes cold weather",
        ),
        (
            "My favorite color is blue",
            "preference:User's favorite color is blue",
        ),
        (
            "My favourite food is pizza",
            "preference:User's favorite food is pizza",
        ),
        (
            "My dog is named Rex",
            "relationship:User's dog is named Rex",
        ),
        (
            "My name is Jared and I love coding",
            "identity:User's name is Jared|preference:User likes coding",
        ),
        ("What time is it?", ""),
        ("Can you help me?", ""),
        (
            "Remember that I have a meeting tomorrow",
            "fact:I have a meeting tomorrow",
        ),
        ("Remember I need to buy milk", "fact:I need to buy milk"),
        ("I'm a bit tired", ""),
        (
            "I live in Denver and I work downtown",
            "identity:User lives in denver",
        ),
        (
            "I work at Google with my friend Bob",
            "identity:User works at google",
        ),
        (
            "I'm a software engineer but I hate meetings",
            "identity:User is a software engineer|preference:User dislikes meetings",
        ),
        (
            "I love hiking when the weather is nice",
            "preference:User likes hiking",
        ),
        (
            "My favorite food is pizza and pasta",
            "preference:User's favorite food is pizza",
        ),
        (
            "I work at Android Labs",
            "identity:User works at android labs",
        ),
        ("the weather today is nice and i went for a walk", ""),
        ("my plan today is to relax and read a good book", ""),
        ("REMEMBER my pin is 1234", "fact:my pin is 1234"),
        ("Remember that", ""),
    ];

    for (input, expected) in CORPUS {
        assert_eq!(
            facts_key(&extract_facts(input)),
            *expected,
            "corpus mismatch for {input:?}"
        );
    }
}

#[test]
fn extract_name() {
    let facts = extract_facts("My name is Jared");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].category, "identity");
    assert!(facts[0].content.contains("Jared"));
}

#[test]
fn extract_name_call_me() {
    let facts = extract_facts("Call me Alex");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].category, "identity");
    assert!(facts[0].content.contains("Alex"));
}

#[test]
fn extract_age() {
    let facts = extract_facts("I'm 25 years old");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].category, "identity");
    assert!(facts[0].content.contains("25"));
}

#[test]
fn extract_job() {
    let facts = extract_facts("I work at TrioSpace");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].category, "identity");
    assert!(facts[0].content.to_lowercase().contains("triospace"));
}

#[test]
fn extract_occupation() {
    let facts = extract_facts("I'm a software engineer");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].category, "identity");
    assert!(facts[0].content.contains("software engineer"));
}

#[test]
fn extract_location() {
    let facts = extract_facts("I live in Denver");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].category, "identity");
    assert!(facts[0].content.to_lowercase().contains("denver"));
}

#[test]
fn extract_preference_like() {
    let facts = extract_facts("I love spicy food");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].category, "preference");
    assert!(facts[0].content.contains("spicy food"));
}

#[test]
fn extract_preference_dislike() {
    let facts = extract_facts("I hate cold weather");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].category, "preference");
    assert!(facts[0].content.contains("cold weather"));
}

#[test]
fn extract_favorite() {
    let facts = extract_facts("My favorite color is blue");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].category, "preference");
    assert!(facts[0].content.contains("blue"));
}

#[test]
fn extract_relationship() {
    let facts = extract_facts("My dog is named Rex");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].category, "relationship");
    assert!(facts[0].content.contains("Rex"));
}

#[test]
fn extract_multiple_facts() {
    let facts = extract_facts("My name is Jared and I love coding");
    assert!(facts.len() >= 2);
    assert!(facts.iter().any(|f| f.category == "identity"));
    assert!(facts.iter().any(|f| f.category == "preference"));
}

#[test]
fn extract_nothing() {
    let facts = extract_facts("What time is it?");
    assert!(facts.is_empty());
}

#[test]
fn extract_nothing_from_question() {
    let facts = extract_facts("Can you help me?");
    assert!(facts.is_empty());
}

#[test]
fn explicit_remember() {
    let facts = extract_facts("Remember that I have a meeting tomorrow");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].category, "fact");
    assert!(facts[0].content.contains("meeting tomorrow"));
}

#[test]
fn remember_that_stripped() {
    let facts = extract_facts("Remember I need to buy milk");
    assert_eq!(facts.len(), 1);
    assert!(facts[0].content.contains("buy milk"));
}

#[test]
fn no_false_positive_im_a() {
    let facts = extract_facts("I'm a bit tired");
    assert!(facts.iter().all(|f| f.category != "identity"));
}

#[test]
fn name_stops_at_conjunction() {
    let facts = extract_facts("My name is Jared and I love coding");
    let name = facts
        .iter()
        .find(|f| f.category == "identity")
        .expect("name fact");
    assert_eq!(name.content, "User's name is Jared");
    assert!(!name.content.to_lowercase().contains("coding"));
}

#[test]
fn location_stops_at_conjunction() {
    let facts = extract_facts("I live in Denver and I work downtown");
    let loc = facts
        .iter()
        .find(|f| f.content.starts_with("User lives in"))
        .expect("location fact");
    assert_eq!(loc.content, "User lives in denver");
}

#[test]
fn workplace_stops_at_subordinate_clause() {
    let facts = extract_facts("I work at Google with my friend Bob");
    let job = facts
        .iter()
        .find(|f| f.content.starts_with("User works at"))
        .expect("workplace fact");
    assert_eq!(job.content, "User works at google");
}

#[test]
fn occupation_stops_at_contrast_clause() {
    let facts = extract_facts("I'm a software engineer but I hate meetings");
    let job = facts
        .iter()
        .find(|f| f.content.starts_with("User is a"))
        .expect("occupation fact");
    assert_eq!(job.content, "User is a software engineer");
}

#[test]
fn preference_stops_at_relative_clause() {
    let facts = extract_facts("I love hiking when the weather is nice");
    let pref = facts
        .iter()
        .find(|f| f.category == "preference")
        .expect("preference fact");
    assert_eq!(pref.content, "User likes hiking");
}

#[test]
fn favorite_stops_at_conjunction() {
    let facts = extract_facts("My favorite food is pizza and pasta");
    let fav = facts
        .iter()
        .find(|f| f.category == "preference")
        .expect("favorite fact");
    assert_eq!(fav.content, "User's favorite food is pizza");
}

#[test]
fn android_is_not_split_on_and() {
    let facts = extract_facts("I work at Android Labs");
    let job = facts
        .iter()
        .find(|f| f.content.starts_with("User works at"))
        .expect("workplace fact");
    assert_eq!(job.content, "User works at android labs");
}

#[test]
fn auto_store_rejects_password_memory() {
    let path = std::env::temp_dir().join(format!(
        "geniepod-extract-policy-test-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let memory = Memory::open(&path).unwrap();

    let stored = extract_and_store(&memory, "Remember that my password is swordfish");

    assert_eq!(stored, 0);
    assert!(memory.search("password", 5).unwrap().is_empty());
}
