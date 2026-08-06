//! Unit tests for `deepseek_custom::tools` (`src/tools/mod.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use deepseek_custom::config::settings::PermissionsConfig;
use deepseek_custom::tools::ToolRegistry;

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
