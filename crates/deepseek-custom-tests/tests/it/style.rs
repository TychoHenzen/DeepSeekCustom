//! Tests for `deepseek_custom::style` (`src/style/mod.rs`): the
//! `flesch_kincaid_grade` function and the plain-language revise loop.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use deepseek_custom::agent::agent_loop::{AgentConfig, AgentLoop};
use deepseek_custom::api::client::{ApiClient, Provider};
use deepseek_custom::style::flesch_kincaid_grade;
use deepseek_custom::tools::ToolRegistry;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// flesch_kincaid_grade unit tests
// ---------------------------------------------------------------------------

/// An empty string returns 0.0.
#[test]
fn empty_string_is_zero() {
    assert_eq!(flesch_kincaid_grade(""), 0.0);
    assert_eq!(flesch_kincaid_grade("   "), 0.0);
}

/// A single word with no sentence-ending punctuation still gets a grade
/// (clamped to at least one sentence). Whitespace-only input has zero
/// words and returns 0.0.
#[test]
fn single_word_and_no_words() {
    // "Hello" = 2 syllables, 1 word, 1 sentence (clamped)
    // grade = 0.39 * 1 + 11.8 * 2 - 15.59 = 0.39 + 23.6 - 15.59 = 8.4
    let grade = flesch_kincaid_grade("Hello");
    assert!((grade - 8.4).abs() < 0.1, "expected ~8.4, got {grade}");

    // Whitespace-only: zero words → 0.0
    assert_eq!(flesch_kincaid_grade("   \t\n  "), 0.0);
}

/// A short, simple sentence with a known low grade.
///
/// "The cat sat on the mat." has 6 words, 1 sentence, 6 syllables.
/// grade = 0.39 * 6 + 11.8 * 1 - 15.59 = 2.34 + 11.8 - 15.59 = -1.45
#[test]
fn simple_sentence_low_grade() {
    let grade = flesch_kincaid_grade("The cat sat on the mat.");
    assert!(
        (grade - (-1.45)).abs() < 0.2,
        "expected ~-1.45, got {grade}"
    );
}

/// A long, jargon-heavy sentence with a known high grade.
///
/// "The implementation of sophisticated computational methodologies
///  facilitates resource optimization."
/// 8 words, 1 sentence, ~34 syllables.
/// grade ≈ 0.39 * 8 + 11.8 * (34/8) - 15.59
///       = 3.12 + 50.15 - 15.59 = 37.68
#[test]
fn jargon_sentence_high_grade() {
    let text = "The implementation of sophisticated computational methodologies facilitates resource optimization.";
    let grade = flesch_kincaid_grade(text);
    // Allow some tolerance: syllable counting is heuristic.
    assert!(
        grade > 25.0,
        "expected a high grade for jargon text, got {grade}"
    );
}

/// Two simple sentences produce a lower grade than one jargon sentence.
#[test]
fn multiple_simple_sentences_stay_low() {
    let text = "The cat sat on the mat. The dog ran in the park.";
    let grade = flesch_kincaid_grade(text);
    // 12 words, 2 sentences, ~12 syllables
    // grade = 0.39 * (12/2) + 11.8 * (12/12) - 15.59
    //       = 0.39*6 + 11.8 - 15.59 = 2.34 + 11.8 - 15.59 = -1.45
    assert!(
        grade < 5.0,
        "expected a low grade for simple sentences, got {grade}"
    );
}

/// Text with zero sentence-ending punctuation gets clamped to one sentence.
#[test]
fn no_punctuation_clamped_to_one_sentence() {
    let grade = flesch_kincaid_grade("hello world");
    // 2 words, 1 sentence (clamped), 3 syllables
    // grade = 0.39 * 2 + 11.8 * (3/2) - 15.59 = 0.78 + 17.7 - 15.59 = 2.89
    assert!(
        (grade - 2.89).abs() < 0.2,
        "expected ~2.89, got {grade}"
    );
}

// ---------------------------------------------------------------------------
// Revise-loop test against wiremock (the plan's "StubBackend" test)
// ---------------------------------------------------------------------------

/// A canned non-streaming chat-completion response carrying a simplified
/// version of the input text, matching the shape `ApiClient::chat` parses.
const REVISED_BODY: &str = r#"{
  "id": "revise-1",
  "object": "chat.completion",
  "created": 1,
  "model": "deepseek-v4-flash",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "We use simple words and short sentences."
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 50,
    "completion_tokens": 12,
    "total_tokens": 62
  }
}"#;

