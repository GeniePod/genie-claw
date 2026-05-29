use anyhow::{Context, Result};
use genie_core::eval::bfcl::{BfclCase, BfclCaseSource, ExpectedToolCall};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use yaml_rust2::{Yaml, YamlLoader};

const HA_INTENTS_URL: &str = "https://github.com/OHF-Voice/intents";
const HA_INTENTS_DATASET: &str = "Home Assistant Intents";
const HA_INTENTS_CITATION: &str = "OHF-Voice/intents";
const HA_INTENTS_LICENSE: &str = "CC BY 4.0";
const DEFAULT_LANGUAGE: &str = "en";
const DEFAULT_LIMIT: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaIntentsImportArgs {
    pub source: PathBuf,
    pub out: PathBuf,
    pub language: String,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaIntentsImportReport {
    pub read_files: usize,
    pub read_sentences: usize,
    pub generated_cases: usize,
}

struct HaSentenceFile {
    language: Option<String>,
    intents: BTreeMap<String, Vec<HaDataEntry>>,
    data: Vec<HaDataEntry>,
}

#[derive(Debug, Clone)]
struct HaDataEntry {
    sentences: Vec<String>,
    slots: BTreeMap<String, Yaml>,
    requires_context: Option<Yaml>,
    name_domains: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupportedIntent {
    TurnOn,
    TurnOff,
    GetState,
    LightSet,
    ClimateGetTemperature,
    ClimateSetTemperature,
    StartTimer,
    GetCurrentTime,
    GetWeather,
    MediaSearchAndPlay,
}

pub fn parse_ha_intents_import_args(args: &[String]) -> Result<HaIntentsImportArgs> {
    let mut source = None;
    let mut out = None;
    let mut language = DEFAULT_LANGUAGE.to_string();
    let mut limit = DEFAULT_LIMIT;
    let mut idx = 0;

    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--source" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--source requires the Home Assistant Intents checkout path");
                };
                source = Some(PathBuf::from(value));
                idx += 2;
            }
            "--out" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--out requires a JSONL output path");
                };
                out = Some(PathBuf::from(value));
                idx += 2;
            }
            "--language" | "--lang" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--language requires a language code");
                };
                language = value.clone();
                idx += 2;
            }
            "--limit" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--limit requires a positive number");
                };
                limit = parse_positive_usize("--limit", value)?;
                idx += 2;
            }
            _ if arg.starts_with("--source=") => {
                source = Some(PathBuf::from(arg.trim_start_matches("--source=")));
                idx += 1;
            }
            _ if arg.starts_with("--out=") => {
                out = Some(PathBuf::from(arg.trim_start_matches("--out=")));
                idx += 1;
            }
            _ if arg.starts_with("--language=") => {
                language = arg.trim_start_matches("--language=").to_string();
                idx += 1;
            }
            _ if arg.starts_with("--lang=") => {
                language = arg.trim_start_matches("--lang=").to_string();
                idx += 1;
            }
            _ if arg.starts_with("--limit=") => {
                limit = parse_positive_usize("--limit", arg.trim_start_matches("--limit="))?;
                idx += 1;
            }
            other => anyhow::bail!("unknown bfcl-import-ha-intents option: {}", other),
        }
    }

    let Some(source) = source else {
        anyhow::bail!(
            "Usage: genie-ctl bfcl-import-ha-intents --source INTENTS_DIR --out CASES.jsonl [--language en] [--limit N]"
        );
    };
    let Some(out) = out else {
        anyhow::bail!(
            "Usage: genie-ctl bfcl-import-ha-intents --source INTENTS_DIR --out CASES.jsonl [--language en] [--limit N]"
        );
    };
    if language.trim().is_empty() {
        anyhow::bail!("--language cannot be empty");
    }

    Ok(HaIntentsImportArgs {
        source,
        out,
        language,
        limit,
    })
}

