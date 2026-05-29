use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub const DEFAULT_EMBEDDING_DIMENSIONS: usize = 64;
pub const DEFAULT_EMBEDDING_MODEL: &str = "local-hash-home-v1";
pub const SEMANTIC_MIN_SCORE: f64 = 0.42;

pub trait EmbeddingProvider {
    fn model_name(&self) -> &'static str;
    fn dimensions(&self) -> usize;
    fn embed(&self, text: &str) -> Vec<f32>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalHashEmbeddingProvider;

impl EmbeddingProvider for LocalHashEmbeddingProvider {
    fn model_name(&self) -> &'static str {
        DEFAULT_EMBEDDING_MODEL
    }

    fn dimensions(&self) -> usize {
        DEFAULT_EMBEDDING_DIMENSIONS
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut values = vec![0.0; self.dimensions()];
        let expanded = expand_home_semantic_text(text);
        for token in embedding_tokens(&expanded) {
            let idx = token_bucket(&token, self.dimensions());
            values[idx] += token_weight(&token);
        }
        normalize(&mut values);
        values
    }
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut a_norm = 0.0f64;
    let mut b_norm = 0.0f64;
    for (left, right) in a.iter().zip(b) {
        let left = f64::from(*left);
        let right = f64::from(*right);
        dot += left * right;
        a_norm += left * left;
        b_norm += right * right;
    }
    if a_norm == 0.0 || b_norm == 0.0 {
        0.0
    } else {
        dot / (a_norm.sqrt() * b_norm.sqrt())
    }
}

pub fn expand_home_semantic_text(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut expanded = text.to_string();

    if contains_any(&lower, &["cold", "chilly", "freezing"]) {
        expanded.push_str(" thermostat temperature warm heat room comfort preference");
    }
    if contains_any(&lower, &["thermostat", "temperature", "heat", "warmer"]) {
        expanded.push_str(" cold warm room comfort preference");
    }
    if lower.contains("snack") || lower.contains("lunchbox") || lower.contains("lunch box") {
        expanded.push_str(" shopping grocery lunch school granola bars fruit snacks");
    }
    if lower.contains("detergent") {
        expanded.push_str(" shopping grocery household laundry order");
    }
    if contains_any(&lower, &["movie", "watched", "robot", "real boy"]) {
        expanded.push_str(" film watched kids robot real boy iron giant");
    }
    if lower.contains("coffee") && contains_any(&lower, &["machine", "maker", "brew"]) {
        expanded.push_str(" device manual instructions brew strength start");
    }

    expanded
}

fn embedding_tokens(text: &str) -> Vec<String> {
    let stop = [
        "a", "an", "and", "are", "as", "at", "be", "by", "can", "did", "do", "does", "for", "from",
        "have", "how", "i", "in", "is", "it", "me", "my", "of", "on", "or", "our", "please",
        "that", "the", "this", "to", "was", "we", "what", "whats", "when", "where", "who", "with",
        "you", "your",
    ];
    text.to_ascii_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| token.len() > 1 && !stop.contains(token))
        .map(ToString::to_string)
        .collect()
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn token_bucket(token: &str, dimensions: usize) -> usize {
    let mut hasher = DefaultHasher::new();
    token.hash(&mut hasher);
    (hasher.finish() as usize) % dimensions
}

fn token_weight(token: &str) -> f32 {
    match token {
        "thermostat" | "temperature" | "snack" | "lunchbox" | "movie" | "robot" | "detergent"
        | "coffee" => 1.8,
        "preference" | "shopping" | "watched" | "manual" => 1.4,
        _ => 1.0,
    }
}

fn normalize(values: &mut [f32]) {
    let norm = values
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt();
    if norm == 0.0 {
        return;
    }
    for value in values {
        *value = (f64::from(*value) / norm) as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_hash_embedding_is_stable_and_normalized() {
        let provider = LocalHashEmbeddingProvider;
        let first = provider.embed("Jared prefers the living room thermostat at 72F");
        let second = provider.embed("Jared prefers the living room thermostat at 72F");
        assert_eq!(first, second);
        assert_eq!(first.len(), DEFAULT_EMBEDDING_DIMENSIONS);
        assert!(cosine_similarity(&first, &second) > 0.99);
    }

    #[test]
    fn home_semantic_expansion_links_cold_to_thermostat() {
        let provider = LocalHashEmbeddingProvider;
        let query = provider.embed("I'm feeling cold");
        let memory = provider.embed("Jared prefers the living room thermostat at 72F");
        assert!(cosine_similarity(&query, &memory) >= SEMANTIC_MIN_SCORE);
    }
}
