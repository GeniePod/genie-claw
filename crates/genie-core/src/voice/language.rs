use std::collections::HashMap;

pub fn normalize_language_tag(tag: &str) -> String {
    let normalized = tag.trim().to_lowercase().replace('_', "-");
    if normalized.is_empty() {
        return String::new();
    }

    let base = normalized.split('-').next().unwrap_or(&normalized);
    match base {
        "zh" | "cmn" => "zh".into(),
        "es" | "spa" => "es".into(),
        "de" | "ger" | "deu" => "de".into(),
        "en" | "eng" => "en".into(),
        other => other.into(),
    }
}

pub fn configured_language(language: &str) -> Option<String> {
    let normalized = normalize_language_tag(language);
    if normalized.is_empty() || normalized == "auto" {
        None
    } else {
        Some(normalized)
    }
}

pub fn detect_language_from_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.chars().any(is_cjk_char) {
        return Some("zh".into());
    }

    let lower = trimmed.to_lowercase();

    // Tally every Spanish and German accent mark in a single pass over the
    // lowercased text, instead of ten separate full-string `matches()` scans
    // (six Spanish + four German). Each accent is a distinct character, so a
    // combined per-character count is identical to summing the individual
    // `matches(c).count()` values the previous implementation computed.
    let (spanish_accents, german_accents) = count_accent_marks(&lower);

    let spanish_hits = SPANISH_MARKERS
        .iter()
        .filter(|pattern| lower.contains(**pattern))
        .count()
        + spanish_accents;

    if spanish_hits >= 2 {
        return Some("es".into());
    }

    let german_hits = GERMAN_MARKERS
        .iter()
        .filter(|pattern| lower.contains(**pattern))
        .count()
        + german_accents;

    if german_hits >= 2 {
        return Some("de".into());
    }

    if lower.is_ascii() {
        Some("en".into())
    } else {
        None
    }
}

/// Spanish function words / greetings whose presence (substring match on the
/// lowercased text) is one detection hit each.
const SPANISH_MARKERS: [&str; 16] = [
    " el ",
    " la ",
    " los ",
    " las ",
    " un ",
    " una ",
    " por ",
    " para ",
    " gracias ",
    "hola",
    "qué",
    "como ",
    "está",
    "estoy",
    "buenos",
    "buenas",
];

/// German function words / greetings, one detection hit each.
const GERMAN_MARKERS: [&str; 12] = [
    " der ", " die ", " das ", " und ", " nicht ", " ich ", " ist ", " wie ", " danke ", "hallo",
    "guten", "bitte",
];

/// Count Spanish (`ñ á é í ó ú`) and German (`ä ö ü ß`) accent marks in one
/// traversal of the already-lowercased text. The two character sets are
/// disjoint, so each returned count equals the sum of the per-character
/// `matches(c).count()` the previous six/four-scan implementation produced.
fn count_accent_marks(lower: &str) -> (usize, usize) {
    let mut spanish = 0;
    let mut german = 0;
    for ch in lower.chars() {
        match ch {
            'ñ' | 'á' | 'é' | 'í' | 'ó' | 'ú' => spanish += 1,
            'ä' | 'ö' | 'ü' | 'ß' => german += 1,
            _ => {}
        }
    }
    (spanish, german)
}

pub fn select_tts_model<'a>(
    language: Option<&str>,
    configured_models: &'a HashMap<String, String>,
    default_model: &'a str,
) -> &'a str {
    let Some(language) = language else {
        return default_model;
    };

    let normalized = normalize_language_tag(language);
    if normalized.is_empty() {
        return default_model;
    }

    configured_models
        .get(&normalized)
        .map(String::as_str)
        .or_else(|| {
            language
                .split(['-', '_'])
                .next()
                .and_then(|short| configured_models.get(short))
                .map(String::as_str)
        })
        .unwrap_or(default_model)
}

fn is_cjk_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B820..=0x2CEAF
            | 0x2CEB0..=0x2EBEF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_common_language_tags() {
        assert_eq!(normalize_language_tag("en-US"), "en");
        assert_eq!(normalize_language_tag("de_DE"), "de");
        assert_eq!(normalize_language_tag("zh-CN"), "zh");
    }

    #[test]
    fn configured_language_treats_auto_as_none() {
        assert_eq!(configured_language("auto"), None);
        assert_eq!(configured_language("es-ES"), Some("es".into()));
    }

    #[test]
    fn detect_language_handles_chinese() {
        assert_eq!(
            detect_language_from_text("打开客厅的灯。"),
            Some("zh".into())
        );
    }

    #[test]
    fn detect_language_handles_spanish() {
        assert_eq!(
            detect_language_from_text("hola, ¿cómo está la casa hoy?"),
            Some("es".into())
        );
    }

    #[test]
    fn detect_language_handles_german() {
        assert_eq!(
            detect_language_from_text("hallo, wie ist das wetter heute?"),
            Some("de".into())
        );
    }

    #[test]
    fn select_tts_model_prefers_language_specific_voice() {
        let mut models = HashMap::new();
        models.insert("es".into(), "/voices/es.onnx".into());
        assert_eq!(
            select_tts_model(Some("es-ES"), &models, "/voices/en.onnx"),
            "/voices/es.onnx"
        );
        assert_eq!(
            select_tts_model(Some("de"), &models, "/voices/en.onnx"),
            "/voices/en.onnx"
        );
    }
}