pub fn import_ha_intents(args: &HaIntentsImportArgs) -> Result<HaIntentsImportReport> {
    verify_home_assistant_intents_license(&args.source)?;

    let sentence_dirs = sentence_dirs(&args.source, &args.language)?;
    let mut cases = Vec::new();
    let mut read_sentences = 0;
    let mut read_files = 0;
    let mut next_case_index = 1usize;
    for (language, sentence_dir) in &sentence_dirs {
        let mut files = yaml_files(sentence_dir)?;
        files.sort();

        for path in &files {
            read_files += 1;
            let file_cases = cases_from_file(
                &args.source,
                sentence_dir,
                path,
                language,
                &mut next_case_index,
            )
            .with_context(|| format!("convert {}", path.display()))?;
            read_sentences += file_cases.0;
            cases.extend(file_cases.1);
            if cases.len() >= args.limit {
                cases.truncate(args.limit);
                break;
            }
        }

        if cases.len() >= args.limit {
            break;
        }
    }

    if let Some(parent) = args.out.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create output directory {}", parent.display()))?;
    }
    write_cases_jsonl(&args.out, &cases)?;

    Ok(HaIntentsImportReport {
        read_files,
        read_sentences,
        generated_cases: cases.len(),
    })
}

fn cases_from_file(
    source_root: &Path,
    sentence_dir: &Path,
    path: &Path,
    language: &str,
    next_case_index: &mut usize,
) -> Result<(usize, Vec<BfclCase>)> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let parsed =
        parse_ha_sentence_file(&text).with_context(|| format!("parse YAML {}", path.display()))?;

    if let Some(file_language) = parsed.language.as_deref()
        && file_language != language
    {
        return Ok((0, Vec::new()));
    }

    let mut read_sentences = 0;
    let mut cases = Vec::new();
    let mut data_by_intent = parsed
        .intents
        .into_iter()
        .flat_map(|(intent, data)| data.into_iter().map(move |entry| (intent.clone(), entry)))
        .collect::<Vec<_>>();

    if !parsed.data.is_empty()
        && let Some(intent) = infer_intent_from_path(sentence_dir, path)
    {
        data_by_intent.extend(parsed.data.into_iter().map(|entry| (intent.clone(), entry)));
    }

    let relative_path = path
        .strip_prefix(source_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");

    for (intent_name, entry) in data_by_intent {
        let Some(intent) = supported_intent(&intent_name) else {
            continue;
        };
        for template in &entry.sentences {
            read_sentences += 1;
            let domain = domain_for_entry(&entry, template);
            let Some(prompt) = render_sentence_template(template, domain.as_deref()) else {
                continue;
            };
            let Some((tool_call, allow_extra_arguments)) =
                expected_call_for_intent(intent, &entry, template)
            else {
                continue;
            };
            let id = format!(
                "ha-{}-{}-{:05}",
                language,
                slug(&intent_name),
                *next_case_index
            );
            *next_case_index += 1;
            cases.push(BfclCase {
                id,
                category: Some(format!("ha_intents/{}", tool_call.name)),
                source: Some(BfclCaseSource {
                    dataset: HA_INTENTS_DATASET.to_string(),
                    url: Some(HA_INTENTS_URL.to_string()),
                    license: Some(HA_INTENTS_LICENSE.to_string()),
                    citation: Some(HA_INTENTS_CITATION.to_string()),
                    derived_from: Some(relative_path.clone()),
                    notes: Some(format!("sentence template: {template}")),
                }),
                prompt,
                expected_tool_calls: vec![tool_call],
                allow_extra_arguments,
            });
        }
    }

    Ok((read_sentences, cases))
}

