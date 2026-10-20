use egui::{Color32, FontId, TextFormat};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::ops::Range;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Span {
    pub text: String,
    pub strong: bool,
    pub emphasis: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub link: Option<String>,
}

impl Span {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            strong: false,
            emphasis: false,
            strikethrough: false,
            code: false,
            link: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MarkdownBlock {
    Paragraph(Vec<Span>),
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Code {
        language: Option<String>,
        text: String,
    },
    Quote(Vec<Span>),
    Table(Vec<Vec<String>>),
    Rule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceRangeSegment {
    /// Byte range in the flattened text-event stream produced by pulldown-cmark.
    pub rendered: Range<usize>,
    /// UTF-8 byte range in the original markdown source.
    pub source: Range<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SourceRangeMap {
    pub segments: Vec<SourceRangeSegment>,
    pub rendered_len: usize,
}

impl SourceRangeMap {
    fn append(&mut self, rendered_text: &str, source: Range<usize>) {
        if rendered_text.is_empty() {
            return;
        }
        let start = self.rendered_len;
        self.rendered_len = self.rendered_len.saturating_add(rendered_text.len());
        self.segments.push(SourceRangeSegment {
            rendered: start..self.rendered_len,
            source,
        });
    }

    /// Map a selected byte range in the flattened rendered text back to source
    /// byte ranges. Entity decoding and other length-changing transformations
    /// map to the whole originating source token.
    pub(super) fn source_ranges_for_selection(&self, selection: Range<usize>) -> Vec<Range<usize>> {
        self.segments
            .iter()
            .filter_map(|segment| {
                let start = selection.start.max(segment.rendered.start);
                let end = selection.end.min(segment.rendered.end);
                if start >= end {
                    return None;
                }
                if segment.rendered.len() == segment.source.len() {
                    Some(
                        (segment.source.start + start - segment.rendered.start)
                            ..(segment.source.start + end - segment.rendered.start),
                    )
                } else {
                    Some(segment.source.clone())
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedMarkdown {
    pub blocks: Vec<MarkdownBlock>,
    pub source_ranges: SourceRangeMap,
}

#[derive(Default)]
struct InlineState {
    strong: bool,
    emphasis: bool,
    strikethrough: bool,
    code: bool,
    link: Option<String>,
}

/// Parse markdown into a compact, display-oriented model. Original source is
/// retained by the caller and shown in fallback panels for unknown ACP blocks.
pub(super) fn parse_markdown(source: &str) -> Vec<MarkdownBlock> {
    parse_markdown_with_source_ranges(source).blocks
}

pub(super) fn parse_markdown_with_source_ranges(source: &str) -> ParsedMarkdown {
    let parser = Parser::new_ext(
        source,
        Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_MATH,
    );
    let mut blocks = Vec::new();
    let mut current: Vec<Span> = Vec::new();
    let mut block_kind = BlockKind::Paragraph;
    let mut inline = InlineState::default();
    let mut code: Option<(Option<String>, String)> = None;
    let mut table: Option<Vec<Vec<String>>> = None;
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut in_cell = false;
    let mut quote_depth = 0usize;
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut pending_list_prefix: Option<String> = None;
    let mut source_ranges = SourceRangeMap::default();

    let flush = |blocks: &mut Vec<MarkdownBlock>, current: &mut Vec<Span>, kind: &BlockKind| {
        if current.iter().all(|s| s.text.trim().is_empty()) {
            current.clear();
            return;
        }
        let value = std::mem::take(current);
        blocks.push(match kind {
            BlockKind::Paragraph => MarkdownBlock::Paragraph(value),
            BlockKind::Heading(level) => MarkdownBlock::Heading {
                level: *level,
                spans: value,
            },
            BlockKind::Quote => MarkdownBlock::Quote(value),
        });
    };

    for (event, source_range) in parser.into_offset_iter() {
        match &event {
            Event::Text(text)
            | Event::Code(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => source_ranges.append(text.as_ref(), source_range.clone()),
            Event::SoftBreak | Event::HardBreak => source_ranges.append("\n", source_range.clone()),
            Event::TaskListMarker(checked) => {
                source_ranges.append(if *checked { "☑ " } else { "☐ " }, source_range.clone())
            }
            Event::FootnoteReference(name) => {
                source_ranges.append(&format!("[^{name}]"), source_range.clone())
            }
            _ => {}
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    flush(&mut blocks, &mut current, &block_kind);
                    block_kind = if quote_depth > 0 {
                        BlockKind::Quote
                    } else {
                        BlockKind::Paragraph
                    };
                    if let Some(prefix) = pending_list_prefix.take() {
                        current.push(Span::plain(prefix));
                    }
                }
                Tag::Heading { level, .. } => {
                    flush(&mut blocks, &mut current, &block_kind);
                    block_kind = BlockKind::Heading(heading_number(level));
                }
                Tag::BlockQuote(_) => {
                    flush(&mut blocks, &mut current, &block_kind);
                    quote_depth += 1;
                    block_kind = BlockKind::Quote;
                }
                Tag::List(first) => list_stack.push(first),
                Tag::Item => {
                    pending_list_prefix = Some(match list_stack.last_mut() {
                        Some(Some(next)) => {
                            let label = format!("{next}. ");
                            *next += 1;
                            label
                        }
                        _ => "• ".to_owned(),
                    });
                }
                Tag::CodeBlock(kind) => {
                    flush(&mut blocks, &mut current, &block_kind);
                    let language = match kind {
                        CodeBlockKind::Fenced(s) if !s.is_empty() => Some(s.to_string()),
                        _ => None,
                    };
                    code = Some((language, String::new()));
                }
                Tag::Table(_) => {
                    flush(&mut blocks, &mut current, &block_kind);
                    table = Some(Vec::new());
                }
                Tag::TableHead | Tag::TableRow => row.clear(),
                Tag::TableCell => {
                    cell.clear();
                    in_cell = true;
                }
                Tag::Strong => inline.strong = true,
                Tag::Emphasis => inline.emphasis = true,
                Tag::Strikethrough => inline.strikethrough = true,
                Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                    inline.link = Some(dest_url.to_string())
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) => {
                    flush(&mut blocks, &mut current, &block_kind)
                }
                TagEnd::Item => current.push(Span::plain("\n")),
                TagEnd::List(_) => {
                    list_stack.pop();
                }
                TagEnd::BlockQuote(_) => {
                    flush(&mut blocks, &mut current, &block_kind);
                    quote_depth = quote_depth.saturating_sub(1);
                    block_kind = if quote_depth > 0 {
                        BlockKind::Quote
                    } else {
                        BlockKind::Paragraph
                    };
                }
                TagEnd::CodeBlock => {
                    if let Some((language, text)) = code.take() {
                        blocks.push(MarkdownBlock::Code { language, text });
                    }
                }
                TagEnd::TableHead | TagEnd::TableRow => {
                    if let Some(rows) = table.as_mut() {
                        if !row.is_empty() {
                            rows.push(std::mem::take(&mut row));
                        }
                    }
                }
                TagEnd::TableCell => {
                    if in_cell {
                        row.push(std::mem::take(&mut cell));
                        in_cell = false;
                    }
                }
                TagEnd::Table => {
                    if let Some(rows) = table.take() {
                        blocks.push(MarkdownBlock::Table(rows));
                    }
                }
                TagEnd::Strong => inline.strong = false,
                TagEnd::Emphasis => inline.emphasis = false,
                TagEnd::Strikethrough => inline.strikethrough = false,
                TagEnd::Link | TagEnd::Image => inline.link = None,
                _ => {}
            },
            Event::Text(text) => {
                if let Some((_, body)) = code.as_mut() {
                    body.push_str(&text);
                } else if in_cell {
                    cell.push_str(&text);
                } else {
                    if let Some(prefix) = pending_list_prefix.take() {
                        current.push(Span::plain(prefix));
                    }
                    current.push(Span {
                        text: text.into_string(),
                        strong: inline.strong,
                        emphasis: inline.emphasis,
                        strikethrough: inline.strikethrough,
                        code: inline.code,
                        link: inline.link.clone(),
                    });
                }
            }
            Event::Code(text) => {
                if in_cell {
                    cell.push_str(&text);
                } else {
                    current.push(Span {
                        text: text.into_string(),
                        strong: inline.strong,
                        emphasis: inline.emphasis,
                        strikethrough: inline.strikethrough,
                        code: true,
                        link: inline.link.clone(),
                    });
                }
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                if in_cell {
                    cell.push_str(&text);
                } else if code.is_none() {
                    current.push(Span::plain(text.into_string()));
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, body)) = code.as_mut() {
                    body.push('\n');
                } else if in_cell {
                    cell.push(' ');
                } else {
                    current.push(Span::plain("\n"));
                }
            }
            Event::Rule => blocks.push(MarkdownBlock::Rule),
            Event::TaskListMarker(checked) => {
                current.push(Span::plain(if checked { "☑ " } else { "☐ " }))
            }
            Event::FootnoteReference(name) => current.push(Span::plain(format!("[^{name}]"))),
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                current.push(Span::plain(math.into_string()))
            }
        }
    }
    flush(&mut blocks, &mut current, &block_kind);
    ParsedMarkdown {
        blocks,
        source_ranges,
    }
}

#[derive(Clone, Copy)]
enum BlockKind {
    Paragraph,
    Heading(u8),
    Quote,
}

fn heading_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

pub(super) fn highlighted_job(
    source: &str,
    language: &str,
    fallback: Color32,
    dark: bool,
) -> egui::text::LayoutJob {
    let syntax_set = SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines);
    let theme_set = THEME_SET.get_or_init(ThemeSet::load_defaults);
    let theme_name = if dark {
        "base16-ocean.dark"
    } else {
        "InspiredGitHub"
    };
    let theme = theme_set
        .themes
        .get(theme_name)
        .or_else(|| theme_set.themes.values().next());
    let mut job = egui::text::LayoutJob::default();
    let Some(theme) = theme else {
        job.append(
            source,
            0.0,
            TextFormat {
                font_id: FontId::monospace(13.0),
                color: fallback,
                ..Default::default()
            },
        );
        return job;
    };
    let syntax = syntax_set
        .find_syntax_by_token(language)
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, theme);
    for line in LinesWithEndings::from(source) {
        match highlighter.highlight_line(line, &syntax_set) {
            Ok(ranges) => {
                for (style, text) in ranges {
                    let color = style.foreground;
                    job.append(
                        text,
                        0.0,
                        TextFormat {
                            font_id: FontId::monospace(13.0),
                            color: Color32::from_rgba_unmultiplied(
                                color.r, color.g, color.b, color.a,
                            ),
                            ..Default::default()
                        },
                    );
                }
            }
            Err(_) => job.append(
                line,
                0.0,
                TextFormat {
                    font_id: FontId::monospace(13.0),
                    color: fallback,
                    ..Default::default()
                },
            ),
        }
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_keeps_inline_formatting_links_code_and_unicode() {
        let parsed = parse_markdown(
            "A **bold** *soft* ~~gone~~ `x` [file](workspace://src/main.rs) 🐉\n\n```rust\nfn main() {}\n```",
        );
        assert!(
            matches!(&parsed[0], MarkdownBlock::Paragraph(spans) if spans.iter().any(|s| s.strong) && spans.iter().any(|s| s.emphasis) && spans.iter().any(|s| s.strikethrough) && spans.iter().any(|s| s.code) && spans.iter().any(|s| s.link.as_deref()==Some("workspace://src/main.rs")) && spans.iter().any(|s| s.text.contains("🐉")))
        );
        assert!(
            matches!(&parsed[1], MarkdownBlock::Code { language: Some(language), text } if language == "rust" && text.contains("fn main"))
        );
    }

    #[test]
    fn gfm_tables_are_retained_as_rows_and_cells() {
        let parsed = parse_markdown("| Name | Value |\n| --- | --- |\n| dragon | 🐉 |");
        assert!(
            matches!(&parsed[0], MarkdownBlock::Table(rows) if rows.len() == 2 && rows[1][1] == "🐉")
        );
    }

    #[test]
    fn quote_and_ordered_list_structure_is_retained() {
        let parsed = parse_markdown("> quoted\n\n3. third\n4. fourth");
        assert!(parsed.iter().any(|block| matches!(block, MarkdownBlock::Quote(spans) if spans.iter().any(|s| s.text.contains("quoted")))));
        let list_text = parsed
            .iter()
            .filter_map(|block| match block {
                MarkdownBlock::Paragraph(spans) => Some(
                    spans
                        .iter()
                        .map(|span| span.text.as_str())
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect::<String>();
        assert!(list_text.contains("3. ") && list_text.contains("4. "));
    }

    #[test]
    fn source_ranges_cover_formatted_code_quote_table_and_unicode_text() {
        let source = "A **strong 🐉** phrase.\n\n```rust\nlet x = 1;\n```\n\n> quoted 🐉 text\n\n| name | value |\n| --- | --- |\n| dragon 🐉 | done |";
        let parsed = parse_markdown_with_source_ranges(source);

        for expected in ["strong 🐉", "let x = 1;", "quoted 🐉 text", "dragon 🐉"] {
            let segment = parsed
                .source_ranges
                .segments
                .iter()
                .find(|segment| source[segment.source.clone()].contains(expected))
                .unwrap_or_else(|| panic!("no source range maps rendered text {expected:?}"));
            let mapped = parsed
                .source_ranges
                .source_ranges_for_selection(segment.rendered.clone());
            assert!(mapped.contains(&segment.source));
            assert!(source[mapped[0].clone()].contains(expected));
        }

        let unicode_segment = parsed
            .source_ranges
            .segments
            .iter()
            .find(|segment| {
                segment.rendered.len() == segment.source.len()
                    && source[segment.source.clone()].contains("strong 🐉")
            })
            .unwrap();
        let emoji = "🐉";
        let emoji_start = unicode_segment.rendered.start
            + source[unicode_segment.source.clone()]
                .find(emoji)
                .expect("strong text keeps its Unicode spelling");
        let emoji_source = parsed
            .source_ranges
            .source_ranges_for_selection(emoji_start..emoji_start + emoji.len());
        assert_eq!(emoji_source.len(), 1);
        assert_eq!(&source[emoji_source[0].clone()], emoji);

        assert!(matches!(
            parsed.blocks.iter().find(|block| matches!(block, MarkdownBlock::Code { .. })),
            Some(MarkdownBlock::Code { language: Some(language), text })
                if language == "rust" && text.contains("let x = 1;")
        ));
        assert!(parsed.blocks.iter().any(|block| matches!(
            block,
            MarkdownBlock::Quote(spans) if spans.iter().any(|span| span.text.contains("quoted 🐉"))
        )));
        assert!(parsed.blocks.iter().any(|block| matches!(
            block,
            MarkdownBlock::Table(rows) if rows.iter().flatten().any(|cell| cell.contains("dragon 🐉"))
        )));
    }
}
