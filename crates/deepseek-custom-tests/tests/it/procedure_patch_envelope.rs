use deepseek_custom::procedure::{
    LocalPatchDraftDispatch, LocalPatchDraftDispatcher, MechanicalVerb, PatchEnvelopeError,
    RouteSignal, decode_frontier_patch_output, decode_patch_envelope,
    patch_envelope_response_format,
};
use deepseek_custom::{
    api::provider::Provider, backend::resolved::ResolvedBackend, effort::Effort,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const VALID_DIFF: &str = "diff --git a/src/lib.rs b/src/lib.rs\nindex 1111111..2222222 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old_name\n+new_name\n";

fn valid_envelope() -> serde_json::Value {
    serde_json::json!({
        "targets": ["src/lib.rs"],
        "rationale": "Rename the selected symbol.",
        "route": {
            "automatic_tier": "local",
            "effective_tier": "local",
            "signals": [
                { "kind": "mechanical_verb", "value": "rename" },
                { "kind": "target_count", "value": 1 }
            ],
            "selected_override": "automatic",
            "overridden": false
        },
        "unified_diff": VALID_DIFF
    })
}

#[test]
fn valid_patch_envelope_decodes_to_one_typed_candidate() {
    let candidate = decode_patch_envelope(&valid_envelope().to_string()).unwrap();

    assert_eq!(candidate.file_count(), 1);
    assert_eq!(candidate.envelope().targets, ["src/lib.rs"]);
    assert_eq!(
        candidate.envelope().rationale,
        "Rename the selected symbol."
    );
    assert_eq!(
        candidate.envelope().route.signals,
        [
            RouteSignal::MechanicalVerb(MechanicalVerb::Rename),
            RouteSignal::TargetCount(1),
        ]
    );
    assert_eq!(candidate.envelope().unified_diff, VALID_DIFF);
}

#[test]
fn patch_envelope_schema_describes_every_required_strict_field() {
    let format = serde_json::to_value(patch_envelope_response_format()).unwrap();
    let schema = &format["json_schema"]["schema"];

    assert_eq!(format["type"], "json_schema");
    assert_eq!(format["json_schema"]["name"], "procedure_patch_envelope");
    assert_eq!(format["json_schema"]["strict"], true);
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["required"],
        serde_json::json!(["targets", "rationale", "route", "unified_diff"])
    );
    assert_eq!(schema["properties"]["route"]["additionalProperties"], false);
    assert_eq!(schema["properties"]["targets"]["minItems"], 1);
    assert_eq!(schema["properties"]["unified_diff"]["minLength"], 1);
}

#[test]
fn missing_extra_and_mistyped_fields_have_deterministic_structural_errors() {
    let mut missing = valid_envelope();
    missing.as_object_mut().unwrap().remove("rationale");
    assert_eq!(
        decode_patch_envelope(&missing.to_string()).unwrap_err(),
        PatchEnvelopeError::Structure {
            reason: "missing field `rationale`".to_string()
        }
    );

    let mut extra = valid_envelope();
    extra["commentary"] = serde_json::json!("guess this field");
    assert_eq!(
        decode_patch_envelope(&extra.to_string()).unwrap_err(),
        PatchEnvelopeError::Structure {
            reason: "unknown field `commentary`, expected one of `targets`, `rationale`, `route`, `unified_diff`".to_string()
        }
    );

    let mut mistyped = valid_envelope();
    mistyped["targets"] = serde_json::json!("src/lib.rs");
    assert_eq!(
        decode_patch_envelope(&mistyped.to_string()).unwrap_err(),
        PatchEnvelopeError::Structure {
            reason: "invalid type: string \"src/lib.rs\", expected a sequence".to_string()
        }
    );

    let mut nested_extra = valid_envelope();
    nested_extra["route"]["signals"][0]["commentary"] = serde_json::json!("ignore me");
    assert_eq!(
        decode_patch_envelope(&nested_extra.to_string()).unwrap_err(),
        PatchEnvelopeError::Structure {
            reason: "unknown field `commentary` in `route.signals[0]`".to_string()
        }
    );
}

#[test]
fn inconsistent_route_metadata_is_rejected() {
    let mut envelope = valid_envelope();
    envelope["route"]["selected_override"] = serde_json::json!("force_frontier");

    assert_eq!(
        decode_patch_envelope(&envelope.to_string()).unwrap_err(),
        PatchEnvelopeError::Structure {
            reason: "field `route.effective_tier` does not match `route.selected_override`"
                .to_string()
        }
    );
}

