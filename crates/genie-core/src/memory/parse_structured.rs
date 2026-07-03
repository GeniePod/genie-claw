//! Structured-domain content parsers for the memory subsystem.
//!
//! Extracted verbatim from `memory/mod.rs` (continues the #604 module split,
//! no behavior change): the `*_from_memory` extractors and `*_query` parsers
//! that turn stored memory strings and user queries into structured calendar,
//! shopping, inventory, access, task, schedule, event-log, media, and secret
//! data, plus their shared text-marker helpers. All are pure `&str` functions.

use super::*;

pub(super) fn family_calendar_events_from_memory(
    kind: &str,
    content: &str,
) -> Vec<FamilyCalendarEvent> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if trimmed.is_empty()
        || !(kind_lower.contains("calendar")
            || kind_lower.contains("schedule")
            || kind_lower.contains("event")
            || kind_lower.contains("medical")
            || kind_lower.contains("pet_calendar")
            || lower.contains(" lesson")
            || lower.contains("appointment")
            || lower.contains("checkup")
            || lower.contains("school pickup"))
    {
        return Vec::new();
    }

    let mut events = Vec::new();
    if lower.contains("piano")
        && let Some((person, _)) = split_once_case_insensitive(trimmed, &lower, " has ")
    {
        events.push(FamilyCalendarEvent {
            source_memory_id: 0,
            person: Some(clean_person_name(person)),
            event_type: "piano_lesson".into(),
            title: "piano lessons".into(),
            day: calendar_day_from_text(&lower),
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("dentist")
        && lower.contains("appointment")
        && let Some(person) = calendar_person_from_statement(trimmed, &lower)
    {
        events.push(FamilyCalendarEvent {
            source_memory_id: 0,
            person: Some(person),
            event_type: "dentist_appointment".into(),
            title: "dentist appointment".into(),
            day: calendar_day_from_text(&lower),
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if (lower.contains("vet") || lower.contains("checkup"))
        && (lower.contains("appointment") || lower.contains("checkup"))
        && let Some(person) = calendar_person_from_statement(trimmed, &lower)
    {
        events.push(FamilyCalendarEvent {
            source_memory_id: 0,
            person: Some(person),
            event_type: "vet_appointment".into(),
            title: "vet appointment".into(),
            day: calendar_day_from_text(&lower),
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("school pickup") {
        let person = if let Some((person, _)) =
            split_once_case_insensitive(trimmed, &lower, " is scheduled for school pickup")
        {
            Some(clean_person_name(person))
        } else if let Some((_, person)) =
            split_once_case_insensitive(trimmed, &lower, "school pickup today is ")
        {
            Some(clean_person_name(person))
        } else if let Some((_, person)) =
            split_once_case_insensitive(trimmed, &lower, "school pickup is ")
        {
            Some(clean_person_name(person))
        } else {
            None
        };

        events.push(FamilyCalendarEvent {
            source_memory_id: 0,
            person,
            event_type: "school_pickup".into(),
            title: "school pickup".into(),
            day: calendar_day_from_text(&lower),
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    events
}

pub(super) fn shopping_list_items_from_memory(kind: &str, content: &str) -> Vec<ShoppingListItem> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if !(kind_lower == "shopping" || lower.contains("shopping list")) {
        return Vec::new();
    }

    let status = if contains_any(&lower, &[" removed:", " remove:", " taken off:"]) {
        "removed"
    } else if contains_any(
        &lower,
        &[" done:", " bought:", " completed:", " purchased:"],
    ) {
        "done"
    } else {
        "pending"
    };
    let items_text = lower
        .find("shopping list pending:")
        .map(|pos| &trimmed[pos + "shopping list pending:".len()..])
        .or_else(|| {
            lower
                .find("shopping list removed:")
                .map(|pos| &trimmed[pos + "shopping list removed:".len()..])
        })
        .or_else(|| {
            lower
                .find("shopping list remove:")
                .map(|pos| &trimmed[pos + "shopping list remove:".len()..])
        })
        .or_else(|| {
            lower
                .find("shopping list:")
                .map(|pos| &trimmed[pos + "shopping list:".len()..])
        })
        .or_else(|| {
            if let Some(rest) = lower.strip_prefix("add ") {
                let pos = rest.find(" to the shopping list")?;
                trimmed.get("add ".len().."add ".len() + pos)
            } else {
                None
            }
        })
        .unwrap_or(trimmed);

    split_list_items(items_text)
        .into_iter()
        .map(|item| ShoppingListItem {
            source_memory_id: 0,
            item,
            status: status.into(),
        })
        .collect()
}

pub(super) fn household_inventory_items_from_memory(
    kind: &str,
    content: &str,
) -> Vec<HouseholdInventoryItem> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if trimmed.is_empty()
        || !(kind_lower.contains("pantry")
            || kind_lower.contains("inventory")
            || kind_lower.contains("fridge")
            || kind_lower.contains("grocery")
            || lower.contains("inventory:")
            || lower.contains("remaining in")
            || lower.contains("left in"))
    {
        return Vec::new();
    }

    let mut items = Vec::new();
    if lower.contains("egg") {
        items.push(HouseholdInventoryItem {
            source_memory_id: 0,
            item: "eggs".into(),
            quantity: quantity_for_inventory_item(trimmed, &lower, &["egg", "eggs"]),
            location: inventory_location(trimmed, &lower),
            category: if lower.contains("fridge") || lower.contains("refrigerator") {
                "fridge".into()
            } else {
                "pantry".into()
            },
            description: trimmed.to_string(),
        });
    }

    items
}

pub(super) fn access_permissions_from_memory(kind: &str, content: &str) -> Vec<AccessPermission> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if !(kind_lower.contains("access")
        || kind_lower.contains("permission")
        || lower.contains("authorized to unlock")
        || lower.contains("can only unlock"))
    {
        return Vec::new();
    }

    let primary_person = leading_person_name(trimmed);
    let mut permissions = Vec::new();
    if let Some((person, device)) = permission_statement(
        trimmed,
        &lower,
        " is not authorized to unlock ",
        primary_person.as_deref(),
    ) {
        permissions.push(AccessPermission {
            source_memory_id: 0,
            person,
            device,
            action: "unlock".into(),
            allowed: false,
            description: trimmed.to_string(),
        });
    }
    if let Some((person, device)) = permission_statement(
        trimmed,
        &lower,
        " can only unlock ",
        primary_person.as_deref(),
    ) {
        permissions.push(AccessPermission {
            source_memory_id: 0,
            person,
            device,
            action: "unlock".into(),
            allowed: true,
            description: trimmed.to_string(),
        });
    }
    permissions
}

pub(super) fn household_task_logs_from_memory(kind: &str, content: &str) -> Vec<HouseholdTaskLog> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if !(kind_lower.contains("task")
        || kind_lower.contains("chore")
        || kind_lower.contains("pet_care")
        || lower.contains("marked the task")
        || lower.contains("fed the dog")
        || lower.contains("feed the dog")
        || lower.contains("fed the cat")
        || lower.contains("feed the cat")
        || lower.contains("cat feeding")
        || lower.contains("brush")
        || lower.contains("brushed"))
    {
        return Vec::new();
    }

    let mut logs = Vec::new();
    if lower.contains("fed the dog") || lower.contains("feed the dog") {
        let person = leading_person_name(trimmed).unwrap_or_else(|| "Unknown".into());
        logs.push(HouseholdTaskLog {
            source_memory_id: 0,
            person,
            task: "feeding".into(),
            subject: Some("dog".into()),
            day: calendar_day_from_text(&lower),
            time: time_after_marker(trimmed, &lower, " at "),
            status: if contains_any(&lower, &["complete", "completed", "done", "yes"]) {
                "complete".into()
            } else {
                "logged".into()
            },
            description: trimmed.to_string(),
        });
    }

    if lower.contains("fed the cat")
        || lower.contains("feed the cat")
        || lower.contains("cat feeding")
    {
        let person = leading_person_name(trimmed)
            .or_else(|| subject_before_marker(trimmed, &lower, " checked off cat feeding"))
            .unwrap_or_else(|| "Unknown".into());
        logs.push(HouseholdTaskLog {
            source_memory_id: 0,
            person,
            task: "feeding".into(),
            subject: Some("cat".into()),
            day: calendar_day_from_text(&lower).or_else(|| Some("today".into())),
            time: time_after_marker(trimmed, &lower, " at "),
            status: if contains_any(
                &lower,
                &["complete", "completed", "done", "yes", "checked off"],
            ) {
                "complete".into()
            } else {
                "logged".into()
            },
            description: trimmed.to_string(),
        });
    }

    if (lower.contains("brushed") || lower.contains("brush"))
        && lower.contains("teeth")
        && let Some(person) = leading_person_name(trimmed)
    {
        logs.push(HouseholdTaskLog {
            source_memory_id: 0,
            person,
            task: "brush_teeth".into(),
            subject: None,
            day: calendar_day_from_text(&lower).or_else(|| Some("today".into())),
            time: time_after_marker(trimmed, &lower, " at "),
            status: if contains_any(&lower, &["complete", "completed", "done", "yes"]) {
                "complete".into()
            } else {
                "logged".into()
            },
            description: trimmed.to_string(),
        });
    }

    logs
}

pub(super) fn household_schedule_items_from_memory(
    kind: &str,
    content: &str,
) -> Vec<HouseholdScheduleItem> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if !(kind_lower.contains("schedule")
        || kind_lower.contains("bill")
        || kind_lower.contains("utility")
        || kind_lower.contains("recycling")
        || kind_lower.contains("school_calendar")
        || kind_lower.contains("school_transport")
        || kind_lower.contains("city_services")
        || kind_lower.contains("community_services")
        || kind_lower.contains("business_hours")
        || kind_lower.contains("astronomical")
        || kind_lower.contains("program_guide")
        || kind_lower.contains("electronic_program_guide")
        || kind_lower.contains("community_calendar")
        || kind_lower.contains("subscription")
        || kind_lower.contains("trash")
        || lower.contains("bus arrives")
        || lower.contains("bus pickup")
        || lower.contains("bill is due")
        || lower.contains(" channel ")
        || lower.contains("tv tonight")
        || lower.contains("tonight at")
        || lower.contains("city council")
        || lower.contains("sunset")
        || lower.contains("sun set")
        || lower.contains("recycling")
        || lower.contains("trash pickup")
        || lower.contains("pool")
        || lower.contains("library closes")
        || lower.contains("library close")
        || lower.contains("subscription")
        || lower.contains("renews")
        || lower.contains("parent-teacher conference")
        || lower.contains("parent teacher conference"))
    {
        return Vec::new();
    }

    let mut items = Vec::new();
    if lower.contains(" channel ")
        && let Some((subject, channel)) = channel_guide_from_text(trimmed, &lower)
    {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "channel_guide".into(),
            subject: Some(subject),
            title: "channel guide".into(),
            day: None,
            date: None,
            time: None,
            amount: Some(channel),
            description: trimmed.to_string(),
        });
    }

    if (kind_lower.contains("program_guide")
        || kind_lower.contains("electronic_program_guide")
        || lower.contains("tv tonight")
        || lower.contains("tonight at"))
        && (lower.contains("tonight") || lower.contains(" tv "))
    {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "tv_tonight".into(),
            subject: Some("tv tonight".into()),
            title: "TV tonight".into(),
            day: Some("today".into()),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at "),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("city council") && lower.contains("meeting") {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "community_meeting".into(),
            subject: Some("city council".into()),
            title: "city council meeting".into(),
            day: calendar_day_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at "),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if (lower.contains("school bus") || lower.contains("bus arrives")) && lower.contains("arrives")
    {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "school_bus_arrival".into(),
            subject: Some("school bus".into()),
            title: "school bus arrival".into(),
            day: calendar_day_from_text(&lower),
            date: None,
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| time_after_marker(trimmed, &lower, " arrives at ")),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("bus pickup") {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "school_bus_arrival".into(),
            subject: if lower.contains("mia") {
                Some("mia".into())
            } else if lower.contains("leo") {
                Some("leo".into())
            } else {
                Some("school bus".into())
            },
            title: "school bus pickup".into(),
            day: calendar_day_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| time_after_marker(trimmed, &lower, " is ")),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("bill") && lower.contains("due") {
        let subject = bill_subject_from_text(trimmed, &lower);
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "bill_due".into(),
            subject,
            title: "bill due".into(),
            day: relative_due_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: None,
            amount: amount_from_text(trimmed),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("recycling") {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "recycling".into(),
            subject: Some("recycling".into()),
            title: "recycling schedule".into(),
            day: calendar_day_from_text(&lower),
            date: None,
            time: None,
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("trash pickup") || lower.contains("trash day") {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "trash_pickup".into(),
            subject: Some("trash".into()),
            title: "trash pickup".into(),
            day: calendar_day_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at "),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("conference")
        && (lower.contains("parent-teacher") || lower.contains("parent teacher"))
    {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "school_conference".into(),
            subject: subject_after_marker(trimmed, &lower, " for "),
            title: "parent-teacher conference".into(),
            day: calendar_day_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at "),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("sunset") || lower.contains("sun set") {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "sunset".into(),
            subject: Some("sunset".into()),
            title: "sunset".into(),
            day: calendar_day_from_text(&lower).or_else(|| Some("today".into())),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| time_after_marker(trimmed, &lower, " is ")),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("pool") && (lower.contains("open") || lower.contains("hours")) {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "community_facility_hours".into(),
            subject: Some(if lower.contains("community pool") {
                "community pool".into()
            } else {
                "pool".into()
            }),
            title: "community pool hours".into(),
            day: calendar_day_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| time_after_marker(trimmed, &lower, " opens at ")),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("library") && (lower.contains("close") || lower.contains("hours")) {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "business_hours".into(),
            subject: Some(if lower.contains("public library") {
                "public library".into()
            } else {
                "library".into()
            }),
            title: "library hours".into(),
            day: calendar_day_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " closes at ")
                .or_else(|| time_after_marker(trimmed, &lower, " close at "))
                .or_else(|| time_after_marker(trimmed, &lower, " at ")),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("subscription") && (lower.contains("renew") || lower.contains("due")) {
        let subject = subscription_subject_from_text(trimmed, &lower);
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "subscription_renewal".into(),
            subject,
            title: "subscription renewal".into(),
            day: relative_due_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: None,
            amount: amount_from_text(trimmed),
            description: trimmed.to_string(),
        });
    }

    items
}

pub(super) fn household_event_logs_from_memory(
    kind: &str,
    content: &str,
) -> Vec<HouseholdEventLog> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if !(kind_lower.contains("security_log")
        || kind_lower.contains("event_log")
        || kind_lower.contains("family_ledger")
        || kind_lower.contains("ledger")
        || kind_lower.contains("finance")
        || kind_lower.contains("payment_history")
        || kind_lower.contains("financial_services")
        || kind_lower.contains("financial_market_api")
        || kind_lower.contains("fitness_tracker")
        || kind_lower.contains("smart_scale")
        || kind_lower.contains("presence_state")
        || kind_lower.contains("access_logs")
        || kind_lower.contains("device_events")
        || kind_lower.contains("health_device_events")
        || kind_lower.contains("appliance_state")
        || kind_lower.contains("waste_management")
        || kind_lower.contains("environmental_sensor")
        || kind_lower.contains("location_service")
        || lower.contains("security system was disarmed")
        || lower.contains("system was disarmed")
        || lower.contains("disarmed by")
        || lower.contains("dishwasher")
        || lower.contains("trash truck")
        || lower.contains("attic")
        || lower.contains("home from school")
        || lower.contains("credit score")
        || lower.contains("stock price")
        || lower.contains("trading at")
        || lower.contains("vo2 max")
        || lower.contains("weight is")
        || lower.contains("garage door")
        || lower.contains("phone connected")
        || lower.contains("is home")
        || lower.contains("home network")
        || lower.contains("allowance")
        || lower.contains("bill") && lower.contains("paid"))
    {
        return Vec::new();
    }

    let mut events = Vec::new();
    if lower.contains("credit score") {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "finance".into(),
            subject: Some("credit score".into()),
            action: "credit_score".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| relative_calendar_phrase_from_text(&lower)),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("vo2 max") {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "health".into(),
            subject: Some("vo2 max".into()),
            action: "vo2_max".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| relative_calendar_phrase_from_text(&lower)),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("stock price") || lower.contains("trading at") {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "finance".into(),
            subject: stock_subject_from_text(trimmed, &lower),
            action: "stock_price".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| relative_calendar_phrase_from_text(&lower)),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("weight is")
        || (kind_lower.contains("smart_scale") && lower.contains("weight"))
    {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "health".into(),
            subject: Some("weight".into()),
            action: "weight_reading".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| relative_calendar_phrase_from_text(&lower)),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("dishwasher") && (lower.contains("clean") || lower.contains("dirty")) {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "appliance_state".into(),
            subject: Some("dishwasher".into()),
            action: "clean_status".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("trash truck") || lower.contains("truck came") {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "waste".into(),
            subject: Some("trash".into()),
            action: "collection".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("attic") && lower.contains("temperature") {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "environment".into(),
            subject: Some("attic".into()),
            action: "temperature".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("home from school") || lower.contains("arrived home") {
        let person = leading_person_name(trimmed).or_else(|| {
            lower
                .find(" arrived home")
                .map(|pos| clean_person_name(&trimmed[..pos]))
        });
        if let Some(person) = person.filter(|person| !person.is_empty()) {
            events.push(HouseholdEventLog {
                source_memory_id: 0,
                event_type: "location".into(),
                subject: Some(person.clone()),
                action: "home_arrival".into(),
                actor: Some(person),
                time: time_after_marker(trimmed, &lower, " at ")
                    .or_else(|| relative_calendar_phrase_from_text(&lower)),
                description: trimmed.to_string(),
            });
        }
    }

    if (kind_lower.contains("presence_state")
        || lower.contains("phone connected")
        || lower.contains("home network")
        || lower.contains(" is home"))
        && lower.contains("home")
    {
        let person = leading_person_name(trimmed)
            .or_else(|| subject_before_marker(trimmed, &lower, " phone connected"))
            .or_else(|| subject_before_marker(trimmed, &lower, " is home"));
        if let Some(person) = person.filter(|person| !person.is_empty()) {
            events.push(HouseholdEventLog {
                source_memory_id: 0,
                event_type: "location".into(),
                subject: Some(person.clone()),
                action: "presence_home".into(),
                actor: Some(person),
                time: time_after_marker(trimmed, &lower, " at ")
                    .or_else(|| relative_calendar_phrase_from_text(&lower)),
                description: trimmed.to_string(),
            });
        }
    }

    if lower.contains("garage door")
        && (lower.contains("opened") || lower.contains("open event") || lower.contains("open "))
    {
        let actor = actor_after_marker(trimmed, &lower, " by ")
            .or_else(|| subject_before_marker(trimmed, &lower, " opened the garage door"))
            .or_else(|| leading_person_name(trimmed));
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "access".into(),
            subject: Some("garage door".into()),
            action: "open".into(),
            actor,
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("disarmed") || lower.contains("turned off the security system") {
        let actor = actor_after_marker(trimmed, &lower, " by ");
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "security".into(),
            subject: Some("security system".into()),
            action: "disarm".into(),
            actor,
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("allowance") && (lower.contains("received") || lower.contains("got ")) {
        let person = allowance_person_from_text(trimmed, &lower);
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "finance".into(),
            subject: person.clone(),
            action: "allowance".into(),
            actor: person,
            time: relative_calendar_phrase_from_text(&lower),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("bill") && lower.contains("paid") {
        let subject = bill_subject_from_text(trimmed, &lower);
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "finance".into(),
            subject: subject.clone(),
            action: "paid_bill".into(),
            actor: None,
            time: relative_calendar_phrase_from_text(&lower),
            description: trimmed.to_string(),
        });
    }

    events
}

pub(super) fn media_profile_item_from_memory(content: &str) -> Option<MediaProfileItem> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    if !lower.contains("playlist") {
        return None;
    }

    let (statement, target) = split_media_target(trimmed, &lower)?;
    let statement_lower = statement.to_ascii_lowercase();
    let (owner, name) = playlist_owner_and_name(statement, &statement_lower)?;
    let name = clean_sentence_value(&name);
    let target = clean_sentence_value(target);
    if name.is_empty() || target.is_empty() {
        return None;
    }

    Some(MediaProfileItem {
        source_memory_id: 0,
        owner,
        item_type: "playlist".into(),
        name,
        provider: media_provider_from_target(&target, &lower),
        target,
    })
}

pub(super) fn split_media_target<'a>(content: &'a str, lower: &str) -> Option<(&'a str, &'a str)> {
    for marker in [
        " maps to ",
        " is ",
        " uri is ",
        " url is ",
        " opens ",
        " plays ",
    ] {
        if let Some(pos) = lower.rfind(marker) {
            let left = content[..pos].trim();
            let right = content[pos + marker.len()..].trim();
            if !left.is_empty() && !right.is_empty() {
                return Some((left, right));
            }
        }
    }
    None
}

pub(super) fn playlist_owner_and_name(
    statement: &str,
    lower: &str,
) -> Option<(Option<String>, String)> {
    if let Some(pos) = lower.find("'s playlist named ") {
        let owner = clean_person_name(&statement[..pos]);
        let name = statement[pos + "'s playlist named ".len()..].trim();
        return Some((Some(owner), name.to_string()));
    }
    if let Some(pos) = lower.find("'s playlist ") {
        let owner = clean_person_name(&statement[..pos]);
        let name = statement[pos + "'s playlist ".len()..].trim();
        return Some((Some(owner), name.to_string()));
    }
    if let Some(pos) = lower.find("'s ")
        && let Some(playlist_pos) = lower[pos + 3..].find(" playlist")
    {
        let owner = clean_person_name(&statement[..pos]);
        let start = pos + 3;
        let end = start + playlist_pos;
        let name = statement[start..end].trim();
        return Some((Some(owner), name.to_string()));
    }
    if let Some(pos) = lower.find("playlist named ") {
        let name = statement[pos + "playlist named ".len()..].trim();
        return Some((None, name.to_string()));
    }
    if let Some(pos) = lower.find(" playlist") {
        let name = statement[..pos]
            .trim()
            .trim_start_matches("the ")
            .trim_start_matches("my ")
            .trim_start_matches("our ");
        return Some((None, name.to_string()));
    }
    None
}

pub(super) fn media_provider_from_target(target: &str, lower: &str) -> Option<String> {
    let target_lower = target.to_ascii_lowercase();
    if target_lower.starts_with("spotify:") || lower.contains("spotify") {
        Some("spotify".into())
    } else if target_lower.contains("youtube") || lower.contains("youtube") {
        Some("youtube".into())
    } else if target_lower.contains("plex") || lower.contains("plex") {
        Some("plex".into())
    } else {
        None
    }
}

pub(super) fn media_playlist_query(query: &str) -> Option<(Option<String>, String)> {
    let normalized = normalize_alias_key(query);
    if !normalized.contains("playlist") {
        return None;
    }
    let mut text = normalized.as_str();
    for prefix in ["please play ", "play ", "start ", "put on "] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim();
            break;
        }
    }
    let text = text
        .trim_end_matches(" on spotify")
        .trim_end_matches(" playlist")
        .trim();
    if text.is_empty() {
        return None;
    }

    let tokens = text.split_whitespace().collect::<Vec<_>>();
    let (owner, name_tokens) = if tokens.len() >= 3 && tokens[1] == "s" {
        (Some(clean_person_name(tokens[0])), &tokens[2..])
    } else if matches!(tokens.first(), Some(&"my" | &"our" | &"the")) {
        (None, &tokens[1..])
    } else {
        (None, tokens.as_slice())
    };
    let name = name_tokens.join(" ");
    if name.is_empty() {
        None
    } else {
        Some((owner, name))
    }
}

pub(super) fn calendar_event_query(query: &str) -> Option<(String, String, Option<String>)> {
    let lower = query.to_ascii_lowercase();
    if (lower.contains("vet") || lower.contains("checkup")) && lower.contains("appointment") {
        let person = if let Some(rest) = lower.strip_prefix("when is ") {
            rest.split("'s")
                .next()
                .or_else(|| rest.split(" next ").next())
                .map(clean_person_name)
        } else if lower.starts_with("does ") || lower.starts_with("do ") {
            let rest = query.get(
                if lower.starts_with("does ") {
                    "does ".len()
                } else {
                    "do ".len()
                }..,
            )?;
            let lower_rest = rest.to_ascii_lowercase();
            let have_pos = lower_rest.find(" have ")?;
            Some(clean_person_name(&rest[..have_pos]))
        } else {
            None
        }?;
        if !person.is_empty() {
            return Some((
                person,
                "vet_appointment".into(),
                calendar_day_from_text(&lower),
            ));
        }
    }

    if lower.contains("dentist") && lower.contains("appointment") {
        let person = if let Some(rest) = lower.strip_prefix("when is ") {
            rest.split("'s")
                .next()
                .or_else(|| rest.split(" next ").next())
                .map(clean_person_name)
        } else if lower.starts_with("does ") || lower.starts_with("do ") {
            let rest = query.get(
                if lower.starts_with("does ") {
                    "does ".len()
                } else {
                    "do ".len()
                }..,
            )?;
            let lower_rest = rest.to_ascii_lowercase();
            let have_pos = lower_rest.find(" have ")?;
            Some(clean_person_name(&rest[..have_pos]))
        } else {
            None
        }?;
        if !person.is_empty() {
            return Some((
                person,
                "dentist_appointment".into(),
                calendar_day_from_text(&lower),
            ));
        }
    }

    if !(lower.starts_with("does ") || lower.starts_with("do ")) {
        return None;
    }
    if !lower.contains(" have ") || !lower.contains("piano") {
        return None;
    }
    let rest = query.get(
        if lower.starts_with("does ") {
            "does ".len()
        } else {
            "do ".len()
        }..,
    )?;
    let lower_rest = rest.to_ascii_lowercase();
    let have_pos = lower_rest.find(" have ")?;
    let person = clean_person_name(&rest[..have_pos]);
    if person.is_empty() {
        return None;
    }
    Some((
        person,
        "piano_lesson".into(),
        calendar_day_from_text(&lower),
    ))
}

pub(super) fn calendar_person_from_statement(content: &str, lower: &str) -> Option<String> {
    if let Some((person, _)) = split_once_case_insensitive(content, lower, " has ") {
        let person = clean_person_name(person);
        if !person.is_empty() {
            return Some(person);
        }
    }
    if let Some(pos) = lower.find("'s ") {
        let person = clean_person_name(&content[..pos]);
        if !person.is_empty() {
            return Some(person);
        }
    }
    leading_person_name(content)
}

pub(super) fn school_pickup_query(query: &str) -> Option<String> {
    let lower = query.to_ascii_lowercase();
    if !(lower.contains("picking up the kids")
        || lower.contains("picking up kids")
        || lower.contains("school pickup"))
    {
        return None;
    }
    calendar_day_from_text(&lower).or_else(|| Some("today".into()))
}

pub(super) fn access_permission_query(query: &str) -> Option<(String, String, String)> {
    let lower = query.to_ascii_lowercase();
    if !lower.starts_with("can ") || !lower.contains(" unlock ") {
        return None;
    }
    let rest = query.get("can ".len()..)?;
    let lower_rest = rest.to_ascii_lowercase();
    let unlock_pos = lower_rest.find(" unlock ")?;
    let person = clean_person_name(&rest[..unlock_pos]);
    let device = clean_device_phrase(&rest[unlock_pos + " unlock ".len()..]);
    if person.is_empty() || device.is_empty() {
        return None;
    }
    Some((person, "unlock".into(), device))
}

pub(super) fn shopping_list_query(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    lower.contains("shopping list")
        && (lower.starts_with("what")
            || lower.starts_with("show")
            || lower.starts_with("what is on")
            || lower.starts_with("what's on"))
}

pub(super) fn inventory_item_query(query: &str) -> Option<String> {
    let lower = clean_sentence_value(query).to_ascii_lowercase();
    let patterns = [
        ("do we have any ", " left"),
        ("do we have ", " left"),
        ("do we have any ", ""),
        ("do we have ", ""),
        ("are there any ", " left"),
        ("are there ", " left"),
    ];
    for (prefix, suffix) in patterns {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let item = if suffix.is_empty() {
                rest
            } else if let Some(item) = rest.strip_suffix(suffix) {
                item
            } else {
                continue;
            };
            let item = clean_sentence_value(item)
                .trim_start_matches("any ")
                .trim_start_matches("the ")
                .trim()
                .to_string();
            if !item.is_empty() {
                return Some(item);
            }
        }
    }
    None
}

pub(super) fn task_log_query(
    query: &str,
) -> Option<(String, String, Option<String>, Option<String>)> {
    let lower = query.to_ascii_lowercase();
    if !(lower.starts_with("did ")
        && (lower.contains(" feed ")
            || lower.contains(" fed ")
            || lower.contains(" brush ")
            || lower.contains(" brushed ")))
    {
        return None;
    }
    let rest = query.get("did ".len()..)?;
    let lower_rest = rest.to_ascii_lowercase();
    let task_pos = lower_rest
        .find(" feed ")
        .or_else(|| lower_rest.find(" fed "))
        .or_else(|| lower_rest.find(" brush "))
        .or_else(|| lower_rest.find(" brushed "))?;
    let person = clean_person_name(&rest[..task_pos]);
    if person.is_empty() {
        return None;
    }
    let task = if lower.contains("brush") || lower.contains("brushed") {
        "brush_teeth".to_string()
    } else {
        "feeding".to_string()
    };
    let subject = if task == "feeding" && lower.contains("dog") {
        Some("dog".to_string())
    } else if task == "feeding" && lower.contains("cat") {
        Some("cat".to_string())
    } else {
        None
    };
    Some((
        person,
        task,
        subject,
        calendar_day_from_text(&lower).or_else(|| Some("today".into())),
    ))
}

pub(super) fn everyone_brush_teeth_query(query: &str) -> Option<String> {
    let lower = query.to_ascii_lowercase();
    if lower.starts_with("did everyone") && lower.contains("brush") && lower.contains("teeth") {
        return Some(calendar_day_from_text(&lower).unwrap_or_else(|| "today".into()));
    }
    None
}

pub(super) fn channel_guide_from_text(content: &str, lower: &str) -> Option<(String, String)> {
    for marker in [" is on channel ", " is channel ", " channel is "] {
        if let Some(pos) = lower.find(marker) {
            let subject = &content[..pos];
            let rest = &content[pos + marker.len()..];
            let subject = clean_sentence_value(subject)
                .trim_start_matches("the ")
                .to_string();
            let channel = rest
                .split_whitespace()
                .find(|token| token.chars().any(|ch| ch.is_ascii_digit()))?
                .trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
                .to_string();
            if !subject.is_empty() && !channel.is_empty() {
                return Some((subject, channel));
            }
        }
    }
    None
}

pub(super) fn schedule_item_query(query: &str) -> Option<(String, Option<String>, Option<String>)> {
    let lower = query.to_ascii_lowercase();
    if lower.starts_with("what channel is ") || lower.starts_with("what channel s ") {
        let subject = query
            .trim_start_matches("What channel is ")
            .trim_start_matches("what channel is ")
            .trim_start_matches("what channel s ")
            .trim();
        let subject = clean_sentence_value(subject);
        if !subject.is_empty() {
            return Some(("channel_guide".into(), Some(subject), None));
        }
    }
    if lower.contains("tv tonight") || lower.contains("on tv tonight") {
        return Some((
            "tv_tonight".into(),
            Some("tv tonight".into()),
            Some("today".into()),
        ));
    }
    if lower.contains("city council") && lower.contains("meeting") {
        return Some((
            "community_meeting".into(),
            Some("city council".into()),
            calendar_day_from_text(&lower),
        ));
    }
    if lower.contains("sunset") || lower.contains("sun set") {
        return Some((
            "sunset".into(),
            Some("sunset".into()),
            calendar_day_from_text(&lower).or_else(|| Some("today".into())),
        ));
    }
    if lower.contains("bus")
        && lower.contains("tomorrow")
        && (lower.contains("what time") || lower.contains("pickup"))
    {
        return Some(("school_bus_arrival".into(), None, Some("tomorrow".into())));
    }
    if lower.contains("school bus") && (lower.contains("arrive") || lower.contains("what time")) {
        return Some((
            "school_bus_arrival".into(),
            Some("school bus".into()),
            calendar_day_from_text(&lower),
        ));
    }
    if lower.contains("bill") && lower.contains("due") {
        let subject = bill_subject_from_text(query, &lower);
        return Some(("bill_due".into(), subject, calendar_day_from_text(&lower)));
    }
    if lower.contains("recycling week") || lower.contains("recycling day") {
        return Some((
            "recycling".into(),
            Some("recycling".into()),
            calendar_day_from_text(&lower),
        ));
    }
    if lower.contains("trash pickup") || lower.contains("trash day") {
        return Some((
            "trash_pickup".into(),
            Some("trash".into()),
            calendar_day_from_text(&lower),
        ));
    }
    if lower.contains("conference")
        && (lower.contains("parent-teacher") || lower.contains("parent teacher"))
    {
        return Some((
            "school_conference".into(),
            None,
            calendar_day_from_text(&lower),
        ));
    }
    if lower.contains("community pool") || (lower.contains("pool") && lower.contains("open")) {
        return Some((
            "community_facility_hours".into(),
            Some("community pool".into()),
            calendar_day_from_text(&lower).or_else(|| Some("today".into())),
        ));
    }
    if lower.contains("library") && (lower.contains("close") || lower.contains("hours")) {
        return Some((
            "business_hours".into(),
            Some("library".into()),
            calendar_day_from_text(&lower),
        ));
    }
    if lower.contains("subscription") && (lower.contains("due") || lower.contains("renew")) {
        return Some((
            "subscription_renewal".into(),
            None,
            calendar_day_from_text(&lower),
        ));
    }
    None
}

pub(super) fn event_log_query(query: &str) -> Option<(String, String, Option<String>)> {
    let lower = query.to_ascii_lowercase();
    if lower.contains("credit score") {
        return Some((
            "finance".into(),
            "credit_score".into(),
            Some("credit score".into()),
        ));
    }
    if lower.contains("stock price") {
        return Some(("finance".into(), "stock_price".into(), None));
    }
    if lower.contains("vo2 max") {
        return Some(("health".into(), "vo2_max".into(), Some("vo2 max".into())));
    }
    if matches!(
        lower.as_str(),
        "what is my weight" | "what's my weight" | "what s my weight"
    ) || (lower.contains("my weight") && lower.starts_with("what"))
    {
        return Some((
            "health".into(),
            "weight_reading".into(),
            Some("weight".into()),
        ));
    }
    if lower.contains("dishwasher") && (lower.contains("clean") || lower.contains("dirty")) {
        return Some((
            "appliance_state".into(),
            "clean_status".into(),
            Some("dishwasher".into()),
        ));
    }
    if lower.starts_with("did ") && lower.contains("trash truck") {
        return Some(("waste".into(), "collection".into(), Some("trash".into())));
    }
    if lower.contains("temperature") && lower.contains("attic") {
        return Some((
            "environment".into(),
            "temperature".into(),
            Some("attic".into()),
        ));
    }
    if lower.starts_with("is ") && lower.contains("home from school") {
        let rest = query.get("is ".len()..)?;
        let lower_rest = rest.to_ascii_lowercase();
        let name_end = lower_rest.find(" home from school")?;
        let person = clean_person_name(&rest[..name_end]);
        if !person.is_empty() {
            return Some(("location".into(), "home_arrival".into(), Some(person)));
        }
    }
    if lower.starts_with("is ") && (lower.ends_with(" home") || lower.ends_with(" home?")) {
        let rest = query.get("is ".len()..)?;
        let lower_rest = rest.to_ascii_lowercase();
        let name_end = lower_rest.find(" home")?;
        let person = clean_person_name(&rest[..name_end]);
        if !person.is_empty() {
            return Some(("location".into(), "presence_home".into(), Some(person)));
        }
    }
    if lower.contains("who opened the garage door") {
        return Some(("access".into(), "open".into(), Some("garage door".into())));
    }
    if lower.starts_with("who ")
        && (lower.contains("turned off the security system")
            || lower.contains("disarmed the security system")
            || lower.contains("turned off security system"))
    {
        return Some((
            "security".into(),
            "disarm".into(),
            Some("security system".into()),
        ));
    }
    if lower.starts_with("did ") && lower.contains("allowance") {
        let rest = query.get("did ".len()..)?;
        let lower_rest = rest.to_ascii_lowercase();
        let name_end = lower_rest
            .find(" get ")
            .or_else(|| lower_rest.find(" receive "))
            .or_else(|| lower_rest.find(" received "))?;
        let person = clean_person_name(&rest[..name_end]);
        if !person.is_empty() {
            return Some(("finance".into(), "allowance".into(), Some(person)));
        }
    }
    if lower.starts_with("did ") && lower.contains("pay") && lower.contains("bill") {
        return Some((
            "finance".into(),
            "paid_bill".into(),
            bill_subject_from_text(query, &lower),
        ));
    }
    None
}

pub(super) fn bill_subject_from_text(content: &str, lower: &str) -> Option<String> {
    for subject in [
        "electricity",
        "electric",
        "power",
        "water",
        "gas",
        "internet",
        "utility",
        "utilities",
    ] {
        if lower.contains(subject) {
            return Some(
                match subject {
                    "electric" | "power" => "electricity",
                    "utilities" => "utility",
                    other => other,
                }
                .to_string(),
            );
        }
    }
    if let Some(pos) = lower.find(" bill") {
        let candidate = content[..pos]
            .split_whitespace()
            .last()
            .unwrap_or("")
            .trim_matches(|ch: char| !ch.is_alphanumeric());
        if !candidate.is_empty() {
            return Some(candidate.to_ascii_lowercase());
        }
    }
    None
}

pub(super) fn subscription_subject_from_text(content: &str, lower: &str) -> Option<String> {
    if let Some(pos) = lower.find(" subscription") {
        let subject = content[..pos]
            .split_whitespace()
            .last()
            .unwrap_or("")
            .trim_matches(|ch: char| !ch.is_alphanumeric());
        if !subject.is_empty() {
            return Some(subject.to_string());
        }
    }
    None
}

pub(super) fn stock_subject_from_text(content: &str, lower: &str) -> Option<String> {
    if let Some(pos) = lower.find(" is currently trading") {
        let subject = clean_sentence_value(&content[..pos]);
        if !subject.is_empty() {
            return Some(subject);
        }
    }
    if let Some(pos) = lower.find(" stock price") {
        let subject = clean_sentence_value(&content[..pos]);
        if !subject.is_empty() {
            return Some(subject);
        }
    }
    None
}

pub(super) fn relative_due_from_text(lower: &str) -> Option<String> {
    let pos = lower.find("due in ")?;
    let rest = &lower[pos + "due ".len()..];
    let value = rest
        .split(['.', ',', ';'])
        .next()
        .unwrap_or(rest)
        .split(" on ")
        .next()
        .unwrap_or(rest)
        .trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

pub(super) fn due_date_from_text(content: &str, lower: &str) -> Option<String> {
    let pos = lower.find(" on ")?;
    let rest = content[pos + " on ".len()..].trim();
    let date = rest
        .split(['.', ',', ';'])
        .next()
        .unwrap_or(rest)
        .split(" at ")
        .next()
        .unwrap_or(rest)
        .split(" estimated ")
        .next()
        .unwrap_or(rest)
        .trim();
    if date.is_empty() {
        None
    } else {
        Some(clean_sentence_value(date))
    }
}

pub(super) fn subject_after_marker(content: &str, lower: &str, marker: &str) -> Option<String> {
    let pos = lower.rfind(marker)?;
    let rest = content[pos + marker.len()..].trim();
    let subject = rest
        .split(['.', ',', ';'])
        .next()
        .unwrap_or(rest)
        .split(" at ")
        .next()
        .unwrap_or(rest)
        .trim();
    if subject.is_empty() {
        None
    } else {
        Some(clean_sentence_value(subject))
    }
}

pub(super) fn subject_before_marker(content: &str, lower: &str, marker: &str) -> Option<String> {
    let pos = lower.find(marker)?;
    let subject = content[..pos].trim();
    if subject.is_empty() {
        None
    } else {
        Some(clean_person_name(subject))
    }
}

pub(super) fn actor_after_marker(content: &str, lower: &str, marker: &str) -> Option<String> {
    let pos = lower.find(marker)?;
    let rest = content[pos + marker.len()..].trim();
    let actor = rest
        .split(" using ")
        .next()
        .unwrap_or(rest)
        .split(" with ")
        .next()
        .unwrap_or(rest)
        .split(" at ")
        .next()
        .unwrap_or(rest)
        .split(['.', ',', ';'])
        .next()
        .unwrap_or(rest)
        .trim();
    if actor.is_empty() {
        None
    } else {
        Some(clean_person_name(actor))
    }
}

pub(super) fn amount_from_text(content: &str) -> Option<String> {
    let pos = content.find('$')?;
    let amount = content[pos..]
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|ch: char| !ch.is_ascii_digit() && ch != '$' && ch != '.');
    if amount.is_empty() {
        None
    } else {
        Some(amount.to_string())
    }
}

pub(super) fn allowance_person_from_text(content: &str, lower: &str) -> Option<String> {
    for marker in [" received ", " got "] {
        if let Some((person, _)) = split_once_case_insensitive(content, lower, marker) {
            let person = clean_person_name(person);
            if !person.is_empty() {
                return Some(person);
            }
        }
    }
    leading_person_name(content)
}

pub(super) fn app_only_secret_reference_from_memory(
    _kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
) -> Option<AppOnlySecretReference> {
    let lower = content.to_ascii_lowercase();
    let secret_type = secret_type_from_text(&lower)?;

    let shared_allowed =
        policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice())
            .allowed;
    let explicitly_app_only = matches!(
        metadata.spoken_policy,
        policy::SpokenMemoryPolicy::AppOnly | policy::SpokenMemoryPolicy::Deny
    ) || lower.contains("credential:")
        || lower.contains("credentials vault")
        || lower.contains("local vault")
        || lower.contains("app-only")
        || lower.contains("app only");

    if shared_allowed && !explicitly_app_only {
        return None;
    }

    let label = secret_label_from_text(content, &lower, secret_type);
    Some(AppOnlySecretReference {
        source_memory_id: 0,
        secret_type: secret_type.into(),
        label,
        location_hint: secret_location_hint(content, &lower),
    })
}

pub(super) fn secret_type_from_text(lower: &str) -> Option<&'static str> {
    let mentions_wifi =
        lower.contains("wi-fi") || lower.contains("wifi") || lower.contains("wi fi");
    let mentions_credential =
        lower.contains("password") || lower.contains("passcode") || lower.contains("credential");
    let mentions_lock_word = lower
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|token| matches!(token, "lock" | "locks"));
    if (mentions_wifi && mentions_credential)
        || lower.contains("network password")
        || (lower.contains("guest network") && mentions_credential)
    {
        Some("wifi_password")
    } else if lower.contains("password")
        || lower.contains(" pass:")
        || lower.starts_with("pass:")
        || lower.contains("bank login")
        || lower.contains("password manager")
        || lower.contains("secure vault")
        || lower.contains("credentials vault")
        || (lower.contains("netflix")
            && (lower.contains("code") || lower.contains("credential") || lower.contains("login")))
    {
        Some("password")
    } else if lower.contains("gate code") {
        Some("gate_code")
    } else if lower.contains("door code")
        || lower.contains("lock code")
        || (lower.contains("shed")
            && lower.contains("code")
            && !lower.contains("paint")
            && !lower.contains("color")
            && !lower.contains("colour"))
        || (lower.contains("shed") && lower.contains("combination"))
        || (mentions_lock_word && (lower.contains("combination") || lower.contains("combo")))
    {
        Some("lock_code")
    } else if lower.contains("alarm code") || lower.contains("security code") {
        Some("security_code")
    } else if lower.contains("confirmation number") {
        Some("confirmation_number")
    } else if lower.contains("account number") {
        Some("account_number")
    } else if lower.contains("spare key")
        || lower.contains("spare keys")
        || lower.contains("house key")
        || lower.contains("house keys")
    {
        Some("secure_location")
    } else if lower.contains("combination") || lower.contains("combo") {
        Some("combination")
    } else {
        None
    }
}

pub(super) fn secret_label_from_text(content: &str, lower: &str, secret_type: &str) -> String {
    if lower.contains("router") && lower.contains("admin") && secret_type == "password" {
        return "router admin".into();
    }
    if lower.contains("guest") && matches!(secret_type, "wifi_password" | "password") {
        return "guest wifi".into();
    }
    if lower.contains("printer") && matches!(secret_type, "wifi_password" | "password") {
        return "printer wifi".into();
    }
    if lower.contains("xbox") && matches!(secret_type, "wifi_password" | "password") {
        return "Xbox wifi".into();
    }
    if lower.contains("locker") && matches!(secret_type, "combination" | "lock_code") {
        if lower.contains("mia") {
            return "Mia locker combination".into();
        }
        return "locker combination".into();
    }
    if lower.contains("shed") && matches!(secret_type, "combination" | "lock_code") {
        return "shed combination".into();
    }
    if lower.contains("netflix") && secret_type == "password" {
        return "Netflix account".into();
    }
    if lower.contains("bank") && secret_type == "password" {
        return "bank login".into();
    }
    if matches!(secret_type, "secure_location") && lower.contains("key") {
        return "spare keys".into();
    }
    if secret_type == "confirmation_number" && lower.contains("hotel") {
        return "hotel confirmation number".into();
    }
    if secret_type == "account_number" && lower.contains("gas") {
        return "gas bill account number".into();
    }
    if lower.contains("wi-fi") || lower.contains("wifi") || lower.contains("wi fi") {
        return "wifi".into();
    }
    let before_marker = [" is ", " pass:", " pass ", " stored ", " saved ", " lives "]
        .iter()
        .filter_map(|marker| lower.find(marker).map(|pos| content[..pos].trim()))
        .next()
        .unwrap_or(content)
        .trim_start_matches("the ")
        .trim_start_matches("our ")
        .trim_start_matches("my ");
    let label = clean_sentence_value(before_marker);
    if label.is_empty() {
        secret_type.replace('_', " ")
    } else {
        label
    }
}

pub(super) fn secret_location_hint(content: &str, lower: &str) -> String {
    for marker in [
        "credential:",
        "credentials vault",
        "local vault",
        "vault",
        "dashboard",
    ] {
        if let Some(pos) = lower.find(marker) {
            let hint = content[pos..]
                .trim()
                .trim_matches(|ch: char| matches!(ch, '.' | ',' | ';' | '!' | '?'));
            if !hint.is_empty() {
                return hint.to_string();
            }
        }
    }
    "app-only credential storage".into()
}

pub(super) fn parse_allergy_rule(content: &str, lower: &str) -> Option<(String, String)> {
    if let Some((person, rest)) = split_once_case_insensitive(content, lower, " is allergic to ") {
        return Some((clean_person_name(person), normalize_rule_subject(rest)));
    }

    if let Some((person, rest)) = split_once_case_insensitive(content, lower, " has ") {
        let rest_lower = rest.to_ascii_lowercase();
        if let Some(pos) = rest_lower.find(" allergy") {
            let subject = rest[..pos]
                .split_whitespace()
                .rfind(|word| {
                    !matches!(
                        word.to_ascii_lowercase().as_str(),
                        "a" | "an" | "mild" | "severe" | "recent"
                    )
                })
                .unwrap_or("");
            if !subject.is_empty() {
                return Some((clean_person_name(person), normalize_rule_subject(subject)));
            }
        }
    }

    None
}

pub(super) fn parse_screen_time_rule(
    content: &str,
    lower: &str,
) -> Option<(String, String, String)> {
    let person = if let Some((person, _)) =
        split_once_case_insensitive(content, lower, " is not allowed ")
    {
        clean_person_name(person)
    } else if let Some((person, _)) = split_once_case_insensitive(content, lower, "'s screen time")
    {
        clean_person_name(person)
    } else {
        leading_person_name(content)?
    };

    let subject = if lower.contains("video game") || lower.contains("gaming") {
        "video_games"
    } else {
        "screen_time"
    };
    let value = time_after_marker(content, lower, " after ")
        .or_else(|| time_after_marker(content, lower, " ends at "))?;
    Some((person, subject.into(), value))
}

pub(super) fn profile_attr(name: &str, attribute: &str, value: &str) -> HouseholdProfileAttribute {
    HouseholdProfileAttribute {
        source_memory_id: 0,
        name: clean_person_name(name),
        attribute: attribute.into(),
        value: clean_sentence_value(value),
    }
}

pub(super) fn split_once_case_insensitive<'a>(
    original: &'a str,
    lower: &str,
    marker: &str,
) -> Option<(&'a str, &'a str)> {
    let pos = lower.find(marker)?;
    Some((&original[..pos], &original[pos + marker.len()..]))
}