fn expected_call_for_intent(
    intent: SupportedIntent,
    entry: &HaDataEntry,
    template: &str,
) -> Option<(ExpectedToolCall, bool)> {
    match intent {
        SupportedIntent::TurnOn | SupportedIntent::TurnOff | SupportedIntent::GetState => {
            let domain = domain_for_entry(entry, template);
            if is_unsafe_cover_entry(domain.as_deref(), entry) {
                return None;
            }
            let entity = sample_entity(domain.as_deref(), template)?;
            let action = match intent {
                SupportedIntent::TurnOn => match domain.as_deref() {
                    Some("cover") => "open",
                    _ => "turn_on",
                },
                SupportedIntent::TurnOff => match domain.as_deref() {
                    Some("cover") => "close",
                    _ => "turn_off",
                },
                SupportedIntent::GetState => "",
                _ => unreachable!(),
            };

            if intent == SupportedIntent::GetState {
                return Some((
                    ExpectedToolCall {
                        name: "home_status".to_string(),
                        arguments: serde_json::json!({ "entity": entity }),
                    },
                    true,
                ));
            }

            Some((
                ExpectedToolCall {
                    name: "home_control".to_string(),
                    arguments: serde_json::json!({
                        "entity": entity,
                        "action": action,
                    }),
                },
                true,
            ))
        }
        SupportedIntent::LightSet => {
            if !template.contains("<brightness>") && !template.contains("brightness_level") {
                return None;
            }
            Some((
                ExpectedToolCall {
                    name: "home_control".to_string(),
                    arguments: serde_json::json!({
                        "entity": sample_entity(Some("light"), template)?,
                        "action": "set_brightness",
                        "value": 50,
                    }),
                },
                true,
            ))
        }
        SupportedIntent::ClimateSetTemperature => Some((
            ExpectedToolCall {
                name: "home_control".to_string(),
                arguments: serde_json::json!({
                    "entity": climate_entity(template),
                    "action": "set_temperature",
                    "value": 72,
                }),
            },
            true,
        )),
        SupportedIntent::ClimateGetTemperature => Some((
            ExpectedToolCall {
                name: "home_status".to_string(),
                arguments: serde_json::json!({
                    "entity": climate_entity(template),
                }),
            },
            true,
        )),
        SupportedIntent::StartTimer => {
            if template.contains("conversation_command") {
                return None;
            }
            let mut arguments = serde_json::json!({ "seconds": 300 });
            if template.contains("timer_name")
                && let JsonValue::Object(object) = &mut arguments
            {
                object.insert("label".to_string(), JsonValue::String("tea".to_string()));
            }
            Some((
                ExpectedToolCall {
                    name: "set_timer".to_string(),
                    arguments,
                },
                true,
            ))
        }
        SupportedIntent::GetCurrentTime => Some((
            ExpectedToolCall {
                name: "get_time".to_string(),
                arguments: serde_json::json!({}),
            },
            false,
        )),
        SupportedIntent::GetWeather => Some((
            ExpectedToolCall {
                name: "get_weather".to_string(),
                arguments: serde_json::json!({ "location": "home" }),
            },
            true,
        )),
        SupportedIntent::MediaSearchAndPlay => Some((
            ExpectedToolCall {
                name: "play_media".to_string(),
                arguments: serde_json::json!({ "query": "jazz" }),
            },
            true,
        )),
    }
}

fn render_sentence_template(template: &str, domain: Option<&str>) -> Option<String> {
    if has_unsupported_slots(template) {
        return None;
    }

    let mut rendered = replace_braced_slots(template, domain);
    rendered = replace_angle_slots(&rendered, domain);
    rendered = choose_parenthetical_alternatives(&rendered);
    rendered = remove_optional_segments(&rendered);
    rendered = choose_remaining_pipe_alternative(&rendered);
    rendered = rendered
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c == '"' || c == '\'')
        .to_string();

    if rendered.is_empty()
        || rendered.contains('<')
        || rendered.contains('>')
        || rendered.contains('{')
        || rendered.contains('}')
    {
        return None;
    }

    Some(rendered)
}

fn replace_braced_slots(template: &str, domain: Option<&str>) -> String {
    replace_delimited(template, '{', '}', |slot| {
        match slot.split(':').next().unwrap_or(slot) {
            "name" => sample_name_for_domain(domain).to_string(),
            "timer_name" => "tea".to_string(),
            "brightness_level" => "medium".to_string(),
            "todo_list_name" => "shopping list".to_string(),
            "item" => "milk".to_string(),
            "search_query" => "jazz".to_string(),
            "media_class" => "music".to_string(),
            _ => "item".to_string(),
        }
    })
}

