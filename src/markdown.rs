//! Markdown, read into blocks and drawn as a formatted page.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    div, font, img, prelude::*, px, relative, AnyElement, Font, FontStyle, FontWeight,
    HighlightStyle, Hsla, SharedString, StrikethroughStyle, StyledText, TextRun, UnderlineStyle,
};
use pulldown_cmark::{CodeBlockKind, Event, OffsetIter, Options, Parser, Tag, TagEnd};

use crate::assets::icon;
use crate::selection::{self, Place, Selection};
use crate::theme;

/// Colors for part of a code block, as byte ranges of its text.
pub type CodeColors = Vec<(Range<usize>, Hsla)>;

#[derive(Debug, Default, PartialEq)]
pub struct Document {
    pub blocks: Vec<Block>,
    /// The line of the source that each block begins on, counting from 0.
    pub lines: Vec<usize>,
}

#[derive(Debug, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        text: Inline,
    },
    Paragraph(Inline),
    Code {
        text: SharedString,
        colors: CodeColors,
    },
    Quote(Vec<Block>),
    List {
        /// The number of the first item, for a numbered list.
        start: Option<u64>,
        items: Vec<Item>,
    },
    Table {
        head: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
    },
    Image {
        source: String,
        alt: String,
    },
    Rule,
}

#[derive(Debug, PartialEq)]
pub struct Item {
    /// Whether a task-list item is ticked; `None` for an ordinary item.
    pub checked: Option<bool>,
    pub blocks: Vec<Block>,
}

/// A run of text and the parts of it that are styled.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inline {
    pub text: SharedString,
    /// In order, never overlapping. Text outside them is plain.
    pub spans: Vec<Span>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: Style,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<Arc<str>>,
}

impl Inline {
    #[cfg(test)]
    fn plain(text: &str) -> Self {
        Inline {
            text: text.to_string().into(),
            spans: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Collects the text of a paragraph, heading or table cell.
#[derive(Default)]
struct InlineBuilder {
    text: String,
    spans: Vec<Span>,
    /// The style in force, innermost last.
    styles: Vec<Style>,
    /// Where each picture's description sits in the text, and its address.
    images: Vec<(Range<usize>, String)>,
    open_images: Vec<(usize, String)>,
}

impl InlineBuilder {
    fn style(&self) -> Style {
        self.styles.last().cloned().unwrap_or_default()
    }

    fn open(&mut self, change: impl FnOnce(&mut Style)) {
        let mut style = self.style();
        change(&mut style);
        self.styles.push(style);
    }

    fn close(&mut self) {
        self.styles.pop();
    }

    fn open_image(&mut self, source: &str) {
        self.open_images.push((self.text.len(), source.to_string()));
        self.open(|style| style.link = Some(source.into()));
    }

    fn close_image(&mut self) {
        let Some((start, source)) = self.open_images.pop() else {
            return;
        };
        if self.text.len() == start {
            // A picture with no description still needs something to show.
            let name = source.rsplit('/').next().unwrap_or(&source).to_string();
            self.push(if name.is_empty() { "image" } else { &name });
        }
        self.close();
        self.images.push((start..self.text.len(), source));
    }

    fn push(&mut self, text: &str) {
        self.push_styled(text, self.style());
    }

    fn push_code(&mut self, text: &str) {
        let mut style = self.style();
        style.code = true;
        self.push_styled(text, style);
    }

    fn push_styled(&mut self, text: &str, style: Style) {
        // A paragraph never starts with the space a line break turned into.
        let text = if self.text.is_empty() {
            text.trim_start()
        } else {
            text
        };
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        if style == Style::default() {
            return;
        }
        match self.spans.last_mut() {
            Some(last) if last.range.end == start && last.style == style => {
                last.range.end = self.text.len()
            }
            _ => self.spans.push(Span {
                range: start..self.text.len(),
                style,
            }),
        }
    }

    /// Tags written straight into the text: only a line break means anything.
    fn push_html(&mut self, html: &str) {
        let tag = html.trim().to_ascii_lowercase();
        if tag.starts_with("<br") {
            self.push("\n");
        }
    }

    /// The block this text makes, if it holds anything.
    fn finish(mut self) -> Option<Block> {
        let kept = self.text.trim_end().len();
        self.text.truncate(kept);
        if self.text.is_empty() {
            return None;
        }
        self.spans.retain_mut(|span| {
            span.range.end = span.range.end.min(kept);
            span.range.start < span.range.end
        });
        // A picture on a line of its own is shown as a picture.
        if let [(range, source)] = self.images.as_slice() {
            if *range == (0..kept) {
                return Some(Block::Image {
                    source: source.clone(),
                    alt: self.text,
                });
            }
        }
        Some(Block::Paragraph(Inline {
            text: self.text.into(),
            spans: self.spans,
        }))
    }
}

/// What the Markdown says, piece by piece, remembering where the last
/// piece began in the source.
struct Events<'a> {
    inner: OffsetIter<'a>,
    at: usize,
}

impl<'a> Iterator for Events<'a> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Event<'a>> {
        let (event, range) = self.inner.next()?;
        self.at = range.start;
        Some(event)
    }
}

