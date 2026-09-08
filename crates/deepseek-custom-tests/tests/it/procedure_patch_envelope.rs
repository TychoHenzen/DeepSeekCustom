use deepseek_custom::procedure::{
    LocalPatchDraftDispatch, LocalPatchDraftDispatcher, MechanicalVerb, PatchEnvelopeError,
    RouteSignal, decode_frontier_patch_output, decode_patch_envelope,
    patch_envelope_response_format, validate_patch_boundary,
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

// covers: deepseek-custom/routed-patch-preview :: A patch stays inside the localization boundary :: Patch touches only localized files
#[test]
fn create_update_delete_and_rename_are_eligible_when_every_endpoint_is_localized() {
    let mut envelope = valid_envelope();
    envelope["targets"] = serde_json::json!([
        "src/created.rs",
        "src/deleted.rs",
        "src/new.rs",
        "src/old.rs",
        "src/updated.rs"
    ]);
    envelope["unified_diff"] = serde_json::json!(concat!(
        "diff --git a/src/updated.rs b/src/updated.rs\n",
        "--- a/src/updated.rs\n",
        "+++ b/src/updated.rs\n",
        "@@ -1 +1 @@\n",
        "-old\n",
        "+new\n",
        "diff --git a/src/created.rs b/src/created.rs\n",
        "new file mode 100644\n",
        "--- /dev/null\n",
        "+++ b/src/created.rs\n",
        "@@ -0,0 +1 @@\n",
        "+created\n",
        "diff --git a/src/deleted.rs b/src/deleted.rs\n",
        "deleted file mode 100644\n",
        "--- a/src/deleted.rs\n",
        "+++ /dev/null\n",
        "@@ -1 +0,0 @@\n",
        "-deleted\n",
        "diff --git a/src/old.rs b/src/new.rs\n",
        "similarity index 100%\n",
        "rename from src/old.rs\n",
        "rename to src/new.rs\n"
    ));
    let allowlist = [
        "src/created.rs",
        "src/deleted.rs",
        "src/new.rs",
        "src/old.rs",
        "src/updated.rs",
    ];

    let eligible = validate_patch_boundary(
        decode_patch_envelope(&envelope.to_string()).unwrap(),
        allowlist,
    )
    .unwrap();

    assert_eq!(eligible.file_count(), 4);
    assert_eq!(eligible.paths(), allowlist);
    assert_eq!(eligible.envelope().targets.len(), 5);
}

#[test]
fn standard_prefixes_and_windows_separators_normalize_before_comparison() {
    let mut envelope = valid_envelope();
    envelope["unified_diff"] = serde_json::json!(
        "diff --git a/src\\lib.rs b/src\\lib.rs\n--- a/src\\lib.rs\n+++ b/src\\lib.rs\n@@ -1 +1 @@\n-old_name\n+new_name\n"
    );

    let eligible = validate_patch_boundary(
        decode_patch_envelope(&envelope.to_string()).unwrap(),
        ["src/lib.rs"],
    )
    .unwrap();

    assert_eq!(eligible.paths(), ["src/lib.rs"]);
}

// covers: deepseek-custom/routed-patch-preview :: A patch stays inside the localization boundary :: Patch reaches an unlocalized file
#[test]
fn mixed_valid_and_invalid_paths_reject_the_whole_patch_and_list_every_violation() {
    let mut envelope = valid_envelope();
    envelope["targets"] = serde_json::json!(["src/lib.rs", "src/outside.rs"]);
    envelope["unified_diff"] = serde_json::json!(concat!(
        "diff --git a/src/lib.rs b/src/lib.rs\n",
        "--- a/src/lib.rs\n",
        "+++ b/src/lib.rs\n",
        "@@ -1 +1 @@\n",
        "-old\n",
        "+new\n",
        "diff --git a/src/outside.rs b/src/outside.rs\n",
        "--- a/src/outside.rs\n",
        "+++ b/src/outside.rs\n",
        "@@ -1 +1 @@\n",
        "-old\n",
        "+new\n"
    ));

    let error = validate_patch_boundary(
        decode_patch_envelope(&envelope.to_string()).unwrap(),
        ["src/lib.rs"],
    )
    .unwrap_err();

    assert_eq!(
        error.violations(),
        ["unexpected path `src/outside.rs` is outside the localization allowlist"]
    );
}

#[test]
fn absolute_traversal_and_unlocalized_paths_are_aggregated_and_sorted() {
    let mut envelope = valid_envelope();
    envelope["targets"] = serde_json::json!(["src/lib.rs"]);
    envelope["unified_diff"] = serde_json::json!(concat!(
        "diff --git a/src/lib.rs b/src/lib.rs\n",
        "--- /etc/passwd\n",
        "+++ C:\\outside.rs\n",
        "@@ -1 +1 @@\n",
        "-old\n",
        "+new\n",
        "diff --git a/../escape.rs b/src/outside.rs\n",
        "--- a/../escape.rs\n",
        "+++ b/src/outside.rs\n",
        "@@ -1 +1 @@\n",
        "-old\n",
        "+new\n"
    ));

    let error = validate_patch_boundary(
        decode_patch_envelope(&envelope.to_string()).unwrap(),
        ["src/lib.rs"],
    )
    .unwrap_err();

    assert_eq!(
        error.violations(),
        [
            "invalid new path `C:\\outside.rs` at line 3: absolute paths are not allowed",
            "invalid old path `/etc/passwd` at line 2: absolute paths are not allowed",
            "invalid old path `a/../escape.rs` at line 7: path traversal is not allowed",
            "invalid old path `a/../escape.rs` at line 8: path traversal is not allowed",
            "unexpected path `src/outside.rs` is outside the localization allowlist",
        ]
    );
    assert_eq!(
        error.to_string(),
        concat!(
            "patch violates the localization boundary:\n",
            "- invalid new path `C:\\outside.rs` at line 3: absolute paths are not allowed\n",
            "- invalid old path `/etc/passwd` at line 2: absolute paths are not allowed\n",
            "- invalid old path `a/../escape.rs` at line 7: path traversal is not allowed\n",
            "- invalid old path `a/../escape.rs` at line 8: path traversal is not allowed\n",
            "- unexpected path `src/outside.rs` is outside the localization allowlist"
        )
    );
}

#[test]
fn malformed_and_mismatched_headers_report_together() {
    let mut envelope = valid_envelope();
    envelope["targets"] = serde_json::json!(["src/lib.rs", "src/other.rs"]);
    envelope["unified_diff"] = serde_json::json!(concat!(
        "diff --git a/src/lib.rs\n",
        "--- a/src/lib.rs\n",
        "+++ b/src/lib.rs\n",
        "@@ -1 +1 @@\n",
        "-old\n",
        "+new\n",
        "diff --git a/src/other.rs b/src/other.rs\n",
        "--- a/src/lib.rs\n",
        "+++ b/src/outside.rs\n",
        "@@ -1 +1 @@\n",
        "-old\n",
        "+new\n"
    ));

    let error = validate_patch_boundary(
        decode_patch_envelope(&envelope.to_string()).unwrap(),
        ["src/lib.rs", "src/other.rs"],
    )
    .unwrap_err();

    assert_eq!(
        error.violations(),
        [
            "malformed diff header at line 1: expected two path endpoints",
            "mismatched new file header in section at line 7: diff header has `src/other.rs`, metadata has `src/outside.rs`",
            "mismatched old file header in section at line 7: diff header has `src/other.rs`, metadata has `src/lib.rs`",
            "unexpected path `src/outside.rs` is outside the localization allowlist",
        ]
    );
}

#[test]
fn duplicate_unexpected_paths_are_listed_once() {
    let mut envelope = valid_envelope();
    envelope["unified_diff"] = serde_json::json!(concat!(
        "diff --git a/src/outside.rs b/src/outside.rs\n",
        "--- a/src/outside.rs\n",
        "+++ b/src/outside.rs\n",
        "@@ -1 +1 @@\n",
        "-one\n",
        "+two\n",
        "diff --git a/src/outside.rs b/src/outside.rs\n",
        "--- a/src/outside.rs\n",
        "+++ b/src/outside.rs\n",
        "@@ -1 +1 @@\n",
        "-two\n",
        "+three\n"
    ));

    let error = validate_patch_boundary(
        decode_patch_envelope(&envelope.to_string()).unwrap(),
        ["src/lib.rs"],
    )
    .unwrap_err();

    assert_eq!(
        error.violations(),
        ["unexpected path `src/outside.rs` is outside the localization allowlist"]
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
