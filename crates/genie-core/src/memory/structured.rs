//! Structured derived-table maintenance for the memory subsystem.
//!
//! Extracted verbatim from `memory/mod.rs` (issue #404-style module split, no
//! behavior change): the `rebuild_*` / `upsert_*_from_memory` / `delete_*`
//! free functions that keep the derived household/device-alias tables in sync
//! with the base `memories` table. All operate on `&Connection`.

use super::*;

pub(super) fn rebuild_household_profiles(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_profiles", [])?;

    let mut stmt = conn.prepare(
        "SELECT id, kind, content, scope, sensitivity, spoken_policy
         FROM memories
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                policy::MemoryPolicyMetadata {
                    scope: policy::MemoryScope::from_storage(&row.get::<_, String>(3)?),
                    sensitivity: policy::MemorySensitivity::from_storage(&row.get::<_, String>(4)?),
                    spoken_policy: policy::SpokenMemoryPolicy::from_storage(
                        &row.get::<_, String>(5)?,
                    ),
                },
            ))
        })?
        .filter_map(|row| row.ok())
        .collect::<Vec<_>>();
    drop(stmt);

    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_household_profile_from_memory(conn, id, &kind, &content, metadata, now)?;
    }

    Ok(())
}

pub(super) fn rebuild_device_aliases(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM device_aliases", [])?;

    let mut stmt = conn.prepare(
        "SELECT id, content, scope, sensitivity, spoken_policy
         FROM memories
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                policy::MemoryPolicyMetadata {
                    scope: policy::MemoryScope::from_storage(&row.get::<_, String>(2)?),
                    sensitivity: policy::MemorySensitivity::from_storage(&row.get::<_, String>(3)?),
                    spoken_policy: policy::SpokenMemoryPolicy::from_storage(
                        &row.get::<_, String>(4)?,
                    ),
                },
            ))
        })?
        .filter_map(|row| row.ok())
        .collect::<Vec<_>>();
    drop(stmt);

    let now = now_ms();
    for (id, content, metadata) in rows {
        upsert_device_alias_from_memory(conn, id, &content, metadata, now)?;
    }

    Ok(())
}

pub(super) fn rebuild_household_profile_attributes(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_profile_attributes", [])?;
    let rows = shared_safe_memory_rows(conn)?;
    let now = now_ms();
    for (id, content, metadata) in rows {
        upsert_household_profile_attributes_from_memory(conn, id, &content, metadata, now)?;
    }
    Ok(())
}

pub(super) fn rebuild_household_rules(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_rules", [])?;
    let rows = shared_safe_memory_rows(conn)?;
    let now = now_ms();
    for (id, content, metadata) in rows {
        upsert_household_rules_from_memory(conn, id, &content, metadata, now)?;
    }
    Ok(())
}

