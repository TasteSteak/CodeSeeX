use codeseex_core::context::redact_inline_data_urls;
use encoding_rs::{Encoding, GB18030, UTF_8, WINDOWS_1252};
use regex::Regex;
use std::sync::OnceLock;

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

pub(super) fn strip_html_tags(value: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for ch in value.chars() {
        match ch {
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    clean_visible_text(&text)
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
        "copy" => Some('©'),
        "reg" => Some('®'),
        "trade" => Some('™'),
        "hellip" => Some('…'),
        "mdash" => Some('—'),
        "ndash" => Some('–'),
        "minus" => Some('−'),
        "laquo" => Some('«'),
        "raquo" => Some('»'),
        "lsaquo" => Some('‹'),
        "rsaquo" => Some('›'),
        "ldquo" => Some('“'),
        "rdquo" => Some('”'),
        "lsquo" => Some('‘'),
        "rsquo" => Some('’'),
        "middot" => Some('·'),
        "bull" => Some('•'),
        "times" => Some('×'),
        "deg" => Some('°'),
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

pub(super) fn bytes_have_binary_markers(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let sample = &bytes[..bytes.len().min(4096)];
    sample.starts_with(b"%PDF") || sample.contains(&0)
}

pub(super) fn decode_text_bytes(bytes: &[u8], content_type: &str) -> (String, &'static str, bool) {
    if let Some(encoding) = charset_encoding(content_type) {
        let (text, _, had_errors) = encoding.decode(bytes);
        return (text.into_owned(), encoding.name(), had_errors);
    }

    let (text, _, had_errors) = UTF_8.decode(bytes);
    if !had_errors {
        return (text.into_owned(), UTF_8.name(), false);
    }

    let (text, _, had_errors) = GB18030.decode(bytes);
    if text_is_plausible(&text) {
        return (text.into_owned(), GB18030.name(), had_errors);
    }

    let (text, _, had_errors) = WINDOWS_1252.decode(bytes);
    (text.into_owned(), WINDOWS_1252.name(), had_errors)
}

fn charset_encoding(content_type: &str) -> Option<&'static Encoding> {
    content_type
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("charset="))
        .map(|value| value.trim_matches(['"', '\'']).trim())
        .filter(|value| !value.is_empty())
        .and_then(|label| Encoding::for_label(label.as_bytes()))
}

fn text_is_plausible(text: &str) -> bool {
    let sample = text.chars().take(512);
    let mut meaningful = 0_usize;
    let mut replacement = 0_usize;
    for ch in sample {
        if ch == '\u{fffd}' {
            replacement += 1;
        } else if !ch.is_control() || ch.is_whitespace() {
            meaningful += 1;
        }
    }
    meaningful > replacement.saturating_mul(4)
}

pub(super) fn is_textual_content_type(content_type: &str) -> bool {
    let content_type = content_type.to_ascii_lowercase();
    if content_type.contains("text/css")
        || content_type.contains("javascript")
        || content_type.contains("font/")
        || content_type.contains("image/")
        || content_type.contains("audio/")
        || content_type.contains("video/")
        || content_type.contains("pdf")
        || content_type.contains("octet-stream")
    {
        return false;
    }
    content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("html")
}

pub(super) fn response_looks_like_html(content_type: &str, text: &str) -> bool {
    let content_type = content_type.to_ascii_lowercase();
    if content_type.contains("html") {
        return true;
    }
    let sample = text.trim_start().chars().take(4096).collect::<String>();
    let sample = sample.to_ascii_lowercase();
    sample.starts_with("<!doctype html")
        || sample.starts_with("<html")
        || sample.contains("<body")
        || sample.contains("<script")
        || sample.contains("<style")
}

pub(super) fn response_looks_like_markdown(content_type: &str, url: &str) -> bool {
    let content_type = content_type.to_ascii_lowercase();
    if content_type.contains("markdown") || content_type.contains("mdtext") {
        return true;
    }
    let url = url.to_ascii_lowercase();
    url.ends_with(".md") || url.ends_with(".markdown") || url.ends_with(".mdown")
}

pub(super) fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let after_open = lower[start..].find('>')? + start + 1;
    let end = lower[after_open..].find("</title>")? + after_open;
    let title = compact_whitespace(&html[after_open..end]);
    (!title.is_empty()).then(|| truncate_chars(&clean_visible_text(&title), 240))
}

pub(super) fn extract_markdown_title(markdown: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        let trimmed = line.trim();
        if !trimmed.starts_with('#') {
            return None;
        }
        let title = trimmed.trim_start_matches('#').trim();
        (!title.is_empty()).then(|| truncate_chars(&clean_visible_text(title), 240))
    })
}