fn replace_angle_slots(template: &str, domain: Option<&str>) -> String {
    replace_delimited(template, '<', '>', |slot| match slot {
        "turn" => "turn".to_string(),
        "open" => "open".to_string(),
        "close" => "close".to_string(),
        "what_is" => "what is".to_string(),
        "how_is" => "how is".to_string(),
        "numeric_value_set" => "set".to_string(),
        "timer_set" => "set".to_string(),
        "timer_duration" => "5 minutes".to_string(),
        "brightness" => "50 percent".to_string(),
        "temperature" => "72 degrees".to_string(),
        "temp" => "temperature".to_string(),
        "area" | "area_floor" => "kitchen".to_string(),
        "in_area_floor" => "in kitchen".to_string(),
        "floor" => "upstairs".to_string(),
        "home" => "house".to_string(),
        "here" => "here".to_string(),
        "in" => "in".to_string(),
        "all" => "all".to_string(),
        "the" => "the".to_string(),
        "everywhere" => "everywhere".to_string(),
        "name" => sample_name_for_domain(domain).to_string(),
        "light" => "lights".to_string(),
        "fan" => "fan".to_string(),
        "cover" => "blinds".to_string(),
        "weather" => "weather".to_string(),
        "media" => "music".to_string(),
        "search_query" => "jazz".to_string(),
        _ => String::new(),
    })
}

fn sample_name_for_domain(domain: Option<&str>) -> &'static str {
    match domain {
        Some("cover") => "living room blinds",
        Some("fan") => "bedroom fan",
        Some("switch") => "coffee maker",
        Some("media_player") => "living room tv",
        Some("input_boolean") => "guest mode",
        _ => "kitchen light",
    }
}

fn has_unsupported_slots(template: &str) -> bool {
    has_unsupported_delimited_slot(template, '{', '}', |slot| {
        matches!(
            slot.split(':').next().unwrap_or(slot),
            "name"
                | "timer_name"
                | "todo_list_name"
                | "item"
                | "search_query"
                | "media_class"
                | "brightness_level"
        )
    }) || has_unsupported_delimited_slot(template, '<', '>', |slot| {
        matches!(
            slot,
            "turn"
                | "open"
                | "close"
                | "what_is"
                | "how_is"
                | "numeric_value_set"
                | "timer_set"
                | "timer_duration"
                | "brightness"
                | "temperature"
                | "temp"
                | "area"
                | "area_floor"
                | "in_area_floor"
                | "floor"
                | "home"
                | "here"
                | "in"
                | "all"
                | "the"
                | "everywhere"
                | "name"
                | "light"
                | "fan"
                | "cover"
                | "weather"
                | "media"
                | "search_query"
        )
    })
}

fn has_unsupported_delimited_slot<F>(
    template: &str,
    open: char,
    close: char,
    mut supported: F,
) -> bool
where
    F: FnMut(&str) -> bool,
{
    let mut rest = template;
    while let Some(start) = rest.find(open) {
        let after_open = &rest[start + open.len_utf8()..];
        let Some(end) = after_open.find(close) else {
            return true;
        };
        let slot = &after_open[..end];
        if !supported(slot) {
            return true;
        }
        rest = &after_open[end + close.len_utf8()..];
    }
    false
}

fn replace_delimited<F>(template: &str, open: char, close: char, mut replacement: F) -> String
where
    F: FnMut(&str) -> String,
{
    let mut output = String::new();
    let mut rest = template;
    while let Some(start) = rest.find(open) {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + open.len_utf8()..];
        let Some(end) = after_open.find(close) else {
            output.push(open);
            rest = after_open;
            continue;
        };
        let slot = &after_open[..end];
        output.push_str(&replacement(slot));
        rest = &after_open[end + close.len_utf8()..];
    }
    output.push_str(rest);
    output
}

fn choose_parenthetical_alternatives(input: &str) -> String {
    let mut current = input.to_string();
    while let Some(close) = current.find(')') {
        let Some(open) = current[..close].rfind('(') else {
            break;
        };
        let content = &current[open + 1..close];
        let chosen = content.split('|').next().unwrap_or(content).to_string();
        current.replace_range(open..=close, &chosen);
    }
    current
}

