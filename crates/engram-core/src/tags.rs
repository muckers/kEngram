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

use serde::{Deserialize, Serialize};

/// LLM-extracted metadata attached to a single thought. See the
/// `BUNDLED_TAGGER_PROMPT` for the field-by-field semantics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tags {
    #[serde(default)]
    pub people: Vec<String>,
    #[serde(default)]
    pub action_items: Vec<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub dates_mentioned: Vec<String>,
    #[serde(default)]
    pub kind: Option<TagKind>,
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
        assert_eq!(json["action_items"], serde_json::json!([]));
        assert_eq!(json["topics"], serde_json::json!([]));
        assert_eq!(json["dates_mentioned"], serde_json::json!([]));
        assert_eq!(json["kind"], serde_json::Value::Null);
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
            action_items: vec!["fix the login bug".to_string()],
            topics: vec!["rust".to_string(), "build-systems".to_string()],
            dates_mentioned: vec!["next Thursday".to_string(), "Q3".to_string()],
            kind: Some(TagKind::Task),
        };
        let json = serde_json::to_string(&t).unwrap();
        let parsed: Tags = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, t);
    }

    #[test]
    fn tag_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&TagKind::Observation).unwrap(),
            "\"observation\""
        );
        assert_eq!(
            serde_json::to_string(&TagKind::Task).unwrap(),
            "\"task\""
        );
        assert_eq!(
            serde_json::to_string(&TagKind::Idea).unwrap(),
            "\"idea\""
        );
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
        let json = r#"{"people":[],"action_items":[],"topics":[],"dates_mentioned":[],"kind":null}"#;
        let t: Tags = serde_json::from_str(json).unwrap();
        assert!(t.kind.is_none());
    }
}
