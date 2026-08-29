use deepseek_custom::application::dto::{
    AppChange, AppChangeKind, AppCommand, AppCommandRequest, AppCommandResult, AppError,
    AppErrorCode, AppRevision, AppSnapshot, OperationKind, OperationPhase, OperationProgress,
    OperationState, PendingSessionSwitch, SessionSummary, TranscriptBlock, TranscriptContent,
    VisibleSettings, Workspace,
};
use std::collections::HashMap;

use deepseek_custom::config::settings::{ApiProvider, BackendConfig, Settings};
use serde_json::json;

#[test]
fn application_command_and_result_have_stable_json_shapes() {
    let request = AppCommandRequest {
        revision: AppRevision(12),
        command: AppCommand::SelectWorkspace {
            workspace: Workspace::Procedure,
        },
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({
            "revision": 12,
            "command": "select_workspace",
            "payload": { "workspace": "procedure" }
        })
    );

    let conflict = AppCommandResult::Conflict {
        current_revision: AppRevision(13),
    };
    assert_eq!(
        serde_json::to_value(&conflict).unwrap(),
        json!({ "status": "conflict", "current_revision": 13 })
    );

    let rejected = AppCommandResult::Rejected {
        error: AppError {
            code: AppErrorCode::InvalidInput,
            message: "task is required".to_string(),
            recoverable: true,
            field: Some("task".to_string()),
        },
    };
    let encoded = serde_json::to_string(&rejected).unwrap();
    assert_eq!(
        serde_json::from_str::<AppCommandResult>(&encoded).unwrap(),
        rejected
    );
}

#[test]
fn snapshot_change_operation_and_error_contracts_round_trip() {
    let settings = VisibleSettings::from_settings(&Settings::default(), None, None);
    let operation = OperationState {
        kind: OperationKind::Procedure,
        operation_id: Some("run-7".to_string()),
        phase: OperationPhase::AwaitingReview,
        progress: Some(OperationProgress {
            completed: 2,
            total: Some(3),
        }),
        message: Some("Review the patch evidence".to_string()),
        error: None,
    };
    let snapshot = AppSnapshot {
        revision: AppRevision(7),
        workspace: Workspace::Procedure,
        transcript: vec![TranscriptBlock {
            id: 1,
            content: TranscriptContent::User {
                text: "inspect this".to_string(),
                has_image: false,
            },
        }],
        session: SessionSummary {
            id: "session-1".to_string(),
            title: "Inspect this".to_string(),
            backend: "codex".to_string(),
            model: "gpt-test".to_string(),
        },
        pending_session_switch: Some(PendingSessionSwitch::New),
        settings,
        operations: vec![operation.clone()],
    };
    let change = AppChange {
        revision: AppRevision(8),
        change: AppChangeKind::OperationChanged(operation),
    };

    let snapshot_json = serde_json::to_string(&snapshot).unwrap();
    let change_json = serde_json::to_string(&change).unwrap();
    assert_eq!(
        serde_json::from_str::<AppSnapshot>(&snapshot_json).unwrap(),
        snapshot
    );
    assert_eq!(
        serde_json::from_str::<AppChange>(&change_json).unwrap(),
        change
    );
    assert!(change_json.contains("\"revision\":8"));
    assert!(change_json.contains("\"type\":\"operation_changed\""));
    assert_eq!(AppRevision(8).checked_next(), Some(AppRevision(9)));
    assert_eq!(AppRevision(u64::MAX).checked_next(), None);
}

#[test]
fn visible_settings_projection_never_serializes_credentials() {
    let mut settings = Settings {
        api_key: Some("top-level-secret".to_string()),
        ..Settings::default()
    };
    settings.backends = Some(HashMap::from([(
        "deepseek".to_string(),
        BackendConfig::Api {
            provider: ApiProvider::DeepSeek,
            model: "deepseek-chat".to_string(),
            base_url: None,
            api_key: Some("backend-secret".to_string()),
            models: None,
        },
    )]));

    let visible = VisibleSettings::from_settings(
        &settings,
        Some("deepseek".to_string()),
        Some("deepseek-chat".to_string()),
    );
    let value = serde_json::to_value(&visible).unwrap();
    let encoded = value.to_string();

    assert_eq!(value["selected_backend"], "deepseek");
    assert_eq!(value["selected_model"], "deepseek-chat");
    assert!(!encoded.contains("top-level-secret"));
    assert!(!encoded.contains("backend-secret"));
    assert!(!encoded.contains("api_key"));
    assert!(!encoded.contains("backends"));
    assert!(!encoded.contains("permissions"));
    assert!(!encoded.contains("hooks"));
}
