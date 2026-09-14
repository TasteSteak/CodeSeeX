//! Pure text helpers shared by the search pipeline.

pub(super) fn compact_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn clean_visible_text(text: &str) -> String {
    compact_whitespace(&remove_token_noise(&decode_basic_html_entities(text)))
}

pub(super) fn truncate_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let prefix = text.chars().take(max_chars).collect::<String>();
    format!("{prefix}...[truncated chars={count}]")
}

pub(super) fn char_count(text: &str) -> usize {
    text.chars().count()
}

pub(super) fn decode_basic_html_entities(text: &str) -> String {
    let first = decode_html_entities_once(text);
    let second = decode_html_entities_once(&first);
    if second == first {
        first
    } else {
        second
    }
}

fn decode_html_entities_once(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch != '&' {
            output.push(ch);
            continue;
        }
        let Some(relative_end) = text[index..].find(';') else {
            output.push(ch);
            continue;
        };
        let end = index + relative_end;
        let entity = &text[index + 1..end];
        if entity.is_empty() || entity.len() > 32 || entity.chars().any(char::is_whitespace) {
            output.push(ch);
            continue;
        }
        if let Some(decoded) = decode_html_entity(entity) {
            output.push(decoded);
            while chars.peek().is_some_and(|(next, _)| *next <= end) {
                chars.next();
            }
        } else {
            output.push(ch);
        }
    }
    output
}

fn decode_html_entity(entity: &str) -> Option<char> {
    let lower = entity.to_ascii_lowercase();
    match lower.as_str() {
        "nbsp" | "ensp" | "emsp" | "thinsp" => Some(' '),
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "copy" => Some('\u{00a9}'),
        "reg" => Some('\u{00ae}'),
        "trade" => Some('\u{2122}'),
        "hellip" => Some('\u{2026}'),
        "mdash" => Some('\u{2014}'),
        "ndash" => Some('\u{2013}'),
        "minus" => Some('\u{2212}'),
        "laquo" => Some('\u{00ab}'),
        "raquo" => Some('\u{00bb}'),
        "lsaquo" => Some('\u{2039}'),
        "rsaquo" => Some('\u{203a}'),
        "ldquo" => Some('\u{201c}'),
        "rdquo" => Some('\u{201d}'),
        "lsquo" => Some('\u{2018}'),
        "rsquo" => Some('\u{2019}'),
        "middot" => Some('\u{00b7}'),
        "bull" => Some('\u{2022}'),
        "times" => Some('\u{00d7}'),
        "deg" => Some('\u{00b0}'),
        _ => decode_numeric_html_entity(&lower),
    }
}

fn decode_numeric_html_entity(entity: &str) -> Option<char> {
    let value = if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        u32::from_str_radix(hex, 16).ok()?
    } else if let Some(decimal) = entity.strip_prefix('#') {
        decimal.parse::<u32>().ok()?
    } else {
        return None;
    };
    char::from_u32(value)
}

fn remove_token_noise(text: &str) -> String {
    text.chars()
        .filter_map(|ch| match ch {
            '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}' => Some(' '),
            '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}' => None,
            _ if ch.is_control() && !ch.is_whitespace() => None,
            _ => Some(ch),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_numeric_entities_and_invisible_token_noise() {
        let text = "Python&nbsp;3.14&#8212;docs&#x2014;&amp;#187;\u{200b}\u{feff} end";

        assert_eq!(
            clean_visible_text(text),
            "Python 3.14\u{2014}docs\u{2014}\u{00bb} end"
        );
    }

    #[test]
    fn truncation_reports_the_original_length() {
        let text = "x".repeat(50);
        let cut = truncate_chars(&text, 10);

        assert!(cut.starts_with(&"x".repeat(10)));
        assert!(cut.contains("truncated chars=50"));
    }
}
