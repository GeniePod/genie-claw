/// Information flow control via taint labels.
///
/// Adapted from OpenFang's taint.rs — simplified for GeniePod's
/// compiled-tool architecture (no WASM, no arbitrary code exec).
///
/// Every value flowing through the tool system carries taint labels.
/// Before executing certain operations (network requests, output to user),
/// the labels are checked against sink policies.
///
/// RAM cost: ~0 (labels are enums stored inline on the stack).
use std::collections::HashSet;

/// Taint labels that can be attached to values.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum TaintLabel {
    /// Data from external network (weather API, web fetches).
    ExternalNetwork,
    /// Direct user input (chat messages).
    UserInput,
    /// Contains personally identifiable information.
    Pii,
    /// Contains secrets (API keys, tokens, passwords).
    Secret,
    /// Output from LLM (may contain hallucinated secrets or injected instructions).
    LlmOutput,
}

/// Operations that have taint restrictions.
#[derive(Debug, Clone, Copy)]
pub enum TaintSink {
    /// Displaying to user (block Secret labels).
    DisplayToUser,
    /// Sending over network (block Secret, Pii labels).
    NetworkSend,
    /// Storing in memory/conversation DB (block nothing — store everything).
    Storage,
    /// Passing to tool execution (block ExternalNetwork for shell-like tools).
    ToolExec,
}

/// Where a piece of data came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataOrigin {
    /// Generated locally (calculations, internal state).
    Local,
    /// Fetched from an external network (weather, web search).
    ExternalNetwork,
    /// Produced by an LLM.
    Llm,
    // Additional origins can be added later (e.g., Memory, UserInput).
}

/// A value with taint labels attached.
#[derive(Debug, Clone)]
pub struct Tainted<T> {
    value: T,
    labels: HashSet<TaintLabel>,
}

impl<T> Tainted<T> {
    /// Create a tainted value with a single label.
    pub fn new(value: T, label: TaintLabel) -> Self {
        let mut labels = HashSet::new();
        labels.insert(label);
        Self { value, labels }
    }

    /// Create a clean (untainted) value.
    pub fn clean(value: T) -> Self {
        Self {
            value,
            labels: HashSet::new(),
        }
    }

        /// Create a `Tainted` value from a tool result, inferring the label from the data origin.
    pub fn from_tool_result(value: T, origin: DataOrigin) -> Self {
        match origin {
            DataOrigin::ExternalNetwork => Tainted::new(value, TaintLabel::ExternalNetwork),
            DataOrigin::Llm => Tainted::new(value, TaintLabel::LlmOutput),
            DataOrigin::Local => Tainted::clean(value),
        }
    }

    /// Add a taint label.
    pub fn taint(&mut self, label: TaintLabel) {
        self.labels.insert(label);
    }

    /// Merge taint labels from another tainted value.
    pub fn merge_from<U>(&mut self, other: &Tainted<U>) {
        for label in &other.labels {
            self.labels.insert(*label);
        }
    }

    /// Check if this value can flow to the given sink.
    /// Returns Err with reason if blocked.
    pub fn check_sink(&self, sink: TaintSink) -> Result<(), String> {
        let blocked = blocked_labels(sink);
        for label in &self.labels {
            if blocked.contains(label) {
                return Err(format!(
                    "taint policy violation: {:?} cannot flow to {:?} (blocked by {:?})",
                    self.labels, sink, label
                ));
            }
        }
        Ok(())
    }

    /// Get the inner value after passing a sink check.
    pub fn unwrap_checked(self, sink: TaintSink) -> Result<T, String> {
        self.check_sink(sink)?;
        Ok(self.value)
    }

    /// Get the inner value without checking (use sparingly).
    pub fn into_inner(self) -> T {
        self.value
    }

    /// Reference to the inner value without checking.
    pub fn as_inner(&self) -> &T {
        &self.value
    }

    /// Check if a specific label is present.
    pub fn has_label(&self, label: TaintLabel) -> bool {
        self.labels.contains(&label)
    }

    /// Remove a label (explicit declassification — security decision).
    pub fn declassify(&mut self, label: TaintLabel) {
        self.labels.remove(&label);
    }
}

/// Which taint labels are blocked for each sink.
fn blocked_labels(sink: TaintSink) -> HashSet<TaintLabel> {
    let mut blocked = HashSet::new();
    match sink {
        TaintSink::DisplayToUser => {
            blocked.insert(TaintLabel::Secret);
        }
        TaintSink::NetworkSend => {
            blocked.insert(TaintLabel::Secret);
            blocked.insert(TaintLabel::Pii);
        }
        TaintSink::Storage => {
            // Store everything — memory system handles decay.
        }
        TaintSink::ToolExec => {
            blocked.insert(TaintLabel::ExternalNetwork);
        }
    }
    blocked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_value_passes_all_sinks() {
        let val: Tainted<String> = Tainted::clean("hello".into());
        assert!(val.check_sink(TaintSink::DisplayToUser).is_ok());
        assert!(val.check_sink(TaintSink::NetworkSend).is_ok());
        assert!(val.check_sink(TaintSink::ToolExec).is_ok());
    }

    #[test]
    fn secret_blocked_from_display() {
        let val: Tainted<String> = Tainted::new("sk-12345".into(), TaintLabel::Secret);
        assert!(val.check_sink(TaintSink::DisplayToUser).is_err());
        assert!(val.check_sink(TaintSink::NetworkSend).is_err());
        assert!(val.check_sink(TaintSink::Storage).is_ok()); // Storage always OK.
    }

    #[test]
    fn pii_blocked_from_network() {
        let val: Tainted<String> = Tainted::new("user@email.com".into(), TaintLabel::Pii);
        assert!(val.check_sink(TaintSink::DisplayToUser).is_ok()); // PII can display.
        assert!(val.check_sink(TaintSink::NetworkSend).is_err()); // But not network.
    }

    #[test]
    fn external_network_blocked_from_tool_exec() {
        let val: Tainted<String> = Tainted::new("curl cmd".into(), TaintLabel::ExternalNetwork);
        assert!(val.check_sink(TaintSink::ToolExec).is_err());
    }

    #[test]
    fn merge_propagates_labels() {
        let mut val1: Tainted<String> = Tainted::new("data".into(), TaintLabel::UserInput);
        let val2: Tainted<String> = Tainted::new("secret".into(), TaintLabel::Secret);
        val1.merge_from(&val2);

        assert!(val1.has_label(TaintLabel::UserInput));
        assert!(val1.has_label(TaintLabel::Secret));
        assert!(val1.check_sink(TaintSink::DisplayToUser).is_err()); // Now blocked.
    }

    #[test]
    fn declassify_removes_label() {
        let mut val: Tainted<String> = Tainted::new("sanitized".into(), TaintLabel::Secret);
        assert!(val.check_sink(TaintSink::DisplayToUser).is_err());

        val.declassify(TaintLabel::Secret);
        assert!(val.check_sink(TaintSink::DisplayToUser).is_ok());
    }

    #[test]
    fn unwrap_checked_blocks_on_violation() {
        let val: Tainted<String> = Tainted::new("key".into(), TaintLabel::Secret);
        assert!(val.unwrap_checked(TaintSink::DisplayToUser).is_err());

        let clean: Tainted<String> = Tainted::clean("safe".into());
        assert_eq!(
            clean.unwrap_checked(TaintSink::DisplayToUser).unwrap(),
            "safe"
        );
    }
}
