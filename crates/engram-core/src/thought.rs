//! `ThoughtId` and `Thought` — the row-shape of `thoughts` in Postgres.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Metadata, Scope, Source};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThoughtId(pub Uuid);

impl ThoughtId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }

    pub fn into_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for ThoughtId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Uuid> for ThoughtId {
    fn from(u: Uuid) -> Self {
        Self(u)
    }
}

impl fmt::Display for ThoughtId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for ThoughtId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::from_str(s).map(Self)
    }
}

/// Full thought row as read from or written to the database.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thought {
    pub id: ThoughtId,
    pub scope: Scope,
    pub content: String,
    pub source: Source,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub metadata: Metadata,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_produces_v4_uuid() {
        let id = ThoughtId::new();
        assert_eq!(id.as_uuid().get_version_num(), 4);
    }

    #[test]
    fn fresh_ids_are_unique() {
        assert_ne!(ThoughtId::new(), ThoughtId::new());
    }

    #[test]
    fn parses_from_uuid_string() {
        let id: ThoughtId = "550e8400-e29b-41d4-a716-446655440000".parse().unwrap();
        assert_eq!(id.to_string(), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn serde_roundtrip_is_transparent_uuid() {
        let id = ThoughtId::new();
        let json = serde_json::to_string(&id).unwrap();
        let parsed: ThoughtId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn thought_serde_roundtrip() {
        use serde_json::json;
        let t = Thought {
            id: ThoughtId::from_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            scope: Scope::new("work").unwrap(),
            content: "remember this".to_string(),
            source: Source::new("manual").unwrap(),
            created_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            metadata: Metadata::from(json!({"client_name": "claude-code"})),
        };
        let json = serde_json::to_string(&t).unwrap();
        let parsed: Thought = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, t);
    }
}
