//! Unit tests for `deepseek_custom::session::store` (`src/session/store.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::io;
use std::path::PathBuf;

use deepseek_custom::api::types::{Content, Message, Role};
use deepseek_custom::application::transcript::Transcript;
use deepseek_custom::error::HarnessError;
use deepseek_custom::session::store::SessionStore;
use deepseek_custom::session::{SessionId, SessionMeta, SessionRecord};

fn temp_dir(tag: &str) -> PathBuf {
    super::scratch_dir("dsc-store", tag)
}

fn sample_record(title: &str, seq: u64, updated_at: u64) -> SessionRecord {
    SessionRecord {
        meta: SessionMeta {
            id: SessionId::new(),
            seq,
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
    let record = sample_record("hello", 1, 100);

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
    let mut record = sample_record("first version", 1, 100);

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
fn list_returns_sessions_by_number_highest_first() {
    let dir = temp_dir("list-sort");
    let store = SessionStore::new(dir.clone());

    let a = sample_record("first", 1, 100);
    let b = sample_record("third", 3, 300);
    let c = sample_record("second", 2, 200);
    store.save(&a).unwrap();
    store.save(&b).unwrap();
    store.save(&c).unwrap();

    let metas = store.list();
    assert_eq!(metas.len(), 3);
    assert_eq!(metas[0].title, "third");
    assert_eq!(metas[1].title, "second");
    assert_eq!(metas[2].title, "first");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn saving_an_older_session_does_not_move_it_up_the_list() {
    // The bug this pins: the list used to be ordered by `updated_at`, so
    // every autosave of a running conversation jumped its row to the top
    // and pushed every other row down under the reader. A save must leave
    // the order alone.
    let dir = temp_dir("list-stable");
    let store = SessionStore::new(dir.clone());

    let first = sample_record("first", 1, 100);
    let second = sample_record("second", 2, 200);
    let third = sample_record("third", 3, 300);
    store.save(&first).unwrap();
    store.save(&second).unwrap();
    store.save(&third).unwrap();

    let before: Vec<String> = store.list().into_iter().map(|m| m.title).collect();

    let mut resaved = first;
    resaved.meta.updated_at = 9000;
    store.save(&resaved).unwrap();

    let after: Vec<String> = store.list().into_iter().map(|m| m.title).collect();
    assert_eq!(before, after);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn number_old_sessions_numbers_unnumbered_records_oldest_first() {
    let dir = temp_dir("number-old");
    let store = SessionStore::new(dir.clone());

    // `seq: 0` is what a record written before numbering existed
    // deserializes to, through `serde`'s default.
    let mut older = sample_record("older", 0, 100);
    older.meta.created_at = 1000;
    let mut newer = sample_record("newer", 0, 200);
    newer.meta.created_at = 2000;
    store.save(&older).unwrap();
    store.save(&newer).unwrap();

    assert_eq!(store.number_old_sessions(), 2);

    assert_eq!(store.load(&older.meta.id).unwrap().meta.seq, 1);
    assert_eq!(store.load(&newer.meta.id).unwrap().meta.seq, 2);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn number_old_sessions_leaves_an_already_numbered_record_alone() {
    let dir = temp_dir("number-keep");
    let store = SessionStore::new(dir.clone());

    let numbered = sample_record("numbered", 7, 100);
    let mut unnumbered = sample_record("unnumbered", 0, 200);
    unnumbered.meta.created_at = 2000;
    store.save(&numbered).unwrap();
    store.save(&unnumbered).unwrap();

    assert_eq!(store.number_old_sessions(), 1);

    assert_eq!(store.load(&numbered.meta.id).unwrap().meta.seq, 7);
    // One past the highest number already in use, so no number is reused.
    assert_eq!(store.load(&unnumbered.meta.id).unwrap().meta.seq, 8);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn list_skips_corrupt_file_and_returns_good_ones() {
    let dir = temp_dir("list-corrupt");
    let store = SessionStore::new(dir.clone());

    let a = sample_record("good one", 1, 100);
    let b = sample_record("good two", 2, 200);
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
    let record = sample_record("to delete", 1, 100);
    store.save(&record).unwrap();

    store.delete(&record.meta.id).unwrap();
    assert!(store.load(&record.meta.id).is_err());

    store.delete(&record.meta.id).unwrap();
    std::fs::remove_dir_all(&dir).ok();
}