/// When the plain-language gate is on and the input text is long and
/// complex, the revise loop sends it to the API and returns the revised
/// text with a non-zero attempt count.
#[tokio::test]
async fn revise_loop_returns_revised_text_from_mock_api() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_raw(REVISED_BODY, "application/json"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let client = ApiClient::new(
        Provider::DeepSeek,
        "sk-test".into(),
        Some(mock_server.uri()),
    );

    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    // The original text is long enough and complex enough to trigger
    // the grade gate.  The mock returns a short, simple revision.
    let original =
        "The implementation of sophisticated computational methodologies \
         facilitates the optimization of resource allocation paradigms \
         through the utilization of advanced algorithmic frameworks.";
    let (revised, attempts) = agent.revise_for_plain_language_for_test(original).await;

    assert_eq!(revised, "We use simple words and short sentences.");
    assert_eq!(attempts, 1, "expected one revise attempt, got {attempts}");

    mock_server.verify().await;
}

/// When the text is already plain enough (under the target+tolerance),
/// the revise loop returns it unchanged with zero attempts and makes no
/// API call.
#[tokio::test]
async fn revise_loop_skips_when_already_plain() {
    let mock_server = MockServer::start().await;

    // No mock mounted: zero API calls expected. If the loop fires a
    // request anyway, wiremock will reject it as unmatched.

    let client = ApiClient::new(
        Provider::DeepSeek,
        "sk-test".into(),
        Some(mock_server.uri()),
    );

    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        client,
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    // Set a high target grade so that even modest text passes.
    agent.set_style_config(true, 25.0, 5.0, 2, None);

    let simple_text = "The cat sat on the mat. It was a nice day.";
    let (revised, attempts) = agent.revise_for_plain_language_for_test(simple_text).await;

    assert_eq!(revised, simple_text);
    assert_eq!(attempts, 0);

    // No request was expected on the mock server.
    mock_server.verify().await;
}

/// `maybe_check_plain_language_for_test` returns false when the gate is
/// off, regardless of the text.
#[test]
fn maybe_check_returns_false_when_gate_off() {
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(
        ApiClient::new(Provider::DeepSeek, "sk-test".into(), Some("http://localhost:1".into())),
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    // Gate defaults to off. A long, jargon-heavy text should still
    // return false because the gate is not active.
    let jargon =
        "The implementation of sophisticated computational methodologies \
         facilitates the optimization of resource allocation paradigms.";
    assert!(!agent.maybe_check_plain_language_for_test(jargon));
}

/// `maybe_check_plain_language_for_test` returns false for short text
/// even when the gate is on, because a grade on one sentence is noise.
#[test]
fn maybe_check_returns_false_for_short_text() {
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        ApiClient::new(Provider::DeepSeek, "sk-test".into(), Some("http://localhost:1".into())),
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    // Enable the gate with a low target so almost anything would fail.
    agent.set_style_config(true, 1.0, 0.0, 2, None);

    let short = "Hello.";
    assert!(!agent.maybe_check_plain_language_for_test(short));
}

/// `maybe_check_plain_language_for_test` returns true when the gate is
/// on, the text is long enough, and the grade exceeds the threshold.
#[test]
fn maybe_check_returns_true_for_jargon_with_gate_on() {
    let tools = ToolRegistry::new();
    let mut agent = AgentLoop::new(
        ApiClient::new(Provider::DeepSeek, "sk-test".into(), Some("http://localhost:1".into())),
        tools,
        "sys".into(),
        AgentConfig::default(),
        Arc::new(AtomicBool::new(false)),
    );

    // Target grade 8.0, tolerance 2.0 (the defaults).  Jargon text
    // scores much higher than 10.0, so the gate should fire.
    agent.set_style_config(true, 8.0, 2.0, 2, None);

    // A long enough, complex reply.
    let jargon =
        "The implementation of sophisticated computational methodologies \
         facilitates the optimization of resource allocation paradigms \
         through the utilization of advanced algorithmic frameworks \
         and the integration of heterogeneous data sources.";
    assert!(agent.maybe_check_plain_language_for_test(jargon));
}
