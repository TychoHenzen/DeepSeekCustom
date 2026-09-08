//! Unit tests for `deepseek_custom::effort`, moved out of the production
//! module as part of the two-crate workspace split.

use std::sync::atomic::AtomicU8;

use deepseek_custom::effort::Effort;

#[test]
fn default_is_none() {
    assert_eq!(Effort::default(), Effort::None);
}

#[test]
fn to_u8_and_from_u8_round_trip_every_level() {
    for level in [
        Effort::None,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Max,
    ] {
        assert_eq!(Effort::from_u8(level.to_u8()), level);
    }
}

#[test]
fn from_u8_out_of_range_falls_back_to_none() {
    assert_eq!(Effort::from_u8(5), Effort::None);
    assert_eq!(Effort::from_u8(255), Effort::None);
}

#[test]
fn load_and_store_round_trip_through_a_shared_flag() {
    let flag = AtomicU8::new(0);
    Effort::High.store(&flag);
    assert_eq!(Effort::load(&flag), Effort::High);
}

#[test]
fn deepseek_thinking_mode_collapses_five_levels_into_three() {
    assert_eq!(Effort::None.deepseek_thinking_mode(), "non-thinking");
    assert_eq!(Effort::Low.deepseek_thinking_mode(), "thinking");
    assert_eq!(Effort::Medium.deepseek_thinking_mode(), "thinking");
    assert_eq!(Effort::High.deepseek_thinking_mode(), "thinking");
    assert_eq!(Effort::Max.deepseek_thinking_mode(), "thinking_max");
}

#[test]
fn claude_cli_effort_omits_the_flag_only_for_none() {
    assert_eq!(Effort::None.claude_cli_effort(), None);
    assert_eq!(Effort::Low.claude_cli_effort(), Some("low"));
    assert_eq!(Effort::Medium.claude_cli_effort(), Some("medium"));
    assert_eq!(Effort::High.claude_cli_effort(), Some("high"));
    assert_eq!(Effort::Max.claude_cli_effort(), Some("max"));
}

#[test]
fn codex_cli_effort_keeps_the_public_config_override_contract() {
    assert_eq!(Effort::None.codex_cli_effort(), None);
    assert_eq!(
        Effort::High.codex_cli_effort(),
        Some("-c reasoning.effort=high".to_string())
    );
}

#[test]
fn wire_form_is_lowercase() {
    assert_eq!(serde_json::to_string(&Effort::None).unwrap(), "\"none\"");
    assert_eq!(serde_json::to_string(&Effort::Low).unwrap(), "\"low\"");
    assert_eq!(
        serde_json::to_string(&Effort::Medium).unwrap(),
        "\"medium\""
    );
    assert_eq!(serde_json::to_string(&Effort::High).unwrap(), "\"high\"");
    assert_eq!(serde_json::to_string(&Effort::Max).unwrap(), "\"max\"");
}

#[test]
fn wire_form_deserializes_back() {
    assert_eq!(
        serde_json::from_str::<Effort>("\"max\"").unwrap(),
        Effort::Max
    );
}
