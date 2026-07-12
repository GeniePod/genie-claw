use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::client::{Entity, HaClient};
use super::entity_fidelity::{self, DomainArea};

const AREA_TEMPLATE: &str = r#"
{% set ns = namespace(items=[]) %}
{% for area_id in areas() %}
  {% set ns.items = ns.items + [{"id": area_id, "name": area_name(area_id), "entities": area_entities(area_id)}] %}
{% endfor %}
{{ ns.items | to_json }}
"#;

const GRAPH_CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationHealth {
    pub connected: bool,
    pub cached_graph: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeGraph {
    pub areas: Vec<AreaRef>,
    pub devices: Vec<DeviceRef>,
    pub entities: Vec<EntityRef>,
    pub scenes: Vec<SceneRef>,
    pub scripts: Vec<ScriptRef>,
    pub aliases: Vec<AliasRef>,
    pub domains: Vec<String>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AreaRef {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRef {
    pub id: String,
    pub name: String,
    pub domain: String,
    pub area: Option<String>,
    pub entity_ids: Vec<String>,
    pub aliases: Vec<String>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRef {
    pub entity_id: String,
    pub name: String,
    pub domain: String,
    pub area: Option<String>,
    pub aliases: Vec<String>,
    pub state: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneRef {
    pub entity_id: String,
    pub name: String,
    pub area: Option<String>,
    pub aliases: Vec<String>,
    pub voice_safe: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptRef {
    pub entity_id: String,
    pub name: String,
    pub area: Option<String>,
    pub aliases: Vec<String>,
    pub voice_safe: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasRef {
    pub alias: String,
    pub target_id: String,
    pub kind: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HomeTargetKind {
    Entity,
    Group,
    Scene,
    Script,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeTarget {
    pub kind: HomeTargetKind,
    pub query: String,
    pub display_name: String,
    pub entity_ids: Vec<String>,
    pub domain: Option<String>,
    pub area: Option<String>,
    pub confidence: f32,
    pub voice_safe: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeAction {
    pub kind: HomeActionKind,
    pub target: HomeTarget,
    pub value: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HomeActionKind {
    TurnOn,
    TurnOff,
    Toggle,
    SetBrightness,
    SetTemperature,
    Open,
    Close,
    Lock,
    Unlock,
    Activate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeState {
    pub target_name: String,
    pub domain: Option<String>,
    pub area: Option<String>,
    pub entities: Vec<Entity>,
    pub available: bool,
    pub spoken_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub success: bool,
    pub spoken_summary: String,
    pub affected_targets: Vec<String>,
    pub state_snapshot: Option<HomeState>,
    pub confidence: Option<f32>,
}

#[async_trait]
pub trait HomeAutomationProvider: Send + Sync {
    async fn health(&self) -> IntegrationHealth;
    async fn sync_structure(&self) -> Result<HomeGraph>;
    async fn resolve_target(
        &self,
        query: &str,
        action_hint: Option<HomeActionKind>,
    ) -> Result<HomeTarget>;
    async fn get_state(&self, target: &HomeTarget) -> Result<HomeState>;
    async fn execute(&self, action: HomeAction) -> Result<ActionResult>;
    async fn list_scenes(&self, room: Option<&str>) -> Result<Vec<SceneRef>>;
    async fn list_devices(&self, room: Option<&str>) -> Result<Vec<DeviceRef>>;
}

#[derive(Debug)]
pub struct HomeAssistantProvider {
    client: HaClient,
    cache: RwLock<Option<CachedGraph>>,
}

#[derive(Debug, Clone, Deserialize)]
struct AreaTemplateEntry {
    id: String,
    name: String,
    #[serde(default)]
    entities: Vec<String>,
}

#[derive(Debug, Clone)]
struct CachedGraph {
    graph: HomeGraph,
    synced_at: Instant,
}

impl HomeAssistantProvider {
    pub fn new(client: HaClient) -> Self {
        Self {
            client,
            cache: RwLock::new(None),
        }
    }

    pub fn from_url(url: &str, token: &str) -> Result<Self> {
        Ok(Self::new(HaClient::from_url(url, token)?))
    }

    async fn graph(&self) -> Result<(HomeGraph, bool)> {
        if let Some(cached) = self.cache.read().await.clone()
            && cached.synced_at.elapsed() < GRAPH_CACHE_TTL
        {
            return Ok((cached.graph, true));
        }
        Ok((self.sync_structure().await?, false))
    }

    async fn load_areas(&self) -> Result<Vec<AreaTemplateEntry>> {
        self.client
            .render_template_json(AREA_TEMPLATE)
            .await
            .context("failed to load Home Assistant areas")
    }

    fn build_graph(states: &[Entity], areas: &[AreaTemplateEntry]) -> HomeGraph {
        let mut entity_to_area = HashMap::new();
        let mut area_refs = Vec::new();
        let mut area_aliases = Vec::new();

        for area in areas {
            for entity_id in &area.entities {
                entity_to_area.insert(entity_id.clone(), area.name.clone());
            }

            let aliases = dedup_aliases([normalize(&area.name)].into_iter().collect());
            for alias in &aliases {
                area_aliases.push(AliasRef {
                    alias: alias.clone(),
                    target_id: area.id.clone(),
                    kind: "area".into(),
                });
            }
            area_refs.push(AreaRef {
                id: area.id.clone(),
                name: area.name.clone(),
                aliases,
            });
        }

        let mut entities = Vec::new();
        let mut devices = Vec::new();
        let mut scenes = Vec::new();
        let mut scripts = Vec::new();
        let mut aliases = area_aliases;
        let mut domains = BTreeSet::new();
        let mut capabilities = BTreeSet::new();

        for state in states {
            let domain = state.entity_id.split('.').next().unwrap_or("").to_string();
            if !is_voice_relevant_domain(&domain) {
                continue;
            }

            domains.insert(domain.clone());
            let name = state.friendly_name().to_string();
            let area = entity_to_area.get(&state.entity_id).cloned();
            let caps = capabilities_for(&domain, &state.attributes);
            for cap in &caps {
                capabilities.insert(cap.clone());
            }

            let entity_aliases = build_aliases(&name, &domain, area.as_deref());
            for alias in &entity_aliases {
                aliases.push(AliasRef {
                    alias: alias.clone(),
                    target_id: state.entity_id.clone(),
                    kind: domain.clone(),
                });
            }

            let entity_ref = EntityRef {
                entity_id: state.entity_id.clone(),
                name: name.clone(),
                domain: domain.clone(),
                area: area.clone(),
                aliases: entity_aliases.clone(),
                state: state.state.clone(),
                capabilities: caps.clone(),
            };
            entities.push(entity_ref.clone());

            match domain.as_str() {
                "scene" => {
                    scenes.push(SceneRef {
                        entity_id: state.entity_id.clone(),
                        name: name.clone(),
                        area: area.clone(),
                        aliases: entity_aliases,
                        voice_safe: true,
                    });
                }
                "script" => {
                    scripts.push(ScriptRef {
                        entity_id: state.entity_id.clone(),
                        name: name.clone(),
                        area: area.clone(),
                        aliases: entity_aliases,
                        voice_safe: is_voice_safe_script(state),
                    });
                }
                _ => {
                    devices.push(DeviceRef {
                        id: state.entity_id.clone(),
                        name,
                        domain,
                        area,
                        entity_ids: vec![state.entity_id.clone()],
                        aliases: entity_ref.aliases,
                        capabilities: caps,
                    });
                }
            }
        }

        HomeGraph {
            areas: area_refs,
            devices,
            entities,
            scenes,
            scripts,
            aliases,
            domains: domains.into_iter().collect(),
            capabilities: capabilities.into_iter().collect(),
        }
    }

    pub(crate) fn resolve_target_in_graph(
        graph: &HomeGraph,
        query: &str,
        action_hint: Option<HomeActionKind>,
    ) -> Option<HomeTarget> {
        let query_lower = normalize(query);

        if let Some(target) = Self::resolve_exact_entity_id(graph, query, action_hint) {
            return Some(target);
        }

        let area_match = best_area_match(&graph.areas, &query_lower);
        let domain_match = infer_domain(&query_lower);

        if let (Some((area_name, area_score)), Some(domain)) = (area_match.clone(), domain_match)
            && let Some(target) =
                Self::resolve_group_target(graph, query, &domain, &area_name, area_score)
        {
            return Some(target);
        }

        let domain_match = infer_domain(&query_lower);
        if area_match.is_none()
            && let Some(domain) = domain_match
            && let Some(target) = Self::resolve_domain_target(graph, query, &domain)
        {
            return Some(target);
        }

        if action_hint.is_none()
            && let Some((area_name, area_score)) = area_match
            && let Some(target) =
                Self::resolve_group_target(graph, query, "light", &area_name, area_score * 0.8)
        {
            return Some(target);
        }

        Self::resolve_named_entity(graph, query, action_hint)
    }

    fn query_rejects_whole_home_fidelity(graph: &HomeGraph, query: &str) -> bool {
        let query_lower = normalize(query);
        let area_match = best_area_match(&graph.areas, &query_lower);
        let domain_match = infer_domain(&query_lower);
        if area_match.is_some() || domain_match.is_none() {
            return false;
        }
        let entity_views: Vec<DomainArea<'_>> = graph
            .entities
            .iter()
            .map(|entity| DomainArea {
                domain: entity.domain.as_str(),
                area: entity.area.as_deref(),
            })
            .collect();
        !entity_fidelity::whole_home_resolution_is_trustworthy(&entity_views, query)
    }

    fn resolve_exact_entity_id(
        graph: &HomeGraph,
        query: &str,
        action_hint: Option<HomeActionKind>,
    ) -> Option<HomeTarget> {
        let query = query.trim();
        let entity = graph
            .entities
            .iter()
            .find(|entity| entity.entity_id.eq_ignore_ascii_case(query))?;

        match entity.domain.as_str() {
            "scene" if !matches!(action_hint, Some(HomeActionKind::Activate) | None) => None,
            "script" if !matches!(action_hint, Some(HomeActionKind::Activate) | None) => None,
            "scene" => Some(HomeTarget {
                kind: HomeTargetKind::Scene,
                query: query.to_string(),
                display_name: entity.name.clone(),
                entity_ids: vec![entity.entity_id.clone()],
                domain: Some(entity.domain.clone()),
                area: entity.area.clone(),
                confidence: 1.0,
                voice_safe: true,
            }),
            "script" => graph
                .scripts
                .iter()
                .find(|script| script.entity_id.eq_ignore_ascii_case(query))
                .map(|script| HomeTarget {
                    kind: HomeTargetKind::Script,
                    query: query.to_string(),
                    display_name: script.name.clone(),
                    entity_ids: vec![script.entity_id.clone()],
                    domain: Some("script".into()),
                    area: script.area.clone(),
                    confidence: 1.0,
                    voice_safe: script.voice_safe,
                }),
            _ => Some(HomeTarget {
                kind: HomeTargetKind::Entity,
                query: query.to_string(),
                display_name: entity.name.clone(),
                entity_ids: vec![entity.entity_id.clone()],
                domain: Some(entity.domain.clone()),
                area: entity.area.clone(),
                confidence: 1.0,
                voice_safe: entity.domain != "lock",
            }),
        }
    }

    fn resolve_domain_target(graph: &HomeGraph, query: &str, domain: &str) -> Option<HomeTarget> {
        let entity_ids: Vec<String> = graph
            .entities
            .iter()
            .filter(|entity| entity.domain == domain)
            .map(|entity| entity.entity_id.clone())
            .collect();

        if entity_ids.is_empty() {
            return None;
        }

        let entity_views: Vec<DomainArea<'_>> = graph
            .entities
            .iter()
            .map(|entity| DomainArea {
                domain: entity.domain.as_str(),
                area: entity.area.as_deref(),
            })
            .collect();
        if !entity_fidelity::whole_home_resolution_is_trustworthy(&entity_views, query) {
            return None;
        }

        let display_name = format!("All {}", domain_plural_label(domain));
        Some(HomeTarget {
            kind: HomeTargetKind::Group,
            query: query.to_string(),
            display_name,
            entity_ids,
            domain: Some(domain.to_string()),
            area: None,
            confidence: 0.72,
            voice_safe: domain != "lock",
        })
    }

    fn resolve_group_target(
        graph: &HomeGraph,
        query: &str,
        domain: &str,
        area_name: &str,
        confidence: f32,
    ) -> Option<HomeTarget> {
        let entity_ids: Vec<String> = graph
            .entities
            .iter()
            .filter(|entity| entity.domain == domain && entity.area.as_deref() == Some(area_name))
            .map(|entity| entity.entity_id.clone())
            .collect();

        if entity_ids.is_empty() {
            return None;
        }

        let display_name = format!("{} {}", area_name, domain_label(domain, entity_ids.len()));
        Some(HomeTarget {
            kind: HomeTargetKind::Group,
            query: query.to_string(),
            display_name,
            entity_ids,
            domain: Some(domain.to_string()),
            area: Some(area_name.to_string()),
            confidence,
            voice_safe: domain != "lock",
        })
    }

    fn resolve_named_entity(
        graph: &HomeGraph,
        query: &str,
        action_hint: Option<HomeActionKind>,
    ) -> Option<HomeTarget> {
        let query_lower = normalize(query);
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        let mut best_scene: Option<(f32, HomeTarget)> = None;

        if matches!(action_hint, Some(HomeActionKind::Activate)) {
            for scene in &graph.scenes {
                let score = best_alias_score(&query_words, &query_lower, &scene.aliases);
                if score > 0.42 && best_scene.as_ref().is_none_or(|(s, _)| score > *s) {
                    best_scene = Some((
                        score,
                        HomeTarget {
                            kind: HomeTargetKind::Scene,
                            query: query.to_string(),
                            display_name: scene.name.clone(),
                            entity_ids: vec![scene.entity_id.clone()],
                            domain: Some("scene".into()),
                            area: scene.area.clone(),
                            confidence: score,
                            voice_safe: true,
                        },
                    ));
                }
            }

            for script in &graph.scripts {
                let score = best_alias_score(&query_words, &query_lower, &script.aliases);
                if script.voice_safe
                    && score > 0.42
                    && best_scene.as_ref().is_none_or(|(s, _)| score > *s)
                {
                    best_scene = Some((
                        score,
                        HomeTarget {
                            kind: HomeTargetKind::Script,
                            query: query.to_string(),
                            display_name: script.name.clone(),
                            entity_ids: vec![script.entity_id.clone()],
                            domain: Some("script".into()),
                            area: script.area.clone(),
                            confidence: score,
                            voice_safe: true,
                        },
                    ));
                }
            }
        }

        // Resolve the best-matching named entity, but refuse to silently pick one
        // when several distinct devices tie at the top score (e.g. two "… Lamp"s
        // both matching the bare query "lamp", since every light shares the
        // "lamp"/"lights" domain aliases). Guessing here actuates an arbitrary
        // device chosen by graph iteration order; decline so `resolve_target` can
        // ask the user which one they meant. (#511)
        let mut best_entity: Option<(f32, HomeTarget)> = None;
        if let Some((score, entity)) = top_named_entity_match(graph, &query_lower, &query_words) {
            best_entity = Some((
                score,
                HomeTarget {
                    kind: HomeTargetKind::Entity,
                    query: query.to_string(),
                    display_name: entity.name.clone(),
                    entity_ids: vec![entity.entity_id.clone()],
                    domain: Some(entity.domain.clone()),
                    area: entity.area.clone(),
                    confidence: score,
                    voice_safe: entity.domain != "lock",
                },
            ));
        }

        best_scene.or(best_entity).map(|(_, target)| target)
    }

    async fn get_live_entities(&self, entity_ids: &[String]) -> Result<Vec<Entity>> {
        let wanted: HashSet<&str> = entity_ids.iter().map(String::as_str).collect();
        let states = self.client.get_states().await?;
        Ok(states
            .into_iter()
            .filter(|entity| wanted.contains(entity.entity_id.as_str()))
            .collect())
    }
}

#[async_trait]
impl HomeAutomationProvider for HomeAssistantProvider {
    async fn health(&self) -> IntegrationHealth {
        let cached_graph = self.cache.read().await.is_some();
        match self.client.test_connection().await {
            Ok(()) => IntegrationHealth {
                connected: true,
                cached_graph,
                message: format!(
                    "connected to Home Assistant at {}:{}",
                    self.client.host(),
                    self.client.port()
                ),
            },
            Err(err) => IntegrationHealth {
                connected: false,
                cached_graph,
                message: format!("Home Assistant unavailable: {}", err),
            },
        }
    }

    async fn sync_structure(&self) -> Result<HomeGraph> {
        let states = self.client.get_states().await?;
        let areas = self.load_areas().await.unwrap_or_else(|err| {
            tracing::warn!(error = %err, "failed to load Home Assistant area registry; continuing without area map");
            Vec::new()
        });

        let graph = Self::build_graph(&states, &areas);
        *self.cache.write().await = Some(CachedGraph {
            graph: graph.clone(),
            synced_at: Instant::now(),
        });
        Ok(graph)
    }

    async fn resolve_target(
        &self,
        query: &str,
        action_hint: Option<HomeActionKind>,
    ) -> Result<HomeTarget> {
        let query = query.trim();
        if query.is_empty() {
            anyhow::bail!("missing Home Assistant target");
        }

        let (graph, from_cache) = self.graph().await?;
        if let Some(target) = Self::resolve_target_in_graph(&graph, query, action_hint) {
            return Ok(target);
        }

        if from_cache {
            match self.sync_structure().await {
                Ok(refreshed_graph) => {
                    if let Some(target) =
                        Self::resolve_target_in_graph(&refreshed_graph, query, action_hint)
                    {
                        return Ok(target);
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        query,
                        "failed to refresh Home Assistant graph after cache miss"
                    );
                }
            }
        }

        // Several distinct devices matched equally well (e.g. "lamp" against two
        // different lamps). Decline to guess and name the candidates so the caller
        // can disambiguate instead of actuating an arbitrary one. (#511)
        if let Some(candidates) = ambiguous_named_entity_candidates(&graph, query) {
            anyhow::bail!(
                "'{}' is ambiguous between {} — please say which one you mean",
                query,
                format_disambiguation_candidates(&candidates)
            );
        }

        if Self::query_rejects_whole_home_fidelity(&graph, query) {
            anyhow::bail!("I couldn't find '{}' in this home", query);
        }

        anyhow::bail!("no Home Assistant target matched '{}'", query)
    }

    async fn get_state(&self, target: &HomeTarget) -> Result<HomeState> {
        let entities = self.get_live_entities(&target.entity_ids).await?;
        if entities.is_empty() {
            anyhow::bail!(
                "no Home Assistant state available for {}",
                target.display_name
            );
        }

        let available = entities.iter().all(|entity| entity.state != "unavailable");
        let spoken_summary =
            summarize_state(&target.display_name, target.domain.as_deref(), &entities);

        Ok(HomeState {
            target_name: target.display_name.clone(),
            domain: target.domain.clone(),
            area: target.area.clone(),
            entities,
            available,
            spoken_summary,
        })
    }

    async fn execute(&self, action: HomeAction) -> Result<ActionResult> {
        let target = &action.target;
        let domain = target
            .domain
            .clone()
            .ok_or_else(|| anyhow::anyhow!("unresolved Home Assistant domain"))?;

        let (service_domain, service, data) = match action.kind {
            HomeActionKind::TurnOn => (
                domain.clone(),
                "turn_on",
                serde_json::json!({ "entity_id": target.entity_ids }),
            ),
            HomeActionKind::TurnOff => (
                domain.clone(),
                "turn_off",
                serde_json::json!({ "entity_id": target.entity_ids }),
            ),
            HomeActionKind::Toggle => (
                domain.clone(),
                "toggle",
                serde_json::json!({ "entity_id": target.entity_ids }),
            ),
            HomeActionKind::SetBrightness => {
                ensure_domain(&domain, &["light"])?;
                // No silent default: a value-less set_brightness is rejected at
                // the dispatch boundary (issue #421); error here too as defense
                // in depth rather than actuating an arbitrary brightness.
                let brightness =
                    normalize_brightness(action.value.ok_or_else(|| {
                        anyhow::anyhow!("set_brightness requires a numeric 'value'")
                    })?);
                (
                    "light".into(),
                    "turn_on",
                    serde_json::json!({ "entity_id": target.entity_ids, "brightness": brightness }),
                )
            }
            HomeActionKind::SetTemperature => {
                ensure_domain(&domain, &["climate"])?;
                // No silent default: a value-less set_temperature is rejected at
                // the dispatch boundary (issue #421); error here too as defense
                // in depth rather than actuating an arbitrary temperature.
                let temp = action
                    .value
                    .ok_or_else(|| anyhow::anyhow!("set_temperature requires a numeric 'value'"))?;
                (
                    "climate".into(),
                    "set_temperature",
                    serde_json::json!({ "entity_id": target.entity_ids, "temperature": temp }),
                )
            }
            HomeActionKind::Open => {
                ensure_domain(&domain, &["cover"])?;
                (
                    "cover".into(),
                    "open_cover",
                    serde_json::json!({ "entity_id": target.entity_ids }),
                )
            }
            HomeActionKind::Close => {
                ensure_domain(&domain, &["cover"])?;
                (
                    "cover".into(),
                    "close_cover",
                    serde_json::json!({ "entity_id": target.entity_ids }),
                )
            }
            HomeActionKind::Lock => {
                ensure_domain(&domain, &["lock"])?;
                (
                    "lock".into(),
                    "lock",
                    serde_json::json!({ "entity_id": target.entity_ids }),
                )
            }
            HomeActionKind::Unlock => {
                ensure_domain(&domain, &["lock"])?;
                (
                    "lock".into(),
                    "unlock",
                    serde_json::json!({ "entity_id": target.entity_ids }),
                )
            }
            HomeActionKind::Activate => match target.kind {
                HomeTargetKind::Scene => (
                    "scene".into(),
                    "turn_on",
                    serde_json::json!({ "entity_id": target.entity_ids }),
                ),
                HomeTargetKind::Script => {
                    if !target.voice_safe {
                        anyhow::bail!(
                            "script '{}' is not marked voice-safe for GeniePod",
                            target.display_name
                        );
                    }
                    (
                        "script".into(),
                        "turn_on",
                        serde_json::json!({ "entity_id": target.entity_ids }),
                    )
                }
                _ => anyhow::bail!("activate is only supported for scenes or voice-safe scripts"),
            },
        };

        self.client
            .call_service(&service_domain, service, &data)
            .await?;

        let state_snapshot = match target.kind {
            HomeTargetKind::Scene | HomeTargetKind::Script => None,
            _ => self.get_state(target).await.ok(),
        };

        let spoken_summary = build_action_summary(&action, state_snapshot.as_ref());

        Ok(ActionResult {
            success: true,
            spoken_summary,
            affected_targets: vec![target.display_name.clone()],
            state_snapshot,
            confidence: Some(target.confidence),
        })
    }

    async fn list_scenes(&self, room: Option<&str>) -> Result<Vec<SceneRef>> {
        let (graph, _) = self.graph().await?;
        let room = room.map(normalize);
        Ok(graph
            .scenes
            .into_iter()
            .filter(|scene| match (&room, &scene.area) {
                (Some(room), Some(area)) => normalize(area) == *room,
                (Some(_), None) => false,
                (None, _) => true,
            })
            .collect())
    }

    async fn list_devices(&self, room: Option<&str>) -> Result<Vec<DeviceRef>> {
        let (graph, _) = self.graph().await?;
        let room = room.map(normalize);
        Ok(graph
            .devices
            .into_iter()
            .filter(|device| match (&room, &device.area) {
                (Some(room), Some(area)) => normalize(area) == *room,
                (Some(_), None) => false,
                (None, _) => true,
            })
            .collect())
    }
}

pub fn into_provider(provider: HomeAssistantProvider) -> Arc<dyn HomeAutomationProvider> {
    Arc::new(provider)
}

fn is_voice_relevant_domain(domain: &str) -> bool {
    matches!(
        domain,
        "light"
            | "switch"
            | "fan"
            | "scene"
            | "script"
            | "climate"
            | "cover"
            | "lock"
            | "sensor"
            | "binary_sensor"
            | "media_player"
    )
}

fn build_aliases(name: &str, domain: &str, area: Option<&str>) -> Vec<String> {
    let mut aliases = vec![normalize(name)];
    aliases.extend(domain_synonyms(domain));

    if let Some(area) = area {
        let area_norm = normalize(area);
        aliases.push(format!("{} {}", area_norm, normalize(name)));
        for synonym in domain_synonyms(domain) {
            aliases.push(format!("{} {}", area_norm, synonym));
        }
    }

    dedup_aliases(aliases)
}

fn dedup_aliases(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for value in values {
        let value = value.trim().to_string();
        if !value.is_empty() && seen.insert(value.clone()) {
            out.push(value);
        }
    }
    out
}

fn capabilities_for(domain: &str, attributes: &serde_json::Value) -> Vec<String> {
    let mut caps = vec![domain.to_string()];

    match domain {
        "light" => {
            caps.push("turn_on".into());
            caps.push("turn_off".into());
            if attributes.get("brightness").is_some() {
                caps.push("set_brightness".into());
            }
        }
        "switch" | "fan" => {
            caps.push("turn_on".into());
            caps.push("turn_off".into());
        }
        "climate" => {
            caps.push("set_temperature".into());
        }
        "cover" => {
            caps.push("open".into());
            caps.push("close".into());
        }
        "lock" => {
            caps.push("lock".into());
            caps.push("unlock".into());
        }
        "scene" | "script" => {
            caps.push("activate".into());
        }
        _ => {}
    }

    caps
}

fn is_voice_safe_script(entity: &Entity) -> bool {
    let name = normalize(entity.friendly_name());
    let entity_id = normalize(&entity.entity_id.replace('.', " "));
    name.contains("[voice]")
        || name.contains("(voice)")
        || name.starts_with("voice ")
        || entity_id.contains(" voice ")
        || entity_id.contains("voice_")
        || entity_id.contains("genie_")
}

fn domain_synonyms(domain: &str) -> Vec<String> {
    match domain {
        "light" => vec![
            "light".into(),
            "lights".into(),
            "lamp".into(),
            "lamps".into(),
        ],
        "switch" => vec![
            "switch".into(),
            "switches".into(),
            "plug".into(),
            "outlet".into(),
        ],
        "fan" => vec!["fan".into(), "fans".into()],
        "climate" => vec!["thermostat".into(), "temperature".into(), "heater".into()],
        "cover" => vec![
            "cover".into(),
            "covers".into(),
            "blind".into(),
            "blinds".into(),
        ],
        "lock" => vec!["lock".into(), "locks".into(), "door lock".into()],
        "scene" => vec!["scene".into(), "scenes".into()],
        "script" => vec!["script".into(), "routine".into()],
        _ => Vec::new(),
    }
}

fn infer_domain(query: &str) -> Option<String> {
    let tokens = entity_fidelity::query_tokens(query);
    for domain in ["light", "switch", "fan", "climate", "cover", "lock"] {
        let matched = tokens.iter().any(|token| {
            entity_fidelity::domain_of_word(token) == Some(domain)
                || (domain == "climate" && matches!(token.as_str(), "warmer" | "cooler"))
        });
        if matched {
            return Some(domain.to_string());
        }
    }

    None
}

/// Minimum alias score for a named entity to be considered a match. Mirrors the
/// `> 0.42` threshold the scene/script resolution loops use.
const NAMED_ENTITY_MATCH_THRESHOLD: f32 = 0.42;

/// Scores within this margin are treated as equal. Shared domain aliases
/// (`lamp`/`lights`/…) make distinct devices score an identical `1.0`, so the
/// epsilon only needs to absorb float noise, not bridge genuinely different
/// matches.
const NAMED_ENTITY_TIE_EPSILON: f32 = 1e-3;

/// All named (non scene/script) entities whose alias score ties for the best,
/// above [`NAMED_ENTITY_MATCH_THRESHOLD`], paired with that best score.
///
/// Returns an empty list when nothing matches; a length greater than one means
/// the query is genuinely ambiguous across distinct devices and must not be
/// silently collapsed to whichever entity the graph happened to list first. (#511)
fn tied_named_entity_matches<'a>(
    graph: &'a HomeGraph,
    query_lower: &str,
    query_words: &[&str],
) -> (f32, Vec<&'a EntityRef>) {
    let mut best = 0.0_f32;
    let mut tied: Vec<&EntityRef> = Vec::new();
    for entity in &graph.entities {
        if matches!(entity.domain.as_str(), "scene" | "script") {
            continue;
        }
        let score = best_alias_score(query_words, query_lower, &entity.aliases);
        if score <= NAMED_ENTITY_MATCH_THRESHOLD {
            continue;
        }
        if score > best + NAMED_ENTITY_TIE_EPSILON {
            best = score;
            tied.clear();
            tied.push(entity);
        } else if (score - best).abs() <= NAMED_ENTITY_TIE_EPSILON {
            best = best.max(score);
            tied.push(entity);
        }
    }
    (best, tied)
}

/// The single best named entity for `query`, or `None` when nothing matches or
/// when several distinct devices tie for the top score (an ambiguous query that
/// the caller should disambiguate rather than guess). (#511)
fn top_named_entity_match<'a>(
    graph: &'a HomeGraph,
    query_lower: &str,
    query_words: &[&str],
) -> Option<(f32, &'a EntityRef)> {
    match tied_named_entity_matches(graph, query_lower, query_words) {
        (score, tied) if tied.len() == 1 => Some((score, tied[0])),
        _ => None,
    }
}

/// Format tied candidate names for a disambiguation prompt: two items use
/// "A or B"; three or more use an Oxford comma ("A, B, or C").
fn format_disambiguation_candidates(candidates: &[String]) -> String {
    match candidates.len() {
        0 => String::new(),
        1 => candidates[0].clone(),
        2 => format!("{} or {}", candidates[0], candidates[1]),
        len => format!(
            "{}, or {}",
            candidates[..len - 1].join(", "),
            candidates[len - 1]
        ),
    }
}

/// Candidate display names when `query` ambiguously matches several distinct
/// named entities at the top score, for surfacing a disambiguation prompt. (#511)
fn ambiguous_named_entity_candidates(graph: &HomeGraph, query: &str) -> Option<Vec<String>> {
    let query_lower = normalize(query);
    let query_words: Vec<&str> = query_lower.split_whitespace().collect();
    let (_, tied) = tied_named_entity_matches(graph, &query_lower, &query_words);
    if tied.len() > 1 {
        Some(tied.iter().map(|entity| entity.name.clone()).collect())
    } else {
        None
    }
}

fn best_area_match(areas: &[AreaRef], query: &str) -> Option<(String, f32)> {
    let query_words: Vec<&str> = query.split_whitespace().collect();
    let mut best: Option<(String, f32)> = None;
    for area in areas {
        let score = best_alias_score(&query_words, query, &area.aliases);
        if score > 0.45
            && best
                .as_ref()
                .is_none_or(|(_, best_score)| score > *best_score)
        {
            best = Some((area.name.clone(), score));
        }
    }
    best
}

fn best_alias_score(query_words: &[&str], query_lower: &str, aliases: &[String]) -> f32 {
    aliases
        .iter()
        .map(|alias| fuzzy_score(query_words, query_lower, alias))
        .fold(0.0, f32::max)
}

fn fuzzy_score(query_words: &[&str], query_lower: &str, candidate: &str) -> f32 {
    let candidate_words: Vec<&str> = candidate.split_whitespace().collect();
    let mut score = 0.0;

    if candidate == query_lower {
        score += 1.0;
    } else if candidate.contains(query_lower) || query_lower.contains(candidate) {
        score += 0.75;
    }

    if !query_words.is_empty() {
        let matching = query_words
            .iter()
            .filter(|query_word| {
                candidate_words.iter().any(|candidate_word| {
                    candidate_word.contains(*query_word) || query_word.contains(candidate_word)
                })
            })
            .count();
        score += (matching as f32 / query_words.len() as f32) * 0.35;
    }

    if candidate.starts_with(query_lower) {
        score += 0.15;
    }

    score.min(1.0)
}

fn normalize(input: &str) -> String {
    input
        .to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn ensure_domain(domain: &str, allowed: &[&str]) -> Result<()> {
    if allowed.contains(&domain) {
        Ok(())
    } else {
        anyhow::bail!(
            "action is not supported for Home Assistant domain '{}'",
            domain
        )
    }
}

fn normalize_brightness(value: f64) -> u8 {
    if value <= 100.0 {
        ((value.clamp(0.0, 100.0) / 100.0) * 255.0).round() as u8
    } else {
        value.clamp(0.0, 255.0).round() as u8
    }
}

fn summarize_state(target_name: &str, domain: Option<&str>, entities: &[Entity]) -> String {
    if entities.is_empty() {
        return format!("I couldn't read the state for {}.", target_name);
    }

    if entities.len() == 1 {
        let entity = &entities[0];
        return match domain.unwrap_or_default() {
            "light" => {
                let brightness = entity
                    .attributes
                    .get("brightness")
                    .and_then(|value| value.as_u64())
                    .map(|value| {
                        // Round to the nearest percent, matching Home
                        // Assistant's own brightness_pct and the rounding
                        // `normalize_brightness` applies on the write path.
                        // Integer `value * 100 / 255` truncates, so a light
                        // set to 75% (stored 191/255) under-reported as 74%.
                        let percent = ((value as f64) * 100.0 / 255.0).round() as u64;
                        format!(", brightness {percent}%")
                    })
                    .unwrap_or_default();
                format!("{} is {}{}", target_name, entity.state, brightness)
            }
            "climate" => {
                let current = entity
                    .attributes
                    .get("current_temperature")
                    .and_then(|value| value.as_f64())
                    .map(|value| format!(", currently {} degrees", value))
                    .unwrap_or_default();
                let target = entity
                    .attributes
                    .get("temperature")
                    .and_then(|value| value.as_f64())
                    .map(|value| format!(", target {} degrees", value))
                    .unwrap_or_default();
                format!("{} is {}{}{}", target_name, entity.state, current, target)
            }
            _ => format!("{} is {}", target_name, entity.state),
        };
    }

    let on_like = entities
        .iter()
        .filter(|entity| matches!(entity.state.as_str(), "on" | "open" | "unlocked"))
        .count();
    let unavailable = entities
        .iter()
        .filter(|entity| entity.state == "unavailable")
        .count();

    let mut summary = format!(
        "{}: {} of {} are active",
        target_name,
        on_like,
        entities.len()
    );
    if unavailable > 0 {
        summary.push_str(&format!(", {} unavailable", unavailable));
    }
    summary
}

fn build_action_summary(action: &HomeAction, state_snapshot: Option<&HomeState>) -> String {
    if let Some(state) = state_snapshot {
        return state.spoken_summary.clone();
    }

    let verb = match action.kind {
        HomeActionKind::TurnOn => "turned on",
        HomeActionKind::TurnOff => "turned off",
        HomeActionKind::Toggle => "toggled",
        HomeActionKind::SetBrightness => "adjusted",
        HomeActionKind::SetTemperature => "updated",
        HomeActionKind::Open => "opened",
        HomeActionKind::Close => "closed",
        HomeActionKind::Lock => "locked",
        HomeActionKind::Unlock => "unlocked",
        HomeActionKind::Activate => "activated",
    };
    format!("{} {}", verb, action.target.display_name)
}

fn domain_label(domain: &str, count: usize) -> &'static str {
    match domain {
        "light" if count == 1 => "light",
        "light" => "lights",
        "switch" if count == 1 => "switch",
        "switch" => "switches",
        "fan" if count == 1 => "fan",
        "fan" => "fans",
        "cover" if count == 1 => "cover",
        "cover" => "covers",
        "lock" if count == 1 => "lock",
        "lock" => "locks",
        "climate" => "thermostat",
        _ => "devices",
    }
}

fn domain_plural_label(domain: &str) -> &'static str {
    match domain {
        "light" => "lights",
        "switch" => "switches",
        "fan" => "fans",
        "cover" => "covers",
        "lock" => "locks",
        "climate" => "thermostats",
        _ => "devices",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_graph() -> HomeGraph {
        HomeGraph {
            areas: vec![
                AreaRef {
                    id: "living_room".into(),
                    name: "Living Room".into(),
                    aliases: vec!["living room".into()],
                },
                AreaRef {
                    id: "bedroom".into(),
                    name: "Bedroom".into(),
                    aliases: vec!["bedroom".into()],
                },
            ],
            devices: vec![],
            entities: vec![
                EntityRef {
                    entity_id: "light.living_room_lamp".into(),
                    name: "Living Room Lamp".into(),
                    domain: "light".into(),
                    area: Some("Living Room".into()),
                    aliases: vec![
                        "living room lamp".into(),
                        "lamp".into(),
                        "living room light".into(),
                        "living room lights".into(),
                    ],
                    state: "off".into(),
                    capabilities: vec!["turn_on".into()],
                },
                EntityRef {
                    entity_id: "climate.living_room".into(),
                    name: "Living Room Thermostat".into(),
                    domain: "climate".into(),
                    area: Some("Living Room".into()),
                    aliases: vec!["living room thermostat".into(), "thermostat".into()],
                    state: "heat".into(),
                    capabilities: vec!["set_temperature".into()],
                },
            ],
            scenes: vec![SceneRef {
                entity_id: "scene.movie_night".into(),
                name: "Movie Night".into(),
                area: Some("Living Room".into()),
                aliases: vec!["movie night".into(), "living room movie night".into()],
                voice_safe: true,
            }],
            scripts: vec![ScriptRef {
                entity_id: "script.voice_relax".into(),
                name: "Voice Relax".into(),
                area: Some("Living Room".into()),
                aliases: vec!["voice relax".into(), "relax".into()],
                voice_safe: true,
            }],
            aliases: vec![],
            domains: vec!["light".into(), "climate".into(), "scene".into()],
            capabilities: vec![],
        }
    }

    fn two_lamp_graph() -> HomeGraph {
        // Two distinct lamps in different rooms. Both carry the shared "lamp"
        // domain alias and a shared custom "reading lamp" alias, so an
        // under-specified query ties at the top score across both devices.
        HomeGraph {
            areas: vec![
                AreaRef {
                    id: "living_room".into(),
                    name: "Living Room".into(),
                    aliases: vec!["living room".into()],
                },
                AreaRef {
                    id: "bedroom".into(),
                    name: "Bedroom".into(),
                    aliases: vec!["bedroom".into()],
                },
            ],
            devices: vec![],
            entities: vec![
                EntityRef {
                    entity_id: "light.sofa_lamp".into(),
                    name: "Sofa Lamp".into(),
                    domain: "light".into(),
                    area: Some("Living Room".into()),
                    aliases: vec![
                        "sofa lamp".into(),
                        "reading lamp".into(),
                        "lamp".into(),
                        "light".into(),
                        "lights".into(),
                    ],
                    state: "off".into(),
                    capabilities: vec!["turn_on".into()],
                },
                EntityRef {
                    entity_id: "light.bed_lamp".into(),
                    name: "Bed Lamp".into(),
                    domain: "light".into(),
                    area: Some("Bedroom".into()),
                    aliases: vec![
                        "bed lamp".into(),
                        "reading lamp".into(),
                        "lamp".into(),
                        "light".into(),
                        "lights".into(),
                    ],
                    state: "off".into(),
                    capabilities: vec!["turn_on".into()],
                },
            ],
            scenes: vec![],
            scripts: vec![],
            aliases: vec![],
            domains: vec!["light".into()],
            capabilities: vec![],
        }
    }

    fn three_lamp_graph() -> HomeGraph {
        let mut graph = two_lamp_graph();
        graph.areas.push(AreaRef {
            id: "office".into(),
            name: "Office".into(),
            aliases: vec!["office".into()],
        });
        graph.entities.push(EntityRef {
            entity_id: "light.desk_lamp".into(),
            name: "Desk Lamp".into(),
            domain: "light".into(),
            area: Some("Office".into()),
            aliases: vec![
                "desk lamp".into(),
                "reading lamp".into(),
                "lamp".into(),
                "light".into(),
                "lights".into(),
            ],
            state: "off".into(),
            capabilities: vec!["turn_on".into()],
        });
        graph
    }

    #[test]
    fn format_disambiguation_candidates_reads_cleanly_for_three_or_more() {
        assert_eq!(
            format_disambiguation_candidates(&[
                "Desk Lamp".into(),
                "Sofa Lamp".into(),
                "Bed Lamp".into(),
            ]),
            "Desk Lamp, Sofa Lamp, or Bed Lamp"
        );
        assert_eq!(
            format_disambiguation_candidates(&["Sofa Lamp".into(), "Bed Lamp".into()]),
            "Sofa Lamp or Bed Lamp"
        );
    }

    #[test]
    fn ambiguous_three_way_tie_names_all_candidates_with_oxford_comma() {
        let graph = three_lamp_graph();
        let mut candidates = ambiguous_named_entity_candidates(&graph, "reading lamp")
            .expect("'reading lamp' ties across three devices");
        candidates.sort();
        assert_eq!(
            candidates,
            vec![
                "Bed Lamp".to_string(),
                "Desk Lamp".to_string(),
                "Sofa Lamp".to_string(),
            ]
        );
        assert_eq!(
            format_disambiguation_candidates(&candidates),
            "Bed Lamp, Desk Lamp, or Sofa Lamp"
        );
        assert!(
            HomeAssistantProvider::resolve_target_in_graph(
                &graph,
                "reading lamp",
                Some(HomeActionKind::TurnOn)
            )
            .is_none()
        );
    }

    #[test]
    fn build_graph_admits_fan_entities_and_resolves_them() {
        // Regression: `fan` is a controllable domain in the quick router and
        // `infer_domain`, but `is_voice_relevant_domain` omitted it, so
        // `build_graph` dropped every fan entity and "turn on the bedroom fan"
        // resolved to nothing even though the fan existed.
        let states = vec![Entity {
            entity_id: "fan.bedroom_ceiling".into(),
            state: "off".into(),
            attributes: serde_json::json!({ "friendly_name": "Bedroom Ceiling Fan" }),
        }];
        let areas = vec![AreaTemplateEntry {
            id: "bedroom".into(),
            name: "Bedroom".into(),
            entities: vec!["fan.bedroom_ceiling".into()],
        }];

        let graph = HomeAssistantProvider::build_graph(&states, &areas);

        // The fan survives the voice-relevant-domain gate.
        assert!(
            graph
                .entities
                .iter()
                .any(|entity| entity.entity_id == "fan.bedroom_ceiling"),
            "fan entity should be admitted to the graph"
        );

        // ...and resolves for a natural "turn on the bedroom fan" request.
        let target = HomeAssistantProvider::resolve_target_in_graph(
            &graph,
            "bedroom fan",
            Some(HomeActionKind::TurnOn),
        )
        .expect("bedroom fan should resolve to the fan entity");
        assert_eq!(target.domain.as_deref(), Some("fan"));
        assert!(
            target
                .entity_ids
                .contains(&"fan.bedroom_ceiling".to_string())
        );
    }

    #[test]
    fn ambiguous_named_entity_tie_declines_instead_of_guessing() {
        // Regression for #511: both lamps score 1.0 on the shared "lamp" alias.
        // The resolver must not silently actuate whichever the graph listed first.
        let graph = two_lamp_graph();
        assert!(
            HomeAssistantProvider::resolve_named_entity(
                &graph,
                "lamp",
                Some(HomeActionKind::TurnOn)
            )
            .is_none(),
            "ambiguous 'lamp' must decline rather than resolve to the first lamp"
        );
    }

    #[test]
    fn unique_named_entity_still_resolves_after_tie_guard() {
        // A query that uniquely identifies one device is unaffected by the guard.
        let graph = two_lamp_graph();
        let target = HomeAssistantProvider::resolve_named_entity(
            &graph,
            "sofa lamp",
            Some(HomeActionKind::TurnOn),
        )
        .expect("'sofa lamp' uniquely identifies one device");
        assert_eq!(target.entity_ids, vec!["light.sofa_lamp"]);
    }

    #[test]
    fn resolve_target_declines_ambiguous_shared_alias_end_to_end() {
        // "reading lamp" is a custom alias on both lamps. The area-less domain
        // group declines (the "reading" token names no real place), so resolution
        // reaches the named path and must decline the tie instead of guessing.
        let graph = two_lamp_graph();
        assert!(
            HomeAssistantProvider::resolve_target_in_graph(
                &graph,
                "reading lamp",
                Some(HomeActionKind::TurnOn)
            )
            .is_none()
        );
    }

    #[test]
    fn ambiguous_candidates_name_the_tied_devices() {
        let graph = two_lamp_graph();
        let mut candidates = ambiguous_named_entity_candidates(&graph, "reading lamp")
            .expect("'reading lamp' is ambiguous across both lamps");
        candidates.sort();
        assert_eq!(
            candidates,
            vec!["Bed Lamp".to_string(), "Sofa Lamp".to_string()]
        );
        // A uniquely-identifying query reports no ambiguity.
        assert!(ambiguous_named_entity_candidates(&graph, "sofa lamp").is_none());
    }

    #[test]
    fn resolve_group_target_by_area_and_domain() {
        let graph = sample_graph();
        let target = HomeAssistantProvider::resolve_group_target(
            &graph,
            "living room lights",
            "light",
            "Living Room",
            0.9,
        )
        .unwrap();

        assert_eq!(target.kind, HomeTargetKind::Group);
        assert_eq!(target.domain.as_deref(), Some("light"));
        assert_eq!(target.entity_ids, vec!["light.living_room_lamp"]);
    }

    #[test]
    fn resolve_domain_target_for_whole_home_status() {
        let graph = sample_graph();
        let target =
            HomeAssistantProvider::resolve_target_in_graph(&graph, "lights", None).unwrap();

        assert_eq!(target.kind, HomeTargetKind::Group);
        assert_eq!(target.display_name, "All lights");
        assert_eq!(target.domain.as_deref(), Some("light"));
        assert_eq!(target.entity_ids, vec!["light.living_room_lamp"]);
    }

    #[test]
    fn resolve_target_rejects_foreign_room_whole_home_fallback() {
        let graph = sample_graph();
        assert!(
            HomeAssistantProvider::resolve_target_in_graph(&graph, "upstairs lights", None)
                .is_none()
        );
    }

    #[test]
    fn query_rejects_whole_home_fidelity_for_foreign_room() {
        let graph = sample_graph();
        assert!(HomeAssistantProvider::query_rejects_whole_home_fidelity(
            &graph,
            "upstairs lights"
        ));
    }

    #[test]
    fn resolve_exact_entity_id_before_fuzzy_matching() {
        let graph = sample_graph();
        let target =
            HomeAssistantProvider::resolve_target_in_graph(&graph, "light.living_room_lamp", None)
                .unwrap();

        assert_eq!(target.kind, HomeTargetKind::Entity);
        assert_eq!(target.display_name, "Living Room Lamp");
        assert_eq!(target.confidence, 1.0);
        assert_eq!(target.entity_ids, vec!["light.living_room_lamp"]);
    }

    #[test]
    fn resolve_scene_for_activate_prefers_scene() {
        let graph = sample_graph();
        let target = HomeAssistantProvider::resolve_named_entity(
            &graph,
            "movie night",
            Some(HomeActionKind::Activate),
        )
        .unwrap();

        assert_eq!(target.kind, HomeTargetKind::Scene);
        assert_eq!(target.entity_ids, vec!["scene.movie_night"]);
    }

    #[test]
    fn infer_domain_from_household_language() {
        assert_eq!(
            infer_domain("make the bedroom warmer").as_deref(),
            Some("climate")
        );
        assert_eq!(
            infer_domain("turn off the living room lamps").as_deref(),
            Some("light")
        );
    }

    #[test]
    fn infer_domain_matches_whole_words_not_substrings() {
        assert_eq!(
            infer_domain("open the back blinds").as_deref(),
            Some("cover")
        );
        assert_eq!(infer_domain("lock the back door").as_deref(), Some("lock"));
        assert_eq!(infer_domain("set the ac to 70").as_deref(), Some("climate"));
        assert_eq!(infer_domain("track the package").as_deref(), None);
    }

    #[test]
    fn brightness_percent_maps_to_255_scale() {
        assert_eq!(normalize_brightness(50.0), 128);
        assert_eq!(normalize_brightness(255.0), 255);
    }

    /// Regression: the `home_status` brightness readback must round to the
    /// nearest percent, matching Home Assistant's own `brightness_pct` and the
    /// rounding `normalize_brightness` applies on the write path. Integer
    /// `value * 100 / 255` truncates, so a light set to 75% (stored 191/255)
    /// used to read back as "74%" and 99% (stored 252) as "98%".
    #[test]
    fn brightness_readback_rounds_to_nearest_percent() {
        let light = |b: u64| Entity {
            entity_id: "light.living".into(),
            state: "on".into(),
            attributes: serde_json::json!({ "brightness": b }),
        };
        // 191/255 = 74.9% -> 75%, not the truncated 74%.
        let summary = summarize_state("Living room lamp", Some("light"), &[light(191)]);
        assert!(
            summary.contains("brightness 75%"),
            "expected rounded 75%, got: {summary}"
        );
        // 252/255 = 98.8% -> 99%.
        let summary = summarize_state("Living room lamp", Some("light"), &[light(252)]);
        assert!(
            summary.contains("brightness 99%"),
            "expected rounded 99%, got: {summary}"
        );
        // Full scale still reports exactly 100%.
        let summary = summarize_state("Living room lamp", Some("light"), &[light(255)]);
        assert!(
            summary.contains("brightness 100%"),
            "expected 100%, got: {summary}"
        );
    }
}
