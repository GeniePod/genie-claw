# GenieClaw Bug Issue — memory_store plants person-scoped rows without verified write context

**GitHub:** https://github.com/GeniePod/genie-claw/issues/454

## Body

### Summary

`memory_store` does not receive `ToolExecutionContext`, unlike `memory_recall` and `memory_forget` (post-#433). Person-scoped rows can be **written** from untrusted channels even though recall/forget now require verified identity context.

This is the **write-side analogue** of #430 — not a read bypass, but a memory-pollution / policy-asymmetry bug.

### Steps to reproduce

1. From repo root (no Jetson / HA needed):

   ```bash
   cd genie-claw
   ```

2. **Primary repro — shadow category injection:**

   ```rust
   dispatcher.exec_memory_store(&serde_json::json!({
       "category": "person_preference",
       "content": "Maya likes oat milk"
   })).unwrap();
   // → stored as person-scoped (infer_metadata → Person scope; assess_memory_write allows)

   dispatcher.exec_memory_recall(
       &serde_json::json!({"query": "oat milk"}),
       ToolExecutionContext::default(),
   ).unwrap();
   // → "I don't remember anything about oat milk yet." (post-#433)
   // Row still exists in DB — write/read asymmetry
   ```

   `category` is optional in the tool schema enum but honored at runtime when `extract_facts` returns empty (`normalize_memories_to_store`).

3. **Clarification — what does *not* trigger person scope:**

   ```rust
   dispatcher.exec_memory_store(&serde_json::json!({
       "content": "Maya likes oat milk"
   }));
   // extract_facts returns [] → stores as household "fact", NOT person_preference
   ```

   The reliable attack is explicit `category: "person_preference"`, not bare content.

4. **Integration repro (API origin):**

   ```bash
   # ToolDispatcher::execute_with_context memory_store from RequestOrigin::Api
   # with category person_preference → row persisted
   # memory_recall same query → denied (post-#433)
   ```

### Expected behavior

- Person-scoped `memory_store` uses the same trusted-context contract as recall/forget ([#389](https://github.com/GeniePod/genie-claw/pull/389), [#433](https://github.com/GeniePod/genie-claw/pull/433)): only `exec_ctx.memory_read_context` from the voice pipeline may persist `person_*` categories.
- API/REPL/chat paths default to household-safe categories; shadow `person_preference` in tool JSON is rejected.
- Read/write/delete symmetry for person-scoped memory.

### Actual behavior

- `memory_store` dispatch: `"memory_store" => self.exec_memory_store(&call.arguments)` — **no exec_ctx**.
- Shadow `category: "person_preference"` → row persisted from API/REPL.
- `assess_memory_write` gates secrets/restricted content but **not** person-scope vs identity context.
- `memory_recall` / `memory_forget` deny access without trusted context, but do not prevent the write.

### Hardware

Non-Jetson (x86_64 dev / cross-build host)

### GenieClaw version / commit

`main` @ `b93fa59`

### Additional context

**Suggested fix direction**

1. Pass `exec_ctx` into `exec_memory_store` (mirror `memory_recall` / `memory_forget`).
2. Reject `person_*` categories when `memory_read_context` is absent or shared-room.
3. Add unit + API-origin integration tests.

Related: closed [#430](https://github.com/GeniePod/genie-claw/issues/430) (read). Parent: [#402](https://github.com/GeniePod/genie-claw/issues/402).
