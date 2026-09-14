//! HTML/plain-text/markdown extraction.
//!
//! The old pipeline located the body with a substring search and stripped tags
//! with a character scanner, which silently dropped content and lost all
//! structure. This version parses a real DOM, produces independent text blocks,
//! scores each block, and reports how much of the page was actually content.

use encoding_rs::{Encoding, GB18030, UTF_8, WINDOWS_1252};
use scraper::node::Node;
use scraper::{ElementRef, Html, Selector};
use std::sync::OnceLock;

use super::text::{char_count, clean_visible_text, truncate_chars};

/// A single readable block of a page.
#[derive(Clone, Debug)]
pub(super) struct TextBlock {
    pub(super) heading_path: String,
    pub(super) text: String,
    pub(super) link_ratio: f64,
    pub(super) weight: f64,
    pub(super) boilerplate: bool,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ExtractionStats {
    pub(super) body_chars: usize,
    pub(super) content_chars: usize,
    pub(super) content_ratio: f64,
    pub(super) link_ratio: f64,
    pub(super) blocks_total: usize,
    pub(super) blocks_content: usize,
}

#[derive(Clone, Debug)]
pub(super) struct ExtractedDocument {
    pub(super) title: Option<String>,
    pub(super) blocks: Vec<TextBlock>,
    pub(super) stats: ExtractionStats,
}

impl ExtractedDocument {
    pub(super) fn is_low_confidence(&self) -> bool {
        self.stats.content_chars < 200 && self.stats.body_chars > 0
            || self.stats.content_ratio < 0.15 && self.stats.body_chars >= 1_500
    }
}

/// Minimal block weights used to separate content from page furniture.
const CONTENT_BLOCK_TAGS: &[&str] = &[
    "p",
    "li",
    "blockquote",
    "pre",
    "figcaption",
    "dd",
    "dt",
    "td",
    "th",
    "caption",
];
const HEADING_TAGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];
/// Tags whose text is emitted as its own block.
const EMIT_TAGS: &[&str] = &[
    "body",
    "main",
    "article",
    "section",
    "div",
    "p",
    "li",
    "blockquote",
    "pre",
    "figcaption",
    "dd",
    "dt",
    "td",
    "th",
    "caption",
    "summary",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
];
/// Subtrees that never carry page content.
const NOISE_TAGS: &[&str] = &[
    "script", "style", "noscript", "svg", "canvas", "template", "iframe", "object", "embed", "nav",
    "aside", "header", "footer", "form", "dialog", "select", "button", "head",
];

pub(super) fn html_to_document(html: &str) -> ExtractedDocument {
    let document = Html::parse_document(&redact_inline_data_urls(html));
    let title = document_title(&document);
    let mut blocks = Vec::new();
    let root = document.root_element();
    let mut headings: Vec<String> = Vec::new();
    walk_element(root, &mut headings, &mut blocks);
    if blocks.is_empty() {
        // A DOM that produced nothing readable still has raw text; fall back to
        // the body's own text so tiny fragments are not silently dropped.
        let fallback = clean_visible_text(&visible_text(root));
        if !fallback.is_empty() {
            blocks.push(TextBlock {
                heading_path: String::new(),
                link_ratio: 0.0,
                weight: 0.5,
                boilerplate: false,
                text: fallback,
            });
        }
    }
    let stats = summarize_blocks(&blocks);
    ExtractedDocument {
        title,
        blocks,
        stats,
    }
}

pub(super) fn plain_text_to_document(text: &str) -> ExtractedDocument {
    let mut blocks = Vec::new();
    for paragraph in text.split("\n\n") {
        let cleaned = clean_visible_text(paragraph);
        if cleaned.is_empty() {
            continue;
        }
        blocks.push(TextBlock {
            heading_path: String::new(),
            link_ratio: 0.0,
            weight: block_weight("p", "", &cleaned, 0.0),
            boilerplate: false,
            text: cleaned,
        });
    }
    let stats = summarize_blocks(&blocks);
    ExtractedDocument {
        title: None,
        blocks,
        stats,
    }
}

