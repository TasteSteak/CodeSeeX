//! Shapes the summary CodeSeeX mirrors for Codex's thinking chain.
//!
//! Codex renders only the summary, never the provider's own reasoning text, so
//! CodeSeeX mirrors one into the other. The provider text itself is never
//! touched: every projection here is a prefix of it, which is what lets the
//! request boundary drop the mirrored summary again before a replay reaches the
//! provider. Nothing is added, reordered or paraphrased.

use codeseex_core::config::ReasoningSummaryMode;

/// Streams one summary for one reasoning item.
///
/// Deltas are emitted at sentence or line boundaries so the text the client has
/// already received always stays a prefix of the final projection; the tail is
/// held back until a boundary arrives or the item completes. `Full` has no
/// budget, so it mirrors every delta as it arrives.
#[derive(Debug)]
pub(crate) struct SummaryProjector {
    mode: ReasoningSummaryMode,
    text: String,
    emitted: usize,
    finished: bool,
}

impl SummaryProjector {
    pub(crate) fn new(mode: ReasoningSummaryMode) -> Self {
        Self {
            mode,
            text: String::new(),
            emitted: 0,
            finished: false,
        }
    }

    /// Appends a provider delta and returns whatever became safe to emit.
    pub(crate) fn push(&mut self, delta: &str) -> Option<String> {
        if self.finished {
            return None;
        }
        self.text.push_str(delta);
        if self.mode != ReasoningSummaryMode::Full && !touches_boundary(delta) {
            // A projection only ever grows at a sentence or line boundary, so a
            // delta without one cannot move it. Skipping the rescan keeps long
            // reasoning linear instead of quadratic in the accumulated text.
            return None;
        }
        self.emit_up_to(streamable_len(&self.text, self.mode))
    }

    /// Completes the projection and returns the held-back tail, if any.
    pub(crate) fn finish(&mut self) -> Option<String> {
        if self.finished {
            return None;
        }
        self.finished = true;
        let target = project(&self.text, self.mode).len();
        self.emit_up_to(target)
    }

    /// The summary as the client has received it so far.
    pub(crate) fn summary(&self) -> &str {
        &self.text[..self.emitted]
    }

    fn emit_up_to(&mut self, target: usize) -> Option<String> {
        if target <= self.emitted {
            return None;
        }
        let chunk = self.text[self.emitted..target].to_owned();
        self.emitted = target;
        Some(chunk)
    }
}

/// The complete summary for a finished piece of reasoning text.
pub(crate) fn project(text: &str, mode: ReasoningSummaryMode) -> &str {
    match mode {
        ReasoningSummaryMode::None => "",
        ReasoningSummaryMode::Full => text,
        ReasoningSummaryMode::Fixed => trim_to_budget(text, mode),
        ReasoningSummaryMode::Smart => trim_to_budget(smart_prefix(text), mode),
    }
}

/// How far the streaming copy may run ahead of itself.
///
/// Everything below the last complete sentence or line is held back, so the
/// client never receives text the final projection could drop. `Full` mirrors
/// the text as it arrives.
fn streamable_len(text: &str, mode: ReasoningSummaryMode) -> usize {
    if mode == ReasoningSummaryMode::None {
        return 0;
    }
    if mode == ReasoningSummaryMode::Full {
        return text.len();
    }
    last_boundary_within(text, project(text, mode).len()).unwrap_or(0)
}

fn trim_to_budget(text: &str, mode: ReasoningSummaryMode) -> &str {
    let Some(budget) = mode.budget() else {
        return text;
    };
    if text.len() <= budget {
        return text;
    }
    match last_boundary_within(text, budget) {
        Some(end) => &text[..end],
        None => &text[..floor_char_boundary(text, budget)],
    }
}

/// The opening point of the reasoning: its first paragraph, extended past a
/// lead-in line that introduces a list so the list itself is not cut away.
fn smart_prefix(text: &str) -> &str {
    let paragraph_end = first_paragraph_end(text);
    if opens_list(&text[..paragraph_end]) {
        return &text[..trim_line_end(text, extend_list(text, paragraph_end))];
    }
    &text[..paragraph_end]
}

fn first_paragraph_end(text: &str) -> usize {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() {
            return trim_line_end(text, offset);
        }
        offset += line.len();
    }
    trim_line_end(text, text.len())
}

/// Drops the line break that ended the paragraph, so the summary does not carry
/// a dangling newline.
fn trim_line_end(text: &str, mut end: usize) -> usize {
    while end > 0 {
        let Some(previous) = text[..end].chars().next_back() else {
            break;
        };
        if previous == '\n' || previous == '\r' {
            end -= previous.len_utf8();
        } else {
            break;
        }
    }
    end
}

fn opens_list(paragraph: &str) -> bool {
    let trimmed = paragraph.trim_end();
    trimmed.ends_with(':') || trimmed.ends_with('\u{ff1a}')
}

fn extend_list(text: &str, from: usize) -> usize {
    let mut end = from;
    let mut seen_item = false;
    for line in text[from..].split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if seen_item {
                break;
            }
            end += line.len();
            continue;
        }
        if !is_list_item(trimmed) {
            break;
        }
        seen_item = true;
        end += line.len();
    }
    if seen_item {
        end
    } else {
        from
    }
}

fn is_list_item(trimmed: &str) -> bool {
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return true;
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    let rest = &trimmed[digits..];
    rest.starts_with(". ") || rest.starts_with(") ")
}

