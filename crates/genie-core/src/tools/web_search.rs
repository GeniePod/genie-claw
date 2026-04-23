use anyhow::Result;
use genie_common::config::{WebSearchConfig, WebSearchProvider};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use std::time::Instant;

const DUCKDUCKGO_INSTANT_ANSWER_URL: &str = "https://api.duckduckgo.com/";
const MAX_RESULTS: usize = 5;

static SEARCH_CACHE: OnceLock<Mutex<SearchCache>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchItem {
    title: Option<String>,
    text: String,
    url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    provider: WebSearchProvider,
    base_url: String,
    query: String,
    limit: usize,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    output: String,
    stored_at: Instant,
}

#[derive(Debug, Default)]
struct SearchCache {
    entries: HashMap<CacheKey, CacheEntry>,
}

pub async fn search_with_config(
    query: &str,
    requested_limit: usize,
    config: &WebSearchConfig,
) -> Result<String> {
    search_with_options(query, requested_limit, config, false).await
}

pub async fn search_with_options(
    query: &str,
    requested_limit: usize,
    config: &WebSearchConfig,
    fresh: bool,
) -> Result<String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok("Please specify what to search for.".to_string());
    }

    if !config.enabled {
        return Ok("Web search is disabled in GeniePod config.".to_string());
    }

    if should_block_private_query(query) {
        return Ok(
            "I will not send private secrets, tokens, passwords, or local credentials to web search."
                .to_string(),
        );
    }

    let limit = requested_limit
        .min(config.max_results.max(1))
        .clamp(1, MAX_RESULTS);
    let cache_key = cache_key(query, limit, config);
    if config.cache_enabled
        && !fresh
        && let Some(output) = cache_lookup(&cache_key, config.cache_ttl_secs)
    {
        return Ok(output);
    }

    let timeout_secs = config.timeout_secs.max(1);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent("GeniePod/1.0 local web search")
        .build()?;

    let output = match config.provider {
        WebSearchProvider::Duckduckgo => search_duckduckgo(&client, query, limit).await,
        WebSearchProvider::Searxng => search_searxng(&client, query, limit, config).await,
    }?;

    if config.cache_enabled {
        cache_store(cache_key, output.clone(), config.cache_max_entries);
    }

    Ok(output)
}

pub async fn search(query: &str, limit: usize) -> Result<String> {
    search_with_config(query, limit, &WebSearchConfig::default()).await
}

pub(crate) fn cache_size() -> usize {
    let Some(cache) = SEARCH_CACHE.get() else {
        return 0;
    };
    cache
        .lock()
        .map(|cache| cache.entries.len())
        .unwrap_or_default()
}

async fn search_duckduckgo(client: &reqwest::Client, query: &str, limit: usize) -> Result<String> {
    let body = client
        .get(DUCKDUCKGO_INSTANT_ANSWER_URL)
        .query(&[
            ("q", query),
            ("format", "json"),
            ("no_html", "1"),
            ("no_redirect", "1"),
            ("skip_disambig", "1"),
        ])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    format_results(query, &body, limit)
}

async fn search_searxng(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
    config: &WebSearchConfig,
) -> Result<String> {
    let base_url = searxng_base_url(config).ok_or_else(|| {
        anyhow::anyhow!(
            "SearXNG web search requires web_search.base_url or GENIEPOD_WEB_SEARCH_BASE_URL"
        )
    })?;
    if !config.allow_remote_base_url && !is_local_base_url(&base_url) {
        anyhow::bail!(
            "SearXNG web search base URL must be local unless web_search.allow_remote_base_url is true"
        );
    }
    let search_url = searxng_search_url(&base_url);

    let body = client
        .get(search_url)
        .query(&[
            ("q", query),
            ("format", "json"),
            ("safesearch", "1"),
            ("language", "auto"),
        ])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    format_searxng_results(query, &body, limit)
}

