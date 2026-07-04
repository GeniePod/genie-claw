//! Household/device-alias content parsers for the memory subsystem.
//!
//! Extracted verbatim from `memory/mod.rs` (module split, no behavior change):
//! the `*_from_memory` free functions that parse a stored memory string into
//! structured household profiles, device aliases, rules, and notes.

use super::*;

pub(super) fn household_profile_from_memory(
    _kind: &str,
    content: &str,
) -> Option<(String, &'static str)> {
    let lower = content.to_ascii_lowercase();

    if let Some((role, name)) = possessive_named_profile(content, &lower) {
        return Some((name, role));
    }

    if let Some((role, name)) = definite_role_profile(content, &lower) {
        return Some((name, role));
    }

    if let Some((name, role)) = subject_role_profile(content, &lower) {
        return Some((name, role));
    }

    None
}

pub(super) fn device_alias_from_memory(content: &str) -> Option<(String, String)> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();

    for marker in [
        " maps to ",
        " points to ",
        " targets ",
        " target is ",
        " entity is ",
        " device is ",
        " means ",
        " = ",
        " -> ",
        " is ",
    ] {
        if let Some(pos) = lower.find(marker) {
            let alias = clean_device_alias(&trimmed[..pos]);
            let target = clean_device_target(&trimmed[pos + marker.len()..]);
            if is_valid_device_alias_pair(&alias, &target, marker == " is ") {
                return Some((alias, target));
            }
        }
    }

    None
}

pub(super) fn clean_device_alias(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("remember that ")
        .trim_start_matches("remember ")
        .trim_start_matches("the ")
        .trim_matches(|ch: char| matches!(ch, '"' | '\''))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn clean_device_target(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("the ")
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '.' | ',' | '!' | '?'))
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

pub(super) fn is_valid_device_alias_pair(alias: &str, target: &str, broad_marker: bool) -> bool {
    if alias.is_empty() || target.is_empty() || alias.eq_ignore_ascii_case(target) {
        return false;
    }

    let alias_lower = alias.to_ascii_lowercase();
    let target_lower = target.to_ascii_lowercase();
    let looks_like_target = target_lower.contains('.') || target_lower.starts_with("smartplug_");
    let looks_like_alias = [
        "light",
        "lights",
        "lamp",
        "plug",
        "switch",
        "outlet",
        "thermostat",
        "scene",
        "routine",
        "fan",
    ]
    .iter()
    .any(|term| alias_lower.contains(term));

    let explicit_alias_shape = !broad_marker && alias.split_whitespace().count() <= 6;

    looks_like_target && (looks_like_alias || explicit_alias_shape)
}

pub(super) fn normalize_alias_key(value: &str) -> String {
    value
        .to_ascii_lowercase()
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

pub(super) fn device_alias_kind(target_id: &str) -> String {
    target_id
        .split_once('.')
        .map(|(domain, _)| domain.to_string())
        .unwrap_or_else(|| "entity".into())
}

pub(super) fn household_profile_attributes_from_memory(
    content: &str,
) -> Vec<HouseholdProfileAttribute> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let mut attrs = Vec::new();

    if let Some((name, rest)) = split_once_case_insensitive(trimmed, &lower, " is ") {
        let rest_lower = rest.to_ascii_lowercase();
        if let Some(age) = leading_age(&rest_lower) {
            attrs.push(profile_attr(name, "age", &age.to_string()));
        }
    }

    for marker in [" likes ", " prefers ", " enjoys "] {
        if let Some((name, value)) = split_once_case_insensitive(trimmed, &lower, marker) {
            let value = clean_sentence_value(value);
            if !value.is_empty() {
                attrs.push(profile_attr(name, "likes", &value));
            }
        }
    }

    if lower.contains("shoe")
        && let Some((name, value)) = shoe_size_statement(trimmed, &lower)
    {
        attrs.push(profile_attr(&name, "shoe_size", &value));
    }

    if let Some((left, value)) = split_once_case_insensitive(trimmed, &lower, " is ")
        && left.to_ascii_lowercase().contains("favorite ")
    {
        let name = left
            .split_once("'s ")
            .map(|(name, _)| name)
            .unwrap_or("household");
        let attribute = left
            .to_ascii_lowercase()
            .split("favorite ")
            .nth(1)
            .map(|subject| format!("favorite_{}", normalize_rule_subject(subject)))
            .unwrap_or_else(|| "favorite".into());
        let value = clean_sentence_value(value);
        if !value.is_empty() {
            attrs.push(profile_attr(name, &attribute, &value));
        }
    }

    attrs
}

