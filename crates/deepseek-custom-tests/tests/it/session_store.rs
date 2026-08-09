//! Unit tests for `deepseek_custom::session::store` (`src/session/store.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::io;
use std::path::PathBuf;

use deepseek_custom::api::types::{Content, Message, Role};
use deepseek_custom::error::HarnessError;
use deepseek_custom::gui::transcript::Transcript;
use deepseek_custom::session::store::SessionStore;
use deepseek_custom::session::{SessionId, SessionMeta, SessionRecord};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-store-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sample_record(title: &str, updated_at: u64) -> SessionRecord {
    SessionRecord {
        meta: SessionMeta {
            id: SessionId::new(),
            title: title.into(),
            created_at: 1000,
            updated_at,
            backend: "deepseek".into(),
            model: "deepseek-v4-flash".into(),
            message_count: 1,
        },
        messages: vec![Message {
            role: Role::User,
            content: Some(Content::text(title)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }],
        transcript: Transcript::new(),
        claude_session_id: None,
    }
}

#[test]
fn save_then_load_returns_equal_record() {
    let dir = temp_dir("roundtrip");
    let store = SessionStore::new(dir.clone());
    let record = sample_record("hello", 100);

    store.save(&record).unwrap();
    let loaded = store.load(&record.meta.id).unwrap();

    assert_eq!(loaded.meta.id, record.meta.id);
    assert_eq!(loaded.meta.title, record.meta.title);
    assert_eq!(loaded.messages.len(), record.messages.len());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn saving_same_id_twice_leaves_second_version_on_disk() {
    let dir = temp_dir("overwrite");
    let store = SessionStore::new(dir.clone());
    let mut record = sample_record("first version", 100);

    store.save(&record).unwrap();
    record.meta.title = "second version".into();
    record.meta.updated_at = 200;
    store.save(&record).unwrap();

    let loaded = store.load(&record.meta.id).unwrap();
    assert_eq!(loaded.meta.title, "second version");
    assert_eq!(loaded.meta.updated_at, 200);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn list_returns_sessions_sorted_newest_first() {
    let dir = temp_dir("list-sort");
    let store = SessionStore::new(dir.clone());

    let a = sample_record("older", 100);
    let b = sample_record("newest", 300);
    let c = sample_record("middle", 200);
    store.save(&a).unwrap();
    store.save(&b).unwrap();
    store.save(&c).unwrap();

    let metas = store.list();
    assert_eq!(metas.len(), 3);
    assert_eq!(metas[0].title, "newest");
    assert_eq!(metas[1].title, "middle");
    assert_eq!(metas[2].title, "older");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn list_skips_corrupt_file_and_returns_good_ones() {
    let dir = temp_dir("list-corrupt");
    let store = SessionStore::new(dir.clone());

    let a = sample_record("good one", 100);
    let b = sample_record("good two", 200);
    store.save(&a).unwrap();
    store.save(&b).unwrap();
    std::fs::write(dir.join("garbage.json"), "{ not valid json").unwrap();

    let metas = store.list();
    assert_eq!(metas.len(), 2);
    assert!(metas.iter().any(|m| m.title == "good one"));
    assert!(metas.iter().any(|m| m.title == "good two"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn list_on_missing_directory_returns_empty_vec() {
    let dir = temp_dir("list-missing");
    std::fs::remove_dir_all(&dir).ok();
    let store = SessionStore::new(dir.join("sessions"));

    assert!(store.list().is_empty());
}

#[test]
fn load_of_never_saved_id_is_distinguishable_from_parse_failure() {
    let dir = temp_dir("load-missing");
    let store = SessionStore::new(dir.clone());
    let missing_id = SessionId::new();

    let missing_err = store.load(&missing_id).unwrap_err();
    assert!(matches!(
        missing_err,
        HarnessError::Io(e) if e.kind() == io::ErrorKind::NotFound
    ));

    std::fs::write(dir.join(format!("{}.json", missing_id.as_str())), "{ bad").unwrap();
    let parse_err = store.load(&missing_id).unwrap_err();
    assert!(matches!(parse_err, HarnessError::Parse(_)));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_session_file_saved_before_content_was_an_enum_still_loads() {
    // Hand-written JSON in the shape a session file had before
    // `Message.content` became `Option<Content>`: a bare string, the
    // exact wire shape `Option<String>` produced. The untagged
    // `Content` enum must still parse it as `Content::Text`.
    let dir = temp_dir("old-shape");
    let store = SessionStore::new(dir.clone());
    let id = SessionId::new();
    let old_shape_json = format!(
        r#"{{"meta":{{"id":"{}","title":"old session","created_at":1000,"updated_at":1000,"backend":"deepseek","model":"deepseek-v4-flash","message_count":1}},"messages":[{{"role":"user","content":"hello from before Content existed"}}],"transcript":{{"blocks":[]}},"claude_session_id":null}}"#,
        id.as_str()
    );
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{}.json", id.as_str())), old_shape_json).unwrap();

    let loaded = store
        .load(&id)
        .expect("old-shape session file should still load");

    assert_eq!(loaded.messages.len(), 1);
    assert_eq!(
        loaded.messages[0]
            .content
            .as_ref()
            .and_then(Content::as_text),
        Some("hello from before Content existed")
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn delete_removes_file_and_second_delete_is_not_an_error() {
    let dir = temp_dir("delete");
    let store = SessionStore::new(dir.clone());
    let record = sample_record("to delete", 100);
    store.save(&record).unwrap();

    store.delete(&record.meta.id).unwrap();
    assert!(store.load(&record.meta.id).is_err());

    store.delete(&record.meta.id).unwrap();
    std::fs::remove_dir_all(&dir).ok();
}