fn searxng_base_url(config: &WebSearchConfig) -> Option<String> {
    let from_env = std::env::var("GENIEPOD_WEB_SEARCH_BASE_URL").unwrap_or_default();
    let base = if from_env.trim().is_empty() {
        config.base_url.trim()
    } else {
        from_env.trim()
    };

    if base.is_empty() {
        None
    } else {
        Some(base.to_string())
    }
}

fn cache_key(query: &str, limit: usize, config: &WebSearchConfig) -> CacheKey {
    CacheKey {
        provider: config.provider,
        base_url: match config.provider {
            WebSearchProvider::Duckduckgo => String::new(),
            WebSearchProvider::Searxng => searxng_base_url(config).unwrap_or_default(),
        },
        query: normalize_cache_query(query),
        limit,
    }
}

fn normalize_cache_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn should_block_private_query(query: &str) -> bool {
    let normalized = normalize_cache_query(query);
    contains_any(
        &normalized,
        &[
            "my password",
            "our password",
            "wifi password",
            "wi fi password",
            "my api key",
            "our api key",
            "my token",
            "our token",
            "home assistant token",
            "telegram bot token",
            "private key",
            "secret key",
            "recovery code",
            "one time code",
            "2fa code",
        ],
    )
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn cache_lookup(key: &CacheKey, ttl_secs: u64) -> Option<String> {
    let ttl = Duration::from_secs(ttl_secs.max(1));
    let cache = SEARCH_CACHE.get_or_init(|| Mutex::new(SearchCache::default()));
    let mut cache = cache.lock().ok()?;
    let entry = cache.entries.get(key)?;
    if entry.stored_at.elapsed() > ttl {
        cache.entries.remove(key);
        return None;
    }
    Some(entry.output.clone())
}

fn cache_store(key: CacheKey, output: String, max_entries: usize) {
    let max_entries = max_entries.max(1);
    let cache = SEARCH_CACHE.get_or_init(|| Mutex::new(SearchCache::default()));
    let Ok(mut cache) = cache.lock() else {
        return;
    };

    if cache.entries.len() >= max_entries
        && !cache.entries.contains_key(&key)
        && let Some(oldest_key) = cache
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.stored_at)
            .map(|(key, _)| key.clone())
    {
        cache.entries.remove(&oldest_key);
    }

    cache.entries.insert(
        key,
        CacheEntry {
            output,
            stored_at: Instant::now(),
        },
    );
}

fn searxng_search_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/search") {
        base.to_string()
    } else {
        format!("{base}/search")
    }
}

fn is_local_base_url(base_url: &str) -> bool {
    let lower = base_url.trim().to_lowercase();
    lower.starts_with("http://127.")
        || lower.starts_with("http://localhost")
        || lower.starts_with("http://[::1]")
        || lower.starts_with("https://127.")
        || lower.starts_with("https://localhost")
        || lower.starts_with("https://[::1]")
}

pub(crate) fn format_results(query: &str, body: &str, limit: usize) -> Result<String> {
    let value: Value = serde_json::from_str(body)?;
    let mut items = Vec::new();

    collect_answer(&value, &mut items);
    collect_abstract(&value, &mut items);
    collect_result_array(value.get("Results"), &mut items);
    collect_related_topics(value.get("RelatedTopics"), &mut items);

    format_items(query, items, limit)
}

pub(crate) fn format_searxng_results(query: &str, body: &str, limit: usize) -> Result<String> {
    let value: Value = serde_json::from_str(body)?;
    let mut items = Vec::new();

    if let Some(answers) = value.get("answers").and_then(Value::as_array) {
        for answer in answers {
            let Some(text) = answer
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            else {
                continue;
            };
            items.push(SearchItem {
                title: Some("Answer".into()),
                text: text.to_string(),
                url: None,
            });
        }
    }

    if let Some(results) = value.get("results").and_then(Value::as_array) {
        for result in results {
            let text = get_str(result, "content")
                .or_else(|| get_str(result, "title"))
                .unwrap_or("");
            if text.is_empty() {
                continue;
            }

            items.push(SearchItem {
                title: get_str(result, "title")
                    .filter(|title| !title.is_empty())
                    .map(str::to_string),
                text: text.to_string(),
                url: get_str(result, "url")
                    .filter(|url| !url.is_empty())
                    .map(str::to_string),
            });
        }
    }

    format_items(query, items, limit)
}