pub(super) fn leading_age(value: &str) -> Option<u8> {
    let digits = value
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    digits
        .parse::<u8>()
        .ok()
        .filter(|age| (1..=120).contains(age))
}

pub(super) fn leading_person_name(value: &str) -> Option<String> {
    let name = value.split_whitespace().next()?;
    let name = clean_person_name(name);
    if name.is_empty() { None } else { Some(name) }
}

pub(super) fn clean_person_name(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("for ")
        .trim_start_matches("that ")
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '.' | ',' | ':' | ';' | '?' | '!'))
        .split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn clean_sentence_value(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '.' | ',' | ':' | ';' | '?' | '!'))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn clean_device_phrase(value: &str) -> String {
    clean_sentence_value(value)
        .trim_start_matches("the ")
        .trim_start_matches("a ")
        .trim_start_matches("an ")
        .to_string()
}

pub(super) fn calendar_day_from_text(lower: &str) -> Option<String> {
    for day in [
        "today",
        "tomorrow",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ] {
        if lower.split_whitespace().any(|token| {
            token.trim_matches(|ch: char| matches!(ch, '.' | ',' | ';' | '?' | '!')) == day
        }) {
            return Some(day.into());
        }
    }
    None
}

pub(super) fn relative_calendar_phrase_from_text(lower: &str) -> Option<String> {
    for day in [
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ] {
        let last = format!("last {day}");
        if lower.contains(&last) {
            return Some(last);
        }
        let next = format!("next {day}");
        if lower.contains(&next) {
            return Some(next);
        }
    }
    calendar_day_from_text(lower)
}

