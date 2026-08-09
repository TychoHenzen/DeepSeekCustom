//! Tests for `deepseek_custom::mcp::config`: which files define an MCP
//! server, which entries this client can actually run, and how a plugin's
//! `${CLAUDE_PLUGIN_ROOT}` placeholder is expanded.

use std::path::{Path, PathBuf};

use deepseek_custom::mcp::config::discover_servers_from;
use deepseek_custom::plugins::PluginRoot;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-mcpcfg-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_mcp_json(dir: &Path, body: &str) {
    std::fs::write(dir.join(".mcp.json"), body).unwrap();
}

/// A `~/.claude` lookalike, so `discover_servers_from` can find its parent
/// and read `~/.claude.json` and `~/.mcp.json` beside it.
fn fake_home(tag: &str) -> (PathBuf, PathBuf) {
    let home = temp_dir(tag);
    let claude = home.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    (home, claude)
}

#[test]
fn reads_a_project_mcp_json() {
    let project = temp_dir("project");
    write_mcp_json(
        &project,
        r#"{"mcpServers": {"local": {"command": "node", "args": ["server.js"]}}}"#,
    );

    let servers = discover_servers_from(&project, None, &[]);

    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "local");
    assert_eq!(servers[0].command, "node");
    assert_eq!(servers[0].args, vec!["server.js"]);
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn reads_the_user_claude_json() {
    let project = temp_dir("user-claude");
    let (home, claude) = fake_home("user-claude-home");
    std::fs::write(
        home.join(".claude.json"),
        r#"{"mcpServers": {"seq": {"type": "stdio", "command": "npx", "args": ["-y", "s"]}}}"#,
    )
    .unwrap();

    let servers = discover_servers_from(&project, Some(claude), &[]);

    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "seq");
    let _ = std::fs::remove_dir_all(&project);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn reads_the_user_mcp_json() {
    let project = temp_dir("user-mcp");
    let (home, claude) = fake_home("user-mcp-home");
    write_mcp_json(&home, r#"{"mcpServers": {"guard": {"command": "npx"}}}"#);

    let servers = discover_servers_from(&project, Some(claude), &[]);

    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "guard");
    let _ = std::fs::remove_dir_all(&project);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_project_entry_overrides_a_user_entry_of_the_same_name() {
    let project = temp_dir("override-project");
    let (home, claude) = fake_home("override-home");
    write_mcp_json(&home, r#"{"mcpServers": {"guard": {"command": "old"}}}"#);
    write_mcp_json(&project, r#"{"mcpServers": {"guard": {"command": "new"}}}"#);

    let servers = discover_servers_from(&project, Some(claude), &[]);

    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].command, "new");
    let _ = std::fs::remove_dir_all(&project);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn expands_the_plugin_root_placeholder() {
    // Every plugin server on this machine is written this way. Without the
    // expansion, `node ${CLAUDE_PLUGIN_ROOT}/dist/bundle.js` cannot start.
    let project = temp_dir("expand-project");
    let plugin_root = temp_dir("expand-plugin");
    write_mcp_json(
        &plugin_root,
        r#"{"mcpServers": {"dod-guard": {"command": "node", "args": ["${CLAUDE_PLUGIN_ROOT}/dist/bundle.js"]}}}"#,
    );
    let plugins = vec![PluginRoot {
        name: "dod-guard".into(),
        marketplace: "dod-guard".into(),
        root: plugin_root.clone(),
    }];

    let servers = discover_servers_from(&project, None, &plugins);

    assert_eq!(servers.len(), 1);
    assert!(!servers[0].args[0].contains("${CLAUDE_PLUGIN_ROOT}"));
    assert!(servers[0].args[0].ends_with("dist/bundle.js"));
    assert!(
        servers[0].args[0].contains(
            &plugin_root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        )
    );
    let _ = std::fs::remove_dir_all(&project);
    let _ = std::fs::remove_dir_all(&plugin_root);
}

#[test]
fn expands_the_placeholder_in_the_command_and_the_environment_too() {
    let project = temp_dir("expand-all");
    let plugin_root = temp_dir("expand-all-plugin");
    write_mcp_json(
        &plugin_root,
        r#"{"mcpServers": {"p": {"command": "${CLAUDE_PLUGIN_ROOT}/bin/run", "env": {"HOME_DIR": "${CLAUDE_PLUGIN_ROOT}"}}}}"#,
    );
    let plugins = vec![PluginRoot {
        name: "p".into(),
        marketplace: "m".into(),
        root: plugin_root.clone(),
    }];

    let servers = discover_servers_from(&project, None, &plugins);

    assert!(!servers[0].command.contains("${CLAUDE_PLUGIN_ROOT}"));
    assert!(!servers[0].env["HOME_DIR"].contains("${CLAUDE_PLUGIN_ROOT}"));
    let _ = std::fs::remove_dir_all(&project);
    let _ = std::fs::remove_dir_all(&plugin_root);
}

#[test]
fn skips_a_non_stdio_transport() {
    // This client speaks stdio alone. An HTTP entry that silently never
    // appeared would look like a bug in the server.
    let project = temp_dir("http");
    write_mcp_json(
        &project,
        r#"{"mcpServers": {"remote": {"type": "http", "command": "curl"}}}"#,
    );

    assert!(discover_servers_from(&project, None, &[]).is_empty());
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn skips_an_entry_with_no_command() {
    let project = temp_dir("no-command");
    write_mcp_json(&project, r#"{"mcpServers": {"broken": {"args": ["x"]}}}"#);

    assert!(discover_servers_from(&project, None, &[]).is_empty());
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn an_absent_type_is_taken_as_stdio() {
    // Several real entries leave `type` out, and dropping them would lose
    // the servers the user actually runs.
    let project = temp_dir("no-type");
    write_mcp_json(&project, r#"{"mcpServers": {"p": {"command": "node"}}}"#);

    assert_eq!(discover_servers_from(&project, None, &[]).len(), 1);
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn a_missing_file_is_not_an_error() {
    let project = temp_dir("missing");

    assert!(discover_servers_from(&project, None, &[]).is_empty());
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn unparseable_json_yields_no_servers_rather_than_failing() {
    let project = temp_dir("bad-json");
    write_mcp_json(&project, "{ not json");

    assert!(discover_servers_from(&project, None, &[]).is_empty());
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn servers_come_back_sorted_by_name() {
    let project = temp_dir("sorted");
    write_mcp_json(
        &project,
        r#"{"mcpServers": {"zebra": {"command": "a"}, "alpha": {"command": "b"}}}"#,
    );

    let servers = discover_servers_from(&project, None, &[]);

    assert_eq!(servers[0].name, "alpha");
    assert_eq!(servers[1].name, "zebra");
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn an_environment_block_is_carried_through() {
    let project = temp_dir("env");
    write_mcp_json(
        &project,
        r#"{"mcpServers": {"p": {"command": "node", "env": {"TOKEN": "abc"}}}}"#,
    );

    let servers = discover_servers_from(&project, None, &[]);

    assert_eq!(servers[0].env["TOKEN"], "abc");
    let _ = std::fs::remove_dir_all(&project);
}
