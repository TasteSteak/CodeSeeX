//! The one place that removes resource payloads from text bound for the model.
//!
//! Page text is preserved as written; only payload *carriers* are replaced, and
//! the judgement is about size, never about the site, engine, or language the
//! text came from.

use super::extract::BlockKind;
use super::text::char_count;

/// Opaque run allowed in flowing text before it is treated as a payload.
const PROSE_RUN_LIMIT: usize = 512;
/// Code legitimately shows long tokens (tokens, hashes, sample keys), so it is
/// allowed a much longer run before the same rule applies.
const CODE_RUN_LIMIT: usize = 4_096;

/// Characters an opaque payload is made of.
///
/// `-` and `=` are excluded on purpose: both are ordinary punctuation in text,
/// and treating them as payload characters would swallow the `key=` in front of
/// a value instead of only the value.
fn is_opaque_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '_')
}

/// Cleans one block of page text for the model.
pub(super) fn sanitize_block_text(text: &str, kind: BlockKind) -> String {
    let limit = match kind {
        BlockKind::Code => CODE_RUN_LIMIT,
        BlockKind::Prose | BlockKind::TableRow => PROSE_RUN_LIMIT,
    };
    sanitize_with_limit(text, limit)
}

/// Same rule for text that has no block kind yet (search snippets, guard passes).
pub(super) fn sanitize_prose(text: &str) -> String {
    sanitize_with_limit(text, PROSE_RUN_LIMIT)
}

fn sanitize_with_limit(text: &str, limit: usize) -> String {
    let without_data_urls = codeseex_core::context::redact_inline_data_urls(text);
    truncate_opaque_runs(&without_data_urls, limit)
}

/// Replaces any single opaque run at or above `limit` with a bounded marker.
///
/// The scan is idempotent: the marker introduces spaces, so a second pass can
/// never grow the text.
fn truncate_opaque_runs(text: &str, limit: usize) -> String {
    if limit == 0 {
        return text.to_owned();
    }
    let mut output = String::with_capacity(text.len());
    let mut run = String::new();
    for ch in text.chars() {
        if is_opaque_char(ch) {
            run.push(ch);
            continue;
        }
        flush_run(&mut run, limit, &mut output);
        output.push(ch);
    }
    flush_run(&mut run, limit, &mut output);
    output
}

fn flush_run(run: &mut String, limit: usize, output: &mut String) {
    if run.is_empty() {
        return;
    }
    if run.chars().count() >= limit {
        output.push_str(&format!("[payload omitted chars={}]", char_count(run)));
    } else {
        output.push_str(run);
    }
    run.clear();
}

/// Walks a finished payload and sanitizes every string leaf.
///
/// This is the boundary guarantee, not the primary filter: block text is
/// already cleaned when it is produced. It exists so a new producer cannot
/// silently leak a payload, and it reports whether it had to act so the caller
/// can surface that as a defect signal.
pub(super) fn sanitize_value(value: serde_json::Value) -> (serde_json::Value, bool) {
    match value {
        serde_json::Value::String(text) => {
            let cleaned = sanitize_prose(&text);
            let changed = cleaned != text;
            (serde_json::Value::String(cleaned), changed)
        }
        serde_json::Value::Array(items) => {
            let mut changed = false;
            let mut output = Vec::with_capacity(items.len());
            for item in items {
                let (item, item_changed) = sanitize_value(item);
                changed |= item_changed;
                output.push(item);
            }
            (serde_json::Value::Array(output), changed)
        }
        serde_json::Value::Object(object) => {
            let mut changed = false;
            let mut output = serde_json::Map::with_capacity(object.len());
            for (key, item) in object {
                let (item, item_changed) = sanitize_value(item);
                changed |= item_changed;
                output.insert(key, item);
            }
            (serde_json::Value::Object(output), changed)
        }
        other => (other, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn inline_data_urls_are_replaced_with_a_marker() {
        let text = "before data:image/png;base64,AAAA after";
        let cleaned = sanitize_prose(text);

        assert!(cleaned.contains("before"));
        assert!(cleaned.contains("after"));
        assert!(!cleaned.contains("base64,AAAA"));
        assert!(cleaned.contains("inline-data-url omitted"));
    }

    #[test]
    fn long_opaque_runs_are_replaced_but_normal_text_survives() {
        let payload = "A".repeat(PROSE_RUN_LIMIT + 10);
        let cleaned = sanitize_prose(&format!("key={payload} done"));

        assert!(cleaned.contains("key="));
        assert!(cleaned.contains("done"));
        assert!(cleaned.contains("payload omitted chars="));
        assert!(!cleaned.contains(&"A".repeat(PROSE_RUN_LIMIT)));

        let ordinary = "This sentence has normal words, numbers 12345 and punctuation.";
        assert_eq!(sanitize_prose(ordinary), ordinary);
    }

    #[test]
    fn code_keeps_tokens_shorter_than_its_own_limit() {
        // A realistic long-but-legitimate token stays in a code block.
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9".repeat(8); // 288 chars
        assert!(token.chars().count() < CODE_RUN_LIMIT);
        let code = format!("const token = \"{token}\";");

        assert_eq!(
            sanitize_block_text(&code, BlockKind::Code),
            code,
            "code must keep long tokens it can legitimately contain"
        );

        let longer = "QUJDRA".repeat(150); // 900 chars: past the prose rule
        assert!(
            sanitize_block_text(&format!("token={longer}"), BlockKind::Prose)
                .contains("payload omitted"),
            "the same kind of value is a payload in flowing text"
        );
        assert!(
            sanitize_block_text(&format!("token={longer}"), BlockKind::Code).contains(&longer),
            "code keeps values below its own limit"
        );
    }

    #[test]
    fn sanitizing_twice_changes_nothing_more() {
        let payload = "QUJD".repeat(400);
        let once = sanitize_prose(&format!("x {payload} y"));
        let twice = sanitize_prose(&once);

        assert_eq!(once, twice);
    }

    #[test]
    fn value_walk_reports_whether_it_had_to_act() {
        let (value, changed) = sanitize_value(json!({
            "ok": true,
            "evidence": [{ "excerpt": "a data:text/plain;base64,AAAA b" }]
        }));

        assert!(changed);
        assert!(!value.to_string().contains("base64,AAAA"));

        let (_, clean_changed) =
            sanitize_value(json!({ "ok": true, "url": "https://example.com/" }));
        assert!(!clean_changed);
    }
}