fn remove_optional_segments(input: &str) -> String {
    let mut output = String::new();
    let mut depth = 0usize;
    for ch in input.chars() {
        match ch {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => output.push(ch),
            _ => {}
        }
    }
    output
}

fn choose_remaining_pipe_alternative(input: &str) -> String {
    if !input.contains('|') {
        return input.to_string();
    }

    input
        .split_whitespace()
        .map(|word| word.split('|').next().unwrap_or(word))
        .collect::<Vec<_>>()
        .join(" ")
}

fn supported_intent(intent: &str) -> Option<SupportedIntent> {
    match intent {
        "HassTurnOn" => Some(SupportedIntent::TurnOn),
        "HassTurnOff" => Some(SupportedIntent::TurnOff),
        "HassGetState" => Some(SupportedIntent::GetState),
        "HassLightSet" => Some(SupportedIntent::LightSet),
        "HassClimateGetTemperature" => Some(SupportedIntent::ClimateGetTemperature),
        "HassClimateSetTemperature" => Some(SupportedIntent::ClimateSetTemperature),
        "HassStartTimer" => Some(SupportedIntent::StartTimer),
        "HassGetCurrentTime" => Some(SupportedIntent::GetCurrentTime),
        "HassGetWeather" => Some(SupportedIntent::GetWeather),
        "HassMediaSearchAndPlay" => Some(SupportedIntent::MediaSearchAndPlay),
        _ => None,
    }
}

fn domain_for_entry(entry: &HaDataEntry, template: &str) -> Option<String> {
    entry
        .slots
        .get("domain")
        .and_then(yaml_string)
        .or_else(|| requires_context_string(entry.requires_context.as_ref(), "domain"))
        .or_else(|| {
            entry
                .name_domains
                .iter()
                .find(|domain| supported_domain(domain))
                .cloned()
        })
        .or_else(|| {
            if template.contains("<light>") {
                Some("light".to_string())
            } else if template.contains("<fan>") {
                Some("fan".to_string())
            } else if template.contains("<cover>") {
                Some("cover".to_string())
            } else {
                None
            }
        })
}

fn supported_domain(domain: &str) -> bool {
    matches!(
        domain,
        "light" | "switch" | "fan" | "media_player" | "input_boolean" | "cover"
    )
}

fn is_unsafe_cover_entry(domain: Option<&str>, entry: &HaDataEntry) -> bool {
    if domain != Some("cover") {
        return false;
    }

    matches!(
        entry
            .slots
            .get("device_class")
            .and_then(yaml_string)
            .as_deref(),
        Some("garage" | "gate" | "door" | "window")
    )
}

fn sample_entity(domain: Option<&str>, template: &str) -> Option<String> {
    let domain = domain.unwrap_or("light");
    if !supported_domain(domain) {
        return None;
    }

    let named = template.contains("{name}") || template.contains("<name>");
    let global = template.contains("<everywhere>") || template.contains("<home>");
    let area = template.contains("<area>") || template.contains("<area_floor>");

    let entity = match (domain, named, global, area) {
        ("light", true, _, _) => "kitchen light",
        ("light", _, true, _) => "all lights",
        ("light", _, _, true) => "kitchen lights",
        ("light", _, _, _) => "kitchen lights",
        ("fan", true, _, _) => "bedroom fan",
        ("fan", _, _, true) => "kitchen fan",
        ("fan", _, _, _) => "fan",
        ("switch", true, _, _) => "coffee maker",
        ("switch", _, _, true) => "kitchen switches",
        ("switch", _, _, _) => "switch",
        ("media_player", true, _, _) => "living room tv",
        ("media_player", _, _, _) => "media player",
        ("input_boolean", true, _, _) => "guest mode",
        ("input_boolean", _, _, _) => "input boolean",
        ("cover", true, _, _) => "living room blinds",
        ("cover", _, _, true) => "kitchen blinds",
        ("cover", _, _, _) => "blinds",
        _ => return None,
    };

    Some(entity.to_string())
}

fn climate_entity(template: &str) -> &'static str {
    if template.contains("<area>") {
        "kitchen thermostat"
    } else {
        "thermostat"
    }
}