struct Reader<'a, 'h> {
    events: Events<'a>,
    highlight: &'h dyn Fn(&str, &str) -> CodeColors,
    /// The tick box read most recently, until its list item claims it.
    task: Option<bool>,
    /// How many blocks deep the reading is.
    depth: usize,
    /// Where in the source each block of the page began.
    starts: Vec<usize>,
}

/// Reads Markdown. `highlight` colors a code block, given its language.
pub fn parse(source: &str, highlight: &dyn Fn(&str, &str) -> CodeColors) -> Document {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let mut reader = Reader {
        events: Events {
            inner: Parser::new_ext(source, options).into_offset_iter(),
            at: 0,
        },
        highlight,
        task: None,
        depth: 0,
        starts: Vec::new(),
    };
    let mut blocks = Vec::new();
    // A stray closing tag ends a run of blocks early; keep going past it.
    loop {
        blocks.extend(reader.blocks());
        if reader.events.next().is_none() {
            break;
        }
    }
    // The starts come in order, so one pass over the source counts them all.
    let mut lines = Vec::with_capacity(blocks.len());
    let (mut line, mut counted) = (0, 0);
    for start in reader.starts.into_iter().take(blocks.len()) {
        let start = start.clamp(counted, source.len());
        line += source.as_bytes()[counted..start]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count();
        counted = start;
        lines.push(line);
    }
    lines.resize(blocks.len(), line);
    Document { blocks, lines }
}

impl Reader<'_, '_> {
    /// Reads blocks up to the end of whatever holds them.
    fn blocks(&mut self) -> Vec<Block> {
        let mut out = Vec::new();
        let mut inline = InlineBuilder::default();
        let flush = |inline: &mut InlineBuilder, out: &mut Vec<Block>| {
            out.extend(std::mem::take(inline).finish());
        };
        self.depth += 1;
        let already = self.starts.len();
        while let Some(event) = self.events.next() {
            // A block of the page began where the piece that made it did.
            let began = self.events.at;
            if self.depth == 1 {
                self.starts.resize(already + out.len(), began);
            }
            match event {
                Event::Start(tag) => match tag {
                    Tag::Emphasis => inline.open(|style| style.italic = true),
                    Tag::Strong => inline.open(|style| style.bold = true),
                    Tag::Strikethrough => inline.open(|style| style.strike = true),
                    Tag::Superscript | Tag::Subscript => inline.open(|_| {}),
                    Tag::Link { dest_url, .. } => {
                        inline.open(|style| style.link = Some(dest_url.as_ref().into()))
                    }
                    Tag::Image { dest_url, .. } => inline.open_image(&dest_url),
                    block => {
                        flush(&mut inline, &mut out);
                        self.block(block, &mut out);
                    }
                },
                Event::End(end) => match end {
                    TagEnd::Emphasis
                    | TagEnd::Strong
                    | TagEnd::Strikethrough
                    | TagEnd::Superscript
                    | TagEnd::Subscript
                    | TagEnd::Link => inline.close(),
                    TagEnd::Image => inline.close_image(),
                    _ => break,
                },
                Event::Text(text) => inline.push(&text),
                Event::Code(code) | Event::InlineMath(code) | Event::DisplayMath(code) => {
                    inline.push_code(&code)
                }
                Event::SoftBreak => inline.push(" "),
                Event::HardBreak => inline.push("\n"),
                Event::InlineHtml(html) => inline.push_html(&html),
                Event::Html(html) => {
                    flush(&mut inline, &mut out);
                    html_blocks(&html, &mut out);
                }
                Event::FootnoteReference(name) => inline.push(&format!("[{name}]")),
                Event::Rule => {
                    flush(&mut inline, &mut out);
                    out.push(Block::Rule);
                }
                Event::TaskListMarker(checked) => self.task = Some(checked),
            }
            if self.depth == 1 {
                self.starts.resize(already + out.len(), began);
            }
        }
        flush(&mut inline, &mut out);
        if self.depth == 1 {
            let at = self.events.at;
            self.starts.resize(already + out.len(), at);
        }
        self.depth -= 1;
        out
    }