#[test]
fn malformed_diff_is_rejected_after_structural_decoding() {
    let mut envelope = valid_envelope();
    envelope["unified_diff"] = serde_json::json!(
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old_name\n"
    );

    assert_eq!(
        decode_patch_envelope(&envelope.to_string()).unwrap_err(),
        PatchEnvelopeError::Diff {
            reason: "hunk ending before line 6 declares 1 old and 1 new lines but contains 1 old and 0 new lines".to_string()
        }
    );
}

#[test]
fn pure_rename_diff_is_a_valid_single_file_candidate() {
    let mut envelope = valid_envelope();
    envelope["unified_diff"] = serde_json::json!(
        "diff --git a/src/old.rs b/src/new.rs\nsimilarity index 100%\nrename from src/old.rs\nrename to src/new.rs\n"
    );

    assert_eq!(
        decode_patch_envelope(&envelope.to_string())
            .unwrap()
            .file_count(),
        1
    );
}

fn ollama_dispatcher(server: &MockServer) -> LocalPatchDraftDispatcher {
    LocalPatchDraftDispatcher::from_resolved_backend(
        ResolvedBackend::Api {
            name: "local-drafter".to_string(),
            provider: Provider::Ollama,
            api_key: "ollama".to_string(),
            base_url: Some(server.uri()),
            model: "qwen-local".to_string(),
        },
        Effort::Low,
        2048,
    )
    .unwrap()
}

fn chat_response(content: String) -> serde_json::Value {
    serde_json::json!({
        "id": "patch-1",
        "object": "chat.completion",
        "created": 1,
        "model": "qwen-local",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30 }
    })
}

// covers: deepseek-custom/routed-patch-preview :: Patch output has one validated envelope :: Valid local envelope
#[tokio::test]
async fn valid_local_envelope_is_schema_constrained_and_uses_the_shared_decoder() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(chat_response(valid_envelope().to_string())),
        )
        .expect(1)
        .mount(&server)
        .await;
    let dispatcher = ollama_dispatcher(&server);

    let candidate = dispatcher
        .draft("Draft only the requested rename.".to_string())
        .await
        .unwrap();

    assert_eq!(candidate.envelope().targets, ["src/lib.rs"]);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = requests[0].body_json().unwrap();
    assert_eq!(body["model"], "qwen-local");
    assert_eq!(body["stream"], false);
    assert_eq!(body["temperature"], 0.0);
    assert_eq!(body["max_tokens"], 2048);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(
        body["messages"][0]["content"],
        "Draft only the requested rename."
    );
    assert!(body.get("tools").is_none());
    assert!(body.get("tool_choice").is_none());
    assert_eq!(
        body["response_format"],
        serde_json::to_value(patch_envelope_response_format()).unwrap()
    );
    server.verify().await;
}

#[tokio::test]
async fn local_draft_rejects_a_structured_envelope_with_an_invalid_diff() {
    let server = MockServer::start().await;
    let mut envelope = valid_envelope();
    envelope["unified_diff"] = serde_json::json!("not a unified diff");
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(envelope.to_string())))
        .expect(1)
        .mount(&server)
        .await;

    let error = ollama_dispatcher(&server)
        .draft("Draft the change.".to_string())
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "local patch response failed shared decoding: patch envelope unified_diff is invalid: first line must start with `diff --git `"
    );
    server.verify().await;
}

#[test]
fn frontier_final_text_accepts_exactly_one_json_envelope() {
    let candidate = decode_frontier_patch_output(&valid_envelope().to_string()).unwrap();

    assert_eq!(candidate.envelope().targets, ["src/lib.rs"]);
    assert_eq!(candidate.envelope().unified_diff, VALID_DIFF);
}

// covers: deepseek-custom/routed-patch-preview :: Patch output has one validated envelope :: Frontier output is malformed
#[test]
fn frontier_commentary_fences_and_extra_documents_are_not_extracted() {
    let json = valid_envelope().to_string();
    for output in [
        format!("Here is the patch:\n{json}"),
        format!("```json\n{json}\n```"),
        format!("{json}\nextra commentary"),
        format!("{json}\n{json}"),
        VALID_DIFF.to_string(),
    ] {
        assert!(
            matches!(
                decode_frontier_patch_output(&output),
                Err(PatchEnvelopeError::Json { .. })
            ),
            "frontier parser unexpectedly accepted {output:?}"
        );
    }
}

#[test]
fn frontier_structural_error_is_reported_without_a_candidate() {
    let mut envelope = valid_envelope();
    envelope.as_object_mut().unwrap().remove("route");

    assert_eq!(
        decode_frontier_patch_output(&envelope.to_string()).unwrap_err(),
        PatchEnvelopeError::Structure {
            reason: "missing field `route`".to_string()
        }
    );
}