pub(super) fn shoe_size_statement(content: &str, lower: &str) -> Option<(String, String)> {
    for marker in [" currently wears ", " now wears ", " wears "] {
        if let Some((name, rest)) = split_once_case_insensitive(content, lower, marker) {
            let rest_lower = rest.to_ascii_lowercase();
            if !rest_lower.contains("size") && !rest_lower.contains("shoe") {
                continue;
            }
            let value = rest
                .trim_start_matches("a ")
                .trim_start_matches("an ")
                .trim_start_matches("shoe ")
                .trim_start_matches("size ")
                .trim_start_matches("shoe size ");
            return Some((clean_person_name(name), clean_sentence_value(value)));
        }
    }

    if let Some((left, value)) = split_once_case_insensitive(content, lower, " shoe size is ") {
        return Some((clean_person_name(left), clean_sentence_value(value)));
    }

    None
}

pub(super) fn household_rules_from_memory(content: &str) -> Vec<HouseholdRule> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let mut rules = Vec::new();

    if lower.contains("allerg")
        && let Some((person, subject)) = parse_allergy_rule(trimmed, &lower)
    {
        rules.push(HouseholdRule {
            source_memory_id: 0,
            person: Some(person),
            rule_type: "allergy".into(),
            subject,
            value: None,
            allowed: false,
            description: trimmed.to_string(),
        });
    }

    if (lower.contains("screen time") || lower.contains("gaming") || lower.contains("video game"))
        && (lower.contains("after ") || lower.contains("ends at "))
        && let Some((person, subject, value)) = parse_screen_time_rule(trimmed, &lower)
    {
        rules.push(HouseholdRule {
            source_memory_id: 0,
            person: Some(person),
            rule_type: "screen_time".into(),
            subject,
            value: Some(value),
            allowed: false,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("homework")
        && let Some(person) = leading_person_name(trimmed)
    {
        rules.push(HouseholdRule {
            source_memory_id: 0,
            person: Some(person),
            rule_type: "homework".into(),
            subject: "homework".into(),
            value: if lower.contains("before screen") {
                Some("before_screen_time".into())
            } else {
                None
            },
            allowed: true,
            description: trimmed.to_string(),
        });
    }

    rules
}

pub(super) fn household_note_from_memory(
    kind: &str,
    content: &str,
) -> Option<(String, String, String)> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    if trimmed.is_empty() {
        return None;
    }

    let kind_lower = kind.to_ascii_lowercase();
    let lower = trimmed.to_ascii_lowercase();
    if secret_type_from_text(&lower).is_some() {
        return None;
    }
    let (note_type, note_content) = if matches!(
        kind_lower.as_str(),
        "note"
            | "notes"
            | "reminder"
            | "manual"
            | "document"
            | "context"
            | "pet_health"
            | "home_maintenance"
            | "storage"
            | "gift"
            | "recipe"
            | "recipe_book"
            | "mechanic"
            | "troubleshooting"
            | "activity"
            | "media_library"
            | "routine"
            | "safe_inventory"
            | "appliance_manual"
            | "photo_metadata"
            | "warranty"
            | "school"
            | "utility"
            | "recycling"
            | "wellness"
            | "science_project"
            | "first_aid"
            | "medicine"
            | "audiobook"
            | "story"
            | "pet_inventory"
            | "travel"
            | "travel_document"
            | "travel_documents"
            | "diet"
            | "watch_history"
            | "doorbell"
            | "visitor"
            | "music_profile"
            | "device_manual"
            | "device_manuals"
            | "home_note"
            | "home_notes"
            | "home_inventory"
            | "meal_history"
            | "recipe_collection"
            | "shopping_list"
            | "shopping_lists"
            | "school_info"
            | "security_log"
            | "beverage"
            | "beverage_preference"
            | "social_connection"
            | "commute"
            | "pantry"
            | "comfort"
            | "location_history"
            | "tracker"
            | "pizza"
            | "arrival"
            | "financial_record"
            | "financial_records"
            | "digital_scan"
            | "digital_scans"
            | "storage_inventory"
            | "game_manual"
            | "game_manuals"
            | "educational_resource"
            | "educational_resources"
            | "entertainment"
            | "dictionary"
            | "dictionary_knowledge_base"
            | "activity_idea"
            | "activity_ideas"
            | "air_quality"
            | "health_profile"
            | "party_theme"
            | "party_themes"
            | "pest_control"
            | "family_reaction"
            | "family_reactions"
            | "food_safety"
            | "contact_book"
            | "social_graph"
            | "educational_content"
            | "documentary_library"
            | "productivity_tip"
            | "productivity_tips"
            | "sleep_routine"
            | "meal_plan"
            | "delivery_instruction"
            | "delivery_instructions"
            | "shipping_tracking"
            | "flight_info"
            | "traffic_to_airport"
            | "travel_preference"
            | "travel_preferences"
            | "party_recipe"
            | "party_recipes"
            | "pet_calendar"
            | "astronomical_data"
            | "payment_history"
            | "tool_inventory"
            | "digital_receipts"
            | "scanned_docs"
            | "network_config"
            | "health_tracker"
            | "cooking_substitutes"
            | "diy_projects"
            | "material_lists"
            | "plumbing_troubleshooting"
            | "injury_recovery"
            | "health_tips"
            | "gym_schedule"
            | "gym_routine"
            | "financial_advice"
            | "contacts"
            | "message_templates"
            | "location_api"
            | "arrival_rain"
            | "safety_protocol"
            | "streaming_services"
            | "user_location"
            | "turkey_thawing_guide"
            | "safety_equipment_log"
            | "school_documents"
            | "contractor_list"
            | "recipe_notes"
            | "wish_list"
            | "interests_profile"
            | "wellness_activities"
            | "food_pairing_database"
            | "device_profiles"
            | "gift_history"
            | "board_games"
            | "baby_monitor_logs"
            | "news_sources"
            | "appliance_states"
            | "waste_management_log"
            | "environmental_sensors"
            | "location_services"
            | "garden_devices"
            | "appliance_manuals"
            | "security_codes"
            | "subscription_credentials"
            | "music_library"
            | "ebook_store"
            | "read_history"
            | "restaurant_history"
            | "delivery_apps"
            | "plant_care"
            | "weight_trend"
            | "lunch_preferences"
            | "outdoor_furniture"
            | "cycling_route"
            | "financial_services"
            | "smart_plug"
            | "electronic_program_guide"
            | "water_heater_sensor"
            | "craft_inventory"
            | "secure_storage_log"
            | "vehicle_registration"
            | "appliance_warranties"
            | "network_credentials"
            | "local_business_reviews"
            | "wardrobe_inventory"
            | "event_dress_code"
            | "wellness_content"
            | "education_app"
            | "takeout_menus"
            | "hotel_preferences"
            | "maintenance_schedule"
            | "filter_model_number"
            | "routine_logs"
            | "family_activities"
            | "plumbing_history"
            | "sewing_instructions"
            | "breathing_monitor"
            | "smart_scale"
            | "connected_car"
            | "printer_status"
            | "financial_market_api"
            | "pool_robot"
            | "backyard_devices"
            | "baby_monitor"
            | "navigation_service"
            | "smart_lock"
            | "shipping_tracker"
            | "digital_documents"
            | "vehicle_documents"
            | "subscriptions"
            | "cooking_reference"
            | "hobby_inventory"
            | "tutorial_videos"
            | "health_advice"
            | "local_businesses"
            | "charity_ratings"
            | "personal_interests"
            | "language_apps"
            | "podcast_library"
            | "audio_library"
            | "wardrobe_database"
            | "fashion_advice"
            | "beverage_prefs"
            | "uv_index"
            | "sun_safety"
            | "friend_availability"
            | "favorite_dishes"
            | "fever_management"
            | "snow_protocol"
            | "device_usage"
            | "site_category"
            | "weather_video_url"
            | "preferred_presenter"
            | "mood_context"
            | "smart_oven"
            | "plumbing_sensors"
            | "basement_monitoring"
            | "fitness_tracker"
            | "kitchen_appliances"
            | "air_quality_monitor"
            | "appliance_docs"
            | "shoe_closet_inventory"
            | "password_manager"
            | "community_calendar"
            | "restaurant_list"
            | "home_warranties"
            | "network_device_list"
            | "financial_archive"
            | "story_library"
            | "literature_database"
            | "local_trail_database"
            | "photo_album"
            | "object_recognition"
            | "pet_names_db"
            | "educational_video"
            | "camping_checklist"
            | "bar_inventory"
            | "restaurants"
            | "babysitter_availability"
            | "dinner_plan"
            | "water_sensor"
            | "bike_tracker"
            | "security_logs"
            | "taco_bar_ingredients"
            | "user_profiles"
            | "presence_state"
            | "device_states"
            | "comfort_preference_embeddings"
            | "activity_preference_embeddings"
            | "room_mood_embeddings"
            | "sleep_preference_embeddings"
            | "safety_intent_embeddings"
            | "parental_rules"
            | "screen_time_usage"
            | "family_schedule"
            | "inventory_items"
            | "notes_fts"
            | "documents"
            | "last_opened_locations"
            | "manuals_fts"
            | "document_store"
            | "scenes"
            | "scene_actions"
            | "ambient_light_sensors"
            | "reminders"
            | "routine_steps"
            | "access_logs"
            | "device_events"
            | "health_device_events"
            | "delivery_events"
            | "camera_object_events"
            | "shopping_notes_fts"
            | "watering_schedule"
            | "garden_zones"
            | "soil_moisture_sensors"
            | "recipes_fts"
            | "recipe_embeddings"
            | "meal_ratings"
            | "automation_runs"
            | "sensor_health"
            | "alarms"
            | "automation_rules"
            | "appliance_thresholds"
            | "sensor_reading_history"
            | "item_location_events"
            | "motion_events"
            | "camera_devices"
            | "replacement_parts"
            | "room_assignments"
            | "pet_care_routines"
            | "pet_device_events"
            | "household_guides_fts"
            | "household_notes_fts"
            | "family_notes_fts"
            | "family_rules"
            | "family_preference_embeddings"
            | "notification_rules"
            | "vent_states"
            | "blind_positions"
            | "meal_notes"
            | "chore_assignments"
            | "chore_checkins"
            | "energy_meter_readings"
            | "documents_fts"
            | "shared_room_reservations"
            | "school_transport_schedule"
            | "door_sensor_events"
            | "temperature_sensors"
            | "gas_sensors"
            | "stove_state"
            | "presence_alerts"
            | "presence_alert"
            | "document_embeddings"
            | "shopping_list_items"
            | "routine_overrides"
            | "school_notes_fts"
            | "smart_plug_states"
            | "lighting_simulation_rules"
            | "thermostat_schedule_overrides"
            | "lock_check_rules"
            | "food_inventory"
            | "vacuum_zones"
            | "restricted_zones"
            | "do_not_disturb_rule"
            | "irrigation_events"
            | "safety_profiles"
            | "permission_requests"
            | "approval_events"
            | "health_documents_fts"
            | "scene_embeddings"
            | "weather_context"
            | "calendar_events"
            | "reservation"
            | "ble_tag_events"
            | "vacuum_events"
            | "room_map_zones"
            | "obstacle_reports"
            | "device_audit_log"
            | "control_source"
            | "family_calendar"
            | "fan_states"
            | "water_leak_sensors"
            | "health_routines"
            | "medicine_cabinet_events"
            | "activity_notes_fts"
            | "learning_history"
            | "device_metadata"
            | "audio_event_classifications"
            | "device_alerts"
            | "battery_status"
            | "network_access_rules"
            | "school_tasks"
            | "trusted_contacts"
            | "security_audit_log"
            | "guest_profiles"
            | "door_open_events"
            | "daily_checklists"
            | "user_preferences"
            | "floor_plan_graph"
            | "safety_routes"
            | "smoke_detector_locations"
            | "door_window_sensor_states"
            | "project_notes_fts"
            | "home_project_records"
            | "glass_break_sensors"
            | "camera_events"
            | "child_contact_rules"
            | "device_health"
            | "child_profiles"
            | "laundry_events"
            | "hvac_runtime"
            | "window_sensor_states"
            | "appliance_events"
            | "notification_log"
            | "air_quality_sensors"
            | "filter_status"
            | "filter_life"
            | "municipal_schedule"
            | "household_routines"
            | "routine_checkins"
            | "home_project_notes_fts"
            | "electrical_panel_map"
            | "item_embeddings"
            | "timers"
            | "home_maintenance_embeddings"
            | "scheduled_device_actions"
            | "medicine_inventory"
            | "guest_access_policies"
            | "household_notes"
            | "alarm_preferences"
            | "media_sessions"
            | "user_media_aliases"
            | "media_preference_embeddings"
            | "kitchen_timers"
            | "temporary_notifications"
            | "device_safety_profiles"
            | "security_mode_attempts"
            | "lock_errors"
            | "device_notes_fts"
            | "device_credentials"
            | "open_reminders"
            | "plant_care_profiles"
            | "last_watered_events"
            | "dishwasher_rack_state"
            | "kitchen_item_locations"
            | "door_sensor_states"
            | "project_lists"
            | "project_list_items"
            | "sensor_alert_rules"
            | "privacy_audit_log"
            | "family_messages"
            | "message_events"
            | "camera_access_logs"
            | "privacy_mode_events"
            | "camera_recording_rules"
            | "meal_memory_embeddings"
            | "child_media_rules"
            | "media_preferences"
            | "pet_feeding_events"
            | "pet_care_profiles"
            | "outdoor_air_quality_feed"
            | "indoor_air_quality_sensors"
            | "power_events"
            | "holiday_calendar"
            | "alarm_preference_embeddings"
            | "network_clients"
            | "known_devices"
            | "outdoor_temperature_sensors"
            | "moisture_sensors"
            | "router_stats"
            | "bandwidth_usage"
            | "activity_templates"
            | "camera_health"
            | "image_quality_metrics"
            | "maintenance_notes"
            | "garage_door_events"
            | "water_pressure_sensors"
            | "home_utility_thresholds"
            | "water_valve_state"
            | "camera_person_events"
            | "humidity_sensors"
            | "dehumidifier_state"
            | "filter_life_remaining"
            | "user_print_rules"
            | "printer_supplies"
            | "cooking_sessions"
            | "appliance_safety_profiles"
            | "camera_privacy_rules"
            | "room_sensor_history"
            | "do_not_disturb_rules"
            | "temporary_mode_overrides"
            | "lock_status"
            | "smoke_detector_status"
            | "security_modes"
            | "recipe_ingredients"
            | "school_forms"
            | "meal_notes_fts"
    ) {
        (note_type_from_kind(&kind_lower, &lower), trimmed)
    } else if let Some(rest) = lower
        .strip_prefix("remember that ")
        .and_then(|_| trimmed.get("remember that ".len()..))
    {
        ("note", rest.trim())
    } else if let Some(rest) = lower
        .strip_prefix("remember to ")
        .and_then(|_| trimmed.get("remember to ".len()..))
    {
        ("reminder", rest.trim())
    } else if let Some(rest) = lower
        .strip_prefix("note: ")
        .and_then(|_| trimmed.get("note: ".len()..))
    {
        ("note", rest.trim())
    } else if lower.contains(" manual:") || lower.starts_with("manual:") {
        ("manual", trimmed)
    } else if lower.starts_with("watched ") || lower.contains(" watched ") {
        ("media", trimmed)
    } else {
        return None;
    };

    let note_content = note_content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    if note_content.is_empty() {
        return None;
    }

    Some((
        note_type.to_string(),
        household_note_title(note_content),
        note_content.to_string(),
    ))
}

pub(super) fn note_type_from_kind<'a>(kind: &'a str, lower_content: &str) -> &'a str {
    match kind {
        "reminder" => "reminder",
        "manual" | "document" => "manual",
        "pet_health" => "pet_health",
        "home_maintenance" => "home_maintenance",
        "storage" => "storage",
        "gift" => "gift",
        "recipe" | "recipe_book" => "recipe",
        "mechanic" | "troubleshooting" => "troubleshooting",
        "activity" => "activity",
        "media_library" => "media",
        "routine" => "routine",
        "safe_inventory" => "storage",
        "appliance_manual" => "manual",
        "photo_metadata" => "photo",
        "warranty" => "warranty",
        "school" => "school",
        "utility" => "utility",
        "recycling" => "recycling",
        "wellness" => "wellness",
        "science_project" => "education",
        "first_aid" | "medicine" => "first_aid",
        "audiobook" | "story" => "story",
        "pet_inventory" => "pet",
        "travel" | "travel_document" | "travel_documents" => "travel",
        "diet" => "diet",
        "watch_history" => "media",
        "doorbell" | "visitor" => "visitor",
        "music_profile" => "media",
        "device_manual" | "device_manuals" => "manual",
        "home_note" | "home_notes" => "home_maintenance",
        "home_inventory" => "storage",
        "meal_history" => "meal",
        "recipe_collection" => "recipe",
        "shopping_list" | "shopping_lists" => "shopping",
        "school_info" => "school",
        "security_log" => "security",
        "beverage" | "beverage_preference" => "beverage",
        "social_connection" => "social",
        "commute" => "commute",
        "pantry" => "pantry",
        "comfort" => "home_comfort",
        "location_history" | "tracker" => "location",
        "pizza" => "shopping",
        "arrival" => "routine",
        "financial_record" | "financial_records" | "digital_scan" | "digital_scans" => "receipt",
        "storage_inventory" => "storage",
        "game_manual" | "game_manuals" => "manual",
        "educational_resource" | "educational_resources" => "education",
        "entertainment" => "entertainment",
        "dictionary" | "dictionary_knowledge_base" => "dictionary",
        "activity_idea" | "activity_ideas" => "activity",
        "air_quality" | "health_profile" => "health",
        "party_theme" | "party_themes" => "party",
        "pest_control" => "pest_control",
        "family_reaction" | "family_reactions" => "family",
        "food_safety" => "food_safety",
        "contact_book" | "social_graph" => "contact",
        "educational_content" | "documentary_library" => "education",
        "productivity_tip" | "productivity_tips" | "sleep_routine" => "routine",
        "meal_plan" => "meal",
        "delivery_instruction" | "delivery_instructions" | "shipping_tracking" => "delivery",
        "flight_info" | "traffic_to_airport" | "travel_preference" | "travel_preferences" => {
            "travel"
        }
        "party_recipe" | "party_recipes" => "party",
        "pet_calendar" => "pet",
        "astronomical_data" => "schedule",
        "payment_history" | "financial_advice" => "finance",
        "tool_inventory" => "tool",
        "digital_receipts" | "scanned_docs" => "receipt",
        "network_config" => "network",
        "health_tracker" | "injury_recovery" | "health_tips" => "health",
        "cooking_substitutes" => "recipe",
        "diy_projects" | "material_lists" => "diy",
        "plumbing_troubleshooting" => "home_maintenance",
        "gym_schedule" | "gym_routine" => "fitness",
        "contacts" | "message_templates" => "contact",
        "location_api" | "arrival_rain" | "safety_protocol" | "user_location" => "safety",
        "streaming_services" => "media",
        "turkey_thawing_guide" => "food_safety",
        "safety_equipment_log" => "safety",
        "school_documents" => "school",
        "contractor_list" => "contact",
        "recipe_notes" => "recipe",
        "wish_list" | "interests_profile" | "gift_history" => "gift",
        "wellness_activities" => "wellness",
        "food_pairing_database" => "recipe",
        "device_profiles" => "device",
        "board_games" => "entertainment",
        "baby_monitor_logs" => "routine",
        "news_sources" => "news",
        "appliance_states" => "device",
        "waste_management_log" => "schedule",
        "environmental_sensors" => "home_comfort",
        "location_services" => "location",
        "garden_devices" => "device",
        "appliance_manuals" => "manual",
        "security_codes" => "security",
        "subscription_credentials" => "security",
        "music_library" => "media",
        "ebook_store" | "read_history" => "entertainment",
        "restaurant_history" | "delivery_apps" => "meal",
        "plant_care" => "home_maintenance",
        "weight_trend" => "health",
        "lunch_preferences" => "meal",
        "outdoor_furniture" => "home_maintenance",
        "cycling_route" => "fitness",
        "financial_services" => "finance",
        "smart_plug" => "device",
        "electronic_program_guide" => "media",
        "water_heater_sensor" => "device",
        "craft_inventory" => "storage",
        "secure_storage_log" => "storage",
        "vehicle_registration" => "vehicle",
        "appliance_warranties" => "warranty",
        "network_credentials" => "network",
        "local_business_reviews" => "business",
        "wardrobe_inventory" | "event_dress_code" => "wardrobe",
        "wellness_content" => "wellness",
        "education_app" => "education",
        "takeout_menus" => "meal",
        "hotel_preferences" => "travel",
        "maintenance_schedule" | "filter_model_number" => "home_maintenance",
        "routine_logs" => "routine",
        "family_activities" => "activity",
        "plumbing_history" => "home_maintenance",
        "sewing_instructions" => "diy",
        "breathing_monitor" => "health",
        "smart_scale" => "health",
        "connected_car" => "vehicle",
        "printer_status" => "device",
        "financial_market_api" => "finance",
        "pool_robot" | "backyard_devices" => "device",
        "baby_monitor" => "health",
        "navigation_service" => "commute",
        "smart_lock" => "security",
        "shipping_tracker" => "delivery",
        "digital_documents" => "warranty",
        "vehicle_documents" => "vehicle",
        "subscriptions" => "finance",
        "cooking_reference" => "recipe",
        "hobby_inventory" | "tutorial_videos" => "activity",
        "health_advice" | "fever_management" => "health",
        "local_businesses" => "business",
        "charity_ratings" | "personal_interests" => "social",
        "language_apps" => "education",
        "podcast_library" | "audio_library" => "media",
        "wardrobe_database" | "fashion_advice" => "wardrobe",
        "beverage_prefs" => "beverage",
        "uv_index" | "sun_safety" => "safety",
        "friend_availability" => "social",
        "favorite_dishes" => "meal",
        "snow_protocol" => "home_maintenance",
        "device_usage" | "site_category" => "education",
        "weather_video_url" | "preferred_presenter" => "news",
        "mood_context" => "wellness",
        "smart_oven" | "plumbing_sensors" | "basement_monitoring" | "kitchen_appliances" => {
            "device"
        }
        "fitness_tracker" => "fitness",
        "air_quality_monitor" => "health",
        "appliance_docs" => "manual",
        "shoe_closet_inventory" => "storage",
        "password_manager" => "security",
        "community_calendar" => "schedule",
        "restaurant_list" => "contact",
        "home_warranties" => "warranty",
        "network_device_list" => "network",
        "financial_archive" => "finance",
        "story_library" => "story",
        "literature_database" => "literature",
        "local_trail_database" => "activity",
        "photo_album" | "object_recognition" => "photo",
        "pet_names_db" => "pet",
        "educational_video" => "education",
        "camping_checklist" => "activity",
        "bar_inventory" => "recipe",
        "restaurants" | "babysitter_availability" => "social",
        "dinner_plan" => "meal",
        "water_sensor" => "home_maintenance",
        "bike_tracker" | "security_logs" => "security",
        "taco_bar_ingredients" => "meal",
        "user_profiles" => "profile",
        "presence_state" | "last_opened_locations" | "item_location_events" => "location",
        "device_states"
        | "scene_actions"
        | "ambient_light_sensors"
        | "motion_events"
        | "camera_devices" => "device",
        "comfort_preference_embeddings"
        | "activity_preference_embeddings"
        | "room_mood_embeddings"
        | "family_preference_embeddings"
        | "scene_embeddings"
        | "sleep_preference_embeddings" => "home_comfort",
        "safety_intent_embeddings" => "safety",
        "parental_rules" | "screen_time_usage" => "family",
        "inventory_items" => "inventory",
        "family_schedule"
        | "school_transport_schedule"
        | "shared_room_reservations"
        | "reminders"
        | "alarms"
        | "presence_alerts"
        | "presence_alert"
        | "reservation" => "schedule",
        "notes_fts" | "documents" | "documents_fts" | "school_notes_fts" => "school",
        "manuals_fts" | "document_store" | "health_documents_fts" => "manual",
        "scenes" | "automation_rules" | "routine_steps" | "routine_overrides" => "routine",
        "access_logs" | "device_events" => "security",
        "health_device_events" => "health",
        "pet_care_routines" | "pet_device_events" => "pet",
        "delivery_events" | "camera_object_events" | "shopping_notes_fts" => "delivery",
        "watering_schedule" | "garden_zones" | "soil_moisture_sensors" | "irrigation_events" => {
            "garden"
        }
        "recipes_fts" | "recipe_embeddings" => "recipe",
        "meal_ratings" | "meal_notes" | "food_inventory" => "meal",
        "household_guides_fts" => "recycling",
        "household_notes_fts" => "home_maintenance",
        "family_notes_fts" | "family_rules" => "family",
        "chore_assignments" | "chore_checkins" => "family",
        "automation_runs" | "sensor_health" | "vent_states" | "blind_positions" => {
            "troubleshooting"
        }
        "appliance_thresholds"
        | "sensor_reading_history"
        | "door_sensor_events"
        | "temperature_sensors"
        | "energy_meter_readings"
        | "smart_plug_states" => "device",
        "room_assignments" | "vacuum_zones" | "restricted_zones" => "location",
        "ble_tag_events" => "location",
        "vacuum_events" | "room_map_zones" | "obstacle_reports" => "device",
        "device_audit_log" | "control_source" | "security_audit_log" => "security",
        "family_calendar" | "daily_checklists" => "schedule",
        "fan_states" | "device_metadata" | "device_health" => "device",
        "water_leak_sensors" | "glass_break_sensors" | "camera_events" => "safety",
        "health_routines" | "medicine_cabinet_events" => "health",
        "activity_notes_fts" | "learning_history" => "education",
        "audio_event_classifications" | "device_alerts" | "battery_status" => "troubleshooting",
        "network_access_rules" | "school_tasks" => "education",
        "trusted_contacts" | "guest_profiles" | "child_profiles" => "family",
        "door_open_events" => "delivery",
        "user_preferences" => "home_comfort",
        "floor_plan_graph"
        | "safety_routes"
        | "smoke_detector_locations"
        | "door_window_sensor_states" => "safety",
        "project_notes_fts" | "home_project_records" => "home_maintenance",
        "child_contact_rules" => "family",
        "laundry_events" | "appliance_events" | "notification_log" => "device",
        "hvac_runtime" | "window_sensor_states" => "home_comfort",
        "air_quality_sensors" | "filter_status" | "filter_life" => "health",
        "municipal_schedule" | "household_routines" | "routine_checkins" => "schedule",
        "home_project_notes_fts" | "electrical_panel_map" => "home_maintenance",
        "item_embeddings" => "inventory",
        "timers" | "scheduled_device_actions" | "alarm_preferences" => "schedule",
        "home_maintenance_embeddings" => "home_maintenance",
        "medicine_inventory" => "health",
        "guest_access_policies" => "family",
        "household_notes" => "home_maintenance",
        "media_sessions" | "user_media_aliases" | "media_preference_embeddings" => "media",
        "kitchen_timers" | "temporary_notifications" => "device",
        "device_safety_profiles" => "safety",
        "security_mode_attempts" | "lock_errors" => "security",
        "device_notes_fts" | "device_credentials" => "device",
        "open_reminders" => "schedule",
        "plant_care_profiles" | "last_watered_events" => "garden",
        "dishwasher_rack_state" | "kitchen_item_locations" => "inventory",
        "door_sensor_states" => "security",
        "project_lists" | "project_list_items" => "school",
        "sensor_alert_rules" => "device",
        "privacy_audit_log"
        | "camera_access_logs"
        | "privacy_mode_events"
        | "camera_recording_rules"
        | "camera_privacy_rules" => "privacy",
        "family_messages" | "message_events" => "family",
        "meal_memory_embeddings" => "meal",
        "child_media_rules" | "media_preferences" => "media",
        "pet_feeding_events" | "pet_care_profiles" => "pet",
        "outdoor_air_quality_feed" | "indoor_air_quality_sensors" => "health",
        "power_events" => "device",
        "holiday_calendar" => "schedule",
        "alarm_preference_embeddings" => "schedule",
        "network_clients" | "known_devices" | "router_stats" | "bandwidth_usage" => "network",
        "outdoor_temperature_sensors" | "moisture_sensors" => "safety",
        "activity_templates" => "activity",
        "camera_health" | "image_quality_metrics" | "maintenance_notes" => "home_maintenance",
        "garage_door_events" => "security",
        "water_pressure_sensors" | "home_utility_thresholds" | "water_valve_state" => "utility",
        "camera_person_events" => "security",
        "humidity_sensors" | "dehumidifier_state" | "filter_life_remaining" => "home_comfort",
        "user_print_rules" | "printer_supplies" => "device",
        "cooking_sessions" => "recipe",
        "appliance_safety_profiles" => "safety",
        "room_sensor_history" => "home_comfort",
        "do_not_disturb_rules" => "home_comfort",
        "temporary_mode_overrides" => "routine",
        "lock_status" | "security_modes" => "security",
        "smoke_detector_status" => "safety",
        "recipe_ingredients" => "recipe",
        "school_forms" => "school",
        "meal_notes_fts" => "meal",
        "notification_rules" | "do_not_disturb_rule" => "home_comfort",
        "gas_sensors" | "stove_state" | "safety_profiles" => "safety",
        "shopping_list_items" => "shopping",
        "permission_requests" | "approval_events" => "family",
        "replacement_parts" => "inventory",
        _ if lower_content.starts_with("watched ") || lower_content.contains(" watched ") => {
            "media"
        }
        _ => "note",
    }
}

pub(super) fn household_note_title(content: &str) -> String {
    let title = content
        .split(['.', '!', '?'])
        .next()
        .unwrap_or(content)
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        "note".into()
    } else {
        title
    }
}