/// The end offset of the last sentence or line that finishes at or before
/// `limit`.
fn last_boundary_within(text: &str, limit: usize) -> Option<usize> {
    let mut boundary = None;
    let mut characters = text.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        let end = index + character.len_utf8();
        if end > limit {
            break;
        }
        let followed_by_space = characters
            .peek()
            .map(|(_, next)| next.is_whitespace())
            .unwrap_or(true);
        if is_boundary(character, followed_by_space) {
            boundary = Some(end);
        }
    }
    boundary
}

fn is_boundary(character: char, followed_by_space: bool) -> bool {
    match character {
        '\n' | '!' | '?' | '\u{3002}' | '\u{ff01}' | '\u{ff1f}' => true,
        '.' => followed_by_space,
        _ => false,
    }
}

/// Whether a delta can create or move a boundary. `is_boundary` treats a `.` at
/// the end of a delta as followed by whitespace, so it counts here too.
fn touches_boundary(delta: &str) -> bool {
    delta.chars().any(|character| {
        matches!(
            character,
            '\n' | '.' | '!' | '?' | '\u{3002}' | '\u{ff01}' | '\u{ff1f}'
        )
    })
}

/// The largest character boundary at or below `limit`.
fn floor_char_boundary(text: &str, limit: usize) -> usize {
    if limit >= text.len() {
        return text.len();
    }
    if text.is_char_boundary(limit) {
        return limit;
    }
    let mut index = limit;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(value: &str) -> ReasoningSummaryMode {
        codeseex_core::config::parse_reasoning_summary_mode(value).expect("known mode")
    }

    /// A delta without a boundary cannot move the projection, so it is skipped -
    /// and nothing is lost: the next boundary still carries the held-back tail.
    #[test]
    fn a_delta_without_a_boundary_emits_nothing_and_loses_nothing() {
        let mut projector = SummaryProjector::new(mode("smart"));

        assert_eq!(projector.push("first sentence"), None);
        assert_eq!(projector.summary(), "");
        assert_eq!(
            projector.push(". second"),
            Some("first sentence.".to_owned())
        );
        assert_eq!(projector.summary(), "first sentence.");
        assert_eq!(
            projector.push(" sentence."),
            Some(" second sentence.".to_owned())
        );
        assert_eq!(projector.summary(), "first sentence. second sentence.");
    }

    #[test]
    fn full_mirrors_everything() {
        let text = "First paragraph.\n\nSecond paragraph that keeps going.";
        assert_eq!(project(text, mode("full")), text);
        assert_eq!(project("short", mode("full")), "short");
    }

    #[test]
    fn none_mirrors_nothing() {
        let mut projector = SummaryProjector::new(mode("none"));
        assert_eq!(projector.push("nothing should show."), None);
        assert_eq!(projector.finish(), None);
        assert_eq!(projector.summary(), "");
    }

    #[test]
    fn fixed_cuts_back_to_the_last_sentence_inside_the_budget() {
        let text = format!(
            "{}. {}. {}",
            "a".repeat(150),
            "b".repeat(150),
            "c".repeat(200)
        );
        let projected = project(&text, mode("fixed"));
        assert!(projected.ends_with("b."), "{projected}");
        assert!(projected.len() <= 400, "{}", projected.len());
        assert!(text.starts_with(projected));
    }

    #[test]
    fn fixed_without_a_boundary_cuts_on_a_character_boundary() {
        let text = "字".repeat(500);
        let projected = project(&text, mode("fixed"));
        assert!(projected.chars().count() <= 400);
        assert!(text.starts_with(projected));
    }

    #[test]
    fn smart_keeps_the_opening_paragraph() {
        let text = "The user wants a summary.\n\nThen a lot of narration follows.";
        assert_eq!(project(text, mode("smart")), "The user wants a summary.");
    }

    #[test]
    fn smart_keeps_a_list_that_the_opening_paragraph_introduces() {
        let text = "Plan:\n\n- first step\n- second step\n\nthen narration";
        let projected = project(text, mode("smart"));
        assert_eq!(projected, "Plan:\n\n- first step\n- second step");
    }

    #[test]
    fn smart_ignores_a_list_that_is_not_introduced() {
        let text = "A plain sentence.\n\n- unrelated list\n";
        assert_eq!(project(text, mode("smart")), "A plain sentence.");
    }

    #[test]
    fn projector_streams_at_boundaries_and_holds_the_tail() {
        let mut projector = SummaryProjector::new(mode("smart"));
        assert_eq!(projector.push("Thinking about"), None);
        assert_eq!(projector.push(" the plan.").as_deref(), Some("Thinking about the plan."));
        assert_eq!(projector.push("\n\nmore narration"), None);
        assert_eq!(projector.finish(), None);
        assert_eq!(projector.summary(), "Thinking about the plan.");
    }

    #[test]
    fn projector_never_emits_text_the_final_projection_drops() {
        let mut projector = SummaryProjector::new(mode("fixed"));
        let mut seen = String::new();
        for chunk in ["word ".repeat(90), "tail".to_owned()] {
            if let Some(emitted) = projector.push(&chunk) {
                seen.push_str(&emitted);
            }
        }
        if let Some(emitted) = projector.finish() {
            seen.push_str(&emitted);
        }
        assert_eq!(seen, projector.summary());
        assert!(
            project(&projector.text, mode("fixed")).starts_with(seen.as_str()),
            "streamed text must be a prefix of the projection"
        );
    }
}
