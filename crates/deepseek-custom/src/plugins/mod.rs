//! Where Claude Code's installed plugins live on disk.
//!
//! Two files decide it, and both are Claude Code's, not this harness's.
//! `~/.claude/settings.json` holds `enabledPlugins`, a map from
//! `"<plugin>@<marketplace>"` to a boolean. `~/.claude/plugins/installed_plugins.json`
//! holds an `installPath` per the same key. Reading the second one is what
//! keeps this simple: a marketplace manifest describes a plugin's source in
//! several different shapes (a relative path, a git subdirectory, a plain
//! URL), while `installPath` is always the resolved directory the plugin
//! was actually unpacked into.
//!
//! Both skills (`src/skills/discovery.rs`) and MCP servers
//! (`src/mcp/config.rs`) start from the roots this module returns. A plugin
//! ships its skills under `<root>/skills/` and its servers in
//! `<root>/.mcp.json`, where `${CLAUDE_PLUGIN_ROOT}` expands to `<root>`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{debug, warn};

/// One installed, enabled plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRoot {
    /// The plugin's own name, the part before `@` in the enabled-plugins key.
    pub name: String,
    /// The marketplace it came from, the part after `@`.
    pub marketplace: String,
    /// The directory it was unpacked into.
    pub root: PathBuf,
}

/// The `enabledPlugins` block of `~/.claude/settings.json`. Every other
/// field of that file belongs to Claude Code and is ignored here.
#[derive(Deserialize, Default)]
struct ClaudeSettings {
    #[serde(rename = "enabledPlugins", default)]
    enabled_plugins: HashMap<String, bool>,
}

/// `~/.claude/plugins/installed_plugins.json`.
#[derive(Deserialize, Default)]
struct InstalledPlugins {
    #[serde(default)]
    plugins: HashMap<String, Vec<InstalledEntry>>,
}

#[derive(Deserialize)]
struct InstalledEntry {
    #[serde(rename = "installPath")]
    install_path: Option<String>,
}

/// The user's `~/.claude` directory, or `None` when neither `HOME` nor
/// `USERPROFILE` is set.
pub fn claude_home() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    if home.is_empty() {
        return None;
    }
    Some(Path::new(&home).join(".claude"))
}

/// Every enabled plugin whose install directory is really on disk.
///
/// A plugin listed as enabled but never installed, or installed to a path
/// that has since been deleted, is skipped with a `debug` line rather than
/// an error. Neither file existing yields an empty list, which is the
/// normal case on a machine without Claude Code.
pub fn enabled_plugin_roots() -> Vec<PluginRoot> {
    let Some(home) = claude_home() else {
        debug!("plugins: no home directory, skipping plugin discovery");
        return Vec::new();
    };
    enabled_plugin_roots_in(&home)
}

/// `enabled_plugin_roots`, against an explicit `~/.claude` directory.
/// Separated so a test can point it at a fixture tree instead of the real
/// machine's plugins.
pub fn enabled_plugin_roots_in(claude_dir: &Path) -> Vec<PluginRoot> {
    let settings: ClaudeSettings = read_json(&claude_dir.join("settings.json"));
    let installed: InstalledPlugins =
        read_json(&claude_dir.join("plugins").join("installed_plugins.json"));

    let mut roots = Vec::new();
    for (key, enabled) in &settings.enabled_plugins {
        if !enabled {
            continue;
        }
        let Some(root) = resolve_install_path(&installed, key) else {
            debug!("plugins: {key} is enabled but has no installed path");
            continue;
        };
        let (name, marketplace) = split_plugin_key(key);
        roots.push(PluginRoot {
            name,
            marketplace,
            root,
        });
    }
    roots.sort_by(|a, b| a.name.cmp(&b.name));
    roots
}

/// The first install path recorded for `key` that still exists on disk.
/// A plugin may carry several entries, one per scope, and an entry may
/// name a directory a later uninstall removed.
fn resolve_install_path(installed: &InstalledPlugins, key: &str) -> Option<PathBuf> {
    installed
        .plugins
        .get(key)?
        .iter()
        .filter_map(|e| e.install_path.as_deref())
        .map(PathBuf::from)
        .find(|p| p.is_dir())
}

/// Split `"<plugin>@<marketplace>"`. A key with no `@` is taken as a plugin
/// name with an unknown marketplace rather than dropped.
fn split_plugin_key(key: &str) -> (String, String) {
    match key.rsplit_once('@') {
        Some((name, marketplace)) => (name.to_string(), marketplace.to_string()),
        None => (key.to_string(), String::new()),
    }
}

/// Read and parse a JSON file, falling back to the type's default. A
/// missing file is normal and logs nothing. A file that is present but
/// unparseable logs at `warn`, since that is a real problem the user can
/// act on, and still falls back rather than failing.
fn read_json<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> T {
    let Ok(text) = std::fs::read_to_string(path) else {
        return T::default();
    };
    match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(e) => {
            warn!("plugins: failed to parse {}: {e}", path.display());
            T::default()
        }
    }
}
