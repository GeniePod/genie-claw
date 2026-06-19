//! Shared whole-home entity resolution fidelity rules for runtime HA and BFCL scoring.

/// Domain + area view used by the fidelity guard (avoids a dependency on `HomeGraph`).
#[derive(Clone, Copy)]
pub struct DomainArea<'a> {
    pub domain: &'a str,
    pub area: Option<&'a str>,
}

/// Query words that do not pin a request to a specific place.
const BENIGN_QUERY_TOKENS: &[&str] = &[
    "all", "the", "a", "an", "every", "any", "my", "our", "this", "that", "please", "now", "here",
    "turn", "switch", "set", "get", "status", "of", "to", "in",
];

/// Split a free-text query into lowercase alphanumeric tokens.
pub fn query_tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Map a single query word to a device domain, mirroring the runtime `infer_domain`.
pub fn domain_of_word(word: &str) -> Option<&'static str> {
    Some(match word {
        "light" | "lights" | "lamp" | "lamps" => "light",
        "fan" | "fans" => "fan",
        "thermostat" | "temperature" | "heat" | "heating" | "cooling" | "ac" => "climate",
        "blind" | "blinds" | "shade" | "shades" | "curtain" | "curtains" | "cover" | "covers"
        | "garage" => "cover",
        "lock" | "locks" | "unlock" => "lock",
        "switch" | "switches" | "plug" | "outlet" => "switch",
        _ => return None,
    })
}

/// Fidelity guard for whole-home (area-less) resolutions. A query that resolved
/// only because every device of its domain happens to live in one place is
/// trustworthy *iff* every place-token it names belongs to an area that actually
/// contains a device of the inferred domain. This rejects foreign rooms the home
/// does not have ("upstairs lights") and known rooms lacking the device ("living
/// room light"), while still allowing bare-domain ("lights") and correctly
/// room-qualified ("kitchen lights") requests.
pub fn whole_home_resolution_is_trustworthy(entities: &[DomainArea<'_>], text: &str) -> bool {
    let tokens = query_tokens(text);
    let Some(domain) = tokens.iter().find_map(|t| domain_of_word(t)) else {
        return true;
    };
    let valid_place: std::collections::HashSet<String> = entities
        .iter()
        .filter(|entity| entity.domain == domain)
        .filter_map(|entity| entity.area)
        .flat_map(query_tokens)
        .collect();
    tokens.iter().all(|token| {
        domain_of_word(token).is_some()
            || BENIGN_QUERY_TOKENS.contains(&token.as_str())
            || valid_place.contains(token)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kitchen_light_home() -> Vec<DomainArea<'static>> {
        vec![
            DomainArea {
                domain: "light",
                area: Some("Kitchen"),
            },
            DomainArea {
                domain: "fan",
                area: Some("Kitchen"),
            },
            DomainArea {
                domain: "climate",
                area: Some("Kitchen"),
            },
            DomainArea {
                domain: "cover",
                area: Some("Living Room"),
            },
        ]
    }

    #[test]
    fn rejects_foreign_and_deviceless_rooms() {
        let entities = kitchen_light_home();
        for foreign in ["upstairs lights", "upstairs_lights", "living room light"] {
            assert!(
                !whole_home_resolution_is_trustworthy(&entities, foreign),
                "'{foreign}' must not pass the whole-home fidelity guard"
            );
        }
    }

    #[test]
    fn allows_bare_domain_and_room_qualified() {
        let entities = kitchen_light_home();
        assert!(whole_home_resolution_is_trustworthy(&entities, "lights"));
        assert!(whole_home_resolution_is_trustworthy(
            &entities,
            "kitchen lights"
        ));
    }
}
