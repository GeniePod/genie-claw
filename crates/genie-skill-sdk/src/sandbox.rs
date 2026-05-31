//! Per-skill Landlock rules for native `.so` workers.
//!
//! genie-core applies the process-wide sandbox at startup (issue #347). Skill
//! workers still run in-process today; tighter per-skill rules are tracked as a
//! follow-up once worker processes are forked before `dlopen`.
