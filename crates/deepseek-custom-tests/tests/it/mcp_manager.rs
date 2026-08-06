//! Tests for `deepseek_custom::mcp::manager`: getting a server's tools
//! into a registry, including one attached before the server had answered.

use std::collections::HashMap;
use std::time::Duration;

use deepseek_custom::mcp::config::ServerConfig;
use deepseek_custom::mcp::manager::McpManager;
use deepseek_custom::tools::ToolRegistry;

fn fake_server(name: &str, markers: &[&str]) -> ServerConfig {
    ServerConfig {
        name: name.to_string(),
        command: env!("CARGO_BIN_EXE_fake_mcp_server").to_string(),
        args: markers.iter().map(|m| m.to_string()).collect(),
        env: HashMap::new(),
    }
}

/// Wait for the registry to hold `expected` tools, or give up.
///
/// Startup is deliberately asynchronous: a server may take minutes to
/// answer, and the window must not wait for it. So a test polls rather than
/// awaiting a completion signal that production code does not have.
async fn wait_for_tools(registry: &ToolRegistry, expected: usize) -> usize {
    for _ in 0..100 {
        let count = registry.list().len();
        if count >= expected {
            return count;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    registry.list().len()
}

#[tokio::test]
async fn a_servers_tools_reach_an_attached_registry() {
    let manager = McpManager::new();
    let registry = ToolRegistry::new();
    manager.attach(&registry).await;

    manager.start(vec![fake_server("fake", &[])]);

    assert_eq!(wait_for_tools(&registry, 2).await, 2);
    let mut names: Vec<String> = registry.list().iter().map(|t| t.name().to_string()).collect();
    names.sort();
    assert_eq!(names, vec!["mcp__fake__boom", "mcp__fake__echo"]);
    manager.shutdown().await;
}

#[tokio::test]
async fn a_registry_attached_after_a_server_started_still_gets_its_tools() {
    // A subagent's registry is built long after startup. It must see the
    // servers that were already running.
    let manager = McpManager::new();
    let early = ToolRegistry::new();
    manager.attach(&early).await;
    manager.start(vec![fake_server("fake", &[])]);
    wait_for_tools(&early, 2).await;

    let late = ToolRegistry::new();
    manager.attach(&late).await;

    assert_eq!(late.list().len(), 2);
    manager.shutdown().await;
}

#[tokio::test]
async fn two_servers_both_contribute() {
    let manager = McpManager::new();
    let registry = ToolRegistry::new();
    manager.attach(&registry).await;

    manager.start(vec![fake_server("one", &[]), fake_server("two", &[])]);

    assert_eq!(wait_for_tools(&registry, 4).await, 4);
    let names: Vec<String> = registry.list().iter().map(|t| t.name().to_string()).collect();
    assert!(names.iter().any(|n| n == "mcp__one__echo"));
    assert!(names.iter().any(|n| n == "mcp__two__echo"));
    manager.shutdown().await;
}

#[tokio::test]
async fn a_server_that_will_not_start_does_not_stop_the_others() {
    let manager = McpManager::new();
    let registry = ToolRegistry::new();
    manager.attach(&registry).await;
    let broken = ServerConfig {
        name: "broken".to_string(),
        command: "definitely-not-a-real-command-xyz".to_string(),
        args: Vec::new(),
        env: HashMap::new(),
    };

    manager.start(vec![broken, fake_server("good", &[])]);

    assert_eq!(wait_for_tools(&registry, 2).await, 2);
    manager.shutdown().await;
}

#[tokio::test]
async fn a_server_with_no_tools_contributes_nothing_and_is_not_an_error() {
    let manager = McpManager::new();
    let registry = ToolRegistry::new();
    manager.attach(&registry).await;

    manager.start(vec![fake_server("empty", &["--no-tools"])]);
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(registry.list().is_empty());
    manager.shutdown().await;
}

#[tokio::test]
async fn starting_no_servers_is_a_no_op() {
    let manager = McpManager::new();
    let registry = ToolRegistry::new();
    manager.attach(&registry).await;

    manager.start(Vec::new());
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(registry.list().is_empty());
}

#[tokio::test]
async fn a_dropped_registry_does_not_keep_being_fed() {
    // A subagent builds a registry per dispatch. Holding those strongly
    // would pile up one dead registry per subagent for the life of the
    // process.
    let manager = McpManager::new();
    let long_lived = ToolRegistry::new();
    manager.attach(&long_lived).await;
    {
        let short_lived = ToolRegistry::new();
        manager.attach(&short_lived).await;
    }

    manager.start(vec![fake_server("fake", &[])]);

    assert_eq!(wait_for_tools(&long_lived, 2).await, 2);
    manager.shutdown().await;
}

#[tokio::test]
async fn a_registered_mcp_tool_actually_calls_its_server() {
    let manager = McpManager::new();
    let registry = ToolRegistry::new();
    manager.attach(&registry).await;
    manager.start(vec![fake_server("fake", &[])]);
    wait_for_tools(&registry, 2).await;

    let tool = registry.get("mcp__fake__echo").unwrap();
    let out = tool
        .execute(serde_json::json!({"text": "through the registry"}))
        .await
        .unwrap();

    assert_eq!(out.content, "through the registry");
    manager.shutdown().await;
}
