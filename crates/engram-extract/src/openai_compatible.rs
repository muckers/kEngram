//! `OpenAICompatibleTagger` — talks to any backend that implements the
//! OpenAI `/v1/chat/completions` API with `response_format: json_schema`.
//! That covers vLLM (production), OpenRouter (cloud fallback), and OpenAI
//! itself, distinguished only by config.
//!
//! Endpoint convention: the configured `endpoint` is the `/v1` base, and
//! the tagger appends `/chat/completions`. For local vLLM that's
//! `http://localhost:8000/v1`.

use async_trait::async_trait;
use engram_core::{ScopeVocab, Tagger, TaggerError, Tags};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct OpenAICompatibleConfig {
    /// Base URL ending in `/v1`.
    pub endpoint: String,
    /// Model name as the backend understands it. For vLLM: the deployed
    /// model (`"qwen2.5-7b-instruct"`). For OpenRouter: a model slug
    /// (`"anthropic/claude-haiku-4.5"`).
    pub model_name: String,
    /// Engram-side stable identity written into `thoughts.tags_extractor_model`.
    /// Conventionally `<vendor>/<model>` — `"vllm/qwen2.5-7b-instruct"`,
    /// `"openrouter/anthropic/claude-haiku-4.5"`.
    pub model_id: String,
    /// Schema-version of this tagger's prompt/response contract. Bump
    /// when the JSON Schema or system prompt changes such that prior tags
    /// are no longer comparable. Written into
    /// `thoughts.tags_extractor_version`.
    pub model_version: i32,
    pub api_key: Option<String>,
    pub timeout: Duration,
    /// Generation temperature. Lower = more deterministic tagging. 0.2 is
    /// a reasonable default; 0 makes some backends loop.
    pub temperature: f32,
    /// Override the bundled system prompt (`BUNDLED_TAGGER_PROMPT`). `None`
    /// means use the bundled default. `Some(_)` means the operator supplied
    /// a custom prompt — the operator is responsible for also bumping
    /// `model_version` so `thoughts.tags_extractor_version` remains
    /// meaningful provenance. A WARN is emitted at construction when this
    /// is `Some(_)`.
    pub system_prompt: Option<String>,
}

impl OpenAICompatibleConfig {
    /// Defaults for a local vLLM dev path on port 8000 with the qwen-7b
    /// instruct model. No API key.
    pub fn vllm_local() -> Self {
        Self {
            endpoint: "http://localhost:8000/v1".to_string(),
            model_name: "qwen2.5-7b-instruct".to_string(),
            model_id: "vllm/qwen2.5-7b-instruct".to_string(),
            model_version: BUNDLED_TAGGER_VERSION,
            api_key: None,
            timeout: Duration::from_secs(60),
            temperature: 0.2,
            system_prompt: None,
        }
    }

    /// Preset for OpenRouter cloud fallback. `model_name` is an OpenRouter
    /// model slug (e.g. `"anthropic/claude-haiku-4.5"`); the model_id is
    /// derived by prefixing with `"openrouter/"` so tags retain a clean
    /// provenance string.
    pub fn open_router(api_key: String, model_name: String) -> Self {
        Self {
            endpoint: "https://openrouter.ai/api/v1".to_string(),
            model_id: format!("openrouter/{model_name}"),
            model_name,
            model_version: BUNDLED_TAGGER_VERSION,
            api_key: Some(api_key),
            timeout: Duration::from_secs(60),
            temperature: 0.2,
            system_prompt: None,
        }
    }
}

/// Version of the bundled tagger prompt + response schema. Paired with the
/// model_version field on each thought row's tag provenance. Bump when the
/// prompt or schema changes such that prior tags shouldn't be considered
/// comparable. Operator runs `engram tag --rerun --since 1970-01-01T00:00:00Z`
/// to backfill after a bump.
///
/// History: v1 was the initial M4 thoughts-only tagger; v2 (M4.1) split
/// `topics` into `entities` (proper-noun-style identifiers) + `topics`
/// (subject categories) and added the optional scope-vocabulary
/// controlled-vocabulary section; v3 (M4.1 prompt iteration) tightened
/// entities to canonical proper names only with an explicit anti-padding
/// rule, and added a kind-isolation clause forbidding the controlled
/// vocabulary from influencing kind classification; v4 (M4.1 prompt
/// iteration, second pass) restructured the entities description to lead
/// with the empty case and a structural NAME-vs-DESCRIBE test (the v3
/// negative-example list backfired — the model emitted those exact phrases
/// from `047d0ce8`), dropped entities maxItems 5→3, and softened the
/// scope-vocabulary section from "vocab dominates" to "vocab tie-breaks"
/// (precision over consistency).
pub const BUNDLED_TAGGER_VERSION: i32 = 4;

#[derive(Debug, Clone)]
pub struct OpenAICompatibleTagger {
    endpoint: String,
    model_name: String,
    model_id: String,
    model_version: i32,
    api_key: Option<String>,
    temperature: f32,
    /// Resolved system prompt — either the bundled default or the operator's
    /// override. Stored at construction so `tag()` doesn't re-resolve on
    /// every request.
    system_prompt: String,
    /// Stored alongside the client so the timeout-error path reports the
    /// actual configured value (the reqwest client owns the same duration
    /// internally but doesn't expose it).
    timeout_seconds: u64,
    client: Client,
}