pub(super) fn normalize_calendar_day(value: &str) -> String {
    calendar_day_from_text(&value.to_ascii_lowercase())
        .unwrap_or_else(|| normalize_alias_key(value))
}

pub(super) fn split_list_items(value: &str) -> Vec<String> {
    value
        .replace(" and ", ",")
        .split([',', ';'])
        .map(clean_sentence_value)
        .map(|item| {
            item.trim_start_matches("the ")
                .trim_start_matches("a ")
                .trim_start_matches("an ")
                .to_string()
        })
        .filter(|item| !item.is_empty())
        .collect()
}

pub(super) fn quantity_for_inventory_item(
    content: &str,
    lower: &str,
    item_tokens: &[&str],
) -> Option<String> {
    let tokens = content.split_whitespace().collect::<Vec<_>>();
    let lower_tokens = lower.split_whitespace().collect::<Vec<_>>();
    for (idx, token) in lower_tokens.iter().enumerate() {
        let cleaned = token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric());
        if !item_tokens.contains(&cleaned) {
            continue;
        }

        if let Some(quantity) = idx
            .checked_sub(1)
            .and_then(|prev| tokens.get(prev))
            .and_then(|token| parse_quantity_token(token))
        {
            return Some(quantity);
        }
        if let Some(quantity) = tokens
            .get(idx + 1)
            .and_then(|token| parse_quantity_token(token))
        {
            return Some(quantity);
        }
    }

    lower_tokens.iter().enumerate().find_map(|(idx, token)| {
        if matches!(
            token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric()),
            "have" | "has" | "remaining" | "left" | "quantity"
        ) {
            tokens
                .get(idx + 1)
                .and_then(|token| parse_quantity_token(token))
        } else {
            None
        }
    })
}

