//! Markdown-to-terminal presentation for model-generated conversation text.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::{style::{Modifier, Style}, text::{Line, Span}};

use crate::theme::THEME;

const TEXT_STYLE: Style = Style::new().fg(THEME.text);
const HEADING_STYLE: Style = Style::new().fg(THEME.accent).add_modifier(Modifier::BOLD);
const MUTED_STYLE: Style = Style::new().fg(THEME.muted);
const CODE_STYLE: Style = Style::new().fg(THEME.text).bg(THEME.surface);
const INLINE_CODE_STYLE: Style = Style::new().fg(THEME.brand).bg(THEME.surface);
const LINK_STYLE: Style = Style::new().fg(THEME.info).add_modifier(Modifier::UNDERLINED);

/// Renders model-authored Markdown as safe ratatui lines.
///
/// The source is parsed on every call so partial streaming buffers are safe;
/// no presentation state is persisted with the conversation.
pub(super) fn render_markdown(source: &str) -> Vec<Line<'static>> {
    let mut renderer = Renderer::default();
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(source, options) {
        renderer.event(event);
    }
    renderer.finish()
}

pub(super) fn render_literal(source: &str) -> Vec<Line<'static>> {
    let mut lines = source
        .split('\n')
        .map(|line| Line::raw(sanitize(line)))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(Line::raw(""));
    }
    lines
}

#[derive(Default)]
struct Renderer {
    lines: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    heading: bool,
    emphasis: bool,
    strong: bool,
    in_code_block: bool,
    list_depth: usize,
    ordered_lists: Vec<u64>,
    link_url: Option<String>,
}

impl Renderer {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(text) => self.push_span(text.as_ref(), INLINE_CODE_STYLE),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.finish_line(),
            Event::Rule => {
                self.finish_line();
                self.push_span("────────────────", MUTED_STYLE);
                self.finish_line();
            }
            Event::Html(html) | Event::InlineHtml(html) => self.text(&html),
            Event::FootnoteReference(reference) => {
                self.push_text("[");
                self.push_text(reference.as_ref());
                self.push_text("]");
            }
            Event::TaskListMarker(checked) => {
                self.push_text(if checked { "[x] " } else { "[ ] " });
            }
            Event::InlineMath(value) | Event::DisplayMath(value) => self.text(&value),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { .. } => {
                self.finish_line();
                self.heading = true;
                self.push_text("  ");
            }
            Tag::Strong => self.strong = true,
            Tag::Emphasis => self.emphasis = true,
            Tag::CodeBlock(kind) => {
                self.finish_line();
                self.in_code_block = true;
                let label = match kind {
                    CodeBlockKind::Fenced(language) if !language.is_empty() => {
                        format!("  ┌─ {}", sanitize(&language))
                    }
                    _ => "  ┌─ code".to_owned(),
                };
                self.push_span(&label, CODE_STYLE);
                self.finish_line();
            }
            Tag::List(start) => {
                self.list_depth += 1;
                self.ordered_lists.push(start.unwrap_or(0));
            }
            Tag::Item => {
                self.finish_line();
                let indent = "  ".repeat(self.list_depth.saturating_sub(1));
                let marker = match self.ordered_lists.last_mut() {
                    Some(next) if *next > 0 => {
                        let marker = format!("{indent}{next}. ");
                        *next = next.saturating_add(1);
                        marker
                    }
                    _ => format!("{indent}• "),
                };
                self.push_span(&marker, TEXT_STYLE);
            }
            Tag::BlockQuote(_) => {
                self.finish_line();
                self.push_span("> ", MUTED_STYLE);
            }
            Tag::Link { dest_url, .. } => {
                self.link_url = Some(sanitize(&dest_url));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.heading = false;
                self.finish_line();
                self.blank_line();
            }
            TagEnd::Strong => self.strong = false,
            TagEnd::Emphasis => self.emphasis = false,
            TagEnd::CodeBlock => {
                if !self.spans.is_empty() {
                    self.finish_line();
                }
                self.push_span("  └─", CODE_STYLE);
                self.finish_line();
                self.in_code_block = false;
            }
            TagEnd::Item => self.finish_line(),
            TagEnd::List(_) => {
                self.finish_line();
                self.list_depth = self.list_depth.saturating_sub(1);
                self.ordered_lists.pop();
                if self.list_depth == 0 {
                    self.blank_line();
                }
            }
            TagEnd::BlockQuote(_) => {
                self.finish_line();
                self.blank_line();
            }
            TagEnd::Link => {
                if let Some(url) = self.link_url.take() {
                    self.push_span(&format!(" ({url})"), MUTED_STYLE);
                }
            }
            TagEnd::Paragraph => {
                self.finish_line();
                if self.list_depth == 0 {
                    self.blank_line();
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, value: &str) {
        let sanitized = sanitize(value);
        if self.in_code_block {
            for (index, line) in sanitized.split('\n').enumerate() {
                if index > 0 {
                    self.finish_line();
                }
                self.push_span(&format!("  │ {line}"), CODE_STYLE);
            }
        } else {
            for (index, line) in sanitized.split('\n').enumerate() {
                if index > 0 {
                    self.finish_line();
                }
                if !line.is_empty() {
                    self.push_text(line);
                }
            }
        }
    }

    fn push_text(&mut self, value: &str) {
        let base_style = if self.link_url.is_some() {
            LINK_STYLE
        } else {
            TEXT_STYLE
        };
        let style = if self.heading {
            HEADING_STYLE
        } else if self.strong && self.emphasis {
            base_style.add_modifier(Modifier::BOLD | Modifier::ITALIC)
        } else if self.strong {
            base_style.add_modifier(Modifier::BOLD)
        } else if self.emphasis {
            base_style.add_modifier(Modifier::ITALIC)
        } else {
            base_style
        };
        self.push_span(value, style);
    }

    fn push_span(&mut self, value: &str, style: Style) {
        if !value.is_empty() {
            self.spans.push(Span::styled(value.to_owned(), style));
        }
    }

    fn finish_line(&mut self) {
        if !self.spans.is_empty() {
            self.lines.push(Line::from(std::mem::take(&mut self.spans)));
        }
    }

    fn blank_line(&mut self) {
        if !self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.push(Line::raw(""));
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.finish_line();
        while self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.pop();
        }
        if self.lines.is_empty() {
            self.lines.push(Line::raw(""));
        }
        self.lines
    }
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            !character.is_control() || matches!(*character, '\t' | '\n')
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{render_literal, render_markdown};

