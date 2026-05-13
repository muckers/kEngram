//! `OpenAICompatibleExtractor` — talks to any backend that implements the
//! OpenAI `/v1/chat/completions` API with `response_format: json_schema`.
//! That covers vLLM (production), OpenRouter (cloud fallback), and OpenAI
//! itself, distinguished only by config.
//!
//! Endpoint convention: the configured `endpoint` is the `/v1` base, and
//! the extractor appends `/chat/completions`. For local vLLM that's
//! `http://localhost:8000/v1`.

use async_trait::async_trait;
use engram_core::{ExtractedFact, ExtractionContext, Extractor, ExtractorError, Thought};
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
    /// Engram-side stable identity written into `facts.extractor_model`.
    /// Conventionally `<vendor>/<model>` — `"vllm/qwen2.5-7b-instruct"`,
    /// `"openrouter/anthropic/claude-haiku-4.5"`.
    pub model_id: String,
    /// Schema-version of this extractor's prompt/response contract. Bump
    /// when the JSON Schema or system prompt changes such that prior facts
    /// are no longer comparable. Written into `facts.extractor_version`.
    pub model_version: i32,
    pub api_key: Option<String>,
    pub timeout: Duration,
    /// Generation temperature. Lower = more deterministic extraction. 0.2
    /// is a reasonable default; 0 makes some backends loop.
    pub temperature: f32,
    /// Soft cap on facts per thought. The reflector context's `max_facts`
    /// wins if it's smaller (so per-run policy can throttle independently).
    pub max_facts_per_thought: usize,
}

impl OpenAICompatibleConfig {
    /// Defaults for a local vLLM dev path on port 8000 with the qwen-7b
    /// instruct model. No API key.
    pub fn vllm_local() -> Self {
        Self {
            endpoint: "http://localhost:8000/v1".to_string(),
            model_name: "qwen2.5-7b-instruct".to_string(),
            model_id: "vllm/qwen2.5-7b-instruct".to_string(),
            model_version: 1,
            api_key: None,
            timeout: Duration::from_secs(60),
            temperature: 0.2,
            max_facts_per_thought: 8,
        }
    }