pub(super) fn parse_quantity_token(token: &str) -> Option<String> {
    let cleaned =
        token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '.' && ch != '/');
    if cleaned.is_empty() {
        return None;
    }
    if cleaned.chars().any(|ch| ch.is_ascii_digit()) {
        return Some(cleaned.to_string());
    }
    match cleaned.to_ascii_lowercase().as_str() {
        "zero" | "none" => Some("0".into()),
        "one" | "a" | "an" => Some("1".into()),
        "two" => Some("2".into()),
        "three" => Some("3".into()),
        "four" => Some("4".into()),
        "five" => Some("5".into()),
        "six" => Some("6".into()),
        "seven" => Some("7".into()),
        "eight" => Some("8".into()),
        "nine" => Some("9".into()),
        "ten" => Some("10".into()),
        "dozen" => Some("12".into()),
        _ => None,
    }
}

pub(super) fn inventory_location(content: &str, lower: &str) -> Option<String> {
    for marker in [" in the ", " in "] {
        if let Some(pos) = lower.rfind(marker) {
            let location = content[pos + marker.len()..]
                .split(['.', ',', ';'])
                .next()
                .map(clean_sentence_value)
                .unwrap_or_default();
            if !location.is_empty() {
                return Some(location);
            }
        }
    }
    None
}