pub(super) fn html_to_text(html: &str) -> String {
    let source = extract_preferred_html_region(html).unwrap_or(html);
    let mut cleaned = redact_inline_data_urls(source);
    for tag in [
        "script", "style", "noscript", "svg", "canvas", "picture", "video", "audio", "iframe",
        "object", "embed", "nav", "header", "footer", "form", "dialog",
    ] {
        cleaned = remove_html_block(&cleaned, tag);
    }
    let mut text = String::new();
    let mut in_tag = false;
    for ch in cleaned.chars() {
        match ch {
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    clean_visible_text(&text)
}

pub(super) fn markdown_to_text(markdown: &str) -> String {
    let mut text = redact_inline_data_urls(markdown)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    for tag in [
        "script", "style", "noscript", "svg", "canvas", "picture", "video", "audio", "iframe",
        "object", "embed",
    ] {
        text = remove_html_block(&text, tag);
    }
    text = html_comment_re().replace_all(&text, " ").into_owned();
    text = html_void_resource_tag_re()
        .replace_all(&text, " ")
        .into_owned();
    text = markdown_inline_image_re()
        .replace_all(&text, " ")
        .into_owned();
    text = markdown_reference_image_re()
        .replace_all(&text, " ")
        .into_owned();
    text = markdown_link_re()
        .replace_all(&text, |caps: &regex::Captures<'_>| {
            caps.get(1)
                .map(|value| value.as_str())
                .unwrap_or_default()
                .to_owned()
        })
        .into_owned();

    let mut output = Vec::new();
    let mut in_code_fence = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            output.push(trimmed.to_owned());
            continue;
        }
        if in_code_fence {
            output.push(line.trim_end().to_owned());
            continue;
        }
        if markdown_reference_definition_re().is_match(trimmed) {
            continue;
        }
        let stripped = html_tag_re().replace_all(trimmed, " ");
        let stripped = clean_visible_text(stripped.as_ref());
        if stripped.is_empty() {
            if output
                .last()
                .is_some_and(|value: &String| !value.is_empty())
            {
                output.push(String::new());
            }
            continue;
        }
        output.push(stripped);
    }
    while output.last().is_some_and(|value| value.is_empty()) {
        output.pop();
    }
    output.join("\n")
}

fn html_comment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<!--.*?-->").expect("valid html comment regex"))
}

fn html_void_resource_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?is)<\s*(img|source|meta|link|br)\b[^>]*>")
            .expect("valid html void resource regex")
    })
}

fn html_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)</?[^>\n]{1,300}>").expect("valid html tag regex"))
}

fn markdown_inline_image_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"!\[[^\]\n]*\]\([^\)\n]*\)").expect("valid markdown inline image regex")
    })
}

fn markdown_reference_image_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"!\[[^\]\n]*\]\[[^\]\n]*\]").expect("valid markdown reference image regex")
    })
}

fn markdown_link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[([^\]\n]+)\]\([^\)\n]+\)").expect("valid markdown link regex"))
}

fn markdown_reference_definition_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"^\[[^\]]+\]:\s+\S+"#).expect("valid markdown reference definition regex")
    })
}

fn extract_preferred_html_region(html: &str) -> Option<&str> {
    for tag in ["main", "article"] {
        if let Some(region) = first_html_block(html, tag) {
            return Some(region);
        }
    }
    None
}

fn first_html_block<'a>(html: &'a str, tag: &str) -> Option<&'a str> {
    let lower = html.to_ascii_lowercase();
    let open_prefix = format!("<{tag}");
    let close_tag = format!("</{tag}>");
    let start = lower.find(&open_prefix)?;
    let after_open = lower[start..].find('>')? + start + 1;
    let end = lower[after_open..].find(&close_tag)? + after_open;
    html.get(after_open..end)
}