    /// The text of something that holds only text, such as a heading.
    fn inline(&mut self) -> Inline {
        self.blocks()
            .into_iter()
            .find_map(|block| match block {
                Block::Paragraph(inline) => Some(inline),
                Block::Image { alt, .. } => Some(Inline {
                    text: alt.into(),
                    spans: Vec::new(),
                }),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Reads the block `tag` opened, through to its end.
    fn block(&mut self, tag: Tag, out: &mut Vec<Block>) {
        match tag {
            Tag::Heading { level, .. } => out.push(Block::Heading {
                level: level as u8,
                text: self.inline(),
            }),
            Tag::BlockQuote(_) => out.push(Block::Quote(self.blocks())),
            Tag::CodeBlock(kind) => {
                let language = match &kind {
                    CodeBlockKind::Fenced(info) => info
                        .split(|c: char| c.is_whitespace() || c == ',')
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                let text = self.raw_text();
                let text = text.trim_end_matches('\n');
                out.push(Block::Code {
                    colors: (self.highlight)(&language, text),
                    text: text.to_string().into(),
                });
            }
            Tag::List(start) => {
                let mut items = Vec::new();
                loop {
                    match self.events.next() {
                        Some(Event::Start(Tag::Item)) => {
                            // An item's tick box arrives among its own text,
                            // before any list nested inside it.
                            let outer = self.task.take();
                            let blocks = self.blocks();
                            items.push(Item {
                                checked: self.task.take(),
                                blocks,
                            });
                            self.task = outer;
                        }
                        Some(Event::End(_)) | None => break,
                        Some(_) => {}
                    }
                }
                out.push(Block::List { start, items });
            }
            Tag::Table(_) => {
                let mut head = Vec::new();
                let mut rows = Vec::new();
                loop {
                    match self.events.next() {
                        Some(Event::Start(Tag::TableHead)) => head = self.cells(),
                        Some(Event::Start(Tag::TableRow)) => rows.push(self.cells()),
                        Some(Event::End(_)) | None => break,
                        Some(_) => {}
                    }
                }
                out.push(Block::Table { head, rows });
            }
            Tag::HtmlBlock => html_blocks(&self.raw_text(), out),
            // Front matter is for programs, not for reading.
            Tag::MetadataBlock(_) => {
                self.raw_text();
            }
            // Anything else shows as the blocks inside it.
            _ => out.extend(self.blocks()),
        }
    }

    fn cells(&mut self) -> Vec<Inline> {
        let mut cells = Vec::new();
        loop {
            match self.events.next() {
                Some(Event::Start(Tag::TableCell)) => cells.push(self.inline()),
                Some(Event::End(_)) | None => return cells,
                Some(_) => {}
            }
        }
    }

    /// The text of a block that is not Markdown inside, through to its end.
    fn raw_text(&mut self) -> String {
        let mut text = String::new();
        loop {
            match self.events.next() {
                Some(Event::Text(part) | Event::Html(part) | Event::InlineHtml(part)) => {
                    text.push_str(&part)
                }
                Some(Event::End(_)) | None => return text,
                Some(_) => {}
            }
        }
    }
}

/// The value of an attribute in an HTML tag.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
        let start = from + found;
        let end = start + name.len();
        from = end;
        let begins_word = lower[..start]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        let rest = tag[end..].trim_start();
        if !begins_word || !rest.starts_with('=') {
            continue;
        }
        let value = rest[1..].trim_start();
        return Some(match value.chars().next() {
            Some(quote @ ('"' | '\'')) => value[1..].split(quote).next()?.to_string(),
            _ => value
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()?
                .to_string(),
        });
    }
    None
}

/// Shows raw HTML as what a reader would see of it: its pictures and its
/// words, without the tags.
fn html_blocks(html: &str, out: &mut Vec<Block>) {
    fn flush(text: &mut String, out: &mut Vec<Block>) {
        let words = text
            .replace("&nbsp;", " ")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&");
        let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
        if !words.is_empty() {
            out.push(Block::Paragraph(Inline {
                text: words.into(),
                spans: Vec::new(),
            }));
        }
        text.clear();
    }

    let mut text = String::new();
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        rest = &rest[open..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            rest = comment.find("-->").map_or("", |end| &comment[end + 3..]);
            continue;
        }
        let Some(close) = rest.find('>') else {
            rest = "";
            break;
        };
        let tag = &rest[..=close];
        rest = &rest[close + 1..];
        let name = tag[1..]
            .trim_start_matches('/')
            .split(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match name.as_str() {
            "img" => {
                if let Some(source) = attribute(tag, "src") {
                    flush(&mut text, out);
                    out.push(Block::Image {
                        alt: attribute(tag, "alt").unwrap_or_default(),
                        source,
                    });
                }
            }
            // These end a line of words.
            "br" | "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "tr" => {
                flush(&mut text, out)
            }
            _ => text.push(' '),
        }
    }
    text.push_str(rest);
    flush(&mut text, out);
}

/// What stands for a picture where only text will do.
fn image_label<'a>(source: &'a str, alt: &'a str) -> &'a str {
    if alt.is_empty() {
        source
    } else {
        alt
    }
}

/// One of the pieces of text a block is drawn as.
struct Piece<'a> {
    text: &'a str,
    /// What parts it from the piece before, when both are copied.
    gap: &'static str,
    /// The mark a list puts in front of it.
    marker: String,
}