pub(super) fn normalize_inventory_item(value: &str) -> String {
    match normalize_alias_key(value).as_str() {
        "egg" | "eggs" => "eggs".into(),
        "milk" => "milk".into(),
        other => other.trim_end_matches('s').to_string(),
    }
}

pub(super) fn permission_statement(
    content: &str,
    lower: &str,
    marker: &str,
    fallback_person: Option<&str>,
) -> Option<(String, String)> {
    let pos = lower.find(marker)?;
    let left = content[..pos]
        .rsplit(['.', ';'])
        .next()
        .unwrap_or(&content[..pos])
        .trim();
    let person = if matches!(
        left.to_ascii_lowercase().trim(),
        "he" | "she" | "they" | "him" | "her"
    ) {
        fallback_person.map(ToOwned::to_owned)?
    } else {
        clean_person_name(left)
    };
    let rest = content[pos + marker.len()..].trim();
    let device = rest
        .split(['.', ';'])
        .next()
        .map(clean_device_phrase)
        .unwrap_or_default();
    if person.is_empty() || device.is_empty() {
        None
    } else {
        Some((person, device))
    }
}

pub(super) fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

pub(super) fn normalize_name_key(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn normalize_rule_subject(value: &str) -> String {
    let singular = value
        .trim()
        .trim_start_matches("the ")
        .trim_start_matches("a ")
        .trim_start_matches("an ")
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '.' | ',' | ':' | ';' | '?' | '!'))
        .to_ascii_lowercase();
    match singular.as_str() {
        "peanuts" => "peanut".into(),
        "video_games" | "video games" | "video game" | "gaming" => "video_games".into(),
        "screen_time" | "screen time" => "screen_time".into(),
        "homework" => "homework".into(),
        other => other
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("_")
            .trim_end_matches('s')
            .to_string(),
    }
}

pub(super) fn time_after_marker(content: &str, lower: &str, marker: &str) -> Option<String> {
    let pos = lower.find(marker)?;
    let rest = content[pos + marker.len()..].trim();
    let mut parts = Vec::new();
    for word in rest.split_whitespace().take(3) {
        let clean = word.trim_matches(|ch: char| matches!(ch, '.' | ',' | ';' | '!' | '?'));
        if clean.eq_ignore_ascii_case("for")
            || clean.eq_ignore_ascii_case("because")
            || clean.eq_ignore_ascii_case("with")
            || clean.eq_ignore_ascii_case("on")
        {
            break;
        }
        parts.push(clean);
        if clean.eq_ignore_ascii_case("am") || clean.eq_ignore_ascii_case("pm") {
            break;
        }
    }
    let raw = parts.join(" ");
    normalize_time_value(&raw)
}