fn requires_context_string(value: Option<&Yaml>, key: &str) -> Option<String> {
    match value {
        Some(Yaml::Hash(mapping)) => mapping
            .get(&Yaml::String(key.to_string()))
            .and_then(yaml_string),
        _ => None,
    }
}

fn yaml_string(value: &Yaml) -> Option<String> {
    match value {
        Yaml::String(value) => Some(value.clone()),
        _ => None,
    }
}

fn parse_ha_sentence_file(text: &str) -> Result<HaSentenceFile> {
    let docs = YamlLoader::load_from_str(text).context("load YAML document")?;
    let Some(doc) = docs.first() else {
        anyhow::bail!("empty YAML document");
    };

    let language = yaml_hash_get(doc, "language").and_then(yaml_string);
    let data = yaml_hash_get(doc, "data")
        .map(parse_data_entries)
        .transpose()?
        .unwrap_or_default();
    let mut intents = BTreeMap::new();

    if let Some(Yaml::Hash(intent_map)) = yaml_hash_get(doc, "intents") {
        for (intent_key, intent_value) in intent_map {
            let Some(intent_name) = yaml_string(intent_key) else {
                continue;
            };
            let entries = yaml_hash_get(intent_value, "data")
                .map(parse_data_entries)
                .transpose()?
                .unwrap_or_default();
            intents.insert(intent_name, entries);
        }
    }

    Ok(HaSentenceFile {
        language,
        intents,
        data,
    })
}

fn parse_data_entries(value: &Yaml) -> Result<Vec<HaDataEntry>> {
    let Some(entries) = value.as_vec() else {
        anyhow::bail!("data must be a YAML sequence");
    };

    entries.iter().map(parse_data_entry).collect()
}

fn parse_data_entry(value: &Yaml) -> Result<HaDataEntry> {
    let sentences = yaml_hash_get(value, "sentences")
        .map(parse_string_array)
        .transpose()?
        .unwrap_or_default();
    let slots = yaml_hash_get(value, "slots")
        .and_then(Yaml::as_hash)
        .map(|hash| {
            hash.iter()
                .filter_map(|(key, value)| yaml_string(key).map(|key| (key, value.clone())))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let requires_context = yaml_hash_get(value, "requires_context").cloned();
    let name_domains = yaml_hash_get(value, "name_domains")
        .map(parse_string_array)
        .transpose()?
        .unwrap_or_default();

    Ok(HaDataEntry {
        sentences,
        slots,
        requires_context,
        name_domains,
    })
}

fn parse_string_array(value: &Yaml) -> Result<Vec<String>> {
    let Some(items) = value.as_vec() else {
        anyhow::bail!("expected YAML sequence of strings");
    };

    Ok(items.iter().filter_map(yaml_string).collect())
}

fn yaml_hash_get<'a>(value: &'a Yaml, key: &str) -> Option<&'a Yaml> {
    value
        .as_hash()?
        .get(&Yaml::String(key.to_string()))
        .filter(|value| !matches!(value, Yaml::BadValue | Yaml::Null))
}

fn infer_intent_from_path(sentence_dir: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(sentence_dir).ok()?;
    let parent = relative.parent()?.file_name()?.to_string_lossy();
    if parent.starts_with("Hass") {
        return Some(parent.to_string());
    }

    let stem = relative.file_stem()?.to_string_lossy();
    stem.split('_')
        .find(|part| part.starts_with("Hass"))
        .map(str::to_string)
}

fn verify_home_assistant_intents_license(source: &Path) -> Result<()> {
    let license_path = source.join("LICENSE.md");
    let text = fs::read_to_string(&license_path)
        .with_context(|| format!("read license file {}", license_path.display()))?;
    let normalized = text.to_lowercase();
    if !normalized.contains("creative commons attribution 4.0")
        && !normalized.contains("attribution 4.0 international")
    {
        anyhow::bail!(
            "Home Assistant Intents license check failed for {}; expected CC BY 4.0 text",
            license_path.display()
        );
    }
    if normalized.contains("noncommercial") || normalized.contains("non-commercial") {
        anyhow::bail!(
            "Home Assistant Intents license check failed for {}; noncommercial data is not allowed in product eval fixtures",
            license_path.display()
        );
    }
    Ok(())
}