/// Lists the pieces of `blocks` in the order the painter draws them, which
/// is how a selection numbers them. `gap` parts each block from the last;
/// `depth` counts the lists around them.
fn pieces<'a>(blocks: &'a [Block], gap: &'static str, depth: usize, out: &mut Vec<Piece<'a>>) {
    let piece = |text: &'a str, gap| Piece {
        text,
        gap,
        marker: String::new(),
    };
    for block in blocks {
        let first = out.len();
        match block {
            Block::Heading { text, .. } | Block::Paragraph(text) => out.push(piece(&text.text, gap)),
            Block::Code { text, .. } => out.push(piece(text, gap)),
            Block::Image { source, alt } => out.push(piece(image_label(source, alt), gap)),
            Block::Quote(blocks) => pieces(blocks, "\n\n", depth, out),
            Block::List { start, items } => {
                for (index, item) in items.iter().enumerate() {
                    let first = out.len();
                    pieces(&item.blocks, "\n", depth + 1, out);
                    let Some(piece) = out.get_mut(first) else {
                        continue;
                    };
                    let marker = match (item.checked, start) {
                        (Some(true), _) => "- [x] ".to_string(),
                        (Some(false), _) => "- [ ] ".to_string(),
                        (None, Some(first)) => format!("{}. ", first + index as u64),
                        (None, None) => "- ".to_string(),
                    };
                    piece.marker = format!("{}{marker}", "  ".repeat(depth));
                    piece.gap = "\n";
                }
            }
            Block::Table { head, rows } => {
                let filled = (!head.is_empty()).then_some(head);
                for cells in filled.into_iter().chain(rows) {
                    for (column, cell) in cells.iter().enumerate() {
                        out.push(piece(&cell.text, if column == 0 { "\n" } else { "\t" }));
                    }
                }
            }
            Block::Rule => {}
        }
        if let Some(piece) = out.get_mut(first) {
            piece.gap = gap;
        }
    }
}

impl Document {
    /// The lines of the source that block `block` is made from, up to the
    /// start of the next block; `count` is how many lines the source has.
    pub fn lines_of_block(&self, block: usize, count: usize) -> Range<usize> {
        let start = self.lines.get(block).copied().unwrap_or(count).min(count);
        let end = self.lines.get(block + 1).copied().unwrap_or(count);
        start..end.clamp(start, count.max(start))
    }

    /// The block that line `line` of the source is in, or follows.
    pub fn block_at_line(&self, line: usize) -> usize {
        self.lines
            .partition_point(|start| *start <= line)
            .saturating_sub(1)
    }

    /// The text a selection covers, as it would be typed out plainly.
    pub fn text_between(&self, selected: &Range<Place>) -> String {
        let mut out = String::new();
        let mut copied_any = false;
        let rows = self.blocks.iter().enumerate().skip(selected.start.row);
        for (row, block) in rows.take_while(|(row, _)| *row <= selected.end.row) {
            let mut row_pieces = Vec::new();
            pieces(std::slice::from_ref(block), "\n\n", 0, &mut row_pieces);
            for (index, piece) in row_pieces.iter().enumerate() {
                let Some(part) = selection::part(selected, row, index, piece.text) else {
                    continue;
                };
                if copied_any {
                    out.push_str(piece.gap);
                }
                // A list item's mark comes with it when more than that one
                // piece is copied.
                if part.range.start == 0 && (copied_any || part.continues) {
                    out.push_str(&piece.marker);
                }
                copied_any = true;
                out.push_str(&piece.text[part.range]);
            }
        }
        out
    }
}

/// What a link or picture address points at.
#[derive(Debug, PartialEq)]
pub enum Target {
    /// Something for the browser or another program.
    Url(String),
    /// A file on this machine.
    File(PathBuf),
    /// A place within the same page.
    Anchor,
}

/// Works out what `address`, written in a document in `folder`, points at.
pub fn target(address: &str, folder: &Path) -> Target {
    let address = address.trim();
    if address.starts_with('#') || address.is_empty() {
        return Target::Anchor;
    }
    let scheme = address
        .split_once(':')
        .map(|(scheme, _)| scheme)
        .filter(|scheme| {
            // One letter is a Windows drive, not a scheme.
            scheme.len() > 1 && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        });
    if let Some(scheme) = scheme {
        if !scheme.eq_ignore_ascii_case("file") {
            return Target::Url(address.to_string());
        }
    }
    let path = address
        .strip_prefix("file://")
        .unwrap_or(address)
        .split(['#', '?'])
        .next()
        .unwrap_or_default();
    Target::File(folder.join(percent_decoded(path)))
}

fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes
            .get(index + 1..index + 3)
            .filter(|_| bytes[index] == b'%')
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                index += 3;
            }
            None => {
                out.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

const TEXT_SIZE: f32 = 14.;
const CODE_SIZE: f32 = 12.5;
const PAGE_WIDTH: f32 = 820.;
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"];

struct Painter<'a> {
    folder: &'a Path,
    /// Makes the text selectable, and knows what is selected of it.
    selection: &'a Selection,
    /// The row being drawn, and how many pieces of its text have been.
    /// [`pieces`] lists them in the same order.
    row: usize,
    next_piece: usize,
    /// The color of plain text where the painter is.
    color: Hsla,
}

/// Draws one of a document's top-level blocks as a row of the page.
/// `folder` is where the document lives, for the pictures it refers to.
///
/// The page is a list of these rows rather than one tall column: a list
/// gives each row a known width to wrap its text at, and draws only the
/// rows in view.
pub fn render_block(
    document: &Document,
    index: usize,
    folder: &Path,
    selection: &Selection,
) -> AnyElement {
    let Some(block) = document.blocks.get(index) else {
        return div().into_any_element();
    };
    let mut painter = Painter {
        folder,
        selection,
        row: index,
        next_piece: 0,
        color: theme::text().into(),
    };
    div()
        .w_full()
        .px(px(28.))
        .pt(px(if index == 0 { 18. } else { 0. }))
        .pb(px(if index + 1 == document.blocks.len() { 48. } else { 12. }))
        .font_family(theme::UI_FONT)
        .text_size(px(TEXT_SIZE))
        .line_height(relative(1.55))
        .text_color(theme::text())
        .child(
            div()
                .w_full()
                .max_w(px(PAGE_WIDTH))
                .child(painter.block(block)),
        )
        .into_any_element()
}