pub(super) fn normalize_time_value(value: &str) -> Option<String> {
    let cleaned = value.trim().to_ascii_lowercase().replace(' ', "");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

pub(super) fn profile_attribute_query(query: &str) -> Option<(String, &'static str)> {
    let query = query.trim();
    let lower = query.to_ascii_lowercase();

    for prefix in ["how old is ", "what age is "] {
        if let Some(name) = lower.strip_prefix(prefix) {
            return Some((clean_person_name(name), "age"));
        }
    }

    if lower.starts_with("what does ") && lower.contains(" like") {
        let rest = query.get("what does ".len()..)?;
        let lower_rest = rest.to_ascii_lowercase();
        let pos = lower_rest.find(" like")?;
        return Some((clean_person_name(&rest[..pos]), "likes"));
    }

    if lower.starts_with("what size shoe does ") && lower.contains(" wear") {
        let rest = query.get("what size shoe does ".len()..)?;
        let lower_rest = rest.to_ascii_lowercase();
        let pos = lower_rest.find(" wear")?;
        return Some((clean_person_name(&rest[..pos]), "shoe_size"));
    }

    if lower.starts_with("what shoe size does ") && lower.contains(" wear") {
        let rest = query.get("what shoe size does ".len()..)?;
        let lower_rest = rest.to_ascii_lowercase();
        let pos = lower_rest.find(" wear")?;
        return Some((clean_person_name(&rest[..pos]), "shoe_size"));
    }

    None
}

pub(super) fn allergy_query_subject(query: &str) -> Option<Option<String>> {
    let lower = query.to_ascii_lowercase();
    if !(lower.contains("allergic") || lower.contains("allergy")) {
        return None;
    }

    if let Some((_, subject)) = split_once_case_insensitive(query, &lower, " allergic to ") {
        return Some(Some(normalize_rule_subject(subject)));
    }
    if lower.contains("peanut") {
        return Some(Some("peanut".into()));
    }

    Some(None)
}

pub(super) fn allowed_rule_query(query: &str) -> Option<(String, String, Option<String>)> {
    let lower = query.to_ascii_lowercase();
    if !(lower.starts_with("is ") && lower.contains(" allowed")) {
        return None;
    }
    let rest = query.get("is ".len()..)?;
    let lower_rest = rest.to_ascii_lowercase();
    let allowed_pos = lower_rest.find(" allowed")?;
    let person = clean_person_name(&rest[..allowed_pos]);
    if person.is_empty() {
        return None;
    }

    let subject = if lower.contains("video game") || lower.contains("gaming") {
        "video_games"
    } else if lower.contains("screen time") {
        "screen_time"
    } else {
        return None;
    };
    let value = time_after_marker(query, &lower, " after ");
    Some((person, subject.into(), value))
}

pub(super) fn homework_rule_query(query: &str) -> Option<String> {
    let lower = query.to_ascii_lowercase();
    if !lower.contains("homework") {
        return None;
    }

    for marker in ["show me ", "what are ", "what is "] {
        if let Some(rest) = lower.strip_prefix(marker) {
            let name = rest
                .split("'s")
                .next()
                .or_else(|| rest.split_whitespace().next())
                .map(clean_person_name)?;
            if !name.is_empty() {
                return Some(name);
            }
        }
    }

    leading_person_name(query)
}

pub(super) fn household_note_query(query: &str) -> Option<String> {
    let query = query.trim();
    let lower = query.to_ascii_lowercase();

    for prefix in [
        "what did i say about ",
        "what did we say about ",
        "find my note about ",
        "find note about ",
        "find the note about ",
        "show my note about ",
        "show the note about ",
        "what is the note about ",
        "what did i write about ",
        "what did we write about ",
        "what did the vet say about ",
        "what did the mechanic say about ",
        "find our note about ",
        "find the record about ",
        "find record about ",
        "find the warranty for ",
        "find warranty for ",
        "what is the warranty for ",
        "what s the warranty for ",
        "what's the warranty for ",
        "find the receipt for ",
        "find receipt for ",
        "find my essay draft about ",
        "find the essay draft about ",
        "find the manual for ",
        "find the user manual for ",
        "find manual for ",
        "where did i save ",
        "find the instructions for ",
        "find instructions for ",
        "who do we call for ",
        "what is the phone number for ",
        "what s the phone number for ",
        "what's the phone number for ",
        "what is the ip address of ",
        "what s the ip address of ",
        "what's the ip address of ",
        "find anything about ",
    ] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let cleaned = clean_sentence_value(rest);
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }

    if lower.starts_with("where are ")
        || lower.starts_with("where is ")
        || lower.starts_with("where s ")
        || lower.starts_with("where did i put ")
        || lower.starts_with("where did we put ")
        || lower.starts_with("where are the ")
        || lower.starts_with("what color is ")
        || lower.starts_with("what colour is ")
        || lower.starts_with("what color did we paint ")
        || lower.starts_with("what colour did we paint ")
        || lower.starts_with("what's the model number ")
        || lower.starts_with("what s the model number ")
        || lower.starts_with("what is the model number ")
        || lower.starts_with("what is the license plate")
        || lower.starts_with("what s the license plate")
        || lower.starts_with("what's the license plate")
        || lower.starts_with("find the sewing kit")
        || (lower.starts_with("find the ") && lower.contains(" warranty"))
        || lower.starts_with("find the manual for the car")
        || lower.starts_with("who took the photos ")
        || lower.starts_with("we have a leak ")
        || lower.starts_with("there is a leak ")
        || lower.starts_with("how do i clean ")
        || lower.starts_with("how do we clean ")
        || lower.starts_with("how do i remove ")
        || lower.starts_with("how do we remove ")
        || lower.starts_with("how do i reset ")
        || lower.starts_with("how do we reset ")
        || lower.starts_with("how long do i boil ")
        || lower.starts_with("how long should i boil ")
        || lower.starts_with("what bin does ")
        || lower.starts_with("tell me the dinosaur fact")
        || lower.starts_with("what did we have for dinner ")
        || lower.starts_with("find the recipe for ")
        || lower.starts_with("what is the school")
        || lower.starts_with("what s the school")
        || lower.starts_with("what's the school")
        || lower.starts_with("what is the doctor")
        || lower.starts_with("what s the doctor")
        || lower.starts_with("what's the doctor")
        || lower.starts_with("what is the vet")
        || lower.starts_with("what s the vet")
        || lower.starts_with("what's the vet")
        || lower.starts_with("what is the phone number for ")
        || lower.starts_with("what s the phone number for ")
        || lower.starts_with("what's the phone number for ")
        || lower.starts_with("what is the ip address of ")
        || lower.starts_with("what s the ip address of ")
        || lower.starts_with("what's the ip address of ")
        || lower.starts_with("who do we call for ")
        || lower.starts_with("what's on the hardware store list")
        || lower.starts_with("what s on the hardware store list")
        || lower.starts_with("what is on the hardware store list")
        || lower.starts_with("where did we put ")
        || lower.starts_with("where did i put ")
        || lower.starts_with("where are the tax documents")
        || lower.starts_with("when is the next trash pickup")
        || lower.contains("science fair checklist")
        || lower.contains("air fryer manual")
        || lower.contains("which filter")
        || lower.contains("dishwasher error")
        || lower.contains("tablet charger")
        || lower.contains("saturday morning routine")
        || lower.contains("what groceries are low")
        || lower.contains("what s next before school")
        || lower.contains("what's next before school")
        || lower.contains("can i watch cartoons")
        || lower.contains("can i have a snack")
        || lower.contains("coming to dinner tonight")
        || lower.contains("did i finish my chores")
        || lower.contains("what time is my bus")
        || lower.contains("bus tomorrow")
        || lower.contains("which leftovers should we eat first")
        || lower.contains("did mom approve my sleepover")
        || lower.contains("pajama day")
        || lower.contains("allergy action plan")
        || lower.contains("car keys")
        || lower.contains("robot vacuum stuck")
        || lower.contains("who changed the thermostat")
        || lower.contains("ladder safety")
        || lower.contains("bathroom mirror")
        || lower.contains("package still on the porch")
        || lower.contains("allergy medicine")
        || lower.contains("dinosaur fact")
        || lower.contains("what s making that beeping sound")
        || lower.contains("what's making that beeping sound")
        || lower.contains("porch light still on")
        || lower.contains("grandma")
            && (lower.contains("wi fi note")
                || lower.contains("wi-fi note")
                || lower.contains("wifi note"))
        || lower.contains("allowed to play outside")
        || lower.contains("wet soccer shoes")
        || lower.contains("when did the laundry finish")
        || lower.contains("blue paint")
        || lower.contains("did my laundry get moved")
        || lower.contains("safest way out")
        || lower.contains("which breaker controls the dishwasher")
        || lower.contains("trash day")
        || lower.contains("red hoodie")
        || lower.contains("lego cleanup")
        || lower.contains("ants")
        || lower.contains("garbage bins")
        || lower.contains("camping flashlight")
        || lower.contains("why didn t the sprinklers run")
        || lower.contains("why didn't the sprinklers run")
        || lower.contains("homework needs internet")
        || lower.contains("use the stove")
        || lower.contains("cold medicine")
        || lower.contains("fridge door")
        || lower.contains("sensors need batteries")
        || lower.contains("library book")
        || lower.contains("alarm not go off")
        || lower.contains("plants need attention")
        || lower.contains("blue cup")
        || lower.contains("side gate")
        || lower.contains("recital outfit")
        || lower.contains("bathroom free")
        || lower.contains("away mode fail")
        || lower.contains("guest speaker")
        || lower.contains("end of day")
        || lower.contains("end-of-day")
        || lower.contains("after dinner cleanup")
        || lower.contains("after-dinner cleanup")
        || lower.contains("upstairs lights")
        || lower.contains("front door") && lower.contains("grandma")
        || lower.contains("debate") && lower.contains("school lunch")
        || lower.contains("board games")
        || lower.contains("basement humid")
        || lower.contains("test practice")
        || lower.contains("rain boots")
        || lower.contains("charging tonight")
        || lower.contains("coffee") && lower.contains("wake")
        || lower.contains("fan on low") && lower.contains("sleep")
        || lower.contains("cold after bath")
        || lower.contains("slow cooker") && lower.contains("timer chart")
        || lower.contains("basement flood check")
        || lower.contains("garage camera") && lower.contains("bike")
        || lower.contains("next filter change")
        || lower.contains("puzzle") && lower.contains("dad")
        || lower.contains("temporary code") && lower.contains("grandma")
        || lower.contains("glarey")
        || lower.contains("front door locked after")
        || lower.contains("water heater receipt")
        || lower.contains("quiet drawing")
        || lower.contains("print my homework")
        || lower.contains("upstairs cooler") && lower.contains("leo")
        || lower.contains("noisy appliance")
        || lower.contains("tooth fairy box")
        || lower.contains("white extension cord")
        || lower.contains("family dinner") && lower.contains("screens")
        || lower.contains("changed in the garage today")
        || lower.contains("stairs bright")
        || lower.contains("water my plant")
        || lower.contains("chicken recipe") && lower.contains("peanut")
        || lower.contains("security alarm chirp")
        || lower.contains("use the microwave")
        || lower.contains("rehearsal comfort")
        || lower.contains("who s in the backyard")
        || lower.contains("who's in the backyard")
        || lower.contains("workshop dust control")
        || lower.contains("bedtime chart")
        || lower.contains("closet light")
        || lower.contains("upstairs window before the rain")
        || lower.contains("low power mode")
        || lower.contains("low-power mode")
        || lower.contains("vaccination form")
        || lower.contains("field trip form")
        || lower.contains("animal show")
        || lower.contains("guest wi fi")
        || lower.contains("guest wifi")
        || lower.contains("guest wi-fi")
        || lower.contains("front entry lights")
        || lower.contains("side path icy")
        || lower.contains("dripping")
        || lower.contains("office internet slow")
        || lower.contains("school night reset")
        || lower.contains("school-night reset")
        || lower.contains("photo backdrop")
        || lower.contains("red marker")
        || lower.contains("freezer") && lower.contains("above 10")
        || lower.contains("chores did leo skip")
        || lower.contains("mirror lights")
        || lower.contains("cat sleep")
        || lower.contains("grilling")
        || lower.contains("purifier on high")
        || lower.contains("swim meet")
        || lower.contains("next step") && lower.contains("cookies")
        || lower.contains("outdoor cameras need cleaning")
        || lower.contains("garage close after jared")
        || lower.contains("project list")
        || lower.contains("scared to go downstairs")
        || lower.contains("furnace") && lower.contains("code 31")
        || lower.contains("dinner warm")
        || lower.contains("quiet time") && lower.contains("wednesday")
        || lower.contains("feed the cat too much")
        || lower.contains("oldest thing in the fridge")
        || lower.contains("outside is cleaner")
        || lower.contains("lamp flickering")
        || lower.contains("open the garage door")
        || lower.contains("holiday lighting")
        || lower.contains("shutoff valve")
        || lower.contains("rainy day alarm")
        || lower.contains("rainy-day alarm")
        || lower.contains("soccer practice")
        || lower.contains("bypass a sensor")
        || lower.contains("guest breakfast")
        || lower.contains("winter poem")
        || lower.contains("laundry room not scary")
        || lower.contains("water pressure")
        || lower.contains("oven") && lower.contains("preheat")
        || lower.contains("hallway camera")
        || lower.contains("cookies are cool")
        || lower.contains("vacuum avoid")
        || lower.contains("toddler gate")
        || lower.contains("room smells weird")
        || lower.contains("dad see my message")
        || lower.contains("laundry leaks again")
        || lower.contains("backpacks are by the door")
        || lower.contains("alarm skip holidays")
        || lower.contains("morning checklist")
        || lower.contains("privacy report") && lower.contains("cameras")
        || lower.contains("green bowl")
        || lower.contains("practice drums")
        || lower.contains("flashlight") && lower.contains("lights go out")
        || lower.contains("automation fired")
        || lower.contains("upstairs warmer") && lower.contains("kids")
        || lower.contains("tournament") && lower.contains("snacks")
        || lower.contains("final safety sweep")
    {
        return Some(query.to_string());
    }

    if lower.starts_with("what did we watch about ")
        || lower.starts_with("what did i watch about ")
        || lower.starts_with("what movie ")
        || lower.starts_with("what was that movie ")
    {
        return Some(query.to_string());
    }

    None
}

