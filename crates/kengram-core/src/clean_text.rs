//! Clean text surfaces for embedding.
//!
//! Raw `thoughts.content` remains the display/audit source of truth. This
//! helper strips Argus transport/import envelopes before embedding so vector
//! search sees the factual body instead of mostly metadata.

pub const CLEAN_EMBED_STRATEGY: &str = "kengram-clean-v1";
/// Conservative proxy for Gemini embedding's 2,048-token input ceiling.
/// Dense operational text can run near 3 chars/token, so this stays below
/// 2,048 tokens without needing a provider tokenizer in the hot path.
pub const GEMINI_CLEAN_EMBED_MAX_CHARS: usize = 6_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanEmbedText {
    pub text: String,
    pub reason: &'static str,
}

pub fn clean_embed_text(content: &str) -> CleanEmbedText {
    let normalized = normalize_distilled_newlines(content);
    let input = normalized.as_deref().unwrap_or(content);

    let candidates = [
        strip_document_envelope(input).map(|s| (s, "document_envelope")),
        strip_a2a_envelope(input).map(|s| (s, "a2a_envelope")),
        strip_telegram_envelope(input).map(|s| (s, "telegram_envelope")),
        strip_distilled_batch(input).map(|s| (s, "distilled_batch_summary")),
        strip_hive_memory_envelope(input).map(|s| (s, "hive_memory_envelope")),
    ];

    for candidate in candidates.into_iter().flatten() {
        let cleaned = candidate.0.trim();
        if !cleaned.is_empty() {
            return CleanEmbedText {
                text: cleaned.to_string(),
                reason: candidate.1,
            };
        }
    }

    CleanEmbedText {
        text: input.trim().to_string(),
        reason: "unchanged",
    }
}

pub fn gemini_clean_embed_text(content: &str) -> CleanEmbedText {
    let clean = clean_embed_text(content);
    truncate_for_gemini(clean)
}

fn truncate_for_gemini(clean: CleanEmbedText) -> CleanEmbedText {
    if clean.text.chars().count() <= GEMINI_CLEAN_EMBED_MAX_CHARS {
        return clean;
    }

    let truncated = clean
        .text
        .chars()
        .take(GEMINI_CLEAN_EMBED_MAX_CHARS)
        .collect::<String>();

    CleanEmbedText {
        text: truncated,
        reason: truncated_reason(clean.reason),
    }
}

fn truncated_reason(reason: &'static str) -> &'static str {
    match reason {
        "document_envelope" => "document_envelope_truncated",
        "a2a_envelope" => "a2a_envelope_truncated",
        "telegram_envelope" => "telegram_envelope_truncated",
        "distilled_batch_summary" => "distilled_batch_summary_truncated",
        "hive_memory_envelope" => "hive_memory_envelope_truncated",
        "unchanged" => "unchanged_truncated",
        _ => "unknown_truncated",
    }
}

fn normalize_distilled_newlines(content: &str) -> Option<String> {
    if (content.starts_with("Session distilled batch for ")
        || content.starts_with("Telegram distilled batch for "))
        && content.contains("\\n")
    {
        Some(content.replace("\\n", "\n"))
    } else {
        None
    }
}

fn strip_document_envelope(content: &str) -> Option<&str> {
    let first_line = content.lines().next()?;
    if !matches_document_header(first_line) {
        return None;
    }

    let chunk_marker = content.find("\nChunk:")?;
    let line_start = chunk_marker + 1;
    let line_end = content[line_start..]
        .find('\n')
        .map(|offset| line_start + offset)
        .unwrap_or(content.len());
    let chunk_line = &content[line_start..line_end];

    if let Some(body_start) = chunk_line.find('#') {
        return Some(&content[line_start + body_start..]);
    }

    Some(content[line_end..].trim_start_matches('\n'))
}

fn matches_document_header(line: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "Spec document:",
        "Review document:",
        "Report document:",
        "Plan document:",
        "Task document:",
        "Implementation document:",
        "Design document:",
        "Handoff document:",
        "Return prompt document:",
        "Runbook document:",
    ];

    PREFIXES.iter().any(|prefix| line.starts_with(prefix))
}