    /// Preset for OpenRouter cloud fallback. `model_name` is an OpenRouter
    /// model slug (e.g. `"anthropic/claude-haiku-4.5"`); the model_id is
    /// derived by prefixing with `"openrouter/"` so facts retain a clean
    /// provenance string.
    pub fn open_router(api_key: String, model_name: String) -> Self {
        Self {
            endpoint: "https://openrouter.ai/api/v1".to_string(),
            model_id: format!("openrouter/{model_name}"),
            model_name,
            model_version: 1,
            api_key: Some(api_key),
            timeout: Duration::from_secs(60),
            temperature: 0.2,
            max_facts_per_thought: 8,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenAICompatibleExtractor {
    endpoint: String,
    model_name: String,
    model_id: String,
    model_version: i32,
    api_key: Option<String>,
    temperature: f32,
    max_facts_per_thought: usize,
    client: Client,
}

impl OpenAICompatibleExtractor {
    pub fn new(config: OpenAICompatibleConfig) -> Result<Self, ExtractorError> {
        if config.endpoint.is_empty() {
            return Err(ExtractorError::Misconfigured(
                "extractor endpoint must not be empty".into(),
            ));
        }
        if config.model_name.is_empty() {
            return Err(ExtractorError::Misconfigured(
                "extractor model_name must not be empty".into(),
            ));
        }
        if config.max_facts_per_thought == 0 {
            return Err(ExtractorError::Misconfigured(
                "max_facts_per_thought must be > 0".into(),
            ));
        }
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| ExtractorError::Unreachable(format!("client build: {e}")))?;
        Ok(Self {
            endpoint: config.endpoint,
            model_name: config.model_name,
            model_id: config.model_id,
            model_version: config.model_version,
            api_key: config.api_key,
            temperature: config.temperature,
            max_facts_per_thought: config.max_facts_per_thought,
            client,
        })
    }
}

const SYSTEM_PROMPT: &str = "\
You are an information-extraction assistant. Given a single thought from a memory service, identify discrete factual claims and return them as structured JSON.

Each fact has:
- statement: a self-contained natural-language sentence the user could read on its own.
- subject, predicate, object: optional (S, P, O) triple if the fact maps cleanly to one. Use null when no clean triple exists.
- confidence: your self-reported [0.0, 1.0] score. Use lower values (< 0.7) when the claim is hedged, inferred, or you'd want a human to double-check.

Rules:
- Do not invent facts that aren't supported by the input.
- Skip purely conversational, social, or temporal-greeting content — return an empty facts array.
- One fact per claim. Don't bundle multiple distinct claims into a single statement.
- Return at most {MAX_FACTS} facts.";

#[derive(Serialize)]
struct ChatRequestBody<'a> {
    model: &'a str,
    temperature: f32,
    messages: [ChatMessage<'a>; 2],
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

#[derive(Deserialize)]
struct ExtractionPayload {
    facts: Vec<ExtractedFactDto>,
}

#[derive(Deserialize)]
struct ExtractedFactDto {
    statement: String,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    predicate: Option<String>,
    #[serde(default)]
    object: Option<String>,
    confidence: f32,
}

impl From<ExtractedFactDto> for ExtractedFact {
    fn from(d: ExtractedFactDto) -> Self {
        Self {
            statement: d.statement,
            subject: d.subject.filter(|s| !s.is_empty()),
            predicate: d.predicate.filter(|s| !s.is_empty()),
            object: d.object.filter(|s| !s.is_empty()),
            confidence: d.confidence.clamp(0.0, 1.0),
        }
    }
}

#[async_trait]
impl Extractor for OpenAICompatibleExtractor {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn version(&self) -> i32 {
        self.model_version
    }

    async fn extract(
        &self,
        thought: &Thought,
        ctx: &ExtractionContext,
    ) -> Result<Vec<ExtractedFact>, ExtractorError> {
        let max_facts = ctx.max_facts.min(self.max_facts_per_thought).max(1);
        let url = format!("{}/chat/completions", self.endpoint.trim_end_matches('/'));

        let system_prompt = SYSTEM_PROMPT.replace("{MAX_FACTS}", &max_facts.to_string());
        let body = ChatRequestBody {
            model: &self.model_name,
            temperature: self.temperature,
            messages: [
                ChatMessage {
                    role: "system",
                    content: system_prompt,
                },
                ChatMessage {
                    role: "user",
                    content: thought.content.clone(),
                },
            ],
            response_format: facts_response_format(),
        };

        let mut req = self.client.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req.send().await.map_err(map_send_error)?;
        let status = resp.status();
        if !status.is_success() {
            let message = resp.text().await.unwrap_or_default();
            return Err(ExtractorError::Backend {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: ChatResponseBody = resp.json().await.map_err(|e| {
            ExtractorError::MalformedResponse(format!("decoding chat completions response: {e}"))
        })?;

        let content = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ExtractorError::MalformedResponse("response had zero choices".into()))?
            .message
            .content;

        let payload: ExtractionPayload = serde_json::from_str(&content).map_err(|e| {
            ExtractorError::MalformedResponse(format!(
                "decoding facts payload (content={content:?}): {e}"
            ))
        })?;

        Ok(payload
            .facts
            .into_iter()
            .take(max_facts)
            .map(ExtractedFact::from)
            .collect())
    }
}

/// The `response_format` JSON object sent to the chat completions API. The
/// schema constrains the model to a `{facts: [...]}` shape with the
/// statement/subject/predicate/object/confidence fields per item.
fn facts_response_format() -> serde_json::Value {
    serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "engram_facts",
            "strict": true,
            "schema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "facts": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "statement": {"type": "string"},
                                "subject": {"type": ["string", "null"]},
                                "predicate": {"type": ["string", "null"]},
                                "object": {"type": ["string", "null"]},
                                "confidence": {"type": "number"}
                            },
                            "required": ["statement", "subject", "predicate", "object", "confidence"]
                        }
                    }
                },
                "required": ["facts"]
            }
        }
    })
}