fn yaml_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_yaml_files(root, &mut files)?;
    Ok(files)
}

fn sentence_dirs(source: &Path, language: &str) -> Result<Vec<(String, PathBuf)>> {
    let sentences_root = source.join("sentences");
    if language == "all" {
        let mut dirs = Vec::new();
        for entry in fs::read_dir(&sentences_root)
            .with_context(|| format!("read directory {}", sentences_root.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(language) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            dirs.push((language.to_string(), path));
        }
        dirs.sort_by(|a, b| a.0.cmp(&b.0));
        if dirs.is_empty() {
            anyhow::bail!(
                "Home Assistant Intents sentence directory contains no language dirs: {}",
                sentences_root.display()
            );
        }
        return Ok(dirs);
    }

    let sentence_dir = sentences_root.join(language);
    if !sentence_dir.is_dir() {
        anyhow::bail!(
            "Home Assistant Intents sentence directory not found: {}",
            sentence_dir.display()
        );
    }
    Ok(vec![(language.to_string(), sentence_dir)])
}

fn collect_yaml_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read directory {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_yaml_files(&path, files)?;
        } else if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("yaml" | "yml")
        ) {
            files.push(path);
        }
    }
    Ok(())
}

fn write_cases_jsonl(path: &Path, cases: &[BfclCase]) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    for case in cases {
        serde_json::to_writer(&mut writer, case)
            .with_context(|| format!("serialize BFCL case {}", case.id))?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

fn parse_positive_usize(name: &str, value: &str) -> Result<usize> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("invalid {name} value: {value}"))?;
    if parsed == 0 {
        anyhow::bail!("{name} must be greater than zero");
    }
    Ok(parsed)
}