pub(super) fn markdown_to_document(markdown: &str) -> ExtractedDocument {
    let mut blocks = Vec::new();
    let mut headings: Vec<String> = Vec::new();
    let mut paragraph = String::new();
    let mut in_code = false;
    for line in markdown.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush_paragraph(&mut paragraph, &mut blocks, &headings);
            in_code = !in_code;
            continue;
        }
        if in_code {
            blocks.push(TextBlock {
                heading_path: headings.join(" > "),
                link_ratio: 0.0,
                weight: 1.0,
                boilerplate: false,
                text: line.trim_end().to_owned(),
            });
            continue;
        }
        if let Some(level) = markdown_heading_level(trimmed) {
            flush_paragraph(&mut paragraph, &mut blocks, &headings);
            let text = clean_visible_text(trimmed.trim_start_matches('#'));
            headings.truncate(level.saturating_sub(1));
            if !text.is_empty() {
                headings.push(text);
            }
            continue;
        }
        if trimmed.is_empty() {
            flush_paragraph(&mut paragraph, &mut blocks, &headings);
            continue;
        }
        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(trimmed);
    }
    flush_paragraph(&mut paragraph, &mut blocks, &headings);
    let stats = summarize_blocks(&blocks);
    ExtractedDocument {
        title: blocks
            .first()
            .map(|block| block.heading_path.clone())
            .filter(|value| !value.is_empty()),
        blocks,
        stats,
    }
}

fn flush_paragraph(paragraph: &mut String, blocks: &mut Vec<TextBlock>, headings: &[String]) {
    let cleaned = clean_visible_text(paragraph);
    paragraph.clear();
    if cleaned.is_empty() {
        return;
    }
    blocks.push(TextBlock {
        heading_path: headings.join(" > "),
        link_ratio: 0.0,
        weight: block_weight("p", "", &cleaned, 0.0),
        boilerplate: false,
        text: cleaned,
    });
}

fn markdown_heading_level(line: &str) -> Option<usize> {
    if !line.starts_with('#') {
        return None;
    }
    let level = line.chars().take_while(|ch| *ch == '#').count();
    (level >= 1 && level <= 6 && line.chars().nth(level) == Some(' ')).then_some(level)
}

fn walk_element(
    element: ElementRef<'_>,
    headings: &mut Vec<String>,
    blocks: &mut Vec<TextBlock>,
) -> bool {
    let tag = element.value().name();
    if NOISE_TAGS.contains(&tag) {
        return false;
    }
    let heading = HEADING_TAGS.contains(&tag);
    let heading_text = if heading {
        clean_visible_text(&element.text().collect::<String>())
    } else {
        String::new()
    };
    if heading && !heading_text.is_empty() {
        headings.push(heading_text);
    }

    let hints = element_hints(element);
    let mut buffer = String::new();
    let mut emitted = false;
    for child in element.children() {
        match child.value() {
            Node::Text(text) => buffer.push_str(&text.text),
            Node::Element(_) => {
                let Some(child_element) = ElementRef::wrap(child.clone()) else {
                    continue;
                };
                if walk_element(child_element, headings, blocks) {
                    push_buffer(&mut buffer, tag, &hints, headings, blocks);
                    emitted = true;
                } else if !NOISE_TAGS.contains(&child_element.value().name()) {
                    // A container that produced no block of its own is folded
                    // into its parent, but never carries noise-subtree text.
                    buffer.push_str(&visible_text(child_element));
                }
            }
            _ => {}
        }
    }

    let has_text = !buffer.trim().is_empty();
    let is_emit_point = EMIT_TAGS.contains(&tag) || heading;
    if has_text && (is_emit_point || emitted) {
        let link_ratio = element_link_ratio(element);
        push_text_block(buffer, tag, &hints, link_ratio, headings, blocks);
        emitted = true;
    }

    if heading {
        headings.pop();
    }
    emitted
}

fn push_buffer(
    buffer: &mut String,
    tag: &str,
    hints: &ElementHints,
    headings: &[String],
    blocks: &mut Vec<TextBlock>,
) {
    let text = std::mem::take(buffer);
    if text.trim().is_empty() {
        return;
    }
    push_text_block(text, tag, hints, 0.0, headings, blocks);
}