impl Painter<'_> {
    /// The number of the next piece of text in the row.
    fn piece(&mut self) -> usize {
        self.next_piece += 1;
        self.next_piece - 1
    }

    fn blocks(&mut self, blocks: &[Block]) -> Vec<AnyElement> {
        blocks.iter().map(|block| self.block(block)).collect()
    }

    fn block(&mut self, block: &Block) -> AnyElement {
        match block {
            Block::Heading { level, text } => {
                let size = match level {
                    1 => 25.,
                    2 => 20.,
                    3 => 17.,
                    4 => 15.,
                    _ => TEXT_SIZE,
                };
                div()
                    .w_full()
                    .mt(px(if *level <= 2 { 10. } else { 4. }))
                    .text_size(px(size))
                    .line_height(relative(1.3))
                    .when(*level <= 2, |heading| {
                        heading
                            .pb(px(5.))
                            .border_b_1()
                            .border_color(theme::border())
                    })
                    .child(self.inline(text, FontWeight::BOLD))
                    .into_any_element()
            }
            Block::Paragraph(text) => div()
                .w_full()
                .child(self.inline(text, FontWeight::NORMAL))
                .into_any_element(),
            Block::Code { text, colors } => {
                let piece = self.piece();
                let styled = StyledText::new(text.clone()).with_highlights(
                    colors
                        .iter()
                        .map(|(range, color)| (range.clone(), HighlightStyle::color(*color))),
                );
                div()
                    // Rows are drawn one by one; their boxes must not share numbers.
                    .id(("markdown-code", self.row * 100_000 + piece))
                    .w_full()
                    .overflow_x_scroll()
                    .px(px(12.))
                    .py(px(9.))
                    .rounded(px(6.))
                    .bg(theme::bg_input())
                    .border_1()
                    .border_color(theme::border())
                    .font_family(theme::MONO_FONT)
                    .text_size(px(CODE_SIZE))
                    .line_height(relative(1.5))
                    .whitespace_nowrap()
                    .child(self.selection.text(self.row, piece, text.clone(), styled))
                    .into_any_element()
            }
            Block::Quote(blocks) => {
                let outside = std::mem::replace(&mut self.color, theme::text_muted().into());
                let inside = self.blocks(blocks);
                self.color = outside;
                div()
                    .w_full()
                    .pl(px(14.))
                    .border_l_2()
                    .border_color(theme::border_strong())
                    .text_color(theme::text_muted())
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .children(inside)
                    .into_any_element()
            }
            Block::List { start, items } => {
                let rows: Vec<AnyElement> = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let marker = match (item.checked, start) {
                            (Some(checked), _) => div()
                                .mt(px(4.))
                                .size(px(13.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(3.))
                                .border_1()
                                .border_color(theme::border_strong())
                                .when(checked, |tick| {
                                    tick.bg(theme::accent())
                                        .border_color(theme::accent())
                                        .child(icon("check", px(10.), theme::bg_app()))
                                })
                                .into_any_element(),
                            (None, Some(first)) => {
                                SharedString::from(format!("{}.", first + index as u64))
                                    .into_any_element()
                            }
                            (None, None) => "•".into_any_element(),
                        };
                        // Each part of an item sits in a plain block: text
                        // inside a nested column wraps at the wrong width.
                        let parts = self
                            .blocks(&item.blocks)
                            .into_iter()
                            .enumerate()
                            .map(|(part, block)| {
                                div().when(part > 0, |part| part.mt(px(6.))).child(block)
                            });
                        div()
                            .w_full()
                            .flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_none()
                                    .min_w(px(18.))
                                    .flex()
                                    .justify_end()
                                    .text_color(theme::text_muted())
                                    .child(marker),
                            )
                            .child(div().flex_1().min_w_0().children(parts))
                            .into_any_element()
                    })
                    .collect();
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .children(rows)
                    .into_any_element()
            }
            Block::Table { head, rows } => {
                let mut table = div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme::border())
                    .overflow_hidden()
                    .text_size(px(13.));
                let mut row = |cells: &[Inline], heading: bool, first: bool| {
                    let weight = if heading {
                        FontWeight::BOLD
                    } else {
                        FontWeight::NORMAL
                    };
                    let cells: Vec<AnyElement> = cells
                        .iter()
                        .enumerate()
                        .map(|(column, cell)| {
                            div()
                                .flex_1()
                                .min_w_0()
                                .px(px(10.))
                                .py(px(5.))
                                .when(column > 0, |cell| {
                                    cell.border_l_1().border_color(theme::border())
                                })
                                .child(self.inline(cell, weight))
                                .into_any_element()
                        })
                        .collect();
                    div()
                        .w_full()
                        .flex()
                        .when(heading, |row| row.bg(theme::bg_input()))
                        .when(!first, |row| row.border_t_1().border_color(theme::border()))
                        .children(cells)
                };
                if !head.is_empty() {
                    table = table.child(row(head, true, true));
                }
                for (index, cells) in rows.iter().enumerate() {
                    table = table.child(row(cells, false, head.is_empty() && index == 0));
                }
                table.into_any_element()
            }
            Block::Image { source, alt } => self.image(source, alt),
            Block::Rule => div()
                .w_full()
                .h(px(1.))
                .my(px(6.))
                .bg(theme::border_strong())
                .into_any_element(),
        }
    }

    /// A picture kept beside the document is shown; one from elsewhere is a
    /// link to it.
    fn image(&mut self, source: &str, alt: &str) -> AnyElement {
        if let Target::File(path) = target(source, self.folder) {
            let known = path.extension().is_some_and(|extension| {
                IMAGE_EXTENSIONS.contains(&extension.to_string_lossy().to_lowercase().as_str())
            });
            if known && path.is_file() {
                // The picture stands where its description would be.
                self.piece();
                return div()
                    .w_full()
                    .flex()
                    .child(img(path).max_w_full().max_h(px(560.)))
                    .into_any_element();
            }
        }
        let label = image_label(source, alt);
        let link = Inline {
            text: label.to_string().into(),
            spans: vec![Span {
                range: 0..label.len(),
                style: Style {
                    link: Some(source.into()),
                    ..Style::default()
                },
            }],
        };
        div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(icon("file", px(13.), theme::text_faint()))
            .child(self.inline(&link, FontWeight::NORMAL))
            .into_any_element()
    }

    /// Styled text that can be selected, with its links marked.
    fn inline(&mut self, inline: &Inline, weight: FontWeight) -> AnyElement {
        let piece = self.piece();
        // Left to inherit: plain text needs nothing worked out.
        if inline.spans.is_empty() {
            let styled = StyledText::new(inline.text.clone());
            return div()
                .font_weight(weight)
                .child(self.selection.text(self.row, piece, inline.text.clone(), styled))
                .into_any_element();
        }
        let color = self.color;
        let base = Font {
            weight,
            ..font(theme::UI_FONT)
        };
        let run = |len: usize, style: &Style, filled: bool| {
            let mut font = base.clone();
            if style.bold {
                font.weight = FontWeight::BOLD;
            }
            if style.italic {
                font.style = FontStyle::Italic;
            }
            if style.code {
                font.family = theme::MONO_FONT.into();
            }
            TextRun {
                len,
                font,
                color: if style.link.is_some() {
                    theme::accent().into()
                } else {
                    color
                },
                background_color: (style.code && filled).then(|| theme::bg_active().into()),
                underline: style.link.as_ref().map(|_| UnderlineStyle {
                    thickness: px(1.),
                    color: None,
                    wavy: false,
                }),
                strikethrough: style.strike.then(|| StrikethroughStyle {
                    thickness: px(1.),
                    color: None,
                }),
            }
        };

        let plain = Style::default();
        let mut runs = Vec::new();
        let mut links: Vec<(Range<usize>, Arc<str>)> = Vec::new();
        let mut at = 0;
        // Code gives up its own background where it is selected, which
        // would otherwise cover the selection's.
        let selected = self
            .selection
            .part(self.row, piece, &inline.text)
            .map_or(0..0, |part| part.range);
        for span in &inline.spans {
            if span.range.start > at {
                runs.push(run(span.range.start - at, &plain, true));
            }
            let bare = selected.start.clamp(span.range.start, span.range.end)
                ..selected.end.clamp(span.range.start, span.range.end);
            if span.style.code && !bare.is_empty() {
                runs.push(run(bare.start - span.range.start, &span.style, true));
                runs.push(run(bare.len(), &span.style, false));
                runs.push(run(span.range.end - bare.end, &span.style, true));
            } else {
                runs.push(run(span.range.len(), &span.style, true));
            }
            at = span.range.end;
            if let Some(link) = &span.style.link {
                match links.last_mut() {
                    // One link, styled in parts, is still one thing to click.
                    Some((range, last)) if range.end == span.range.start && last == link => {
                        range.end = span.range.end
                    }
                    _ => links.push((span.range.clone(), link.clone())),
                }
            }
        }
        if at < inline.text.len() {
            runs.push(run(inline.text.len() - at, &plain, true));
        }
        runs.retain(|run| run.len > 0);

        let styled = StyledText::new(inline.text.clone()).with_runs(runs);
        self.selection
            .text(self.row, piece, inline.text.clone(), styled)
            .links(links)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(source: &str) -> Vec<Block> {
        parse(source, &|_, _| Vec::new()).blocks
    }

    fn styled(text: &str, spans: &[(Range<usize>, Style)]) -> Inline {
        Inline {
            text: text.to_string().into(),
            spans: spans
                .iter()
                .cloned()
                .map(|(range, style)| Span { range, style })
                .collect(),
        }
    }

    fn link(address: &str) -> Style {
        Style {
            link: Some(address.into()),
            ..Style::default()
        }
    }

    #[test]
    fn headings_and_paragraphs_carry_their_styles() {
        let bold = Style {
            bold: true,
            ..Style::default()
        };
        let code = Style {
            code: true,
            ..Style::default()
        };
        let bold_italic = Style {
            bold: true,
            italic: true,
            ..Style::default()
        };
        assert_eq!(
            read("# The `nexus` app\n\nPlain, **bold and *both***,\nthen [a link](docs/a.md).\n"),
            vec![
                Block::Heading {
                    level: 1,
                    text: styled("The nexus app", &[(4..9, code)]),
                },
                Block::Paragraph(styled(
                    "Plain, bold and both, then a link.",
                    &[
                        (7..16, bold),
                        (16..20, bold_italic),
                        (27..33, link("docs/a.md"))
                    ]
                )),
            ]
        );
    }

    #[test]
    fn lists_nest_and_keep_their_tick_boxes() {
        let item = |checked, blocks| Item { checked, blocks };
        let text = |text: &str| Block::Paragraph(Inline::plain(text));
        assert_eq!(
            read("3. first\n4. second\n\n- [x] done\n  - [ ] inner\n- plain\n"),
            vec![
                Block::List {
                    start: Some(3),
                    items: vec![item(None, vec![text("first")]), item(None, vec![text("second")])],
                },
                Block::List {
                    start: None,
                    items: vec![
                        item(
                            Some(true),
                            vec![
                                text("done"),
                                Block::List {
                                    start: None,
                                    items: vec![item(Some(false), vec![text("inner")])],
                                }
                            ]
                        ),
                        item(None, vec![text("plain")]),
                    ],
                },
            ]
        );
    }

    #[test]
    fn code_quotes_rules_and_tables() {
        let blocks = parse(
            "```rust,ignore\nfn main() {}\n```\n\n> quoted\n\n---\n\n| a | b |\n|---|---|\n| 1 | ~~2~~ |\n",
            &|language, text| {
                assert_eq!((language, text), ("rust", "fn main() {}"));
                vec![(0..2, Hsla::default())]
            },
        )
        .blocks;
        let strike = Style {
            strike: true,
            ..Style::default()
        };
        assert_eq!(
            blocks,
            vec![
                Block::Code {
                    text: "fn main() {}".into(),
                    colors: vec![(0..2, Hsla::default())],
                },
                Block::Quote(vec![Block::Paragraph(Inline::plain("quoted"))]),
                Block::Rule,
                Block::Table {
                    head: vec![Inline::plain("a"), Inline::plain("b")],
                    rows: vec![vec![Inline::plain("1"), styled("2", &[(0..1, strike)])]],
                },
            ]
        );
    }

    #[test]
    fn pictures_stand_alone_or_become_links() {
        assert_eq!(
            read("![The logo](assets/logo.png)\n\nSee ![a badge](https://x.test/b.svg) here.\n\n![](shot.png)\n"),
            vec![
                Block::Image {
                    source: "assets/logo.png".into(),
                    alt: "The logo".into(),
                },
                Block::Paragraph(styled(
                    "See a badge here.",
                    &[(4..11, link("https://x.test/b.svg"))]
                )),
                Block::Image {
                    source: "shot.png".into(),
                    alt: "shot.png".into(),
                },
            ]
        );
    }

    #[test]
    fn html_shows_as_its_pictures_and_words() {
        assert_eq!(
            read("<p align=\"center\">\n  <img src='assets/icon.png' width=120 alt=\"Icon\">\n  <br>Fast &amp; small <b>app</b>\n</p>\n<!-- hidden -->\n\nline<br>break\n"),
            vec![
                Block::Image {
                    source: "assets/icon.png".into(),
                    alt: "Icon".into(),
                },
                Block::Paragraph(Inline::plain("Fast & small app")),
                Block::Paragraph(Inline::plain("line\nbreak")),
            ]
        );
        assert_eq!(attribute("<img data-src=no src=yes.png>", "src").as_deref(), Some("yes.png"));
        assert_eq!(attribute("<img alt='x'>", "src"), None);
    }

    #[test]
    fn a_selection_copies_as_plain_text() {
        let document = parse(
            "# Title\n\nSome **bold** text.\n\n3. first\n4. second\n   - [x] inner\n\n\
             | a | b |\n|---|---|\n| 1 | 2 |\n\n```\nlet x = 1;\nlet y = 2;\n```\n\n> said\n",
            &|_, _| Vec::new(),
        );
        let place = |row, piece, offset| Place { row, piece, offset };
        let copy = |from, to| document.text_between(&(from..to));
        assert_eq!(
            copy(Place::default(), Place::END),
            "Title\n\nSome bold text.\n\n3. first\n4. second\n  - [x] inner\n\n\
             a\tb\n1\t2\n\nlet x = 1;\nlet y = 2;\n\nsaid"
        );
        // Part of one piece, and from inside one block into the next.
        assert_eq!(copy(place(1, 0, 5), place(1, 0, 9)), "bold");
        assert_eq!(copy(place(0, 0, 2), place(1, 0, 4)), "tle\n\nSome");
        // A list item taken from partway in has lost its mark, and so has
        // one copied alone.
        assert_eq!(copy(place(2, 0, 2), place(2, 1, 3)), "rst\n4. sec");
        assert_eq!(copy(place(2, 0, 0), place(2, 0, 5)), "first");
        // Across the cells of a table.
        assert_eq!(copy(place(3, 1, 0), place(3, 3, 1)), "b\n1\t2");
        // Ending at the very start of a block takes nothing from it.
        assert_eq!(copy(place(0, 0, 0), place(1, 0, 0)), "Title");
        assert_eq!(copy(place(9, 0, 0), Place::END), "");
    }

    #[test]
    fn each_block_knows_the_line_it_began_on() {
        let source = "---\ntitle: x\n---\n# Title\n\nA paragraph\nof two lines.\n\n- one\n- two\n\n\n```sh\nls\n```\n\n<img src=\"a.png\">\n\nLast.\n";
        let document = parse(source, &|_, _| Vec::new());
        assert_eq!(document.blocks.len(), document.lines.len());
        assert_eq!(document.lines, [3, 5, 8, 12, 16, 18]);
        // A line inside a block, or in the gap after it, belongs to it.
        assert_eq!(document.block_at_line(0), 0);
        assert_eq!(document.block_at_line(6), 1);
        assert_eq!(document.block_at_line(11), 2);
        assert_eq!(document.block_at_line(13), 3);
        assert_eq!(document.block_at_line(99), 5);
        assert_eq!(parse("", &|_, _| Vec::new()).block_at_line(4), 0);
        assert_eq!(document.lines_of_block(1, 19), 5..8);
        assert_eq!(document.lines_of_block(5, 19), 18..19);
        assert_eq!(document.lines_of_block(9, 19), 19..19);
    }

    #[test]
    fn front_matter_is_left_out() {
        assert_eq!(
            read("---\ntitle: Notes\n---\n\nBody\n"),
            vec![Block::Paragraph(Inline::plain("Body"))]
        );
    }

    #[test]
    fn addresses_resolve_to_files_urls_and_anchors() {
        let folder = Path::new("/work/docs");
        assert_eq!(
            target("https://example.com/a", folder),
            Target::Url("https://example.com/a".into())
        );
        assert_eq!(
            target("mailto:me@example.com", folder),
            Target::Url("mailto:me@example.com".into())
        );
        assert_eq!(target("#usage", folder), Target::Anchor);
        assert_eq!(
            target("../src/my%20file.rs#L10", folder),
            Target::File(PathBuf::from("/work/docs/../src/my file.rs"))
        );
        assert_eq!(
            target("file:///tmp/a.md", folder),
            Target::File(PathBuf::from("/tmp/a.md"))
        );
        // A drive letter is not a scheme.
        assert!(matches!(target(r"C:\notes\a.md", folder), Target::File(_)));
    }
}
