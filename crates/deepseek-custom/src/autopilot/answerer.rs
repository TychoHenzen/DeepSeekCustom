//! Policy-driven answering of `AskUserQuestion` calls during an autopilot run.
//!
//! `build_prompt`, `parse_reply`, and `resolve_answers` are pure and offline,
//! so they are testable without a network. `PolicyAnswerer::answer` is the
//! only piece that talks to the API, and it never returns an error for a
//! model or network problem: a failed answer degrades to the first option of
//! every question, it does not break the agent's turn.

use async_trait::async_trait;
use tracing::{info, warn};

use crate::api::client::ApiClient;
use crate::api::types::{ChatRequest, ChatResponse, Content, Message};
use crate::autopilot::policy::{PolicyStore, format_policy_prompt_section};
use crate::autopilot::question::{Answer, AskInput, Question};
use crate::effort::Effort;
use crate::error::Result;
use crate::json_reply::extract_array_span;

/// Cap on how many recent decision-log lines are folded into the prompt.
const RECENT_DECISIONS_LIMIT: usize = 20;

const SYSTEM_PROMPT: &str = "You are answering questions on behalf of a human operator who is \
not available. Follow the policy below, and stay consistent with recent decisions. Reply with \
JSON only, no prose around it.";

/// Answers a batch of questions from an `AskUserQuestion` tool call, standing
/// in for a human. Exists as a trait so the tool wrapper can be tested
/// against a stub instead of a real model.
#[async_trait]
pub trait QuestionAnswerer: Send + Sync {
    async fn answer(&self, input: &AskInput) -> Result<Vec<Answer>>;
}

/// Answers questions with one non-streaming call to the DeepSeek API, guided
/// by a policy file and a log of recent decisions.
pub struct PolicyAnswerer {
    client: ApiClient,
    policy_store: PolicyStore,
    model: String,
}

impl PolicyAnswerer {
    pub fn new(client: ApiClient, policy_store: PolicyStore, model: String) -> Self {
        Self {
            client,
            policy_store,
            model,
        }
    }
}

#[async_trait]
impl QuestionAnswerer for PolicyAnswerer {
    async fn answer(&self, input: &AskInput) -> Result<Vec<Answer>> {
        let policy = self.policy_store.load_policy();
        let recent_decisions = self.policy_store.recent_decisions(RECENT_DECISIONS_LIMIT);
        let policy_section = format_policy_prompt_section(&policy, &recent_decisions);

        let prompt = build_prompt(&policy_section, input);

        let req = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                Message::system(SYSTEM_PROMPT.to_string()),
                Message::user(prompt),
            ],
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.0),
            max_tokens: Some((input.questions.len() as u32) * 200 + 128),
            thinking: None,
            thinking_mode: None,
            reasoning_effort: None,
            response_format: None,
            effort: Some(Effort::None),
        };

        let parsed = match self.client.chat(&req).await {
            Ok(response) => parse_response(&response),
            Err(e) => {
                warn!("autopilot answerer: request failed: {e}");
                None
            }
        };

        let answers = resolve_answers(parsed, input);

        for answer in &answers {
            self.policy_store
                .append_decision(&answer.question, &answer.labels.join(", "));
        }

        info!(
            "autopilot answerer resolved: {}",
            answers
                .iter()
                .map(|a| format!("{}={}", a.question, a.labels.join("|")))
                .collect::<Vec<_>>()
                .join("; ")
        );

        Ok(answers)
    }
}

/// Build the prompt asking the model to answer every question in `input`,
/// given the policy section built from the policy file and recent decisions.
///
/// Unconditionally `pub`: the moved test crate exercises this pure function
/// directly, and it has no side effects that would make a narrower seam
/// meaningful.
pub fn build_prompt(policy_section: &str, input: &AskInput) -> String {
    let mut sections: Vec<String> = Vec::new();

    if !policy_section.is_empty() {
        sections.push(policy_section.to_string());
    }

    let mut questions_text = String::from("## Questions\n");
    for (i, q) in input.questions.iter().enumerate() {
        questions_text.push_str(&format!("\n{}. {} ({})\n", i + 1, q.question, q.header));
        for opt in &q.options {
            questions_text.push_str(&format!("- {}: {}\n", opt.label, opt.description));
        }
        if q.multi_select {
            questions_text.push_str("This question allows choosing several labels.\n");
        } else {
            questions_text.push_str("This question allows choosing exactly one label.\n");
        }
    }
    sections.push(questions_text);

    sections.push(
        "Reply with a JSON array, one entry per question, in the same order, shaped like \
        {\"question\": \"<the question text>\", \"labels\": [\"<chosen label>\", ...]}. Every \
        label must be copied exactly from the offered options for that question. A \
        single-select question takes exactly one label. Reply with JSON only, no prose around \
        it."
        .to_string(),
    );

    sections.join("\n\n")
}