fn push_text_block(
    text: String,
    tag: &str,
    hints: &ElementHints,
    link_ratio: f64,
    headings: &[String],
    blocks: &mut Vec<TextBlock>,
) {
    let text = clean_visible_text(&text);
    if text.is_empty() {
        return;
    }
    let boilerplate = hints.boilerplate;
    blocks.push(TextBlock {
        heading_path: headings.join(" > "),
        weight: if boilerplate {
            0.0
        } else {
            block_weight(tag, &hints.keywords, &text, link_ratio)
        },
        link_ratio,
        boilerplate,
        text,
    });
}

#[derive(Default)]
struct ElementHints {
    keywords: String,
    boilerplate: bool,
}

fn element_hints(element: ElementRef<'_>) -> ElementHints {
    let mut keywords = String::new();
    for attribute in ["class", "id", "role", "aria-label"] {
        if let Some(value) = element.value().attr(attribute) {
            if !keywords.is_empty() {
                keywords.push(' ');
            }
            keywords.push_str(value);
        }
    }
    let lower = keywords.to_ascii_lowercase();
    let boilerplate = !lower.is_empty()
        && [
            "nav",
            "menu",
            "sidebar",
            "comment",
            "related",
            "promo",
            "advert",
            "cookie",
            "breadcrumb",
            "pagination",
            "subscribe",
            "login",
            "banner",
            "social",
            "toc",
            "share",
            "footer",
            "header",
            "masthead",
            "disclaimer",
            "newsletter",
        ]
        .iter()
        .any(|token| lower.contains(token));
    ElementHints {
        keywords: lower,
        boilerplate,
    }
}

fn element_link_ratio(element: ElementRef<'_>) -> f64 {
    let total = char_count(&element.text().collect::<String>());
    if total == 0 {
        return 0.0;
    }
    let Ok(selector) = Selector::parse("a") else {
        return 0.0;
    };
    let linked = element
        .select(&selector)
        .map(|anchor| char_count(&anchor.text().collect::<String>()))
        .sum::<usize>();
    (linked as f64 / total as f64).clamp(0.0, 1.0)
}

/// Text of a subtree with noise elements removed.
fn visible_text(element: ElementRef<'_>) -> String {
    let mut output = String::new();
    collect_visible_text(element, &mut output);
    output
}

fn collect_visible_text(element: ElementRef<'_>, output: &mut String) {
    for child in element.children() {
        match child.value() {
            Node::Text(text) => output.push_str(&text.text),
            Node::Element(value) => {
                if NOISE_TAGS.contains(&value.name()) {
                    continue;
                }
                if let Some(child_element) = ElementRef::wrap(child.clone()) {
                    collect_visible_text(child_element, output);
                }
            }
            _ => {}
        }
    }
}

fn block_weight(tag: &str, hints: &str, text: &str, link_ratio: f64) -> f64 {
    let chars = char_count(text);
    let mut weight: f64 = 1.0;
    if CONTENT_BLOCK_TAGS.contains(&tag) {
        weight += 0.3;
    }
    if HEADING_TAGS.contains(&tag) {
        weight += 0.2;
    }
    weight += match chars {
        0..=24 => -0.3,
        25..=79 => 0.0,
        80..=199 => 0.25,
        _ => 0.5,
    };
    if link_ratio > 0.6 {
        weight -= 0.6;
    } else if link_ratio > 0.4 {
        weight -= 0.3;
    }
    if !hints.is_empty()
        && [
            "article",
            "content",
            "body",
            "post",
            "entry",
            "prose",
            "markdown",
            "readme",
            "documentation",
            "doc",
            "main",
        ]
        .iter()
        .any(|token| hints.contains(token))
    {
        weight += 0.4;
    }
    weight.clamp(0.0, 2.0)
}

