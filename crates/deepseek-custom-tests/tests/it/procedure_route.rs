use deepseek_custom::procedure::{
    DifficultyAssessment, MechanicalVerb, RouteOverride, RouteSignal, RouteTier,
    apply_route_override, assess_route,
};

#[test]
fn route_types_have_stable_equality_display_and_serialization() {
    assert_eq!(RouteTier::Local, RouteTier::Local);
    assert_ne!(RouteTier::Local, RouteTier::Frontier);
    assert_eq!(RouteTier::Local.to_string(), "local");
    assert_eq!(
        serde_json::to_string(&RouteTier::Frontier).unwrap(),
        r#""frontier""#
    );

    let signals = [
        RouteSignal::MechanicalVerb(MechanicalVerb::Rename),
        RouteSignal::MechanicalVerb(MechanicalVerb::Import),
        RouteSignal::MechanicalVerb(MechanicalVerb::SignaturePropagation),
        RouteSignal::MechanicalVerb(MechanicalVerb::Boilerplate),
        RouteSignal::MechanicalVerb(MechanicalVerb::TestScaffolding),
        RouteSignal::MechanicalVerb(MechanicalVerb::Formatting),
        RouteSignal::MechanicalVerb(MechanicalVerb::Documentation),
        RouteSignal::TargetCount(1),
        RouteSignal::Architecture,
        RouteSignal::CrossCuttingBehavior,
        RouteSignal::Concurrency,
        RouteSignal::Security,
        RouteSignal::Migration,
        RouteSignal::PublicApi,
        RouteSignal::SubtleBug,
        RouteSignal::SubstantiveLogic,
        RouteSignal::UnknownWording,
    ];

    let mut reverse = signals;
    reverse.reverse();
    reverse.sort();
    assert_eq!(reverse, signals);
    assert_eq!(
        RouteSignal::MechanicalVerb(MechanicalVerb::SignaturePropagation).to_string(),
        "mechanical verb: signature propagation"
    );
    assert_eq!(RouteSignal::TargetCount(3).to_string(), "target count: 3");
    assert_eq!(RouteSignal::PublicApi.to_string(), "public API");
    assert_eq!(
        serde_json::to_value(RouteSignal::Security).unwrap(),
        serde_json::json!({ "kind": "security" })
    );
}

// covers: deepseek-custom/routed-patch-preview :: Route decisions use deterministic difficulty signals :: Mechanical single-file step routes locally
#[test]
fn every_explicit_mechanical_verb_routes_one_localized_file_locally() {
    let cases = [
        ("Rename the selected type.", MechanicalVerb::Rename),
        ("Add the missing import.", MechanicalVerb::Import),
        (
            "Perform signature propagation for the selected function.",
            MechanicalVerb::SignaturePropagation,
        ),
        (
            "Generate the required boilerplate.",
            MechanicalVerb::Boilerplate,
        ),
        (
            "Add test scaffolding for the selected module.",
            MechanicalVerb::TestScaffolding,
        ),
        (
            "Apply formatting to the selected file.",
            MechanicalVerb::Formatting,
        ),
        (
            "Update documentation for the selected function.",
            MechanicalVerb::Documentation,
        ),
    ];

    for (contract_text, verb) in cases {
        let assessment = assess_route(contract_text, 1);
        assert_eq!(assessment.tier, RouteTier::Local, "{contract_text}");
        assert_eq!(
            assessment.signals,
            vec![
                RouteSignal::MechanicalVerb(verb),
                RouteSignal::TargetCount(1),
            ],
            "{contract_text}"
        );
    }
}

// covers: deepseek-custom/routed-patch-preview :: Route decisions use deterministic difficulty signals :: Higher-risk step routes to frontier
#[test]
fn every_frontier_marker_routes_to_frontier_and_records_its_signal() {
    let cases = [
        ("Change the architecture.", RouteSignal::Architecture),
        (
            "Change cross-cutting behavior.",
            RouteSignal::CrossCuttingBehavior,
        ),
        ("Change concurrency handling.", RouteSignal::Concurrency),
        ("Fix the security boundary.", RouteSignal::Security),
        ("Perform the storage migration.", RouteSignal::Migration),
        ("Change the public API.", RouteSignal::PublicApi),
        ("Fix subtle bugs.", RouteSignal::SubtleBug),
        ("Replace substantive logic.", RouteSignal::SubstantiveLogic),
    ];

    for (contract_text, expected_signal) in cases {
        let assessment = assess_route(contract_text, 1);
        assert_eq!(assessment.tier, RouteTier::Frontier, "{contract_text}");
        assert_eq!(
            assessment.signals,
            vec![RouteSignal::TargetCount(1), expected_signal],
            "{contract_text}"
        );
    }
}

