use std::sync::Arc;

use anyhow::Result;

use crate::memory::Memory;
use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};

pub struct MemoryStatusTool {
    pub memory: Arc<std::sync::Mutex<Memory>>,
}

impl ToolEntry for MemoryStatusTool {
    fn name(&self) -> &str {
        "memory_status"
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: "memory_status".into(),
            description: "Check memory database health, row counts, FTS consistency, and promoted memory count. Use for memory system diagnostics, not for recalling personal facts.".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    fn execute<'a>(
        &'a self,
        _args: &'a serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let memory = Arc::clone(&self.memory);
        Box::pin(async move {
            let mem = memory
                .lock()
                .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;
            let health = mem.health()?;
            let promoted = mem.promoted_count()?;
            let state =
                if health.quick_check_ok && health.fts_consistent && !health.migration_degraded {
                    "ok"
                } else {
                    "degraded"
                };

            Ok(format!(
                "Memory status: {}. Rows: {}. FTS rows: {}. FTS consistent: {}. Migration degraded: {}. Promoted memories: {}. Canonical root: {}. Namespace notes: {}. Daily notes: {}. Event logs: {}. Person-scoped memories: {}. Private memories: {}. Restricted memories: {}.",
                state,
                health.memory_rows,
                health.fts_rows,
                if health.fts_consistent { "yes" } else { "no" },
                if health.migration_degraded {
                    "yes"
                } else {
                    "no"
                },
                promoted,
                if health.canonical_root_exists {
                    "present"
                } else {
                    "missing"
                },
                health.canonical_namespace_files,
                health.canonical_daily_files,
                health.canonical_event_logs,
                health.person_rows,
                health.private_rows,
                health.restricted_rows,
            ))
        })
    }
}