fn remove_html_block(html: &str, tag: &str) -> String {
    let mut output = html.to_owned();
    let open_prefix = format!("<{tag}");
    let close_tag = format!("</{tag}>");
    loop {
        let lower = output.to_ascii_lowercase();
        let Some(start) = lower.find(&open_prefix) else {
            break;
        };
        let Some(relative_open_end) = lower[start..].find('>') else {
            output.truncate(start);
            break;
        };
        let after_open = start + relative_open_end + 1;
        if let Some(relative_end) = lower[after_open..].find(&close_tag) {
            let end = after_open + relative_end + tag.len() + 3;
            output.replace_range(start..end, " ");
        } else if matches!(tag, "script" | "style") {
            output.truncate(start);
            break;
        } else {
            output.replace_range(start..after_open, " ");
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_html_even_without_html_content_type() {
        let html = "<html><head><script>window.noise = true;</script></head><body>VISIBLE_TEXT</body></html>";
        assert!(response_looks_like_html("text/plain", html));
        let text = html_to_text(html);
        assert!(text.contains("VISIBLE_TEXT"));
        assert!(!text.contains("window.noise"));
    }

    #[test]
    fn removes_resource_noise() {
        let html = r#"
            <html><head>
              <style>body { color: red; }</style>
              <script>window.secret = "noise";</script>
            </head>
            <body>
              <svg><text>SVG_NOISE</text></svg>
              <img src="data:image/png;base64,AAAA" />
              <main>VISIBLE evidence text.</main>
            </body></html>
        "#;
        let text = html_to_text(html);

        assert!(text.contains("VISIBLE evidence text."));
        assert!(!text.contains("window.secret"));
        assert!(!text.contains("SVG_NOISE"));
        assert!(!text.contains("base64"));
    }

    #[test]
    fn cleans_numeric_entities_and_invisible_token_noise() {
        let text = "Python&nbsp;3.14&#8212;docs&#x2014;&amp;#187;\u{200b}\u{feff} end";

        assert_eq!(clean_visible_text(text), "Python 3.14—docs—» end");
    }

    #[test]
    fn html_to_text_decodes_nested_entities() {
        let html = "<main>Docs &amp;#8212; API &rsquo;reference&rsquo;</main>";

        assert_eq!(html_to_text(html), "Docs — API ’reference’");
    }

    #[test]
    fn truncated_unclosed_style_does_not_leak_css_as_text() {
        let html = r#"
            <html>
              <head><title>Example</title></head>
              <body>
                Intro text before a truncated style block.
                <style>@layer ads { .ad-slot{display:block}.promo{font-size:12px}
        "#;

        let text = html_to_text(html);

        assert!(text.contains("Intro text"));
        assert!(!text.contains("@layer"));
        assert!(!text.contains("ad-slot"));
    }

    #[test]
    fn malformed_resource_tag_does_not_drop_following_body() {
        let html = r#"
            <html><body>
              <svg viewBox="0 0 1 1" />
              <main>VISIBLE_TEXT_AFTER_RESOURCE</main>
            </body></html>
        "#;

        let text = html_to_text(html);

        assert!(text.contains("VISIBLE_TEXT_AFTER_RESOURCE"));
        assert!(!text.contains("viewBox"));
    }

    #[test]
    fn prefers_semantic_page_body() {
        let html = r#"
            <html>
              <body>
                <header>HEADER_NAV_NOISE</header>
                <main>
                  <nav>LOCAL_NAV_NOISE</nav>
                  <article>Primary documentation content.</article>
                </main>
                <footer>FOOTER_NOISE</footer>
              </body>
            </html>
        "#;
        let text = html_to_text(html);

        assert!(text.contains("Primary documentation content."));
        assert!(!text.contains("HEADER_NAV_NOISE"));
        assert!(!text.contains("LOCAL_NAV_NOISE"));
        assert!(!text.contains("FOOTER_NOISE"));
    }

    #[test]
    fn decodes_gb18030_textual_html() {
        let (bytes, _, _) = GB18030.encode("<html><body>上海天气</body></html>");
        let (text, encoding, had_errors) = decode_text_bytes(&bytes, "text/html; charset=gb18030");

        assert_eq!(encoding, "gb18030");
        assert!(!had_errors);
        assert!(text.contains("上海天气"));
    }

    #[test]
    fn binary_markers_do_not_treat_legacy_chinese_text_as_binary() {
        let (bytes, _, _) = GB18030.encode("上海天气");

        assert!(!bytes_have_binary_markers(&bytes));
    }

    #[test]
    fn detects_markdown_from_file_extension() {
        assert!(response_looks_like_markdown(
            "text/plain; charset=utf-8",
            "https://example.com/docs/README.md"
        ));
    }

    #[test]
    fn cleans_markdown_resource_noise() {
        let markdown = r#"
            <div align="center">
            <picture>
              <source media="(prefers-color-scheme: dark)" srcset="data:image/svg+xml;base64,AAAA">
              <img alt="Logo" src="https://example.test/logo.png">
            </picture>
            </div>

            # Rust

            ![badge](https://example.test/badge.svg)

            Rust is a language empowering everyone to build reliable software.

            [Install Rust](https://www.rust-lang.org/tools/install)

            ```rust
            fn main() {
                println!("hello");
            }
            ```
        "#;

        let text = markdown_to_text(markdown);

        assert_eq!(extract_markdown_title(markdown).as_deref(), Some("Rust"));
        assert!(text.contains("# Rust"));
        assert!(text.contains("Rust is a language"));
        assert!(text.contains("Install Rust"));
        assert!(text.contains("fn main()"));
        assert!(!text.contains("<picture"));
        assert!(!text.contains("<source"));
        assert!(!text.contains("<img"));
        assert!(!text.contains("base64"));
        assert!(!text.contains("badge.svg"));
    }

    #[test]
    fn markdown_cleanup_preserves_code_block_angle_brackets() {
        let markdown = r#"
            # Example

            ```rust
            fn parse<T>(value: T) -> T {
                value
            }
            ```
        "#;

        let text = markdown_to_text(markdown);

        assert!(text.contains("parse<T>"));
    }
}