#[test]
fn multi_file_conflicting_and_unknown_work_route_conservatively() {
    let multi_file = assess_route("Rename the selected types.", 2);
    assert_eq!(multi_file.tier, RouteTier::Frontier);
    assert_eq!(
        multi_file.signals,
        vec![
            RouteSignal::MechanicalVerb(MechanicalVerb::Rename),
            RouteSignal::TargetCount(2),
        ]
    );

    let conflicting = assess_route(
        "Rename the selected type while changing the architecture and security boundary.",
        1,
    );
    assert_eq!(conflicting.tier, RouteTier::Frontier);
    assert_eq!(
        conflicting.signals,
        vec![
            RouteSignal::MechanicalVerb(MechanicalVerb::Rename),
            RouteSignal::TargetCount(1),
            RouteSignal::Architecture,
            RouteSignal::Security,
        ]
    );

    let unknown = assess_route("Improve the selected behavior.", 1);
    assert_eq!(unknown.tier, RouteTier::Frontier);
    assert_eq!(
        unknown.signals,
        vec![RouteSignal::TargetCount(1), RouteSignal::UnknownWording]
    );
}

#[test]
fn all_detected_signals_use_the_stable_enum_order() {
    let assessment = assess_route(
        "Rename and import boilerplate during signature propagation, test scaffolding, formatting, and documentation. Change architecture, cross-cutting behavior, concurrency, security, migration, the public API, a subtle bug, and substantive logic.",
        3,
    );

    assert_eq!(assessment.tier, RouteTier::Frontier);
    assert_eq!(
        assessment.signals,
        vec![
            RouteSignal::MechanicalVerb(MechanicalVerb::Rename),
            RouteSignal::MechanicalVerb(MechanicalVerb::Import),
            RouteSignal::MechanicalVerb(MechanicalVerb::SignaturePropagation),
            RouteSignal::MechanicalVerb(MechanicalVerb::Boilerplate),
            RouteSignal::MechanicalVerb(MechanicalVerb::TestScaffolding),
            RouteSignal::MechanicalVerb(MechanicalVerb::Formatting),
            RouteSignal::MechanicalVerb(MechanicalVerb::Documentation),
            RouteSignal::TargetCount(3),
            RouteSignal::Architecture,
            RouteSignal::CrossCuttingBehavior,
            RouteSignal::Concurrency,
            RouteSignal::Security,
            RouteSignal::Migration,
            RouteSignal::PublicApi,
            RouteSignal::SubtleBug,
            RouteSignal::SubstantiveLogic,
        ]
    );
}

#[test]
fn route_api_accepts_no_token_confidence_input() {
    let route_from_contract_and_target_count: fn(&str, usize) -> DifficultyAssessment =
        assess_route;

    assert_eq!(
        route_from_contract_and_target_count("Rename the selected type.", 1).tier,
        RouteTier::Local
    );
}

#[test]
fn automatic_and_forced_routes_retain_the_automatic_evidence() {
    let local_assessment = assess_route("Rename the selected type.", 1);
    let local_signals = local_assessment.signals.clone();

    let automatic = apply_route_override(local_assessment.clone(), RouteOverride::Automatic);
    assert_eq!(automatic.automatic_tier, RouteTier::Local);
    assert_eq!(automatic.effective_tier, RouteTier::Local);
    assert_eq!(automatic.signals, local_signals);
    assert_eq!(automatic.selected_override, RouteOverride::Automatic);
    assert!(!automatic.overridden);

    let forced_frontier = apply_route_override(local_assessment, RouteOverride::ForceFrontier);
    assert_eq!(forced_frontier.automatic_tier, RouteTier::Local);
    assert_eq!(forced_frontier.effective_tier, RouteTier::Frontier);
    assert_eq!(forced_frontier.signals, local_signals);
    assert_eq!(
        forced_frontier.selected_override,
        RouteOverride::ForceFrontier
    );
    assert!(forced_frontier.overridden);
}

// covers: deepseek-custom/routed-patch-preview :: A user can override the automatic route :: Local override is selected
#[test]
fn force_local_replaces_a_frontier_route_for_only_that_decision() {
    let frontier_assessment = assess_route("Change the architecture.", 1);
    let frontier_signals = frontier_assessment.signals.clone();

    let forced_local = apply_route_override(frontier_assessment.clone(), RouteOverride::ForceLocal);
    assert_eq!(forced_local.automatic_tier, RouteTier::Frontier);
    assert_eq!(forced_local.effective_tier, RouteTier::Local);
    assert_eq!(forced_local.signals, frontier_signals);
    assert_eq!(forced_local.selected_override, RouteOverride::ForceLocal);
    assert!(forced_local.overridden);

    let later_automatic = apply_route_override(frontier_assessment, RouteOverride::Automatic);
    assert_eq!(later_automatic.automatic_tier, RouteTier::Frontier);
    assert_eq!(later_automatic.effective_tier, RouteTier::Frontier);
    assert_eq!(later_automatic.selected_override, RouteOverride::Automatic);
    assert!(!later_automatic.overridden);
}

#[test]
fn forcing_the_already_automatic_tier_records_selection_without_a_replacement() {
    let decision = apply_route_override(
        assess_route("Rename the selected type.", 1),
        RouteOverride::ForceLocal,
    );

    assert_eq!(decision.automatic_tier, RouteTier::Local);
    assert_eq!(decision.effective_tier, RouteTier::Local);
    assert_eq!(decision.selected_override, RouteOverride::ForceLocal);
    assert!(!decision.overridden);
}