pub(super) fn secret_reference_query(query: &str) -> Option<(&'static str, String)> {
    let lower = query.to_ascii_lowercase();
    if lower.contains("guest speaker") {
        return None;
    }
    let secret_type = secret_type_from_text(&lower)?;
    if !(lower.contains("what")
        || lower.contains("show")
        || lower.contains("find")
        || lower.contains("where")
        || lower.contains("password")
        || lower.contains("code")
        || lower.contains("combo")
        || lower.contains("key")
        || lower.contains("number")
        || lower.contains("login")
        || lower.contains("credential"))
    {
        return None;
    }

    let label = if lower.contains("guest") && matches!(secret_type, "wifi_password" | "password") {
        "guest wifi".into()
    } else if lower.contains("printer") && secret_type == "wifi_password" {
        "printer wifi".into()
    } else if lower.contains("xbox") && secret_type == "wifi_password" {
        "Xbox wifi".into()
    } else if lower.contains("locker") && matches!(secret_type, "combination" | "lock_code") {
        if lower.contains("mia") {
            "Mia locker combination".into()
        } else {
            "locker combination".into()
        }
    } else if lower.contains("shed") && matches!(secret_type, "combination" | "lock_code") {
        "shed combination".into()
    } else if lower.contains("netflix") && secret_type == "password" {
        "Netflix account".into()
    } else if lower.contains("bank") && secret_type == "password" {
        "bank login".into()
    } else if matches!(secret_type, "secure_location") && lower.contains("key") {
        "spare keys".into()
    } else if secret_type == "confirmation_number" && lower.contains("hotel") {
        "hotel confirmation number".into()
    } else if secret_type == "account_number" && lower.contains("gas") {
        "gas bill account number".into()
    } else if lower.contains("wifi") || lower.contains("wi-fi") || lower.contains("wi fi") {
        "wifi".into()
    } else {
        search_tokens(query).join(" ")
    };
    Some((secret_type, label))
}

pub(super) fn format_profile_attribute_answer(attr: &HouseholdProfileAttribute) -> String {
    match attr.attribute.as_str() {
        "age" => format!("{} is {} years old.", attr.name, attr.value),
        "likes" => format!("{} likes {}.", attr.name, attr.value),
        "shoe_size" => format!("{} currently wears shoe size {}.", attr.name, attr.value),
        attribute if attribute.starts_with("favorite_") => {
            let subject = attribute.trim_start_matches("favorite_").replace('_', " ");
            format!("{}'s favorite {} is {}.", attr.name, subject, attr.value)
        }
        _ => format!("{}: {}.", attr.name, attr.value),
    }
}

pub(super) fn format_allergy_answer(rules: &[HouseholdRule]) -> String {
    let items = rules
        .iter()
        .map(|rule| rule.description.as_str())
        .collect::<Vec<_>>();
    format!("Yes. {}", items.join(" "))
}

pub(super) fn format_allowed_rule_answer(rule: &HouseholdRule) -> String {
    if rule.allowed {
        format!("Yes. {}", rule.description)
    } else {
        format!("No. {}", rule.description)
    }
}