    fn plain(lines: &[ratatui::text::Line<'static>]) -> String {
        lines.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn renders_common_markdown_without_source_tokens() {
        let rendered = plain(&render_markdown(
            "# Plan\n\n**bold** *italic* `code`\n\n- one\n- two\n\n[OpenRouter](https://openrouter.ai)",
        ));
        assert!(rendered.contains("Plan"));
        assert!(rendered.contains("bold"));
        assert!(rendered.contains("• one"));
        assert!(rendered.contains("OpenRouter (https://openrouter.ai)"));
        assert!(!rendered.contains("**"));
        assert!(!rendered.contains("]("));
    }

    #[test]
    fn preserves_code_structure_and_streaming_fragments() {
        let rendered = plain(&render_markdown("```rust\nfn main() {\n\tprintln!(\"hi\");\n}"));
        assert!(rendered.contains("┌─ rust"));
        assert!(rendered.contains("│ fn main() {"));
        assert!(rendered.contains("println!"));
        assert!(rendered.contains("└─"));
        assert!(plain(&render_markdown("**bold wor")).contains("bold wor"));
        assert!(plain(&render_markdown("```rust\nfn main() {")).contains("fn main()"));
    }

    #[test]
    fn sanitizes_terminal_controls_and_handles_nested_lists() {
        let rendered = plain(&render_markdown("- outer\n  - inner\n\n> quoted\n\ntext\u{1b}[2J"));
        assert!(rendered.contains("• outer"));
        assert!(rendered.contains("  • inner"));
        assert!(rendered.contains("> quoted"));
        assert!(!rendered.contains('\u{1b}'));
    }

    #[test]
    fn preserves_source_and_degrades_malformed_markdown() {
        let source = "**Frontend**\n\n| name | value |\n| --- | --- |\n| 🧭 | ok |";
        let original = source.to_owned();
        let rendered = plain(&render_markdown(source));
        assert_eq!(source, original);
        assert!(rendered.contains("Frontend"));
        assert!(rendered.contains("name"));
        assert!(rendered.contains("🧭"));
    }

    #[test]
    fn keeps_long_lines_and_unicode_readable_for_narrow_layouts() {
        let source = "🧭 ".repeat(512);
        let rendered = plain(&render_markdown(&source));
        assert!(rendered.contains("🧭"));
        assert!(rendered.len() >= source.len() - 2);
    }

    #[test]
    fn literal_rendering_is_safe_for_user_messages() {
        let rendered = plain(&render_literal("requirements\u{1b}[2J\n- keep this literal"));
        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.contains("- keep this literal"));
    }
}