/// Keeps only blocks whose weight clears the content bar, in page order.
fn summarize_blocks(blocks: &[TextBlock]) -> ExtractionStats {
    let body_chars = blocks
        .iter()
        .map(|block| char_count(&block.text))
        .sum::<usize>();
    let content_blocks = blocks
        .iter()
        .filter(|block| !block.boilerplate && block.weight >= 0.7)
        .collect::<Vec<_>>();
    let content_chars = content_blocks
        .iter()
        .map(|block| char_count(&block.text))
        .sum::<usize>();
    let linked_chars = blocks
        .iter()
        .map(|block| (char_count(&block.text) as f64 * block.link_ratio) as usize)
        .sum::<usize>();
    ExtractionStats {
        body_chars,
        content_chars,
        content_ratio: if body_chars == 0 {
            0.0
        } else {
            (content_chars as f64 / body_chars as f64).clamp(0.0, 1.0)
        },
        link_ratio: if body_chars == 0 {
            0.0
        } else {
            (linked_chars as f64 / body_chars as f64).clamp(0.0, 1.0)
        },
        blocks_total: blocks.len(),
        blocks_content: content_blocks.len(),
    }
}

/// One block chosen for the evidence payload.
#[derive(Clone, Debug)]
pub(super) struct SelectedBlock {
    pub(super) heading_path: String,
    pub(super) text: String,
    /// Number of page blocks skipped between this block and the previous one.
    pub(super) skipped_blocks: usize,
    pub(super) skipped_chars: usize,
}

#[derive(Clone, Debug)]
pub(super) struct Selection {
    pub(super) blocks: Vec<SelectedBlock>,
    pub(super) kept_chars: usize,
    pub(super) omitted_chars: usize,
    pub(super) omitted_blocks: usize,
}

/// Chooses a bounded, information-dense slice of the page.
///
/// Instead of truncating the head of the text, the opening blocks, the highest
/// scoring blocks, and a tail sample compete for a fixed character budget, and
/// every gap is reported explicitly.
pub(super) fn select_blocks(
    document: &ExtractedDocument,
    query_terms: &[String],
    budget: usize,
) -> Selection {
    let blocks = &document.blocks;
    if blocks.is_empty() {
        return Selection {
            blocks: Vec::new(),
            kept_chars: 0,
            omitted_chars: 0,
            omitted_blocks: 0,
        };
    }
    let scored = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            let hits = query_terms
                .iter()
                .filter(|term| block.text.to_lowercase().contains(term.as_str()))
                .count();
            (index, block.weight * (1.0 + 2.0 * hits as f64))
        })
        .collect::<Vec<_>>();
    let mut order = scored
        .iter()
        .take(2)
        .map(|(index, _)| *index)
        .collect::<Vec<_>>();
    let mut by_score = scored.clone();
    by_score.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (index, _) in by_score {
        if !order.contains(&index) {
            order.push(index);
        }
    }

    let mut chosen: Vec<(usize, usize)> = Vec::new();
    let mut used = 0usize;
    for index in order {
        if used >= budget {
            break;
        }
        let block = &blocks[index];
        if block.boilerplate || block.weight < 0.5 {
            continue;
        }
        let length = char_count(&block.text);
        let remaining = budget - used;
        if length > remaining {
            // Keep a partial slice rather than dropping a long, relevant block.
            chosen.push((index, remaining));
            used = budget;
            continue;
        }
        chosen.push((index, length));
        used += length;
    }
    chosen.sort_by_key(|(index, _)| *index);

    let mut selected = Vec::with_capacity(chosen.len());
    let mut previous: Option<usize> = None;
    let mut omitted_blocks = 0usize;
    let mut kept_chars = 0usize;
    for (index, keep) in chosen {
        let block = &blocks[index];
        let (skipped_blocks, skipped_chars) = match previous {
            Some(previous) => {
                let skipped = &blocks[previous + 1..index];
                (
                    skipped.len(),
                    skipped
                        .iter()
                        .map(|item| char_count(&item.text))
                        .sum::<usize>(),
                )
            }
            None => (
                index,
                blocks[..index]
                    .iter()
                    .map(|item| char_count(&item.text))
                    .sum(),
            ),
        };
        omitted_blocks += skipped_blocks;
        previous = Some(index);
        let text = if keep >= char_count(&block.text) {
            block.text.clone()
        } else {
            truncate_chars(&block.text, keep)
        };
        kept_chars += char_count(&text);
        selected.push(SelectedBlock {
            heading_path: block.heading_path.clone(),
            text,
            skipped_blocks,
            skipped_chars,
        });
    }
    let total_chars = char_count(
        &blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join(""),
    );
    Selection {
        blocks: selected,
        kept_chars,
        omitted_chars: total_chars.saturating_sub(kept_chars),
        omitted_blocks,
    }
}