fn format_items(query: &str, items: Vec<SearchItem>, limit: usize) -> Result<String> {
    let mut deduped = Vec::new();
    for item in items {
        if item.text.trim().is_empty() {
            continue;
        }
        let duplicate = deduped.iter().any(|existing: &SearchItem| {
            existing.text.eq_ignore_ascii_case(&item.text) || existing.url == item.url
        });
        if !duplicate {
            deduped.push(item);
        }
    }

    if deduped.is_empty() {
        return Ok(format!(
            "No web search results found for \"{}\".",
            query.trim()
        ));
    }

    let limit = limit.clamp(1, MAX_RESULTS);
    let mut lines = vec![format!("Web search results for \"{}\":", query.trim())];
    for item in deduped.into_iter().take(limit) {
        let text = truncate(&clean_text(&item.text), 260);
        let line = match (item.title.as_deref(), item.url.as_deref()) {
            (Some(title), Some(url)) if !title.eq_ignore_ascii_case(&text) => {
                format!("- {}: {} ({})", clean_text(title), text, url)
            }
            (_, Some(url)) => format!("- {} ({})", text, url),
            (Some(title), None) if !title.eq_ignore_ascii_case(&text) => {
                format!("- {}: {}", clean_text(title), text)
            }
            _ => format!("- {}", text),
        };
        lines.push(line);
    }

    Ok(lines.join("\n"))
}

fn collect_answer(value: &Value, items: &mut Vec<SearchItem>) {
    let Some(answer) = get_str(value, "Answer") else {
        return;
    };
    if answer.is_empty() {
        return;
    }

    items.push(SearchItem {
        title: get_str(value, "AnswerType")
            .filter(|title| !title.is_empty())
            .map(str::to_string),
        text: answer.to_string(),
        url: None,
    });
}

fn collect_abstract(value: &Value, items: &mut Vec<SearchItem>) {
    let Some(text) = get_str(value, "AbstractText").or_else(|| get_str(value, "Abstract")) else {
        return;
    };
    if text.is_empty() {
        return;
    }

    items.push(SearchItem {
        title: get_str(value, "Heading")
            .filter(|heading| !heading.is_empty())
            .map(str::to_string),
        text: text.to_string(),
        url: get_str(value, "AbstractURL")
            .filter(|url| !url.is_empty())
            .map(str::to_string),
    });
}

fn collect_result_array(value: Option<&Value>, items: &mut Vec<SearchItem>) {
    let Some(results) = value.and_then(Value::as_array) else {
        return;
    };

    for result in results {
        collect_result_item(result, items);
    }
}

fn collect_related_topics(value: Option<&Value>, items: &mut Vec<SearchItem>) {
    let Some(topics) = value.and_then(Value::as_array) else {
        return;
    };

    for topic in topics {
        if let Some(children) = topic.get("Topics") {
            collect_related_topics(Some(children), items);
        } else {
            collect_result_item(topic, items);
        }
    }
}

fn collect_result_item(value: &Value, items: &mut Vec<SearchItem>) {
    let Some(text) = get_str(value, "Text") else {
        return;
    };
    if text.is_empty() {
        return;
    }

    items.push(SearchItem {
        title: title_from_text(text),
        text: text.to_string(),
        url: get_str(value, "FirstURL")
            .filter(|url| !url.is_empty())
            .map(str::to_string),
    });
}

fn title_from_text(text: &str) -> Option<String> {
    text.split_once(" - ")
        .map(|(title, _)| clean_text(title))
        .filter(|title| !title.is_empty())
}

fn get_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str).map(str::trim)
}

