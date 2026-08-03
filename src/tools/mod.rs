pub mod ask;
pub mod bash;
pub mod read;
pub mod reset;
pub mod write;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::api::types::ToolDef;
use crate::config::settings::PermissionsConfig;
use crate::error::Result;

/// Trait implemented by all tools the agent can invoke.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name (e.g., "bash", "read").
    fn name(&self) -> &str;

    /// Human-readable description for the model's context.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's input parameters.
    fn input_schema(&self) -> serde_json::Value;

    /// Execute the tool with the given JSON input.
    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput>;
}

/// Result of a tool execution.
#[derive(Debug)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

/// Registry of all available tools, keyed by name.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn list(&self) -> Vec<&Arc<dyn Tool>> {
        self.tools.values().collect()
    }

    pub fn to_api_definitions(&self) -> Vec<ToolDef> {
        self.tools
            .values()
            .map(|t| ToolDef {
                tool_type: "function".to_string(),
                function: crate::api::types::FunctionDef {
                    name: t.name().to_string(),
                    description: t.description().to_string(),
                    parameters: t.input_schema(),
                },
            })
            .collect()
    }

    /// Check whether a tool is permitted based on allow/deny lists.
    ///
    /// Logic: deny checked first (always overrides), then allow.
    /// If allow is empty → all tools permitted (except denied ones).
    pub fn check_permission(&self, tool_name: &str, permissions: &PermissionsConfig) -> bool {
        // Deny list always takes precedence
        if let Some(ref deny) = permissions.deny {
            for pattern in deny {
                if tool_matches(tool_name, pattern) {
                    return false;
                }
            }
        }

        // If allow list is empty, permit everything not denied
        let Some(ref allow) = permissions.allow else {
            return true;
        };

        // Check allow list
        for pattern in allow {
            if tool_matches(tool_name, pattern) {
                return true;
            }
        }

        false
    }
}

/// Check if a tool name matches a pattern (exact or wildcard suffix).
fn tool_matches(name: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    name == pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_overrides_allow() {
        let perm = PermissionsConfig {
            allow: Some(vec!["bash".into()]),
            deny: Some(vec!["bash".into()]),
        };
        let registry = ToolRegistry::new();
        assert!(!registry.check_permission("bash", &perm));
    }

    #[test]
    fn empty_allow_permits_all_except_denied() {
        let perm = PermissionsConfig {
            allow: None,
            deny: Some(vec!["dangerous".into()]),
        };
        let registry = ToolRegistry::new();
        assert!(registry.check_permission("bash", &perm));
        assert!(registry.check_permission("read", &perm));
        assert!(!registry.check_permission("dangerous", &perm));
    }

    #[test]
    fn explicit_allow_required_when_list_present() {
        let perm = PermissionsConfig {
            allow: Some(vec!["bash".into(), "read".into()]),
            deny: None,
        };
        let registry = ToolRegistry::new();
        assert!(registry.check_permission("bash", &perm));
        assert!(registry.check_permission("read", &perm));
        assert!(!registry.check_permission("write", &perm));
    }

    #[test]
    fn wildcard_allow_matches_prefix() {
        let perm = PermissionsConfig {
            allow: Some(vec!["test_*".into()]),
            deny: None,
        };
        let registry = ToolRegistry::new();
        assert!(registry.check_permission("test_foo", &perm));
        assert!(registry.check_permission("test_bar", &perm));
        assert!(!registry.check_permission("bash", &perm));
    }
}