fn document_title(document: &Html) -> Option<String> {
    static TITLE: OnceLock<Option<Selector>> = OnceLock::new();
    let selector = TITLE
        .get_or_init(|| Selector::parse("title").ok())
        .as_ref()?;
    document
        .select(selector)
        .next()
        .map(|element| clean_visible_text(&element.text().collect::<String>()))
        .filter(|value| !value.is_empty())
        .map(|value| truncate_chars(&value, 240))
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

pub(super) fn redact_inline_data_urls(text: &str) -> String {
    codeseex_core::context::redact_inline_data_urls(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_body_text_and_drops_page_furniture() {
        let html = r#"
            <html><head><title>Example</title>
            <style>body { color: red; }</style><script>window.noise = 1;</script>
            </head><body>
              <header>HEADER_NAV_NOISE</header>
              <nav>LOCAL_NAV_NOISE</nav>
              <main><article><p>Primary documentation content.</p></article></main>
              <footer>FOOTER_NOISE</footer>
            </body></html>"#;
        let document = html_to_document(html);
        let text = document
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(document.title.as_deref(), Some("Example"));
        assert!(text.contains("Primary documentation content."));
        assert!(!text.contains("HEADER_NAV_NOISE"));
        assert!(!text.contains("LOCAL_NAV_NOISE"));
        assert!(!text.contains("FOOTER_NOISE"));
        assert!(!text.contains("window.noise"));
    }

    #[test]
    fn preserves_angle_brackets_inside_code() {
        let html = "<html><body><p>Compare a &lt; b and x &lt;&lt; 2.</p></body></html>";
        let document = html_to_document(html);
        let text = document
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("a < b"), "{text}");
        assert!(text.contains("x << 2"), "{text}");
    }

    #[test]
    fn inline_links_stay_inside_their_paragraph() {
        let html =
            r#"<html><body><p>Read the <a href="/docs">install guide</a> first.</p></body></html>"#;
        let document = html_to_document(html);

        assert_eq!(document.blocks.len(), 1);
        assert!(document.blocks[0].text.contains("install guide"));
    }

    #[test]
    fn reports_low_content_ratio_for_navigation_heavy_pages() {
        let items = (0..40)
            .map(|index| format!("<li><a href=\"/p/{index}\">Menu item {index}</a></li>"))
            .collect::<String>();
        let html = format!("<html><body><ul class=\"menu\">{items}</ul><p>short</p></body></html>");
        let document = html_to_document(&html);

        assert!(document.is_low_confidence(), "{:?}", document.stats);
    }

    #[test]
    fn budgeted_selection_reports_omissions_instead_of_truncating_the_head() {
        let mut html = String::from("<html><body>");
        for index in 0..30 {
            html.push_str(&format!("<p>Paragraph number {index} with some text.</p>"));
        }
        html.push_str("</body></html>");
        let document = html_to_document(&html);
        let selection = select_blocks(&document, &[], 200);

        assert!(selection.kept_chars <= 260);
        assert!(selection.omitted_chars > 0);
        assert!(selection.blocks.len() > 1);
    }

    #[test]
    fn decodes_gb18030_textual_html() {
        let (bytes, _, _) = GB18030.encode("<html><body>Shanghai weather</body></html>");
        let (text, encoding, had_errors) = decode_text_bytes(&bytes, "text/html; charset=gb18030");

        assert_eq!(encoding, "gb18030");
        assert!(!had_errors);
        assert!(text.contains("Shanghai weather"));
    }

    #[test]
    fn detects_markdown_from_file_extension() {
        assert!(response_looks_like_markdown(
            "text/plain; charset=utf-8",
            "https://example.com/docs/README.md"
        ));
    }

    #[test]
    fn markdown_headings_become_block_paths() {
        let document =
            markdown_to_document("# Rust\n\nRust is a language.\n\n## Install\n\nUse rustup.");
        let paths = document
            .blocks
            .iter()
            .map(|block| block.heading_path.clone())
            .collect::<Vec<_>>();

        assert!(paths.iter().any(|path| path == "Rust"));
        assert!(paths.iter().any(|path| path == "Rust > Install"));
    }
}