fn clean_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{}...", truncated.trim_end())
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_abstract_result() {
        let body = r#"{
            "Heading": "Home Assistant",
            "AbstractText": "Home Assistant is free and open-source software for home automation.",
            "AbstractURL": "https://www.home-assistant.io/",
            "RelatedTopics": []
        }"#;

        let output = format_results("home assistant", body, 3).unwrap();
        assert!(output.contains("Web search results"));
        assert!(output.contains("Home Assistant"));
        assert!(output.contains("https://www.home-assistant.io/"));
    }

    #[test]
    fn formats_nested_related_topics() {
        let body = r#"{
            "RelatedTopics": [
                {
                    "Name": "Group",
                    "Topics": [
                        {
                            "Text": "Matter - Matter is an open smart home connectivity standard.",
                            "FirstURL": "https://example.test/matter"
                        }
                    ]
                }
            ]
        }"#;

        let output = format_results("matter", body, 3).unwrap();
        assert!(output.contains("Matter"));
        assert!(output.contains("https://example.test/matter"));
    }

    #[test]
    fn formats_searxng_results() {
        let body = r#"{
            "results": [
                {
                    "title": "ESP32-C6",
                    "url": "https://example.test/esp32-c6",
                    "content": "ESP32-C6 supports Wi-Fi 6, Bluetooth LE, Zigbee, and Thread."
                }
            ],
            "answers": ["Matter can run over Thread for supported devices."]
        }"#;

        let output = format_searxng_results("esp32 c6 thread", body, 3).unwrap();
        assert!(output.contains("Matter can run over Thread"));
        assert!(output.contains("ESP32-C6 supports"));
        assert!(output.contains("https://example.test/esp32-c6"));
    }

    #[test]
    fn searxng_base_adds_search_path() {
        assert_eq!(
            searxng_search_url("http://127.0.0.1:8888"),
            "http://127.0.0.1:8888/search"
        );
        assert_eq!(
            searxng_search_url("http://127.0.0.1:8888/search"),
            "http://127.0.0.1:8888/search"
        );
    }

    #[test]
    fn local_base_url_detection_allows_loopback() {
        assert!(is_local_base_url("http://127.0.0.1:8888"));
        assert!(is_local_base_url("http://localhost:8888"));
        assert!(is_local_base_url("http://[::1]:8888"));
        assert!(!is_local_base_url("https://searx.example.com"));
    }

    #[test]
    fn cache_query_normalization_is_stable() {
        assert_eq!(
            normalize_cache_query("  ESP32-C6   Thread Support "),
            "esp32-c6 thread support"
        );
    }

    #[test]
    fn cache_lookup_respects_ttl() {
        let mut config = WebSearchConfig::default();
        config.cache_ttl_secs = 60;
        let key = cache_key("Matter news", 3, &config);
        cache_store(key.clone(), "cached output".into(), 8);

        assert_eq!(cache_lookup(&key, 60).as_deref(), Some("cached output"));
        assert!(cache_lookup(&key, 0).is_some());
    }

    #[test]
    fn private_queries_are_blocked() {
        assert!(should_block_private_query("search my password"));
        assert!(should_block_private_query("Home Assistant token"));
        assert!(should_block_private_query("wifi password for my router"));
        assert!(!should_block_private_query(
            "how to rotate an api key safely"
        ));
    }

    #[test]
    fn handles_empty_results() {
        let output = format_results("nope", r#"{"RelatedTopics":[]}"#, 3).unwrap();
        assert_eq!(output, "No web search results found for \"nope\".");
    }

    #[test]
    fn clamps_result_count() {
        let body = r#"{
            "RelatedTopics": [
                {"Text": "One - first", "FirstURL": "https://example.test/1"},
                {"Text": "Two - second", "FirstURL": "https://example.test/2"}
            ]
        }"#;

        let output = format_results("numbers", body, 1).unwrap();
        assert!(output.contains("One"));
        assert!(!output.contains("Two"));
    }
}