impl OpenAICompatibleTagger {
    pub fn new(config: OpenAICompatibleConfig) -> Result<Self, TaggerError> {
        if config.endpoint.is_empty() {
            return Err(TaggerError::Misconfigured(
                "tagger endpoint must not be empty".into(),
            ));
        }
        if config.model_name.is_empty() {
            return Err(TaggerError::Misconfigured(
                "tagger model_name must not be empty".into(),
            ));
        }

        // Resolve the system prompt: operator override wins; otherwise the
        // bundled default.
        let (system_prompt, is_override) = match config.system_prompt {
            Some(custom) => (custom, true),
            None => (BUNDLED_TAGGER_PROMPT.to_string(), false),
        };
        if is_override {
            tracing::warn!(
                model_id = %config.model_id,
                model_version = config.model_version,
                "tagger: custom system_prompt in use; ensure model_version reflects this prompt's identity. \
                 Past tags with the same tagger_version were produced under the bundled prompt; \
                 tags produced under a custom prompt should bump model_version so provenance partitions cleanly."
            );
        }

        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| TaggerError::Unreachable(format!("client build: {e}")))?;
        Ok(Self {
            endpoint: config.endpoint,
            model_name: config.model_name,
            model_id: config.model_id,
            model_version: config.model_version,
            api_key: config.api_key,
            temperature: config.temperature,
            system_prompt,
            timeout_seconds: config.timeout.as_secs(),
            client,
        })
    }
}

/// The bundled tagger system prompt. Exposed `pub const` so operators can
/// inspect it (`engram-cli` can print it; configuration can compare against
/// it) and so a custom prompt loaded from `system_prompt_file` can be diffed
/// against the bundled one at startup.
///
/// The prompt is **paired** with `OpenAICompatibleConfig::model_version`
/// (default 1 when the bundled prompt is in use). Bump the version whenever
/// this prompt or the response schema changes such that prior tags
/// shouldn't be considered comparable; `engram tag --rerun` then re-tags
/// under the new version. If you override this via
/// `OpenAICompatibleConfig::system_prompt`, you are responsible for also
/// bumping the version — see `docs/engram-design-v0.md` §6 / §10.
pub const BUNDLED_TAGGER_PROMPT: &str = "\
You are a tagging assistant. Given a single thought from a memory service, return its metadata tags as JSON.

