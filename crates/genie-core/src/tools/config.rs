use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use genie_common::config::{ActuationSafetyConfig, ToolPolicyConfig, WebSearchConfig};

use crate::ha::HomeAutomationProvider;
use crate::memory::Memory;
use crate::skills::SkillLoader;

pub struct ToolDispatcherConfig {
    pub ha: Option<Arc<dyn HomeAutomationProvider>>,
    pub memory: Option<Arc<Mutex<Memory>>>,
    pub skill_loader: Option<Arc<Mutex<SkillLoader>>>,
    pub web_search: WebSearchConfig,
    pub tool_policy: ToolPolicyConfig,
    pub actuation_safety: ActuationSafetyConfig,
    pub actuation_audit_path: Option<PathBuf>,
    pub tool_audit_path: Option<PathBuf>,
}

impl ToolDispatcherConfig {
    pub fn new(ha: Option<Arc<dyn HomeAutomationProvider>>) -> Self {
        Self {
            ha,
            memory: None,
            skill_loader: None,
            web_search: WebSearchConfig::default(),
            tool_policy: ToolPolicyConfig::default(),
            actuation_safety: ActuationSafetyConfig::default(),
            actuation_audit_path: None,
            tool_audit_path: None,
        }
    }
}