fn map_send_error(e: reqwest::Error) -> ExtractorError {
    if e.is_timeout() {
        ExtractorError::Timeout { seconds: 60 }
    } else if e.is_connect() {
        ExtractorError::Unreachable(e.to_string())
    } else if let Some(status) = e.status() {
        ExtractorError::Backend {
            status: status.as_u16(),
            message: e.to_string(),
        }
    } else {
        ExtractorError::Unreachable(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_core::{Metadata, Scope, Source, ThoughtId};
    use serde_json::json;
    use time::OffsetDateTime;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_thought(content: &str) -> Thought {
        Thought {
            id: ThoughtId::new(),
            scope: Scope::global(),
            content: content.to_string(),
            source: Source::new("test").unwrap(),
            created_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            metadata: Metadata::empty(),
        }
    }

    fn ctx(max: usize) -> ExtractionContext {
        ExtractionContext::new(Scope::global(), max)
    }

    fn config_for(endpoint: String, api_key: Option<String>) -> OpenAICompatibleConfig {
        OpenAICompatibleConfig {
            endpoint,
            model_name: "test-model".to_string(),
            model_id: "test/test-model".to_string(),
            model_version: 1,
            api_key,
            timeout: Duration::from_secs(2),
            temperature: 0.0,
            max_facts_per_thought: 8,
        }
    }

    fn chat_response_with_facts(facts: serde_json::Value) -> serde_json::Value {
        let content = serde_json::to_string(&json!({"facts": facts})).unwrap();
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
    async fn valid_response_parses_to_facts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(chat_response_with_facts(json!([
                {
                    "statement": "Engram uses pgvector",
                    "subject": "Engram",
                    "predicate": "uses",
                    "object": "pgvector",
                    "confidence": 0.92
                },
                {
                    "statement": "Single-user assumption holds in v0",
                    "subject": null,
                    "predicate": null,
                    "object": null,
                    "confidence": 0.75
                }
            ]))))
            .mount(&server)
            .await;

        let e = OpenAICompatibleExtractor::new(config_for(format!("{}/v1", server.uri()), None))
            .unwrap();
        let facts = e.extract(&make_thought("..."), &ctx(8)).await.unwrap();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].statement, "Engram uses pgvector");
        assert_eq!(facts[0].subject.as_deref(), Some("Engram"));
        assert!((facts[0].confidence - 0.92).abs() < 1e-4);
        assert!(facts[1].subject.is_none());
    }

    #[tokio::test]
    async fn malformed_json_in_message_content_returns_malformed_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"role": "assistant", "content": "not json"}}]
            })))
            .mount(&server)
            .await;

        let e = OpenAICompatibleExtractor::new(config_for(format!("{}/v1", server.uri()), None))
            .unwrap();
        let err = e.extract(&make_thought("x"), &ctx(8)).await.unwrap_err();
        assert!(matches!(err, ExtractorError::MalformedResponse(_)));
        assert!(!err.is_transient());
    }

    #[tokio::test]
    async fn http_500_returns_backend_transient() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream gone"))
            .mount(&server)
            .await;

        let e = OpenAICompatibleExtractor::new(config_for(format!("{}/v1", server.uri()), None))
            .unwrap();
        let err = e.extract(&make_thought("x"), &ctx(8)).await.unwrap_err();
        match err {
            ExtractorError::Backend { status, .. } => assert_eq!(status, 503),
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

        let e = OpenAICompatibleExtractor::new(config_for(format!("{}/v1", server.uri()), None))
            .unwrap();
        let err = e.extract(&make_thought("x"), &ctx(8)).await.unwrap_err();
        match &err {
            ExtractorError::Backend { status, .. } => assert_eq!(*status, 400),
            other => panic!("expected Backend error, got {other:?}"),
        }
        assert!(!err.is_transient());
    }

    #[tokio::test]
    async fn connect_failure_maps_to_unreachable_or_timeout() {
        // Port 1 is reliably refused on macOS/Linux.
        let e =
            OpenAICompatibleExtractor::new(config_for("http://127.0.0.1:1/v1".to_string(), None))
                .unwrap();
        let err = e.extract(&make_thought("x"), &ctx(8)).await.unwrap_err();
        assert!(
            matches!(err, ExtractorError::Unreachable(_) | ExtractorError::Timeout { .. }),
            "expected Unreachable or Timeout, got {err:?}"
        );
        assert!(err.is_transient());
    }

    #[tokio::test]
    async fn system_prompt_includes_max_facts_limit() {
        let server = MockServer::start().await;
        // Match only when the system message text mentions "at most 4 facts."
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({
                "messages": [
                    {"role": "system", "content": serde_json::Value::String("__placeholder__".to_string())},
                    {"role": "user", "content": "x"}
                ]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(chat_response_with_facts(json!([]))))
            .mount(&server)
            .await;

        let e = OpenAICompatibleExtractor::new(config_for(format!("{}/v1", server.uri()), None))
            .unwrap();
        // Lower max — used to substitute {MAX_FACTS} in the system prompt.
        let _ = e.extract(&make_thought("x"), &ctx(4)).await;
        // The mock accepts any system content, but we also verify by
        // inspecting all requests it received and asserting the substitution
        // happened.
        let received = server.received_requests().await.unwrap();
        let last = received.last().expect("at least one request");
        let body: serde_json::Value = serde_json::from_slice(&last.body).unwrap();
        let sys = body["messages"][0]["content"].as_str().unwrap();
        assert!(
            sys.contains("at most 4 facts"),
            "system prompt did not substitute max_facts: {sys}"
        );
    }

    #[tokio::test]
    async fn request_uses_bearer_auth_when_api_key_present() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(chat_response_with_facts(json!([]))))
            .mount(&server)
            .await;

        let e = OpenAICompatibleExtractor::new(config_for(
            format!("{}/v1", server.uri()),
            Some("sk-test".into()),
        ))
        .unwrap();
        // If the auth header is wrong, wiremock returns 404 and the parse fails.
        e.extract(&make_thought("x"), &ctx(8))
            .await
            .expect("auth header must match");
    }

    #[tokio::test]
    async fn empty_endpoint_is_misconfigured() {
        let mut cfg = config_for("".to_string(), None);
        cfg.endpoint = "".into();
        let err = OpenAICompatibleExtractor::new(cfg).unwrap_err();
        assert!(matches!(err, ExtractorError::Misconfigured(_)));
    }

    #[tokio::test]
    async fn caps_facts_at_min_of_ctx_and_config() {
        let server = MockServer::start().await;
        // Server returns 10 facts; ctx max is 3; config max is 8 — result is 3.
        let many = (0..10).map(|i| json!({
            "statement": format!("f{i}"),
            "subject": null, "predicate": null, "object": null,
            "confidence": 0.9
        })).collect::<Vec<_>>();
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(chat_response_with_facts(json!(many))))
            .mount(&server)
            .await;

        let e = OpenAICompatibleExtractor::new(config_for(format!("{}/v1", server.uri()), None))
            .unwrap();
        let facts = e.extract(&make_thought("x"), &ctx(3)).await.unwrap();
        assert_eq!(facts.len(), 3);
    }

    /// Live test against a real OpenAI-compatible endpoint (vLLM by default).
    /// Gated on the `integration` feature; off in CI. Run with
    /// `cargo test -p engram-extract --features integration -- live_vllm`.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn live_vllm_round_trip() {
        let cfg = OpenAICompatibleConfig::vllm_local();
        let e = OpenAICompatibleExtractor::new(cfg).unwrap();
        let t = make_thought(
            "Engram uses pgvector for vector storage and pg_trgm for trigram search.",
        );
        let facts = e
            .extract(&t, &ctx(4))
            .await
            .expect("vLLM unreachable — is it running on :8000?");
        assert!(!facts.is_empty(), "live extractor produced zero facts");
        assert!(facts.iter().all(|f| (0.0..=1.0).contains(&f.confidence)));
    }
}