# Output shape
{ \"people\": [...], \"entities\": [...], \"action_items\": [...], \"topics\": [...], \"dates_mentioned\": [...], \"kind\": \"...\" }

# Field semantics
- people: bare names of people mentioned. Empty array if none.
- entities: default to []. Only add an item if the thought explicitly names a specific entity by its canonical name — a project, product, library, tool, technology, or organization that exists by that name outside this thought. Examples of valid entities: \"engram\", \"pgvector\", \"PostgreSQL\", \"MCP\", \"TCGplayer\", \"Cap'n Proto\", \"Rust\", \"Hummingbird\". Test before including any phrase: does this phrase NAME a specific thing that has its own canonical identity (yes → entity), or does the thought DESCRIBE an action or concept using a noun phrase (no → belongs in topics if anywhere)? Preserve the thought's casing, or use canonical casing if the thought is inconsistent. If you are unsure whether a phrase qualifies, omit it.
- action_items: short imperative phrases describing tasks the thought commits to or implies (e.g., \"fix the login bug\", \"review the migration plan\"). Empty array if none.
- topics: 1-3 short tag-like subject categories, lowercase, hyphen-separated, no punctuation. What broad SUBJECT AREA is this thought about? Examples: \"rust\", \"build-systems\", \"team-management\", \"memory-systems\". Distinct from entities: a topic is a category the thought falls under; an entity is a specific named thing the thought mentions. A thought naming \"engram\" and \"pgvector\" might have entities [\"engram\", \"pgvector\"] and topics [\"memory-systems\", \"databases\"].
- dates_mentioned: any dates or temporal references appearing in the prose (\"next Thursday\", \"Q3\", \"2026-05-15\", \"before the release\"). Free-form strings, copied roughly as they appear. Empty array if none.
- kind: a single classification of the thought's intrinsic shape — not its subject area, and not influenced by what other thoughts in this scope tend to be classified as. Classify from the writing's structural form (factual claim → observation; imperative → task; proposal → idea; pointer to a resource → reference; statement about a person → person_note; transient narrative → session). Use null if genuinely ambiguous. Categories:
  - observation: a factual claim about the world (\"Rust has stronger memory safety than C\").
  - task: a thing the writer or someone else needs to do (\"fix the login bug\").
  - idea: a proposal or hypothesis (\"we could use Bloom filters here\").
  - reference: a pointer to an external resource (a URL, a paper, a tool, or a definition of a named thing).
  - person_note: a fact about a specific person (\"Sarah prefers async meetings\").
  - session: transient session/test narrative (\"the search returned 3 results\", \"I just ran the migration\"). These should also have otherwise-empty arrays.

# Rules
- Entities require explicit mention by name in the thought. Do not invent entities. Do not pad entities with descriptive phrases when no named entities are present — empty array is correct.
- Topics may be inferred from prose context when the subject is clear, even if the exact topic word doesn't appear.
- Kind is classified from the thought's intrinsic shape, not from the scope's typical content or any controlled-vocabulary hints below. The vocabulary section, when present, informs topic and entity term choice only.
- Empty arrays are correct for any field that has no content.
- One classification only; pick the most-load-bearing category. If genuinely ambiguous, return null.
- This is a tagging pass, not a paraphrase or rewrite. Do not rephrase the thought's content; only emit metadata.";

/// Render the optional controlled-vocabulary section appended to the system
/// prompt when scope vocabulary is available. Returns an empty string when
/// the vocab is `None` or completely empty, so callers can unconditionally
/// concatenate the result.
fn render_vocab_section(vocab: Option<&ScopeVocab>) -> String {
    let Some(v) = vocab else {
        return String::new();
    };
    if v.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\n# Controlled vocabulary (this scope's established terms)\n");
    if !v.topics.is_empty() {
        out.push_str("Topics already used in this scope: ");
        out.push_str(&v.topics.join(", "));
        out.push_str(".\n");
    }
    if !v.entities.is_empty() {
        out.push_str("Entities already used in this scope: ");
        out.push_str(&v.entities.join(", "));
        out.push_str(".\n");
    }
    out.push_str(
        "These are terms other thoughts in this scope have used. Use one when it accurately describes the thought's subject. Use a more specific or different term when no vocab term is a close fit — precision over consistency.",
    );
    out
}

#[derive(Serialize)]
struct ChatRequestBody<'a> {
    model: &'a str,
    temperature: f32,
    messages: Vec<ChatMessage<'a>>,
    response_format: serde_json::Value,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponseBody {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

#[async_trait]
impl Tagger for OpenAICompatibleTagger {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn version(&self) -> i32 {
        self.model_version
    }

    async fn tag(
        &self,
        thought_content: &str,
        vocab: Option<&ScopeVocab>,
    ) -> Result<Tags, TaggerError> {
        let url = format!("{}/chat/completions", self.endpoint.trim_end_matches('/'));

        let system_content = {
            let vocab_section = render_vocab_section(vocab);
            if vocab_section.is_empty() {
                self.system_prompt.clone()
            } else {
                let mut s = self.system_prompt.clone();
                s.push_str(&vocab_section);
                s
            }
        };
        let messages: Vec<ChatMessage<'_>> = vec![
            ChatMessage {
                role: "system",
                content: system_content,
            },
            ChatMessage {
                role: "user",
                content: thought_content.to_string(),
            },
        ];
        let body = ChatRequestBody {
            model: &self.model_name,
            temperature: self.temperature,
            messages,
            response_format: tags_response_format(),
        };

        let mut req = self.client.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| map_send_error(e, self.timeout_seconds))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(TaggerError::Backend {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: ChatResponseBody = resp.json().await.map_err(|e| {
            TaggerError::MalformedResponse(format!("decoding chat completions response: {e}"))
        })?;

        let content = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| TaggerError::MalformedResponse("response had zero choices".into()))?
            .message
            .content;

        let tags: Tags = serde_json::from_str(&content).map_err(|e| {
            TaggerError::MalformedResponse(format!(
                "decoding tags payload (content={content:?}): {e}"
            ))
        })?;

        Ok(tags)
    }
}

/// The `response_format` JSON object sent to the chat completions API. The
/// schema constrains the model to the `Tags` wire shape with six required
/// fields; `topics` is capped at 3 items, `entities` at 3 (lowered from 5
/// in the v4 prompt iteration to force selectivity), and `kind` is nullable
/// with an enum of `TagKind` snake_case variants.
fn tags_response_format() -> serde_json::Value {
    serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "engram_tags",
            "strict": true,
            "schema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["people", "entities", "action_items", "topics", "dates_mentioned", "kind"],
                "properties": {
                    "people": { "type": "array", "items": { "type": "string" } },
                    "entities": { "type": "array", "items": { "type": "string" }, "maxItems": 3 },
                    "action_items": { "type": "array", "items": { "type": "string" } },
                    "topics": { "type": "array", "items": { "type": "string" }, "maxItems": 3 },
                    "dates_mentioned": { "type": "array", "items": { "type": "string" } },
                    "kind": {
                        "type": ["string", "null"],
                        "enum": ["observation", "task", "idea", "reference", "person_note", "session", null]
                    }
                }
            }
        }
    })
}

fn map_send_error(e: reqwest::Error, timeout_seconds: u64) -> TaggerError {
    if e.is_timeout() {
        TaggerError::Timeout {
            seconds: timeout_seconds,
        }
    } else if e.is_connect() {
        TaggerError::Unreachable(e.to_string())
    } else if let Some(status) = e.status() {
        TaggerError::Backend {
            status: status.as_u16(),
            body: e.to_string(),
        }
    } else {
        TaggerError::Unreachable(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_core::TagKind;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn config_for(endpoint: String, api_key: Option<String>) -> OpenAICompatibleConfig {
        OpenAICompatibleConfig {
            endpoint,
            model_name: "test-model".to_string(),
            model_id: "test/test-model".to_string(),
            model_version: 1,
            api_key,
            timeout: Duration::from_secs(2),
            temperature: 0.0,
            system_prompt: None,
        }
    }

    fn chat_response_with_tags(tags: serde_json::Value) -> serde_json::Value {
        let content = serde_json::to_string(&tags).unwrap();
        json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": "stop"
            }]
        })
    }

    #[tokio::test]
    async fn valid_response_parses_to_tags() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(chat_response_with_tags(json!({
                    "people": ["Sarah", "Ron"],
                    "entities": ["engram", "pgvector"],
                    "action_items": ["fix the login bug"],
                    "topics": ["rust", "build-systems"],
                    "dates_mentioned": ["next Thursday"],
                    "kind": "task"
                }))),
            )
            .mount(&server)
            .await;

        let t =
            OpenAICompatibleTagger::new(config_for(format!("{}/v1", server.uri()), None)).unwrap();
        let tags = t.tag("anything", None).await.unwrap();
        assert_eq!(tags.people, vec!["Sarah".to_string(), "Ron".to_string()]);
        assert_eq!(
            tags.entities,
            vec!["engram".to_string(), "pgvector".to_string()]
        );
        assert_eq!(tags.action_items, vec!["fix the login bug".to_string()]);
        assert_eq!(
            tags.topics,
            vec!["rust".to_string(), "build-systems".to_string()]
        );
        assert_eq!(tags.dates_mentioned, vec!["next Thursday".to_string()]);
        assert_eq!(tags.kind, Some(TagKind::Task));
    }

    #[tokio::test]
    async fn malformed_response_returns_malformed_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"role": "assistant", "content": "not json"}}]
            })))
            .mount(&server)
            .await;

        let t =
            OpenAICompatibleTagger::new(config_for(format!("{}/v1", server.uri()), None)).unwrap();
        let err = t.tag("x", None).await.unwrap_err();
        assert!(matches!(err, TaggerError::MalformedResponse(_)));
        assert!(!err.is_transient());
    }

    #[tokio::test]
    async fn timeout_returns_transient_error() {
        let server = MockServer::start().await;
        // Delay > configured timeout (2s) — reqwest will time out first.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(chat_response_with_tags(json!({
                        "people": [], "action_items": [], "topics": [],
                        "dates_mentioned": [], "kind": null
                    })))
                    .set_delay(Duration::from_secs(5)),
            )
            .mount(&server)
            .await;

        let t =
            OpenAICompatibleTagger::new(config_for(format!("{}/v1", server.uri()), None)).unwrap();
        let err = t.tag("x", None).await.unwrap_err();
        assert!(
            matches!(err, TaggerError::Timeout { .. }),
            "expected Timeout, got {err:?}"
        );
        assert!(err.is_transient());
    }

    #[tokio::test]
    async fn http_500_returns_backend_transient() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream gone"))
            .mount(&server)
            .await;

        let t =
            OpenAICompatibleTagger::new(config_for(format!("{}/v1", server.uri()), None)).unwrap();
        let err = t.tag("x", None).await.unwrap_err();
        match err {
            TaggerError::Backend { status, .. } => assert_eq!(status, 503),
            other => panic!("expected Backend error, got {other:?}"),
        }
        assert!(err.is_transient());
    }

    #[tokio::test]
    async fn http_400_returns_backend_non_transient() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .mount(&server)
            .await;

        let t =
            OpenAICompatibleTagger::new(config_for(format!("{}/v1", server.uri()), None)).unwrap();
        let err = t.tag("x", None).await.unwrap_err();
        match &err {
            TaggerError::Backend { status, .. } => assert_eq!(*status, 400),
            other => panic!("expected Backend error, got {other:?}"),
        }
        assert!(!err.is_transient());
    }

    #[tokio::test]
    async fn connect_failure_maps_to_unreachable_or_timeout() {
        // Port 1 is reliably refused on macOS/Linux.
        let t = OpenAICompatibleTagger::new(config_for("http://127.0.0.1:1/v1".to_string(), None))
            .unwrap();
        let err = t.tag("x", None).await.unwrap_err();
        assert!(
            matches!(
                err,
                TaggerError::Unreachable(_) | TaggerError::Timeout { .. }
            ),
            "expected Unreachable or Timeout, got {err:?}"
        );
        assert!(err.is_transient());
    }

    #[tokio::test]
    async fn request_uses_bearer_auth_when_api_key_present() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(chat_response_with_tags(json!({
                    "people": [], "action_items": [], "topics": [],
                    "dates_mentioned": [], "kind": null
                }))),
            )
            .mount(&server)
            .await;

        let t = OpenAICompatibleTagger::new(config_for(
            format!("{}/v1", server.uri()),
            Some("sk-test".into()),
        ))
        .unwrap();
        // If the auth header is wrong, wiremock returns 404 and the parse fails.
        t.tag("x", None).await.expect("auth header must match");
    }

    #[tokio::test]
    async fn empty_endpoint_is_misconfigured() {
        let mut cfg = config_for("".to_string(), None);
        cfg.endpoint = "".into();
        let err = OpenAICompatibleTagger::new(cfg).unwrap_err();
        assert!(matches!(err, TaggerError::Misconfigured(_)));
    }

    #[tokio::test]
    async fn empty_model_name_is_misconfigured() {
        let mut cfg = config_for("http://127.0.0.1:1/v1".to_string(), None);
        cfg.model_name = "".into();
        let err = OpenAICompatibleTagger::new(cfg).unwrap_err();
        assert!(matches!(err, TaggerError::Misconfigured(_)));
    }

    #[tokio::test]
    async fn custom_system_prompt_flows_into_request_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(chat_response_with_tags(json!({
                    "people": [], "action_items": [], "topics": [],
                    "dates_mentioned": [], "kind": null
                }))),
            )
            .mount(&server)
            .await;

        let mut cfg = config_for(format!("{}/v1", server.uri()), None);
        cfg.system_prompt =
            Some("Custom prompt for the dogfood week. Return tags only.".to_string());
        let t = OpenAICompatibleTagger::new(cfg).unwrap();
        let _ = t.tag("x", None).await;

        let received = server.received_requests().await.unwrap();
        let last = received.last().expect("at least one request");
        let body: serde_json::Value = serde_json::from_slice(&last.body).unwrap();
        let sys = body["messages"][0]["content"].as_str().unwrap();
        assert!(sys.contains("Custom prompt for the dogfood week"));
        // Bundled-prompt language must NOT leak in.
        assert!(!sys.contains("Field semantics"));
    }

    /// v4 prompt content pin: the tagger prompt must mention field semantics,
    /// list each of the six fields, preserve the entities/topics distinction
    /// and kind-isolation clauses from earlier versions, and surface the v4
    /// entities restructuring (lead-with-empty + NAME-vs-DESCRIBE structural
    /// test) without regressing to the v3 negative-example list (which
    /// backfired in dogfood — the model emitted the listed phrases verbatim).
    #[test]
    fn tagger_v4_prompt_contains_field_semantics_lead_with_empty_and_kind_isolation() {
        let p = BUNDLED_TAGGER_PROMPT;
        assert!(
            p.contains("Field semantics"),
            "v4 prompt must contain a 'Field semantics' section"
        );
        for field in [
            "people",
            "entities",
            "action_items",
            "topics",
            "dates_mentioned",
            "kind",
        ] {
            assert!(p.contains(field), "v4 prompt must mention field {field}");
        }
        // The entities/topics distinction must be explicit (kept from v2).
        assert!(
            p.contains("Distinct from entities"),
            "v4 prompt must explicitly distinguish entities from topics"
        );
        // v4 entities lead-with-empty framing: the description must open
        // with the default-empty case rather than burying it as a constraint.
        assert!(
            p.contains("entities: default to []"),
            "v4 prompt must lead the entities description with the empty case"
        );
        // v4 structural NAME-vs-DESCRIBE test replaces the v3 negative-example
        // list (which backfired — the model emitted listed phrases verbatim).
        assert!(
            p.contains("NAME a specific thing"),
            "v4 prompt must include the structural NAME-vs-DESCRIBE entities test"
        );
        assert!(
            p.contains("DESCRIBE an action"),
            "v4 prompt must include the structural NAME-vs-DESCRIBE entities test"
        );
        // v3's negative-example list must be gone — its presence reinforced
        // the very phrases it was trying to exclude (047d0ce8 dogfood case).
        assert!(
            !p.contains("The following are NOT entities"),
            "v4 prompt must NOT contain the v3 negative-example list lead-in"
        );
        // v3 kind-isolation clause remains: kind framed as intrinsic-shape
        // and the Rules section forbids vocab from influencing kind.
        assert!(
            p.contains("intrinsic shape"),
            "v4 prompt must frame kind as intrinsic-shape classification"
        );
        assert!(
            p.contains("not from the scope's typical content"),
            "v4 prompt must explicitly isolate kind from scope-typical content"
        );
        // Presets pinned to the bundled version (4 as of M4.1's v4 iteration).
        assert_eq!(BUNDLED_TAGGER_VERSION, 4);
        let cfg = OpenAICompatibleConfig::vllm_local();
        assert_eq!(cfg.model_version, BUNDLED_TAGGER_VERSION);
        let cfg = OpenAICompatibleConfig::open_router("k".into(), "m".into());
        assert_eq!(cfg.model_version, BUNDLED_TAGGER_VERSION);
    }

    #[test]
    fn tags_response_format_pins_v2_shape() {
        let v = tags_response_format();
        let schema = &v["json_schema"]["schema"];
        let required = schema["required"].as_array().unwrap();
        let required: Vec<&str> = required.iter().map(|x| x.as_str().unwrap()).collect();
        assert_eq!(
            required,
            vec![
                "people",
                "entities",
                "action_items",
                "topics",
                "dates_mentioned",
                "kind"
            ]
        );
        assert_eq!(schema["properties"]["topics"]["maxItems"], 3);
        assert_eq!(schema["properties"]["entities"]["maxItems"], 3);
        // `kind` must allow null on the wire.
        let kind_type = &schema["properties"]["kind"]["type"];
        assert!(
            kind_type.as_array().unwrap().iter().any(|x| x == "null"),
            "kind must be nullable: {kind_type:?}"
        );
    }

    #[test]
    fn render_vocab_section_handles_none_and_empty() {
        assert_eq!(render_vocab_section(None), "");
        assert_eq!(render_vocab_section(Some(&ScopeVocab::default())), "");
    }

    #[test]
    fn render_vocab_section_lists_topics_and_entities() {
        let v = ScopeVocab {
            topics: vec!["rust".into(), "memory-systems".into()],
            entities: vec!["engram".into(), "pgvector".into()],
        };
        let rendered = render_vocab_section(Some(&v));
        assert!(rendered.contains("Controlled vocabulary"));
        assert!(rendered.contains("rust, memory-systems"));
        assert!(rendered.contains("engram, pgvector"));
        // v4: vocab is a tie-breaker, not a hard preference.
        assert!(
            rendered.contains("precision over consistency"),
            "v4 vocab section must frame vocab as tie-breaker, not dominator"
        );
        assert!(
            !rendered.contains("prefer the established form"),
            "v4 vocab section must not preserve the v2/v3 'prefer established form' framing"
        );
    }

    #[test]
    fn render_vocab_section_omits_empty_arm() {
        let topics_only = ScopeVocab {
            topics: vec!["rust".into()],
            entities: vec![],
        };
        let rendered = render_vocab_section(Some(&topics_only));
        assert!(rendered.contains("Topics already used"));
        assert!(!rendered.contains("Entities already used"));
    }

    #[tokio::test]
    async fn vocab_section_flows_into_request_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(chat_response_with_tags(json!({
                    "people": [], "entities": [], "action_items": [], "topics": [],
                    "dates_mentioned": [], "kind": null
                }))),
            )
            .mount(&server)
            .await;

        let t =
            OpenAICompatibleTagger::new(config_for(format!("{}/v1", server.uri()), None)).unwrap();
        let vocab = ScopeVocab {
            topics: vec!["memory-systems".into()],
            entities: vec!["engram".into()],
        };
        let _ = t.tag("any thought", Some(&vocab)).await;

        let received = server.received_requests().await.unwrap();
        let last = received.last().expect("at least one request");
        let body: serde_json::Value = serde_json::from_slice(&last.body).unwrap();
        let sys = body["messages"][0]["content"].as_str().unwrap();
        assert!(
            sys.contains("Controlled vocabulary"),
            "vocab section must be present in system message"
        );
        assert!(sys.contains("memory-systems"));
        assert!(sys.contains("engram"));
    }

    #[tokio::test]
    async fn no_vocab_omits_section_from_request_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(chat_response_with_tags(json!({
                    "people": [], "entities": [], "action_items": [], "topics": [],
                    "dates_mentioned": [], "kind": null
                }))),
            )
            .mount(&server)
            .await;

        let t =
            OpenAICompatibleTagger::new(config_for(format!("{}/v1", server.uri()), None)).unwrap();
        let _ = t.tag("any thought", None).await;

        let received = server.received_requests().await.unwrap();
        let last = received.last().expect("at least one request");
        let body: serde_json::Value = serde_json::from_slice(&last.body).unwrap();
        let sys = body["messages"][0]["content"].as_str().unwrap();
        assert!(
            !sys.contains("Controlled vocabulary"),
            "vocab section must be absent when vocab is None"
        );
    }

    /// Live test against a real OpenAI-compatible endpoint (vLLM by default).
    /// Gated on the `integration` feature; off in CI. Run with
    /// `cargo test -p engram-extract --features integration -- live_vllm`.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn live_vllm_round_trip() {
        let cfg = OpenAICompatibleConfig::vllm_local();
        let t = OpenAICompatibleTagger::new(cfg).unwrap();
        let tags = t
            .tag(
                "Engram uses pgvector for vector storage. Sarah will review the migration plan.",
                None,
            )
            .await
            .expect("vLLM unreachable — is it running on :8000?");
        // We can't assert specific tags (model output varies) but the call
        // must succeed and parse.
        let _ = tags;
    }

    /// Common fixtures for both kind-stability diagnostics (vocab-off and
    /// vocab-on). Six fixture thoughts pulled from the post-M4.1 corpus.
    /// Format: (short_id, scope, current_v2_kind, descriptor, content).
    #[cfg(feature = "integration")]
    fn diagnostic_fixtures() -> Vec<(
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
    )> {
        vec![
            (
                "8a533e15",
                "engram.m3.dogfood",
                "observation",
                "Mission/setup (drift candidate: was task in v1, observation in v2)",
                "Mission for the engram.m3.dogfood scope: We are testing the Engram agent memory system via its MCP toolset. We are testing for accuracy, making sure that facts don't drift negatively, and that searches return expected information.\n\nWhen scope is a parameter, we will use \"engram.m3.dogfood\".\n\nFor any of our conversations, the agent will always consult the facts and thoughts in that scope, giving more weight to facts than thoughts. The agent will add any interesting thought that it or the operator comes up with during the conversation. If unsure, the agent will ask the operator whether it should be stored.",
            ),
            (
                "047d0ce8",
                "engram.m3.dogfood",
                "observation",
                "Probe 2B definitional (drift candidate: was reference in v1, observation in v2)",
                "The agent memory protocol provides five operations: writing notes, querying them by similarity or recency, fetching by id, and marking notes untrusted. Querying combines embedding-based and lexical signals, optionally re-scored by a cross-encoder.",
            ),
            (
                "22bccb3a",
                "engram.test",
                "reference",
                "Clean definitional/reference control (Cap'n Proto)",
                "Cap'n Proto is a serialization format that uses the same memory layout in-memory and on-the-wire, eliminating parse and encode steps. Compared to Protocol Buffers, it offers zero-copy reads but at the cost of more rigid schema evolution. For very high-throughput RPC workloads, it can outperform Protobuf by an order of magnitude.",
            ),
            (
                "5aacd2d8",
                "engram.m3.dogfood",
                "reference",
                "Short definitional/reference control (Hummingbird)",
                "Hummingbird is our internal rollout coordinator. It exposes a percentage knob per cohort and a kill-switch endpoint. Currently used by the privacy-sensitive code paths only.",
            ),
            (
                "b67db532",
                "work.tcgplayer",
                "person_note",
                "Closed-enum person_note control (Ron / Python)",
                "Ron (CTO of TCGplayer) does not like Python or JavaScript, particularly for enterprise software.",
            ),
            (
                "86c3392f",
                "engram.test",
                "observation",
                "Session-shaped narrative control (benchmark run)",
                "I ran a benchmark this morning comparing serde_json and simd-json for parsing 100MB of test JSON. simd-json was 3.2x faster on this hardware (M2 Pro, 16GB). The general finding holds across many tests in the community: SIMD-accelerated JSON parsing significantly outperforms scalar implementations for documents over roughly 1MB. For smaller documents the SIMD setup overhead can negate the benefit.",
            ),
        ]
    }

    /// Top-50 scope vocab for each fixture scope, frozen from the live DB at
    /// 2026-05-17. Lets the vocab-on diagnostic faithfully reproduce what the
    /// worker tick / `engram tag --rerun` actually passes to the tagger,
    /// without taking a sqlx dependency in this crate.
    #[cfg(feature = "integration")]
    fn diagnostic_scope_vocab(scope: &str) -> ScopeVocab {
        match scope {
            "engram.m3.dogfood" => ScopeVocab {
                topics: vec![
                    "tagging-systems".into(),
                    "information-retrieval".into(),
                    "memory-systems".into(),
                    "agent-memory".into(),
                    "concept-mapping".into(),
                    "embedding-models".into(),
                    "search".into(),
                    "fact-management".into(),
                    "internal-tools".into(),
                    "metadata".into(),
                    "privacy".into(),
                    "rollout".into(),
                    "topic-extraction".into(),
                ],
                entities: vec![
                    "Engram".into(),
                    "engram.m3.dogfood".into(),
                    "MCP".into(),
                    "agent memory protocol".into(),
                    "cross-encoder".into(),
                    "embedding-based".into(),
                    "Hummingbird".into(),
                    "lexical signals".into(),
                    "ollama/qwen3-coder:30b v1".into(),
                ],
            },
            "engram.test" => ScopeVocab {
                topics: vec![
                    "performance".into(),
                    "database".into(),
                    "serialization".into(),
                    "storage".into(),
                    "benchmarking".into(),
                    "build-systems".into(),
                    "development-environment".into(),
                    "go".into(),
                    "real-time-updates".into(),
                    "rpc".into(),
                    "rust".into(),
                    "server-sent-events".into(),
                    "tool-comparison".into(),
                    "websockets".into(),
                    "zig".into(),
                ],
                entities: vec![
                    "PostgreSQL".into(),
                    "100MB".into(),
                    "1MB".into(),
                    "Bazel".into(),
                    "C".into(),
                    "Cap'n Proto".into(),
                    "Cassandra".into(),
                    "Go".into(),
                    "long-polling".into(),
                    "M2 Pro".into(),
                    "Make".into(),
                    "MVCC".into(),
                    "Nix".into(),
                    "Protobuf".into(),
                    "Protocol Buffers".into(),
                    "Redis".into(),
                    "Rust".into(),
                    "serde_json".into(),
                    "Server-Sent Events".into(),
                    "simd-json".into(),
                    "SSE".into(),
                    "VACUUM".into(),
                    "WebSockets".into(),
                    "Zig".into(),
                ],
            },
            "work.tcgplayer" => ScopeVocab {
                topics: vec![
                    "programming-languages".into(),
                    "software-development".into(),
                    "technology-preferences".into(),
                    "engram".into(),
                    "enterprise-software".into(),
                    "scope-convention".into(),
                    "scope-design".into(),
                    "search".into(),
                    "thought-management".into(),
                ],
                entities: vec![
                    "TCGplayer".into(),
                    "engram".into(),
                    "Go".into(),
                    "Rust".into(),
                ],
            },
            other => panic!("no frozen scope vocab for scope {other:?}"),
        }
    }

    /// Build the OpenAI-compatible config matching Ron's runtime tagger:
    /// Ollama on :11434 with qwen3-coder:30b, bundled v2 prompt, temperature
    /// 0.2, model_version = BUNDLED_TAGGER_VERSION.
    #[cfg(feature = "integration")]
    fn diagnostic_tagger() -> OpenAICompatibleTagger {
        let cfg = OpenAICompatibleConfig {
            endpoint: "http://localhost:11434/v1".to_string(),
            model_name: "qwen3-coder:30b".to_string(),
            model_id: "ollama/qwen3-coder:30b".to_string(),
            model_version: BUNDLED_TAGGER_VERSION,
            api_key: None,
            timeout: Duration::from_secs(180),
            temperature: 0.2,
            system_prompt: None,
        };
        OpenAICompatibleTagger::new(cfg).expect("OpenAICompatibleTagger::new should succeed")
    }

    /// M4.1 dogfood diagnostic: measure within-tagger `kind` stability by
    /// running N=10 tag passes on each of six fixture thoughts pulled from the
    /// operator's local corpus (two drift-candidates Ron cited plus four
    /// controls). Prints a markdown distribution table; does not assert.
    ///
    /// Configured for Ron's current setup: Ollama on `http://localhost:11434/v1`
    /// with `qwen3-coder:30b`, bundled v2 prompt, temperature 0.2, vocab=None
    /// to isolate from scope-vocab effects.
    ///
    /// Run with:
    /// `cargo test -p engram-extract --features integration --release -- kind_stability_diagnostic --nocapture --ignored`
    ///
    /// `--ignored` because each call is ~5-20s on a 30B Ollama model; 6×10=60
    /// calls means 5-20 minutes wallclock. Not appropriate for the default
    /// integration suite.
    #[cfg(feature = "integration")]
    #[tokio::test]
    #[ignore]
    async fn kind_stability_diagnostic() {
        // Six fixture thoughts pulled from the post-M4.1 corpus. Format:
        // (short_id, current_v2_kind, descriptor, content).
        let fixtures: Vec<(&str, &str, &str, &str)> = vec![
            (
                "8a533e15",
                "observation",
                "Mission/setup (drift candidate: was task in v1, observation in v2)",
                "Mission for the engram.m3.dogfood scope: We are testing the Engram agent memory system via its MCP toolset. We are testing for accuracy, making sure that facts don't drift negatively, and that searches return expected information.\n\nWhen scope is a parameter, we will use \"engram.m3.dogfood\".\n\nFor any of our conversations, the agent will always consult the facts and thoughts in that scope, giving more weight to facts than thoughts. The agent will add any interesting thought that it or the operator comes up with during the conversation. If unsure, the agent will ask the operator whether it should be stored.",
            ),
            (
                "047d0ce8",
                "observation",
                "Probe 2B definitional (drift candidate: was reference in v1, observation in v2)",
                "The agent memory protocol provides five operations: writing notes, querying them by similarity or recency, fetching by id, and marking notes untrusted. Querying combines embedding-based and lexical signals, optionally re-scored by a cross-encoder.",
            ),
            (
                "22bccb3a",
                "reference",
                "Clean definitional/reference control (Cap'n Proto)",
                "Cap'n Proto is a serialization format that uses the same memory layout in-memory and on-the-wire, eliminating parse and encode steps. Compared to Protocol Buffers, it offers zero-copy reads but at the cost of more rigid schema evolution. For very high-throughput RPC workloads, it can outperform Protobuf by an order of magnitude.",
            ),
            (
                "5aacd2d8",
                "reference",
                "Short definitional/reference control (Hummingbird)",
                "Hummingbird is our internal rollout coordinator. It exposes a percentage knob per cohort and a kill-switch endpoint. Currently used by the privacy-sensitive code paths only.",
            ),
            (
                "b67db532",
                "person_note",
                "Closed-enum person_note control (Ron / Python)",
                "Ron (CTO of TCGplayer) does not like Python or JavaScript, particularly for enterprise software.",
            ),
            (
                "86c3392f",
                "observation",
                "Session-shaped narrative control (benchmark run)",
                "I ran a benchmark this morning comparing serde_json and simd-json for parsing 100MB of test JSON. simd-json was 3.2x faster on this hardware (M2 Pro, 16GB). The general finding holds across many tests in the community: SIMD-accelerated JSON parsing significantly outperforms scalar implementations for documents over roughly 1MB. For smaller documents the SIMD setup overhead can negate the benefit.",
            ),
        ];
        const N_RUNS: usize = 10;

        // Match the operator's actual runtime config: Ollama on :11434, the
        // qwen3-coder:30b model, bundled v2 prompt, temperature 0.2, version 2.
        let cfg = OpenAICompatibleConfig {
            endpoint: "http://localhost:11434/v1".to_string(),
            model_name: "qwen3-coder:30b".to_string(),
            model_id: "ollama/qwen3-coder:30b".to_string(),
            model_version: BUNDLED_TAGGER_VERSION,
            api_key: None,
            timeout: Duration::from_secs(180),
            temperature: 0.2,
            system_prompt: None,
        };
        let t =
            OpenAICompatibleTagger::new(cfg).expect("OpenAICompatibleTagger::new should succeed");

        // Per-fixture results: short_id -> (descriptor, current_kind, [observed_kinds; N]).
        type Observed = (String, String, Vec<String>);
        let mut results: Vec<(String, Observed)> = Vec::new();

        for (short_id, current_kind, descriptor, content) in &fixtures {
            let mut observed: Vec<String> = Vec::with_capacity(N_RUNS);
            for run in 0..N_RUNS {
                eprintln!("[diagnostic] {short_id} run {}/{} ...", run + 1, N_RUNS);
                match t.tag(content, None).await {
                    Ok(tags) => {
                        let k = tags
                            .kind
                            .map(|k| format!("{k:?}").to_lowercase())
                            .unwrap_or_else(|| "null".to_string());
                        observed.push(k);
                    }
                    Err(e) => {
                        eprintln!("[diagnostic] {short_id} run {} ERR: {e}", run + 1);
                        observed.push(format!("ERR({e})"));
                    }
                }
            }
            results.push((
                short_id.to_string(),
                (descriptor.to_string(), current_kind.to_string(), observed),
            ));
        }

        // Render results as a markdown table on stderr.
        eprintln!();
        eprintln!("## v2 kind-stability diagnostic results (N={N_RUNS} per thought)");
        eprintln!();
        eprintln!(
            "Tagger: ollama/qwen3-coder:30b @ http://localhost:11434/v1, bundled v2 prompt, temperature 0.2, vocab=None."
        );
        eprintln!();
        eprintln!(
            "| short_id | current kind (v2 stored) | descriptor | kind distribution (N=10) |"
        );
        eprintln!("|---|---|---|---|");
        for (short_id, (descriptor, current_kind, observed)) in &results {
            let mut counts: std::collections::BTreeMap<&str, usize> =
                std::collections::BTreeMap::new();
            for k in observed {
                *counts.entry(k.as_str()).or_insert(0) += 1;
            }
            let dist = counts
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!("| `{short_id}` | `{current_kind}` | {descriptor} | {dist} |");
        }
        eprintln!();
        eprintln!("Raw observations (one row per fixture):");
        for (short_id, (_, _, observed)) in &results {
            eprintln!("- {short_id}: {observed:?}");
        }
    }

    /// M4.1 dogfood diagnostic, vocab-ON variant. Same six fixtures, same
    /// tagger config, N=10 — but each call is made with `vocab=Some(<frozen
    /// scope vocab>)` matching what the worker tick / `engram tag --rerun`
    /// would pass at runtime. Tests the hypothesis that scope-vocab injection
    /// is the lever causing the stored-vs-vocab-off kind divergence.
    ///
    /// Hypothesis: under vocab-on, each fixture's diagnostic kind matches its
    /// stored kind (i.e. vocab is the differentiator, not some unknown drift).
    /// Confirmation supports v3 adding an explicit kind-isolation clause to
    /// the prompt; refutation means a third mechanism is at play and v3 needs
    /// more investigation.
    ///
    /// Run with:
    /// `cargo test -p engram-extract --features integration --release -- kind_stability_diagnostic_with_vocab --nocapture --ignored`
    #[cfg(feature = "integration")]
    #[tokio::test]
    #[ignore]
    async fn kind_stability_diagnostic_with_vocab() {
        let fixtures = diagnostic_fixtures();
        const N_RUNS: usize = 10;

        let t = diagnostic_tagger();

        type Observed = (String, String, String, Vec<String>);
        let mut results: Vec<(String, Observed)> = Vec::new();

        for (short_id, scope, current_kind, descriptor, content) in &fixtures {
            let vocab = diagnostic_scope_vocab(scope);
            let mut observed: Vec<String> = Vec::with_capacity(N_RUNS);
            for run in 0..N_RUNS {
                eprintln!(
                    "[diagnostic-vocab] {short_id} ({scope}) run {}/{} ...",
                    run + 1,
                    N_RUNS
                );
                match t.tag(content, Some(&vocab)).await {
                    Ok(tags) => {
                        let k = tags
                            .kind
                            .map(|k| format!("{k:?}").to_lowercase())
                            .unwrap_or_else(|| "null".to_string());
                        observed.push(k);
                    }
                    Err(e) => {
                        eprintln!("[diagnostic-vocab] {short_id} run {} ERR: {e}", run + 1);
                        observed.push(format!("ERR({e})"));
                    }
                }
            }
            results.push((
                short_id.to_string(),
                (
                    scope.to_string(),
                    descriptor.to_string(),
                    current_kind.to_string(),
                    observed,
                ),
            ));
        }

        eprintln!();
        eprintln!("## v2 kind-stability diagnostic results — VOCAB-ON (N={N_RUNS} per thought)");
        eprintln!();
        eprintln!(
            "Tagger: ollama/qwen3-coder:30b @ http://localhost:11434/v1, bundled v2 prompt, temperature 0.2, vocab=Some(<frozen scope vocab>)."
        );
        eprintln!();
        eprintln!("| short_id | scope | stored kind | descriptor | kind distribution (N=10) |");
        eprintln!("|---|---|---|---|---|");
        for (short_id, (scope, descriptor, current_kind, observed)) in &results {
            let mut counts: std::collections::BTreeMap<&str, usize> =
                std::collections::BTreeMap::new();
            for k in observed {
                *counts.entry(k.as_str()).or_insert(0) += 1;
            }
            let dist = counts
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!("| `{short_id}` | `{scope}` | `{current_kind}` | {descriptor} | {dist} |");
        }
        eprintln!();
        eprintln!("Raw observations (one row per fixture):");
        for (short_id, (_, _, _, observed)) in &results {
            eprintln!("- {short_id}: {observed:?}");
        }
    }
}