fn slug(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_home_assistant_template_with_sample_slots() {
        let rendered =
            render_sentence_template("<turn> on [<the>] {name} [<in_area_floor>]", Some("light"))
                .unwrap();

        assert_eq!(rendered, "turn on kitchen light");
    }

    #[test]
    fn skips_templates_with_unknown_slots() {
        assert!(
            render_sentence_template("is {cover_states:state} [<in> <area>]", Some("cover"))
                .is_none(),
            "unknown HA list slots should not become low-signal placeholder text"
        );
    }

    #[test]
    fn parses_import_args_with_defaults() {
        let args = vec![
            "--source".to_string(),
            "/tmp/intents".to_string(),
            "--out".to_string(),
            "/tmp/cases.jsonl".to_string(),
        ];

        let parsed = parse_ha_intents_import_args(&args).unwrap();

        assert_eq!(parsed.language, "en");
        assert_eq!(parsed.limit, 1_000);
        assert_eq!(parsed.source, PathBuf::from("/tmp/intents"));
        assert_eq!(parsed.out, PathBuf::from("/tmp/cases.jsonl"));
    }

    #[test]
    fn imports_all_languages_when_requested() {
        let root = unique_temp_dir("ha-intents-import-all");
        fs::create_dir_all(root.join("sentences/en")).unwrap();
        fs::create_dir_all(root.join("sentences/es")).unwrap();
        fs::write(
            root.join("LICENSE.md"),
            "Creative Commons Attribution 4.0 International Public License",
        )
        .unwrap();
        let yaml = r#"language: "en"
intents:
  HassGetCurrentTime:
    data:
      - sentences:
          - "what time is it"
"#;
        fs::write(
            root.join("sentences/en/homeassistant_HassGetCurrentTime.yaml"),
            yaml,
        )
        .unwrap();
        fs::write(
            root.join("sentences/es/homeassistant_HassGetCurrentTime.yaml"),
            yaml.replace("language: \"en\"", "language: \"es\""),
        )
        .unwrap();

        let out = root.join("cases.jsonl");
        let report = import_ha_intents(&HaIntentsImportArgs {
            source: root.clone(),
            out: out.clone(),
            language: "all".to_string(),
            limit: 10,
        })
        .unwrap();

        assert_eq!(report.generated_cases, 2);
        let cases = fs::read_to_string(out).unwrap();
        assert!(cases.contains("\"id\":\"ha-en-hassgetcurrenttime-00001\""));
        assert!(cases.contains("\"id\":\"ha-es-hassgetcurrenttime-00002\""));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn import_assigns_unique_case_ids_across_files() {
        let root = unique_temp_dir("ha-intents-unique-ids");
        fs::create_dir_all(root.join("sentences/en")).unwrap();
        fs::write(
            root.join("LICENSE.md"),
            "Creative Commons Attribution 4.0 International Public License",
        )
        .unwrap();
        let turn_on_yaml = |sentence: &str| {
            format!(
                r#"language: "en"
intents:
  HassTurnOn:
    data:
      - sentences:
          - "{sentence}"
        slots:
          domain: "light"
"#
            )
        };
        fs::write(
            root.join("sentences/en/light_HassTurnOn.yaml"),
            turn_on_yaml("<turn> on [<the>] {name}"),
        )
        .unwrap();
        fs::write(
            root.join("sentences/en/switch_HassTurnOn.yaml"),
            turn_on_yaml("<turn> on [<the>] {name} please"),
        )
        .unwrap();

        let out = root.join("cases.jsonl");
        let report = import_ha_intents(&HaIntentsImportArgs {
            source: root.clone(),
            out: out.clone(),
            language: "en".to_string(),
            limit: 10,
        })
        .unwrap();

        assert_eq!(report.generated_cases, 2);
        let cases = fs::read_to_string(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<BfclCase>(line).unwrap())
            .collect::<Vec<_>>();
        let ids = cases
            .iter()
            .map(|case| case.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), cases.len());
        assert!(ids.contains("ha-en-hassturnon-00001"));
        assert!(ids.contains("ha-en-hassturnon-00002"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn imports_sample_home_assistant_intents_with_source_metadata() {
        let root = unique_temp_dir("ha-intents-import");
        fs::create_dir_all(root.join("sentences/en")).unwrap();
        fs::write(
            root.join("LICENSE.md"),
            "Creative Commons Attribution 4.0 International Public License",
        )
        .unwrap();
        fs::write(
            root.join("sentences/en/light_HassTurnOn.yaml"),
            r#"language: "en"
intents:
  HassTurnOn:
    data:
      - sentences:
          - "<turn> on [<the>] {name}"
        slots:
          domain: "light"
"#,
        )
        .unwrap();
        fs::write(
            root.join("sentences/en/homeassistant_HassStartTimer.yaml"),
            r#"language: "en"
intents:
  HassStartTimer:
    data:
      - sentences:
          - "<timer_set> [a] timer for <timer_duration> (named|called) {timer_name:name}"
"#,
        )
        .unwrap();

        let out = root.join("cases.jsonl");
        let report = import_ha_intents(&HaIntentsImportArgs {
            source: root.clone(),
            out: out.clone(),
            language: "en".to_string(),
            limit: 10,
        })
        .unwrap();

        assert_eq!(report.generated_cases, 2);
        let lines = fs::read_to_string(out).unwrap();
        let cases = lines
            .lines()
            .map(|line| serde_json::from_str::<BfclCase>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(
            cases
                .iter()
                .any(|case| case.expected_tool_calls[0].name == "home_control")
        );
        assert!(
            cases
                .iter()
                .any(|case| case.expected_tool_calls[0].name == "set_timer")
        );
        assert_eq!(
            cases[0].source.as_ref().unwrap().license.as_deref(),
            Some("CC BY 4.0")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_noncommercial_license_text() {
        let root = unique_temp_dir("ha-intents-license");
        fs::create_dir_all(root.join("sentences/en")).unwrap();
        fs::write(
            root.join("LICENSE.md"),
            "Creative Commons Attribution-NonCommercial 4.0 International",
        )
        .unwrap();

        let err = import_ha_intents(&HaIntentsImportArgs {
            source: root.clone(),
            out: root.join("cases.jsonl"),
            language: "en".to_string(),
            limit: 10,
        })
        .unwrap_err();

        assert!(err.to_string().contains("license check failed"));
        fs::remove_dir_all(root).unwrap();
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "genie-claw-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
