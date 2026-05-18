//! `Tags` — the LLM-extracted metadata sidecar attached to each thought.
//!
//! Replaces the M3 facts pipeline. Where facts were full sentences with
//! provenance and embeddings of their own, tags are bare metadata fields
//! attached to the thought row itself: who is mentioned, what tasks the
//! thought commits to, what topics it's about, and a single
//! kind-classification.
//!
//! Schema lives on the wire as a flat JSON object. Default for every field
//! is the empty value (empty vec / `None`), so deserializing `{}` yields a
//! valid `Tags::default()`. New tagger versions can add fields without
//! breaking older readers.

use crate::{LinkTarget, RelationKind};
use serde::{Deserialize, Serialize};

/// LLM-extracted metadata attached to a single thought. See the
/// `BUNDLED_TAGGER_PROMPT` for the field-by-field semantics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tags {
    #[serde(default)]
    pub people: Vec<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub action_items: Vec<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub dates_mentioned: Vec<String>,
    #[serde(default)]
    pub kind: Option<TagKind>,
    /// Tagger-extracted relations (M6.1+). v1 emits non-thought targets
    /// only — entity / person / URL. Thought-to-thought relations require
    /// entity resolution and are deferred to a future iteration. Older
    /// tagger versions (≤4) didn't emit this field; serde default = `[]`
    /// keeps backward compatibility.
    #[serde(default)]
    pub relations: Vec<ExtractedRelation>,
}

/// One LLM-extracted edge attached to a thought. The drainer inserts these
/// into `thought_links` with `source = 'tagger'` after writing the rest of
/// the tags. v1 (M6.1) targets non-thoughts only — thought-target tagger
/// relations are deferred until entity resolution lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedRelation {
    pub relation: RelationKind,
    #[serde(flatten)]
    pub target: ExtractedTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Non-thought target shape emitted by the tagger. Mirrors `LinkTarget`'s
/// non-thought variants and matches the JSON shape `{to_kind, to_value}` on
/// the wire (flattened into `ExtractedRelation`). Converts losslessly into
/// the full `LinkTarget` for insertion via `engram_storage::insert_link`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "to_kind", content = "to_value", rename_all = "snake_case")]
pub enum ExtractedTarget {
    Entity(String),
    Person(String),
    Url(String),
}

impl ExtractedTarget {
    /// Convert into the polymorphic `LinkTarget` used by the storage layer.
    /// Lossless — `ExtractedTarget` is a strict subset of `LinkTarget`
    /// (omits the `Thought` variant).
    pub fn into_link_target(self) -> LinkTarget {
        match self {
            Self::Entity(name) => LinkTarget::Entity(name),
            Self::Person(name) => LinkTarget::Person(name),
            Self::Url(url) => LinkTarget::Url(url),
        }
    }

    /// Stable discriminator string (mirrors `LinkTarget::kind_str`).
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Entity(_) => "entity",
            Self::Person(_) => "person",
            Self::Url(_) => "url",
        }
    }
}

/// Top-N established tag terms from a given scope, supplied to the tagger as
/// controlled-vocabulary hints. Helps the tagger emit consistent terms when
/// it sees similar concepts in different prose, addressing v1's phrase-driven
/// divergence at corpus level.
///
/// Empty vectors are valid — they signal "no established vocabulary yet" and
/// the tagger falls back to free-form term coinage.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScopeVocab {
    pub topics: Vec<String>,
    pub entities: Vec<String>,
}

impl ScopeVocab {
    pub fn is_empty(&self) -> bool {
        self.topics.is_empty() && self.entities.is_empty()
    }
}