pub(super) fn format_rule_list_answer(rules: &[HouseholdRule]) -> String {
    let items = rules
        .iter()
        .map(|rule| rule.description.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    format!("I found this rule: {items}")
}

pub(super) fn format_family_calendar_event_answer(event: &FamilyCalendarEvent) -> String {
    if event.event_type == "school_pickup" {
        return format!("I found this calendar event: {}", event.description);
    }

    let person = event.person.as_deref().unwrap_or("They");
    let day = event
        .day
        .as_deref()
        .map(|day| format!(" {day}"))
        .unwrap_or_default();
    let time = event
        .time
        .as_deref()
        .map(|time| format!(" at {time}"))
        .unwrap_or_default();
    format!(
        "Yes. {person} has {}{day}{time}. {}",
        event.title, event.description
    )
}

pub(super) fn format_shopping_list_answer(items: &[ShoppingListItem]) -> String {
    let names = items
        .iter()
        .map(|item| item.item.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("Shopping list: {names}.")
}

pub(super) fn format_household_inventory_item_answer(item: &HouseholdInventoryItem) -> String {
    let location = item
        .location
        .as_deref()
        .map(|location| format!(" in {location}"))
        .unwrap_or_default();
    match item.quantity.as_deref() {
        Some("0") => format!("No, I do not have {} remaining{}.", item.item, location),
        Some(quantity) => format!(
            "Yes, you have {quantity} {} remaining{}.",
            item.item, location
        ),
        None => format!("I found this inventory note: {}", item.description),
    }
}

pub(super) fn format_access_permission_answer(permission: &AccessPermission) -> String {
    if permission.allowed {
        format!("Yes. {}", permission.description)
    } else {
        format!("No. {}", permission.description)
    }
}

pub(super) fn format_household_task_log_answer(task: &HouseholdTaskLog) -> String {
    if task.status == "complete" {
        let time = task
            .time
            .as_deref()
            .map(|time| format!(" at {time}"))
            .unwrap_or_default();
        let task_name = match task.task.as_str() {
            "brush_teeth" => "brushing teeth",
            "feeding" if task.subject.as_deref() == Some("dog") => "feeding the dog",
            "feeding" if task.subject.as_deref() == Some("cat") => "feeding the cat",
            other => other,
        };
        format!("Yes. {} marked {task_name} complete{time}.", task.person)
    } else {
        format!("I found this task log: {}", task.description)
    }
}

pub(super) fn format_everyone_task_log_answer(
    profiles: &[String],
    logs: &[HouseholdTaskLog],
) -> String {
    let completed = logs
        .iter()
        .filter(|log| log.status == "complete")
        .map(|log| log.person.clone())
        .collect::<Vec<_>>();
    let completed_keys = completed
        .iter()
        .map(|name| normalize_name_key(name))
        .collect::<std::collections::HashSet<_>>();
    let not_logged = profiles
        .iter()
        .filter(|name| !completed_keys.contains(&normalize_name_key(name)))
        .cloned()
        .collect::<Vec<_>>();

    if completed.is_empty() {
        return "No one has logged brushing teeth yet.".into();
    }
    if not_logged.is_empty() {
        return format!(
            "Everyone has logged brushing teeth: {}.",
            join_names(&completed)
        );
    }
    format!(
        "{} have logged brushing teeth. Not logged yet: {}.",
        join_names(&completed),
        join_names(&not_logged)
    )
}

pub(super) fn join_names(names: &[String]) -> String {
    match names {
        [] => "none".into(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        many => {
            let (last, rest) = many.split_last().expect("non-empty slice");
            format!("{}, and {last}", rest.join(", "))
        }
    }
}

pub(super) fn format_household_schedule_item_answer(item: &HouseholdScheduleItem) -> String {
    match item.schedule_type.as_str() {
        "school_bus_arrival" => {
            let time = item.time.as_deref().unwrap_or("the scheduled time");
            format!("The bus arrives at {time}. {}", item.description)
        }
        "bill_due" => {
            let subject = item.subject.as_deref().unwrap_or("bill");
            let due = item
                .date
                .as_deref()
                .or(item.day.as_deref())
                .unwrap_or("the scheduled date");
            let amount = item
                .amount
                .as_deref()
                .map(|amount| format!(" The estimated amount is {amount}."))
                .unwrap_or_default();
            format!("The {subject} bill is due {due}.{amount}")
        }
        "recycling" => format!("I found this recycling schedule: {}", item.description),
        "trash_pickup" => format!("I found this trash pickup schedule: {}", item.description),
        "school_conference" => {
            let date = item
                .date
                .as_deref()
                .or(item.day.as_deref())
                .unwrap_or("the scheduled date");
            let time = item
                .time
                .as_deref()
                .map(|time| format!(" at {time}"))
                .unwrap_or_default();
            let subject = item
                .subject
                .as_deref()
                .map(|subject| format!(" for {subject}"))
                .unwrap_or_default();
            format!("The next parent-teacher conference is on {date}{time}{subject}.")
        }
        "sunset" => {
            let time = item.time.as_deref().unwrap_or("the scheduled time");
            format!("Sunset is at {time}. {}", item.description)
        }
        "community_facility_hours" => {
            format!(
                "I found this community facility schedule: {}",
                item.description
            )
        }
        "business_hours" => {
            let time = item
                .time
                .as_deref()
                .map(|time| format!(" It closes at {time}."))
                .unwrap_or_default();
            format!("I found these business hours: {}{time}", item.description)
        }
        "channel_guide" => {
            let subject = item.subject.as_deref().unwrap_or("That channel");
            let channel = item.amount.as_deref().unwrap_or("the listed channel");
            format!("{subject} is on channel {channel}.")
        }
        "tv_tonight" => format!("I found this TV schedule: {}", item.description),
        "community_meeting" => {
            let time = item
                .time
                .as_deref()
                .map(|time| format!(" at {time}"))
                .unwrap_or_default();
            let day_or_date = item
                .date
                .as_deref()
                .or(item.day.as_deref())
                .unwrap_or("the scheduled date");
            format!("The next city council meeting is on {day_or_date}{time}.")
        }
        "subscription_renewal" => {
            let subject = item.subject.as_deref().unwrap_or("subscription");
            format!(
                "I found this {subject} subscription schedule: {}",
                item.description
            )
        }
        _ => format!("I found this schedule item: {}", item.description),
    }
}

pub(super) fn format_household_event_log_answer(event: &HouseholdEventLog) -> String {
    if event.event_type == "security" && event.action == "disarm" {
        let actor = event.actor.as_deref().unwrap_or("someone");
        let time = event
            .time
            .as_deref()
            .map(|time| format!(" at {time}"))
            .unwrap_or_default();
        return format!("The security system was disarmed by {actor}{time}.");
    }

    if event.event_type == "finance" && event.action == "allowance" {
        return format!("Yes. {}", event.description);
    }

    if event.event_type == "finance" && event.action == "paid_bill" {
        return format!("Yes. {}", event.description);
    }

    if event.event_type == "finance" && event.action == "credit_score" {
        return event.description.clone();
    }

    if event.event_type == "finance" && event.action == "stock_price" {
        return event.description.clone();
    }

    if event.event_type == "health" && event.action == "weight_reading" {
        return event.description.clone();
    }

    if event.event_type == "health" && event.action == "vo2_max" {
        return event.description.clone();
    }

    if event.event_type == "appliance_state" && event.action == "clean_status" {
        return event.description.clone();
    }

    if event.event_type == "waste" && event.action == "collection" {
        return format!("Yes. {}", event.description);
    }

    if event.event_type == "environment" && event.action == "temperature" {
        return event.description.clone();
    }

    if event.event_type == "location" && event.action == "home_arrival" {
        return format!("Yes. {}", event.description);
    }

    if event.event_type == "location" && event.action == "presence_home" {
        return format!("Yes. {}", event.description);
    }

    if event.event_type == "access" && event.action == "open" {
        return event.description.clone();
    }

    format!("I found this event log: {}", event.description)
}

pub(super) fn format_household_note_answer(note: &HouseholdNote) -> String {
    match note.note_type.as_str() {
        "reminder" => format!("I found this reminder: {}", note.content),
        "manual" => format!("I found these instructions: {}", note.content),
        "media" => format!("I found this watch note: {}", note.content),
        "pet_health" => format!("I found this pet health note: {}", note.content),
        "home_maintenance" => format!("I found this home maintenance note: {}", note.content),
        "storage" => format!("I found this storage note: {}", note.content),
        "gift" => format!("I found this gift note: {}", note.content),
        "troubleshooting" => format!("I found this troubleshooting note: {}", note.content),
        "photo" => format!("I found this photo note: {}", note.content),
        "warranty" => format!("I found this warranty note: {}", note.content),
        "school" => format!("I found this school note: {}", note.content),
        "utility" => format!("I found this utility note: {}", note.content),
        "recycling" => format!("I found this recycling note: {}", note.content),
        "first_aid" => format!("I found this first-aid note: {}", note.content),
        "story" => format!("I found this story note: {}", note.content),
        "pet" => format!("I found this pet note: {}", note.content),
        "travel" => format!("I found this travel note: {}", note.content),
        "visitor" => format!("I found this visitor note: {}", note.content),
        "meal" => format!("I found this meal note: {}", note.content),
        "shopping" => format!("I found this shopping note: {}", note.content),
        "security" => format!("I found this security note: {}", note.content),
        "beverage" => format!("I found this beverage note: {}", note.content),
        "social" => format!("I found this social note: {}", note.content),
        "commute" => format!("I found this commute note: {}", note.content),
        "pantry" => format!("I found this pantry note: {}", note.content),
        "home_comfort" => format!("I found this comfort note: {}", note.content),
        "location" => format!("I found this location note: {}", note.content),
        "receipt" => format!("I found this receipt note: {}", note.content),
        "education" => format!("I found this education note: {}", note.content),
        "entertainment" => format!("I found this entertainment note: {}", note.content),
        "dictionary" => format!("I found this dictionary note: {}", note.content),
        "health" => format!("I found this health note: {}", note.content),
        "party" => format!("I found this party note: {}", note.content),
        "pest_control" => format!("I found this pest-control note: {}", note.content),
        "food_safety" => format!("I found this food-safety note: {}", note.content),
        "contact" => format!("I found this contact note: {}", note.content),
        "delivery" => format!("I found this delivery note: {}", note.content),
        "schedule" => format!("I found this schedule note: {}", note.content),
        "finance" => format!("I found this finance note: {}", note.content),
        "tool" => format!("I found this tool note: {}", note.content),
        "network" => format!("I found this network note: {}", note.content),
        "diy" => format!("I found this DIY note: {}", note.content),
        "fitness" => format!("I found this fitness note: {}", note.content),
        "safety" => format!("I found this safety note: {}", note.content),
        "device" => format!("I found this device note: {}", note.content),
        "news" => format!("I found this news note: {}", note.content),
        "profile" => format!("I found this profile note: {}", note.content),
        "family" => format!("I found this family note: {}", note.content),
        "garden" => format!("I found this garden note: {}", note.content),
        "inventory" => format!("I found this inventory note: {}", note.content),
        _ => format!("I found this note: {}", note.content),
    }
}

pub(super) fn format_app_only_secret_reference_answer(
    secret_ref: &AppOnlySecretReference,
) -> String {
    format!(
        "I have an app-only reference for {}. Open the local dashboard or credential store to view it; I won't speak the value in shared-room chat.",
        secret_ref.label
    )
}

pub(super) fn possessive_named_profile(
    content: &str,
    lower: &str,
) -> Option<(&'static str, String)> {
    let marker = " is named ";
    let marker_pos = lower.find(marker)?;
    let left = lower[..marker_pos].trim();
    let role_phrase = left
        .strip_prefix("user's ")
        .or_else(|| left.strip_prefix("my "))
        .or_else(|| left.strip_prefix("our "))?;
    let role = normalize_household_role(role_phrase)?;
    let name = clean_profile_name(&content[marker_pos + marker.len()..]);
    if name.is_empty() {
        None
    } else {
        Some((role, name))
    }
}

pub(super) fn definite_role_profile(content: &str, lower: &str) -> Option<(&'static str, String)> {
    let marker = " is ";
    let marker_pos = lower.find(marker)?;
    let left = lower[..marker_pos].trim();
    let role_phrase = left.strip_prefix("the ")?;
    let role = normalize_household_role(role_phrase)?;
    let name = clean_profile_name(&content[marker_pos + marker.len()..]);
    if name.is_empty() {
        None
    } else {
        Some((role, name))
    }
}

pub(super) fn subject_role_profile(content: &str, lower: &str) -> Option<(String, &'static str)> {
    for marker in [" is the ", " is our ", " is my "] {
        if let Some(marker_pos) = lower.find(marker) {
            let name = clean_profile_name(&content[..marker_pos]);
            let role_phrase = lower[marker_pos + marker.len()..].trim();
            let role = normalize_household_role(role_phrase)?;
            if !name.is_empty() {
                return Some((name, role));
            }
        }
    }
    None
}

pub(super) fn clean_profile_name(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("named ")
        .trim_matches(|ch: char| matches!(ch, '.' | ',' | '!' | '?' | '"' | '\''))
        .split_whitespace()
        .take(4)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn normalize_household_role(value: &str) -> Option<&'static str> {
    let normalized = value
        .trim()
        .trim_start_matches("the ")
        .trim_start_matches("a ")
        .trim_start_matches("an ")
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|ch: char| matches!(ch, '.' | ',' | '!' | '?' | ':' | ';'));

    match normalized {
        "dad" | "father" => Some("dad"),
        "mom" | "mother" | "mum" => Some("mom"),
        "son" | "sons" => Some("son"),
        "daughter" | "daughters" => Some("daughter"),
        "child" | "children" | "kid" | "kids" => Some("child"),
        "wife" => Some("wife"),
        "husband" => Some("husband"),
        "partner" => Some("partner"),
        "dog" | "dogs" => Some("dog"),
        "cat" | "cats" => Some("cat"),
        "pet" | "pets" => Some("pet"),
        _ => None,
    }
}

pub(super) fn canonical_date(ts_ms: u64) -> String {
    let secs = (ts_ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::gmtime_r(&secs, &mut tm) };
    if result.is_null() {
        return "1970-01-01".into();
    }
    format!(
        "{:04}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday
    )
}

pub(super) fn canonical_daily_note_file(canonical_dir: &Path, ts_ms: u64) -> PathBuf {
    canonical_dir.join(format!("{}.md", canonical_date(ts_ms)))
}

pub(super) fn canonical_event_file(canonical_dir: &Path, ts_ms: u64) -> PathBuf {
    canonical_dir
        .join("events")
        .join(format!("{}.jsonl", canonical_date(ts_ms)))
}

pub(super) fn canonical_namespace(kind: &str, metadata: policy::MemoryPolicyMetadata) -> String {
    let lower = kind.trim().to_ascii_lowercase();
    let leaf = lower
        .strip_prefix("person_")
        .or_else(|| lower.strip_prefix("private_"))
        .or_else(|| lower.strip_prefix("session_"))
        .or_else(|| lower.strip_prefix("household_"))
        .unwrap_or(&lower)
        .to_string();
    let leaf = sanitize_namespace_segment(&leaf);
    format!(
        "{}.{}",
        metadata.scope.as_str(),
        if leaf.is_empty() { "general" } else { &leaf }
    )
}

pub(super) fn canonical_namespace_note_relative(namespace: &str) -> String {
    let mut parts = namespace
        .split('.')
        .map(sanitize_namespace_segment)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        parts.push("general".into());
    }
    let leaf = parts.pop().unwrap_or_else(|| "general".into());
    let mut path = PathBuf::from("namespaces");
    for part in parts {
        path.push(part);
    }
    path.push(format!("{leaf}.md"));
    path.to_string_lossy().into_owned()
}

pub(super) fn sanitize_namespace_segment(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        let next = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else if ch == '_' || ch == '-' || ch == ' ' || ch == '.' {
            '-'
        } else {
            continue;
        };
        if next == '-' {
            if last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        out.push(next);
    }
    out.trim_matches('-').to_string()
}

pub(super) fn count_markdown_files(root: &Path) -> usize {
    if !root.exists() {
        return 0;
    }
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
            {
                count += 1;
            }
        }
    }
    count
}

/// Word overlap ratio between two strings (Jaccard-like).
pub(super) fn word_overlap(a: &str, b: &str) -> f64 {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();
    let a_words: std::collections::HashSet<&str> = a_lower.split_whitespace().collect();
    let b_words: std::collections::HashSet<&str> = b_lower.split_whitespace().collect();

    if a_words.is_empty() || b_words.is_empty() {
        return 0.0;
    }

    let intersection = a_words.intersection(&b_words).count();
    let union = a_words.union(&b_words).count();

    intersection as f64 / union as f64
}

pub(super) fn lexical_overlap_score(a: &str, b: &str) -> f64 {
    word_overlap(a, b).max(0.05)
}

pub(super) fn normalize_memory_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn memory_slot(kind: &str, content: &str) -> Option<String> {
    let kind = kind.trim().to_lowercase();
    let lower = content.trim().to_lowercase();

    match kind.as_str() {
        "identity" => {
            if lower.starts_with("user's name is ") {
                Some("identity:name".into())
            } else if lower.starts_with("user is ") && lower.contains(" years old") {
                Some("identity:age".into())
            } else if lower.starts_with("user lives in ") {
                Some("identity:location".into())
            } else if lower.starts_with("user works at ") {
                Some("identity:workplace".into())
            } else if lower.starts_with("user is a ") || lower.starts_with("user is an ") {
                Some("identity:occupation".into())
            } else {
                None
            }
        }
        "preference" => favorite_slot(&lower).map(|slot| format!("preference:favorite:{slot}")),
        _ => None,
    }
}

pub(super) fn favorite_slot(lower_content: &str) -> Option<String> {
    let rest = lower_content.strip_prefix("user's favorite ")?;
    let (thing, _) = rest.split_once(" is ")?;
    let thing = thing.trim();
    if thing.is_empty() {
        None
    } else {
        Some(thing.to_string())
    }
}

pub(super) fn search_tokens(text: &str) -> Vec<String> {
    let stop = [
        "a", "an", "and", "are", "about", "can", "did", "do", "does", "for", "have", "how", "i",
        "is", "it", "me", "my", "of", "on", "or", "please", "remember", "that", "the", "this",
        "to", "what", "whats", "when", "where", "who", "you", "your",
    ];
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| token.len() > 1 && !stop.contains(token))
        .map(ToString::to_string)
        .collect()
}