pub(super) fn rebuild_household_notes(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_notes", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_household_note_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

pub(super) fn rebuild_app_only_secret_references(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM app_only_secret_references", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_app_only_secret_reference_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

pub(super) fn rebuild_media_profile_items(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM media_profile_items", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_media_profile_item_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

pub(super) fn rebuild_family_calendar_events(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM family_calendar_events", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_family_calendar_events_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

pub(super) fn rebuild_shopping_list_items(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM shopping_list_items", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_shopping_list_items_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

pub(super) fn rebuild_household_inventory_items(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_inventory_items", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_household_inventory_items_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

pub(super) fn rebuild_access_permissions(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM access_permissions", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_access_permissions_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

pub(super) fn rebuild_household_task_logs(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_task_logs", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_household_task_logs_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

pub(super) fn rebuild_household_schedule_items(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_schedule_items", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_household_schedule_items_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

pub(super) fn rebuild_household_event_logs(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_event_logs", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_household_event_logs_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

pub(super) fn rebuild_embedded_memories(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM embedded_memories", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_embedded_memory_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

pub(super) fn shared_safe_memory_rows(
    conn: &Connection,
) -> Result<Vec<(i64, String, policy::MemoryPolicyMetadata)>> {
    let mut stmt = conn.prepare(
        "SELECT id, content, scope, sensitivity, spoken_policy
         FROM memories
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                policy::MemoryPolicyMetadata {
                    scope: policy::MemoryScope::from_storage(&row.get::<_, String>(2)?),
                    sensitivity: policy::MemorySensitivity::from_storage(&row.get::<_, String>(3)?),
                    spoken_policy: policy::SpokenMemoryPolicy::from_storage(
                        &row.get::<_, String>(4)?,
                    ),
                },
            ))
        })?
        .filter_map(|row| row.ok())
        .collect();
    Ok(rows)
}

pub(super) fn shared_safe_memory_rows_with_kind(
    conn: &Connection,
) -> Result<Vec<(i64, String, String, policy::MemoryPolicyMetadata)>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, content, scope, sensitivity, spoken_policy
         FROM memories
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                policy::MemoryPolicyMetadata {
                    scope: policy::MemoryScope::from_storage(&row.get::<_, String>(3)?),
                    sensitivity: policy::MemorySensitivity::from_storage(&row.get::<_, String>(4)?),
                    spoken_policy: policy::SpokenMemoryPolicy::from_storage(
                        &row.get::<_, String>(5)?,
                    ),
                },
            ))
        })?
        .filter_map(|row| row.ok())
        .collect();
    Ok(rows)
}

pub(super) fn upsert_household_profile_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_profiles WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    let Some((name, role)) = household_profile_from_memory(kind, content) else {
        return Ok(());
    };

    conn.execute(
        "INSERT INTO household_profiles (source_memory_id, name, role, updated_ms)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![source_memory_id, name, role, updated_ms],
    )?;
    Ok(())
}

pub(super) fn upsert_household_profile_attributes_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_profile_attributes WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for attr in household_profile_attributes_from_memory(content) {
        let normalized_name = normalize_name_key(&attr.name);
        conn.execute(
            "INSERT INTO household_profile_attributes (
                source_memory_id, name, normalized_name, attribute, value, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                source_memory_id,
                attr.name,
                normalized_name,
                attr.attribute,
                attr.value,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

pub(super) fn upsert_household_rules_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_rules WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for rule in household_rules_from_memory(content) {
        let normalized_person = rule.person.as_deref().map(normalize_name_key);
        conn.execute(
            "INSERT INTO household_rules (
                source_memory_id, person, normalized_person, rule_type, subject,
                value, allowed, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                source_memory_id,
                rule.person,
                normalized_person,
                rule.rule_type,
                rule.subject,
                rule.value,
                if rule.allowed { 1 } else { 0 },
                rule.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

pub(super) fn upsert_household_note_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_notes WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    let Some((note_type, title, content)) = household_note_from_memory(kind, content) else {
        return Ok(());
    };

    conn.execute(
        "INSERT INTO household_notes (source_memory_id, note_type, title, content, updated_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![source_memory_id, note_type, title, content, updated_ms],
    )?;
    Ok(())
}

pub(super) fn upsert_app_only_secret_reference_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM app_only_secret_references WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    let Some(secret_ref) = app_only_secret_reference_from_memory(kind, content, metadata) else {
        return Ok(());
    };

    conn.execute(
        "INSERT INTO app_only_secret_references (
            source_memory_id, secret_type, label, normalized_label, location_hint, updated_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            source_memory_id,
            secret_ref.secret_type,
            secret_ref.label,
            normalize_alias_key(&secret_ref.label),
            secret_ref.location_hint,
            updated_ms
        ],
    )?;
    Ok(())
}

pub(super) fn upsert_media_profile_item_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    _kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM media_profile_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    let Some(item) = media_profile_item_from_memory(content) else {
        return Ok(());
    };
    let normalized_owner = item
        .owner
        .as_deref()
        .map(normalize_name_key)
        .unwrap_or_default();
    let normalized_name = normalize_alias_key(&item.name);

    conn.execute(
        "INSERT INTO media_profile_items (
            source_memory_id, owner, normalized_owner, item_type, name,
            normalized_name, provider, target, updated_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            source_memory_id,
            item.owner,
            normalized_owner,
            item.item_type,
            item.name,
            normalized_name,
            item.provider,
            item.target,
            updated_ms
        ],
    )?;
    Ok(())
}

pub(super) fn upsert_family_calendar_events_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM family_calendar_events WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for event in family_calendar_events_from_memory(kind, content) {
        let normalized_person = event.person.as_deref().map(normalize_name_key);
        conn.execute(
            "INSERT INTO family_calendar_events (
                source_memory_id, person, normalized_person, event_type, title,
                day, time, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                source_memory_id,
                event.person,
                normalized_person,
                event.event_type,
                event.title,
                event.day,
                event.time,
                event.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

pub(super) fn upsert_shopping_list_items_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM shopping_list_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for item in shopping_list_items_from_memory(kind, content) {
        let normalized_item = normalize_alias_key(&item.item);
        conn.execute(
            "INSERT INTO shopping_list_items (
                source_memory_id, item, normalized_item, status, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                source_memory_id,
                item.item,
                normalized_item,
                item.status,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

pub(super) fn upsert_household_inventory_items_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_inventory_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for item in household_inventory_items_from_memory(kind, content) {
        conn.execute(
            "INSERT INTO household_inventory_items (
                source_memory_id, item, normalized_item, quantity, location,
                category, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                source_memory_id,
                item.item,
                normalize_inventory_item(&item.item),
                item.quantity,
                item.location,
                item.category,
                item.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

pub(super) fn upsert_access_permissions_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM access_permissions WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for permission in access_permissions_from_memory(kind, content) {
        let normalized_person = normalize_name_key(&permission.person);
        let normalized_device = normalize_alias_key(&permission.device);
        conn.execute(
            "INSERT INTO access_permissions (
                source_memory_id, person, normalized_person, device, normalized_device,
                action, allowed, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                source_memory_id,
                permission.person,
                normalized_person,
                permission.device,
                normalized_device,
                permission.action,
                if permission.allowed { 1 } else { 0 },
                permission.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

pub(super) fn upsert_household_task_logs_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_task_logs WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for task in household_task_logs_from_memory(kind, content) {
        let normalized_person = normalize_name_key(&task.person);
        let normalized_subject = task.subject.as_deref().map(normalize_alias_key);
        conn.execute(
            "INSERT INTO household_task_logs (
                source_memory_id, person, normalized_person, task, subject,
                normalized_subject, day, time, status, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                source_memory_id,
                task.person,
                normalized_person,
                task.task,
                task.subject,
                normalized_subject,
                task.day,
                task.time,
                task.status,
                task.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

pub(super) fn upsert_household_schedule_items_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_schedule_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for item in household_schedule_items_from_memory(kind, content) {
        let normalized_subject = item.subject.as_deref().map(normalize_alias_key);
        conn.execute(
            "INSERT INTO household_schedule_items (
                source_memory_id, schedule_type, subject, normalized_subject, title,
                day, date, time, amount, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                source_memory_id,
                item.schedule_type,
                item.subject,
                normalized_subject,
                item.title,
                item.day,
                item.date,
                item.time,
                item.amount,
                item.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

pub(super) fn upsert_household_event_logs_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_event_logs WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for event in household_event_logs_from_memory(kind, content) {
        let normalized_subject = event.subject.as_deref().map(normalize_alias_key);
        let normalized_actor = event.actor.as_deref().map(normalize_name_key);
        conn.execute(
            "INSERT INTO household_event_logs (
                source_memory_id, event_type, subject, normalized_subject, action,
                actor, normalized_actor, time, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                source_memory_id,
                event.event_type,
                event.subject,
                normalized_subject,
                event.action,
                event.actor,
                normalized_actor,
                event.time,
                event.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

pub(super) fn upsert_embedded_memory_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM embedded_memories WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !should_embed_memory(kind, content, metadata) {
        return Ok(());
    }

    let provider = LocalHashEmbeddingProvider;
    let embedding_text = embedding_text_for_memory(kind, content);
    let embedding = provider.embed(&embedding_text);
    let embedding_blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

    conn.execute(
        "INSERT INTO embedded_memories (
            source_memory_id, memory_type, embedding_model, dimensions, embedding, updated_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            source_memory_id,
            semantic_memory_type(kind, content),
            provider.model_name(),
            provider.dimensions() as i64,
            embedding_blob,
            updated_ms
        ],
    )?;
    Ok(())
}

pub(super) fn delete_structured_household_rows(
    conn: &Connection,
    source_memory_id: i64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_profile_attributes WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM household_rules WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM household_notes WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM app_only_secret_references WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM media_profile_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM family_calendar_events WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM shopping_list_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM household_inventory_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM access_permissions WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM household_task_logs WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM household_schedule_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM household_event_logs WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM embedded_memories WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    Ok(())
}

pub(super) fn upsert_device_alias_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM device_aliases WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    let Some((alias, target_id)) = device_alias_from_memory(content) else {
        return Ok(());
    };
    let normalized_alias = normalize_alias_key(&alias);
    if normalized_alias.is_empty() || target_id.is_empty() {
        return Ok(());
    }
    let kind = device_alias_kind(&target_id);

    conn.execute(
        "INSERT INTO device_aliases (
            source_memory_id, alias, normalized_alias, target_id, kind, updated_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            source_memory_id,
            alias,
            normalized_alias,
            target_id,
            kind,
            updated_ms
        ],
    )?;
    warn_if_device_alias_conflict(conn, &normalized_alias)?;
    Ok(())
}

pub(super) fn build_device_alias_conflict(
    normalized_alias: String,
    entries: Vec<DeviceAliasConflictEntry>,
) -> DeviceAliasConflict {
    let winner = entries.first().expect("conflict entries must not be empty");
    DeviceAliasConflict {
        normalized_alias,
        winning_source_memory_id: winner.source_memory_id,
        winning_target_id: winner.target_id.clone(),
        entries,
    }
}

pub(super) fn warn_if_device_alias_conflict(
    conn: &Connection,
    normalized_alias: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT COUNT(DISTINCT target_id)
         FROM device_aliases
         WHERE normalized_alias = ?1",
    )?;
    let distinct_targets: i64 = stmt.query_row([normalized_alias], |row| row.get(0))?;
    if distinct_targets > 1 {
        tracing::warn!(
            normalized_alias = normalized_alias,
            distinct_targets = distinct_targets,
            "device alias conflict: multiple Home Assistant targets share this alias; \
             using deterministic precedence (evergreen > promoted > lowest memory id)"
        );
    }
    Ok(())
}