/// Single high-level classification a thought belongs to. `PersonNote`
/// serializes as `"person_note"` per the snake_case rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagKind {
    Observation,
    Task,
    Idea,
    Reference,
    PersonNote,
    Session,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_default_round_trips_as_empty_object_shape() {
        let t = Tags::default();
        let json = serde_json::to_value(&t).unwrap();
        // Default emits every field as empty/null, not as a literal `{}`,
        // but the inverse direction below confirms `{}` is accepted as default.
        assert_eq!(json["people"], serde_json::json!([]));
        assert_eq!(json["entities"], serde_json::json!([]));
        assert_eq!(json["action_items"], serde_json::json!([]));
        assert_eq!(json["topics"], serde_json::json!([]));
        assert_eq!(json["dates_mentioned"], serde_json::json!([]));
        assert_eq!(json["kind"], serde_json::Value::Null);
    }

    #[test]
    fn v1_shape_without_entities_deserializes_with_empty_entities() {
        // Backward-compat: rows tagged under v1 (no `entities` key) must still
        // parse, with `entities` defaulting to `vec![]`.
        let v1_json = r#"{
            "people":["Sarah"],
            "action_items":[],
            "topics":["rust"],
            "dates_mentioned":[],
            "kind":"observation"
        }"#;
        let t: Tags = serde_json::from_str(v1_json).unwrap();
        assert_eq!(t.entities, Vec::<String>::new());
        assert_eq!(t.topics, vec!["rust".to_string()]);
        assert_eq!(t.kind, Some(TagKind::Observation));
    }

    #[test]
    fn empty_object_deserializes_into_default_tags() {
        let t: Tags = serde_json::from_str("{}").unwrap();
        assert_eq!(t, Tags::default());
    }

    #[test]
    fn full_field_serde_roundtrip() {
        let t = Tags {
            people: vec!["Sarah".to_string(), "Ron".to_string()],
            entities: vec!["engram".to_string(), "pgvector".to_string()],
            action_items: vec!["fix the login bug".to_string()],
            topics: vec!["rust".to_string(), "build-systems".to_string()],
            dates_mentioned: vec!["next Thursday".to_string(), "Q3".to_string()],
            kind: Some(TagKind::Task),
            relations: vec![],
        };
        let json = serde_json::to_string(&t).unwrap();
        let parsed: Tags = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, t);
    }

    #[test]
    fn v4_shape_without_relations_deserializes_with_empty_relations() {
        // Backward-compat: rows tagged under v1-v4 (no `relations` key) must
        // still parse, with `relations` defaulting to `vec![]`. Critical for
        // the M6.1 rollout — v4-tagged thoughts in the corpus pre-date v5.
        let v4_json = r#"{
            "people":["Sarah"],
            "entities":["engram"],
            "action_items":[],
            "topics":["rust"],
            "dates_mentioned":[],
            "kind":"observation"
        }"#;
        let t: Tags = serde_json::from_str(v4_json).unwrap();
        assert_eq!(t.relations, Vec::<ExtractedRelation>::new());
    }

    #[test]
    fn extracted_relation_serde_round_trip() {
        let r = ExtractedRelation {
            relation: RelationKind::References,
            target: ExtractedTarget::Url("https://anthropic.com".into()),
            note: Some("explicit citation in opening sentence".into()),
        };
        let json = serde_json::to_value(&r).unwrap();
        // Wire shape: {relation, to_kind, to_value, note} — flat object.
        assert_eq!(json["relation"], "references");
        assert_eq!(json["to_kind"], "url");
        assert_eq!(json["to_value"], "https://anthropic.com");
        assert_eq!(json["note"], "explicit citation in opening sentence");

        let parsed: ExtractedRelation = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, r);
    }

    #[test]
    fn extracted_relation_note_optional() {
        // Note field is optional on the wire (skip_serializing_if when None).
        let r = ExtractedRelation {
            relation: RelationKind::BelongsTo,
            target: ExtractedTarget::Entity("Probe 2".into()),
            note: None,
        };
        let json = serde_json::to_value(&r).unwrap();
        assert!(
            json.get("note").is_none(),
            "note should be omitted when None"
        );

        // And deserialize a payload without `note` works.
        let parsed: ExtractedRelation = serde_json::from_str(
            r#"{"relation":"belongs_to","to_kind":"entity","to_value":"Probe 2"}"#,
        )
        .unwrap();
        assert_eq!(parsed.note, None);
    }

    #[test]
    fn extracted_target_into_link_target_preserves_kind_and_value() {
        let e = ExtractedTarget::Entity("Probe 2".into());
        assert_eq!(e.kind_str(), "entity");
        assert_eq!(
            e.clone().into_link_target(),
            LinkTarget::Entity("Probe 2".into())
        );

        let p = ExtractedTarget::Person("Ron".into());
        assert_eq!(p.kind_str(), "person");
        assert_eq!(
            p.clone().into_link_target(),
            LinkTarget::Person("Ron".into())
        );

        let u = ExtractedTarget::Url("https://x.io".into());
        assert_eq!(u.kind_str(), "url");
        assert_eq!(
            u.clone().into_link_target(),
            LinkTarget::Url("https://x.io".into())
        );
    }

    #[test]
    fn scope_vocab_is_empty_helper() {
        assert!(ScopeVocab::default().is_empty());
        let v = ScopeVocab {
            topics: vec!["rust".to_string()],
            entities: vec![],
        };
        assert!(!v.is_empty());
    }

    #[test]
    fn tag_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&TagKind::Observation).unwrap(),
            "\"observation\""
        );
        assert_eq!(serde_json::to_string(&TagKind::Task).unwrap(), "\"task\"");
        assert_eq!(serde_json::to_string(&TagKind::Idea).unwrap(), "\"idea\"");
        assert_eq!(
            serde_json::to_string(&TagKind::Reference).unwrap(),
            "\"reference\""
        );
        assert_eq!(
            serde_json::to_string(&TagKind::PersonNote).unwrap(),
            "\"person_note\""
        );
        assert_eq!(
            serde_json::to_string(&TagKind::Session).unwrap(),
            "\"session\""
        );
    }

    #[test]
    fn tag_kind_deserializes_snake_case() {
        let k: TagKind = serde_json::from_str("\"person_note\"").unwrap();
        assert_eq!(k, TagKind::PersonNote);
        let k: TagKind = serde_json::from_str("\"observation\"").unwrap();
        assert_eq!(k, TagKind::Observation);
    }

    #[test]
    fn kind_null_deserializes_to_none() {
        let json = r#"{"people":[],"entities":[],"action_items":[],"topics":[],"dates_mentioned":[],"kind":null}"#;
        let t: Tags = serde_json::from_str(json).unwrap();
        assert!(t.kind.is_none());
    }
}