/// Parse a model reply into a list of raw (question, labels) answers.
/// Tolerates a markdown code fence or surrounding prose by locating the
/// outermost `[` and its matching `]`. Returns `None` when the JSON does not
/// parse or an entry is missing its question or labels.
///
/// Unconditionally `pub`: the moved test crate exercises this pure function
/// directly, and it has no side effects that would make a narrower seam
/// meaningful.
pub fn parse_reply(reply: &str) -> Option<Vec<Answer>> {
    let span = extract_array_span(reply)?;
    let raw: Vec<serde_json::Value> = serde_json::from_str(span).ok()?;

    let mut answers = Vec::with_capacity(raw.len());
    for entry in raw {
        let question = entry.get("question")?.as_str()?.to_string();
        let labels_value = entry.get("labels")?.as_array()?;
        let mut labels = Vec::with_capacity(labels_value.len());
        for label in labels_value {
            labels.push(label.as_str()?.to_string());
        }
        answers.push(Answer { question, labels });
    }

    Some(answers)
}

/// Validate parsed answers against `input` and fall back to the first option
/// of a question wherever the parsed answer for it is missing or invalid.
/// Any violation logs at `warn` naming what was wrong.
///
/// Unconditionally `pub`: the moved test crate exercises this pure function
/// directly, and it has no side effects that would make a narrower seam
/// meaningful.
pub fn resolve_answers(parsed: Option<Vec<Answer>>, input: &AskInput) -> Vec<Answer> {
    let parsed = match parsed {
        Some(answers) if answers.len() == input.questions.len() => answers,
        Some(answers) => {
            warn!(
                "autopilot answerer: reply had {} answers, expected {}, falling back to first \
                option for every question",
                answers.len(),
                input.questions.len()
            );
            return fallback_all(input);
        }
        None => {
            warn!(
                "autopilot answerer: no parsed reply, falling back to first option for every question"
            );
            return fallback_all(input);
        }
    };

    input
        .questions
        .iter()
        .zip(parsed.iter())
        .map(|(question, answer)| resolve_one(question, answer))
        .collect()
}

/// Validate one parsed answer against its question and fall back to the
/// first option when the answer is invalid.
fn resolve_one(question: &Question, answer: &Answer) -> Answer {
    if answer.labels.is_empty() {
        warn!(
            "autopilot answerer: question '{}' got no labels, falling back to first option",
            question.question
        );
        return fallback_one(question);
    }

    for label in &answer.labels {
        if !question.options.iter().any(|opt| &opt.label == label) {
            warn!(
                "autopilot answerer: question '{}' got label '{}' not among offered options, \
                falling back to first option",
                question.question, label
            );
            return fallback_one(question);
        }
    }

    if !question.multi_select && answer.labels.len() > 1 {
        warn!(
            "autopilot answerer: question '{}' is single-select but got {} labels, trimming to \
            the first",
            question.question,
            answer.labels.len()
        );
        return Answer {
            question: question.question.clone(),
            labels: vec![answer.labels[0].clone()],
        };
    }

    Answer {
        question: question.question.clone(),
        labels: answer.labels.clone(),
    }
}

/// The first-option fallback answer for one question.
fn fallback_one(question: &Question) -> Answer {
    Answer {
        question: question.question.clone(),
        labels: vec![question.options[0].label.clone()],
    }
}

/// The first-option fallback answer for every question in `input`.
fn fallback_all(input: &AskInput) -> Vec<Answer> {
    input.questions.iter().map(fallback_one).collect()
}

/// Extract and parse the text reply from a chat response, logging the
/// specific failure point when the response carries no usable text.
fn parse_response(response: &ChatResponse) -> Option<Vec<Answer>> {
    let Some(choice) = response.choices.first() else {
        warn!("autopilot answerer: empty choices in response");
        return None;
    };
    let Some(text) = choice.message.content.as_ref().and_then(Content::as_text) else {
        warn!("autopilot answerer: no content in response");
        return None;
    };
    let parsed = parse_reply(text);
    if parsed.is_none() {
        warn!("autopilot answerer: failed to parse model reply into answers");
    }
    parsed
}