fn strip_a2a_envelope(content: &str) -> Option<&str> {
    if !content.starts_with("Agent-to-agent conversation message.") {
        return None;
    }

    let body = strip_after_text_marker(content)?;
    Some(strip_trailing_source(body, "\nSource: a2a:").trim())
}

fn strip_telegram_envelope(content: &str) -> Option<&str> {
    if !content.starts_with("Telegram message.") {
        return None;
    }

    let body = strip_after_text_marker(content)?;
    Some(strip_trailing_source(body, "\nSource: telegram:").trim())
}

fn strip_after_text_marker(content: &str) -> Option<&str> {
    if let Some(pos) = content.find("\nText:\n") {
        return Some(&content[pos + "\nText:\n".len()..]);
    }
    if let Some(pos) = content.find("\nText:") {
        return Some(&content[pos + "\nText:".len()..]);
    }
    None
}

fn strip_trailing_source<'a>(body: &'a str, marker: &str) -> &'a str {
    body.rfind(marker).map(|pos| &body[..pos]).unwrap_or(body)
}

fn strip_distilled_batch(content: &str) -> Option<&str> {
    if !(content.starts_with("Session distilled batch for ")
        || content.starts_with("Telegram distilled batch for "))
    {
        return None;
    }

    content
        .find("Summary:")
        .map(|pos| &content[pos + "Summary:".len()..])
}

fn strip_hive_memory_envelope(content: &str) -> Option<&str> {
    if !content.starts_with("Hive memory") {
        return None;
    }

    let mut body_start = 0;
    for line in content.lines() {
        let line_len = line.len();
        let next_start = body_start + line_len + 1;
        let is_header = line == "Hive memory"
            || line.starts_with("Scope:")
            || line.starts_with("Kind:")
            || line.starts_with("Memory type:")
            || line.starts_with("Title:");
        if line.trim().is_empty() {
            return Some(&content[next_start.min(content.len())..]);
        }
        if !is_header {
            return Some(&content[body_start..]);
        }
        body_start = next_start.min(content.len());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_document_import_header() {
        let raw =
            "Spec document: knox/specs/foo.md\nOwner: knox\nChunk: 1\n\n## Scope\nThe fact body.";
        let clean = clean_embed_text(raw);
        assert_eq!(clean.reason, "document_envelope");
        assert_eq!(clean.text, "## Scope\nThe fact body.");
    }

    #[test]
    fn preserves_inline_document_heading_on_chunk_line() {
        let raw = "Review document: neo/reviews/a.md\nOwner: neo\nChunk: 2 ## Finding\nBody";
        let clean = clean_embed_text(raw);
        assert_eq!(clean.text, "## Finding\nBody");
    }

    #[test]
    fn strips_a2a_envelope_and_source_footer() {
        let raw = "Agent-to-agent conversation message.\nFrom: smith\nTo: neo\nType: task\nText:\nDo the thing.\nSource: a2a:abc123";
        let clean = clean_embed_text(raw);
        assert_eq!(clean.reason, "a2a_envelope");
        assert_eq!(clean.text, "Do the thing.");
    }

    #[test]
    fn strips_distilled_literal_newline_summary_prefix() {
        let raw = "Telegram distilled batch for neo.\\nSummary: Victory updated.\\n\\nKey Facts:\\n- Gateway moved.";
        let clean = clean_embed_text(raw);
        assert_eq!(clean.reason, "distilled_batch_summary");
        assert_eq!(
            clean.text,
            "Victory updated.\n\nKey Facts:\n- Gateway moved."
        );
    }

    #[test]
    fn unchanged_when_no_known_envelope() {
        let clean = clean_embed_text("  plain fact body  ");
        assert_eq!(clean.reason, "unchanged");
        assert_eq!(clean.text, "plain fact body");
    }

    #[test]
    fn gemini_clean_embed_text_truncates_on_char_boundary() {
        let raw = format!(
            "Spec document: a\nOwner: neo\nChunk: 1\n\n{}tail",
            "x".repeat(GEMINI_CLEAN_EMBED_MAX_CHARS + 10)
        );
        let clean = gemini_clean_embed_text(&raw);
        assert_eq!(clean.reason, "document_envelope_truncated");
        assert_eq!(clean.text.chars().count(), GEMINI_CLEAN_EMBED_MAX_CHARS);
        assert!(!clean.text.contains("tail"));
    }
}
