//! One markdown file, turned into the structure later stages score: its title,
//! its heading sections with line ranges, and its outgoing links.
//!
//! Pure: no network, no model, no global state. [`parse`] reads one file and,
//! when a wikilink needs a file-name match, walks the tree under `root` once.
//! The same files in give the same struct out.
//!
//! Markdown is parsed with `pulldown-cmark`, so fenced code, inline code and
//! HTML are excluded by construction rather than by pattern matching on the
//! source. Two things CommonMark does not have are handled here:
//!
//! - **Frontmatter.** A leading `---` fence is metadata, not content: it is
//!   reported separately and the sections start at the first body line.
//! - **Wikilinks.** `[[target]]` and `[[target|alias]]` arrive as ordinary
//!   text, split at every bracket, so each block's text is scanned for them as
//!   that text is built. A wikilink target is resolved by file name under
//!   `root` when it is a bare name, and as a path relative to the linking file
//!   when it contains a `/`.
//!
//! Only links to `.md`/`.txt` files are reported. A link that resolves outside
//! `root` is still reported, with [`Link::in_root`] `false`, so the caller can
//! see it and refuse to follow it.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::Serialize;

/// What stops a file from being parsed. Malformed markdown is not an error: it
/// is parsed as well as it can be.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not valid UTF-8")]
    NotUtf8 { path: PathBuf },
    #[error("{path} and {root} must both be relative or both absolute")]
    BaseMismatch { path: PathBuf, root: PathBuf },
}

/// One frontmatter field: a flat `key: value` line from the leading `---`
/// block. Nested mappings, lists and multi-line scalars are not parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontmatterField {
    pub key: String,
    pub value: String,
}

/// A run of lines under one heading.
///
/// A section owns its heading line and every line up to the next heading of the
/// same or higher level, so a section covers its own subsections and reading
/// any single range returns a whole section. Ranges are 1-based and inclusive,
/// counted in the file as written, and the sections are in document order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Section {
    /// Heading text, `None` for content before the first heading.
    pub heading: Option<String>,
    /// Heading depth, 1 to 6; `0` for content before the first heading.
    pub level: u8,
    /// `[first, last]` line, inclusive.
    pub lines: [usize; 2],
}

/// One outgoing link to a markdown or text file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    /// The target resolved against `root`, lexically normalised: a path
    /// relative to `root` for a target inside it, and a path starting with `..`
    /// for one that escapes. Any `#fragment` is not part of the target.
    pub target: PathBuf,
    /// The link's text: `alias` for `[[target|alias]]`, the target itself for
    /// `[[target]]`, the bracketed text for `[text](target)`.
    pub anchor: String,
    /// The sentence the anchor sits in, whitespace collapsed. This is the
    /// context the link is judged by.
    pub sentence: String,
    /// The innermost heading the link sits under, `None` before the first one.
    /// A link on a heading line belongs to that heading.
    pub heading: Option<String>,
    /// `false` when the target escapes `root`. These must never be followed.
    pub in_root: bool,
}

/// One parsed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParsedFile {
    /// The path as it was given to [`parse`].
    pub path: PathBuf,
    /// The frontmatter `title`, else the first H1, else the file name.
    pub title: String,
    /// The leading `---` block's fields, empty when the file has none.
    pub frontmatter: Vec<FrontmatterField>,
    /// One entry per heading, plus a heading-less entry for content before the
    /// first heading, in document order.
    pub sections: Vec<Section>,
    /// Links to markdown or text files, in document order.
    pub links: Vec<Link>,
}

/// What a link preview needs from a target file: [`parse`]'s title and
/// frontmatter, plus the first paragraph of prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    /// The path as it was given to [`preview`].
    pub path: PathBuf,
    /// The frontmatter `title`, else the first H1, else the file name.
    pub title: String,
    pub frontmatter: Vec<FrontmatterField>,
    /// The first paragraph's text, whitespace collapsed. `None` when the file
    /// has no paragraph: a file whose prose lives only in headings, list items
    /// or tables has none, and the frontmatter block does not count.
    pub first_paragraph: Option<String>,
}

/// Parses `path`, resolving its links against `root`.
///
/// `path` and `root` must be expressed against the same base — both relative to
/// the working directory, or both absolute. Targets outside `root` are reported
/// with [`Link::in_root`] `false` rather than dropped, and a broken link to a
/// `.md`/`.txt` file is reported too: the source text names a target whether or
/// not it exists. A wikilink whose target matches no file is dropped, because
/// nothing names a target in that case.
pub fn parse(path: impl AsRef<Path>, root: impl AsRef<Path>) -> Result<ParsedFile, ParseError> {
    let path = path.as_ref();
    let root = root.as_ref();
    if path.is_absolute() != root.is_absolute() {
        return Err(ParseError::BaseMismatch {
            path: path.to_path_buf(),
            root: root.to_path_buf(),
        });
    }

    let source = read(path)?;
    let scan = Scan::of(&source);
    let starts = line_starts(&source);
    let last_line = line_count(&source);
    let directory = normalize(&relative_to(root, path.parent().unwrap_or(Path::new(""))));

    // The tree is walked at most once, and only if a wikilink needs it.
    let mut index = None;
    let mut links = Vec::with_capacity(scan.links.len());
    for raw in &scan.links {
        let resolved = match &raw.dest {
            RawDest::Markdown(dest) => resolve_markdown(dest, &directory),
            RawDest::Wiki(dest) => {
                let index = index.get_or_insert_with(|| NameIndex::scan(root));
                resolve_wikilink(dest, &directory, root, index)
            }
        };
        let Some(resolved) = resolved else { continue };
        links.push(Link {
            target: resolved.target,
            anchor: raw.anchor.clone(),
            sentence: raw.sentence.clone(),
            heading: raw.heading.clone(),
            in_root: resolved.in_root,
        });
    }

    let title = scan_title(&scan, path);
    Ok(ParsedFile {
        path: path.to_path_buf(),
        title,
        frontmatter: scan.frontmatter,
        sections: sections(&scan.headings, &source, &starts, scan.body_start, last_line),
        links,
    })
}

/// Reads the preview of one file: title, frontmatter and first paragraph.
pub fn preview(path: impl AsRef<Path>) -> Result<Preview, ParseError> {
    let path = path.as_ref();
    let source = read(path)?;
    let scan = Scan::of(&source);

    Ok(Preview {
        path: path.to_path_buf(),
        title: scan_title(&scan, path),
        frontmatter: scan.frontmatter,
        first_paragraph: scan.first_paragraph,
    })
}

/// Frontmatter title, else first H1, else the file name.
fn scan_title(scan: &Scan, path: &Path) -> String {
    scan.frontmatter_title
        .clone()
        .or_else(|| scan.first_h1.clone())
        .unwrap_or_else(|| file_stem(path))
}

fn read(path: &Path) -> Result<String, ParseError> {
    let bytes = fs::read(path).map_err(|source| ParseError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    String::from_utf8(bytes).map_err(|_| ParseError::NotUtf8 {
        path: path.to_path_buf(),
    })
}

// ---------------------------------------------------------------- the walk

/// The result of one pass over the events of a source.
#[derive(Debug, Default)]
struct Scan {
    frontmatter: Vec<FrontmatterField>,
    frontmatter_title: Option<String>,
    /// Byte offset where the body starts: after the frontmatter.
    body_start: usize,
    headings: Vec<Heading>,
    first_h1: Option<String>,
    first_paragraph: Option<String>,
    /// In document order.
    links: Vec<RawLink>,
}

impl Scan {
    fn of(source: &str) -> Scan {
        let (frontmatter, frontmatter_title, body_start) = split_frontmatter(source);
        Walker {
            source,
            blocks: Vec::new(),
            open_link: None,
            verbatim: 0,
            current: None,
            next_block: 0,
            scan: Scan {
                frontmatter,
                frontmatter_title,
                body_start,
                ..Scan::default()
            },
        }
        .run()
    }
}

#[derive(Debug)]
struct Heading {
    level: u8,
    /// Byte offset of the heading's first `#`.
    offset: usize,
    text: String,
}

#[derive(Debug, Clone)]
struct RawLink {
    dest: RawDest,
    anchor: String,
    sentence: String,
    heading: Option<String>,
    /// The block this link sits in and the anchor's offset in that block's
    /// text: together they sort the links into document order.
    block: usize,
    offset: usize,
}

/// A destination as written, before it is resolved.
#[derive(Debug, Clone)]
enum RawDest {
    /// `[text](dest)`
    Markdown(String),
    /// `[[dest]]` or `[[dest|alias]]`
    Wiki(String),
}

/// The inline container a link was found in. Its text is the sentence's scope.
#[derive(Debug)]
struct Block {
    /// Position in document order, used to order the links the block holds.
    index: usize,
    kind: BlockKind,
    text: String,
    links: Vec<Pending>,
    /// Offset in `text` up to which wikilinks have been looked for.
    scanned_to: usize,
}

impl Block {
    fn new(index: usize, kind: BlockKind) -> Block {
        Block {
            index,
            kind,
            text: String::new(),
            links: Vec::new(),
            scanned_to: 0,
        }
    }

    /// Turns every `[[...]]` the text now contains into a link, leaving the
    /// display text behind so that sentences read as prose — including the
    /// brackets of a wikilink that resolves to no file, which is dropped later.
    ///
    /// Text arrives split at every bracket, so this runs after each addition
    /// and picks up where it left off.
    fn resolve_wikilinks(&mut self, heading: Option<usize>) {
        loop {
            let Some(open) = self.text[self.scanned_to..]
                .find("[[")
                .map(|at| self.scanned_to + at)
            else {
                // Nothing open. Leave a trailing `[` unscanned: the text
                // arrives split at brackets, so its partner may be next.
                self.scanned_to = match self.text.char_indices().next_back() {
                    Some((index, '[')) => index,
                    _ => self.text.len(),
                };
                return;
            };
            let Some(close) = self.text[open + 2..].find("]]") else {
                // An unclosed `[[`: whatever closes it may still arrive.
                self.scanned_to = open;
                return;
            };
            let end = open + 2 + close + 2;
            let Some((target, anchor)) = wikilink_parts(&self.text[open + 2..open + 2 + close])
            else {
                self.scanned_to = end;
                continue;
            };

            self.text.replace_range(open..end, &anchor);
            self.scanned_to = open + anchor.len();
            self.links.push(Pending {
                dest: RawDest::Wiki(target),
                anchor,
                offset: open,
                heading,
                block: self.index,
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    /// Text with no container of its own, e.g. after inline HTML.
    Loose,
    Paragraph,
    Heading,
    Item,
    Cell,
}

#[derive(Debug)]
struct Pending {
    dest: RawDest,
    anchor: String,
    /// Offset of the anchor in the block's text.
    offset: usize,
    heading: Option<usize>,
    block: usize,
}

#[derive(Debug)]
struct OpenLink {
    dest: RawDest,
    /// Offset of the anchor in the block's text.
    offset: usize,
    block: usize,
}

struct Walker<'a> {
    source: &'a str,
    blocks: Vec<Block>,
    open_link: Option<OpenLink>,
    /// Depth of link-anchor and image-alt text: display text, never prose that
    /// a wikilink can be read out of.
    verbatim: usize,
    /// Index into `headings` of the heading the walk is currently under.
    current: Option<usize>,
    next_block: usize,
    scan: Scan,
}

impl<'a> Walker<'a> {
    fn run(mut self) -> Scan {
        let options =
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        for (event, range) in Parser::new_ext(self.source, options).into_offset_iter() {
            // Events inside the frontmatter are metadata, not content.
            if range.start < self.scan.body_start {
                continue;
            }
            match event {
                Event::Start(tag) => self.start(tag, range.start),
                Event::End(tag) => self.end(tag),
                Event::Text(text) => {
                    let linkable = self.verbatim == 0;
                    self.plain(&text, linkable);
                }
                Event::Code(code) => self.plain(&code, false),
                Event::SoftBreak | Event::HardBreak => self.plain(" ", true),
                // Raw HTML is not text, so it cannot be part of a wikilink.
                Event::Html(_) | Event::InlineHtml(_) => self.break_scan(),
                _ => {}
            }
        }
        while !self.blocks.is_empty() {
            self.flush();
        }

        self.scan.first_h1 = self
            .scan
            .headings
            .iter()
            .find(|heading| heading.level == 1)
            .map(|heading| heading.text.clone());
        // Document order: blocks open and close in it, and a block's links are
        // found in it.
        self.scan
            .links
            .sort_by_key(|link| (link.block, link.offset));
        self.scan
    }

    fn start(&mut self, tag: Tag<'a>, at: usize) {
        match tag {
            Tag::Heading { level, .. } => {
                self.scan.headings.push(Heading {
                    level: level as u8,
                    offset: at,
                    text: String::new(),
                });
                self.current = Some(self.scan.headings.len() - 1);
                self.push_block(BlockKind::Heading);
            }
            Tag::Paragraph => self.push_block(BlockKind::Paragraph),
            Tag::Item => self.push_block(BlockKind::Item),
            Tag::TableCell => self.push_block(BlockKind::Cell),
            Tag::Link { dest_url, .. } => {
                self.verbatim += 1;
                // Links cannot nest, so an open link is never replaced.
                if self.open_link.is_none() {
                    self.ensure_block();
                    let block = self.blocks.last().map_or(0, |block| block.index);
                    let offset = self.blocks.last().map_or(0, |block| block.text.len());
                    self.open_link = Some(OpenLink {
                        dest: RawDest::Markdown(dest_url.to_string()),
                        offset,
                        block,
                    });
                }
            }
            // Images are not links; their alt text is display text.
            Tag::Image { .. } => self.verbatim += 1,
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Link => {
                self.verbatim = self.verbatim.saturating_sub(1);
                let Some(open) = self.open_link.take() else {
                    return;
                };
                let Some(block) = self.blocks.last_mut() else {
                    return;
                };
                let anchor = block.text[open.offset.min(block.text.len())..]
                    .trim()
                    .to_string();
                block.links.push(Pending {
                    dest: open.dest,
                    anchor,
                    offset: open.offset,
                    heading: self.current,
                    block: open.block,
                });
            }
            TagEnd::Image => self.verbatim = self.verbatim.saturating_sub(1),
            TagEnd::Heading(_) => {
                let Some(block) = self.blocks.pop() else {
                    return;
                };
                if let Some(index) = self.current {
                    self.scan.headings[index].text = block.text.trim().to_string();
                }
                self.flush_block(block);
            }
            TagEnd::Paragraph | TagEnd::Item | TagEnd::TableCell => {
                if let Some(block) = self.blocks.pop() {
                    self.flush_block(block);
                }
            }
            _ => {}
        }
    }

    /// Adds prose to the innermost block, whitespace collapsed. The block's
    /// text never starts with whitespace, so an offset into it stays valid
    /// after the trailing whitespace is trimmed at flush time.
    ///
    /// `linkable` is false for text that is not prose: code, and the display
    /// text of a link or image.
    fn plain(&mut self, text: &str, linkable: bool) {
        self.ensure_block();
        let heading = self.current;
        let Some(block) = self.blocks.last_mut() else {
            return;
        };
        for character in text.chars() {
            if character.is_whitespace() {
                if !block.text.is_empty() && !block.text.ends_with(' ') {
                    block.text.push(' ');
                }
            } else {
                block.text.push(character);
            }
        }
        if linkable {
            block.resolve_wikilinks(heading);
        } else {
            block.scanned_to = block.text.len();
        }
    }

    /// Raw HTML interrupts a wikilink: the two brackets are not one construct.
    fn break_scan(&mut self) {
        if let Some(block) = self.blocks.last_mut() {
            block.scanned_to = block.text.len();
        }
    }

    fn push_block(&mut self, kind: BlockKind) {
        let index = self.next_block;
        self.next_block += 1;
        self.blocks.push(Block::new(index, kind));
    }

    fn ensure_block(&mut self) {
        if self.blocks.is_empty() {
            self.push_block(BlockKind::Loose);
        }
    }

    fn flush(&mut self) {
        if let Some(block) = self.blocks.pop() {
            self.flush_block(block);
        }
    }

    fn flush_block(&mut self, block: Block) {
        let text = block.text.trim_end();
        for pending in &block.links {
            let heading = pending
                .heading
                .and_then(|index| self.scan.headings.get(index))
                .map(|heading| heading.text.clone());
            self.scan.links.push(RawLink {
                dest: pending.dest.clone(),
                anchor: pending.anchor.clone(),
                sentence: sentence_around(text, pending.offset),
                heading,
                block: pending.block,
                offset: pending.offset,
            });
        }
        if block.kind == BlockKind::Paragraph && !text.is_empty() {
            self.scan
                .first_paragraph
                .get_or_insert_with(|| text.to_string());
        }
    }
}

/// The target and display text of the inside of a `[[...]]`, or `None` when
/// there is no target.
fn wikilink_parts(inner: &str) -> Option<(String, String)> {
    let (target, anchor) = match inner.split_once('|') {
        Some((target, alias)) => (target.trim(), alias.trim()),
        None => {
            let target = inner.trim();
            (target, target.split('#').next().unwrap_or("").trim())
        }
    };
    if target.is_empty() {
        return None;
    }
    Some((target.to_string(), anchor.to_string()))
}

// ------------------------------------------------------------ link targets

/// A destination resolved against the root.
struct Resolved {
    target: PathBuf,
    in_root: bool,
}

fn resolve_markdown(dest: &str, directory: &Path) -> Option<Resolved> {
    let dest = decode_percent(dest);
    let dest = dest.split('#').next().unwrap_or("").trim();
    let target = normalize(&directory.join(destination(dest)?));
    let in_root = !escapes_root(&target);
    Some(Resolved { target, in_root })
}

/// A wikilink target: by path when it names one, by file name otherwise.
fn resolve_wikilink(
    dest: &str,
    directory: &Path,
    root: &Path,
    index: &NameIndex,
) -> Option<Resolved> {
    let dest = dest.split('#').next().unwrap_or("").trim();
    if dest.is_empty() || is_external(dest) {
        return None;
    }

    if !dest.contains('/') {
        return index.best(dest).map(|target| Resolved {
            target,
            in_root: true,
        });
    }

    let path = Path::new(dest);
    if path.is_absolute() {
        return None;
    }
    let candidates = if is_page(path) {
        vec![normalize(&directory.join(path))]
    } else {
        vec![
            normalize(&directory.join(format!("{dest}.md"))),
            normalize(&directory.join(format!("{dest}.txt"))),
        ]
    };
    candidates
        .into_iter()
        .find(|target| is_page(target) && root.join(target).is_file())
        .map(|target| Resolved {
            in_root: !escapes_root(&target),
            target,
        })
}

/// The path part of a link destination, or `None` when it does not name a
/// markdown or text file.
fn destination(dest: &str) -> Option<&Path> {
    let dest = dest.trim();
    if dest.is_empty() || is_external(dest) {
        return None;
    }
    let path = Path::new(dest);
    if path.is_absolute() || !is_page(path) {
        return None;
    }
    Some(path)
}

/// File names of every `.md`/`.txt` file under the root, for wikilinks that
/// name a file rather than a path.
///
/// Hidden directories are not descended and unreadable ones are skipped: this
/// is a name lookup, not a crawl of the tree.
#[derive(Debug, Default)]
struct NameIndex {
    by_name: HashMap<String, Vec<PathBuf>>,
    by_stem: HashMap<String, Vec<PathBuf>>,
}

impl NameIndex {
    fn scan(root: &Path) -> NameIndex {
        let mut index = NameIndex::default();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    if !entry.file_name().to_string_lossy().starts_with('.') {
                        pending.push(entry.path());
                    }
                    continue;
                }
                let path = entry.path();
                if !is_page(&path) {
                    continue;
                }
                let target = normalize(&relative_to(root, &path));
                let name = entry.file_name().to_string_lossy().into_owned();
                let stem = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                index.by_name.entry(name).or_default().push(target.clone());
                index.by_stem.entry(stem).or_default().push(target);
            }
        }
        for targets in index.by_name.values_mut() {
            targets.sort();
        }
        for targets in index.by_stem.values_mut() {
            targets.sort();
        }
        index
    }

    /// The file a wikilink name refers to: `.md` before `.txt`, then the first
    /// path in order, so the answer never depends on directory order.
    fn best(&self, name: &str) -> Option<PathBuf> {
        let candidates = if Path::new(name).extension().is_some() {
            self.by_name.get(name)?
        } else {
            self.by_stem.get(name)?
        };
        candidates
            .iter()
            .find(|target| has_extension(target, "md"))
            .or_else(|| candidates.first())
            .cloned()
    }
}

fn is_page(path: &Path) -> bool {
    has_extension(path, "md") || has_extension(path, "txt")
}

fn has_extension(path: &Path, wanted: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(wanted))
}

/// `scheme:` destinations, which are never local files.
fn is_external(dest: &str) -> bool {
    let Some(colon) = dest.find(':') else {
        return false;
    };
    let scheme = &dest[..colon];
    !scheme.is_empty()
        && !scheme.contains('/')
        && scheme.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

/// `%20` and friends, which is how a link spells a space in a file name.
fn decode_percent(dest: &str) -> String {
    if !dest.contains('%') {
        return dest.to_string();
    }
    let bytes = dest.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            decoded.push(high * 16 + low);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Resolves `.` and `..` without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn escapes_root(target: &Path) -> bool {
    target.is_absolute() || matches!(target.components().next(), Some(Component::ParentDir))
}

/// `to` relative to `from`, lexically. Both are given against the same base;
/// [`parse`] rejects the mixed case before calling this.
fn relative_to(from: &Path, to: &Path) -> PathBuf {
    let from = normalize(from);
    let to = normalize(to);
    let mut from_components = from.components().peekable();
    let mut to_components = to.components().peekable();
    while let (Some(leading), Some(trailing)) = (from_components.peek(), to_components.peek()) {
        if leading != trailing {
            break;
        }
        from_components.next();
        to_components.next();
    }

    let mut out = PathBuf::new();
    for _ in from_components {
        out.push("..");
    }
    for component in to_components {
        out.push(component.as_os_str());
    }
    out
}

// ------------------------------------------------------------------- text

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn line_count(source: &str) -> usize {
    source.lines().count()
}

/// The 1-based line `offset` falls on, clamped to the file the way `lines()`
/// counts it: a trailing newline does not open a line of its own.
fn line_of(starts: &[usize], last_line: usize, offset: usize) -> usize {
    starts
        .partition_point(|&start| start <= offset)
        .min(last_line.max(1))
}

/// The sentence around `offset`: from the previous sentence break to the next
/// one. `text` has no leading whitespace, so `offset` indexes it directly.
fn sentence_around(text: &str, offset: usize) -> String {
    let offset = offset.min(text.len());
    let start = text[..offset]
        .char_indices()
        .rev()
        .find(|&(index, character)| is_break(text, index, character))
        .map_or(0, |(index, character)| index + character.len_utf8() + 1);
    let end = text[offset..]
        .char_indices()
        .find(|&(index, character)| is_break(text, offset + index, character))
        .map_or(text.len(), |(index, character)| {
            offset + index + character.len_utf8()
        });

    text[start..end].trim().to_string()
}

fn is_break(text: &str, index: usize, character: char) -> bool {
    if !matches!(character, '.' | '!' | '?') {
        return false;
    }
    let after = index + character.len_utf8();
    after == text.len() || text[after..].starts_with(' ')
}

/// Splits a leading `---` fence off the source: its fields, its `title` if it
/// has one, and the byte offset where the body starts.
fn split_frontmatter(source: &str) -> (Vec<FrontmatterField>, Option<String>, usize) {
    let mut lines = source.split_inclusive('\n');
    let Some(first) = lines.next() else {
        return (Vec::new(), None, 0);
    };
    if strip_eol(first).trim() != "---" {
        return (Vec::new(), None, 0);
    }

    let mut offset = first.len();
    let mut fields = Vec::new();
    let mut title = None;
    for line in lines {
        let text = strip_eol(line);
        if matches!(text.trim(), "---" | "...") {
            return (fields, title, offset + line.len());
        }
        if let Some((key, value)) = text.split_once(':') {
            let key = key.trim().to_string();
            let value = strip_quotes(value.trim()).to_string();
            if !key.is_empty() && !key.starts_with('#') {
                if key == "title" && title.is_none() && !value.is_empty() {
                    title = Some(value.clone());
                }
                fields.push(FrontmatterField { key, value });
            }
        }
        offset += line.len();
    }

    // No closing fence, so the `---` was a thematic break and this is prose.
    (Vec::new(), None, 0)
}

fn strip_eol(line: &str) -> &str {
    line.trim_end_matches(['\n', '\r'])
}

fn strip_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if value.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[value.len() - 1] == bytes[0]
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

// --------------------------------------------------------------- sections

/// One section per heading plus the preamble, with the line ranges the callers
/// read. Ranges are contiguous: each section ends where the next one it does
/// not contain begins.
fn sections(
    headings: &[Heading],
    source: &str,
    starts: &[usize],
    body_start: usize,
    last_line: usize,
) -> Vec<Section> {
    let mut sections = Vec::with_capacity(headings.len() + 1);
    let body_line = line_of(starts, last_line, body_start);

    let preamble_end = match headings.first() {
        Some(first) => line_of(starts, last_line, first.offset).saturating_sub(1),
        None => last_line,
    };
    let preamble = &source[body_start..headings.first().map_or(source.len(), |first| first.offset)];
    if preamble_end >= body_line && preamble.chars().any(|character| !character.is_whitespace()) {
        sections.push(Section {
            heading: None,
            level: 0,
            lines: [body_line, preamble_end],
        });
    }

    // A section ends before the next heading that is not nested under it, so a
    // parent section runs through its subsections.
    let mut ends = vec![last_line; headings.len()];
    let mut open: Vec<(u8, usize)> = Vec::new();
    for (index, heading) in headings.iter().enumerate().rev() {
        while open.last().is_some_and(|&(level, _)| level > heading.level) {
            open.pop();
        }
        if let Some(&(_, line)) = open.last() {
            ends[index] = line - 1;
        }
        open.push((heading.level, line_of(starts, last_line, heading.offset)));
    }

    for (heading, end) in headings.iter().zip(ends) {
        sections.push(Section {
            heading: Some(heading.text.clone()),
            level: heading.level,
            lines: [line_of(starts, last_line, heading.offset), end],
        });
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_numbers_follow_the_file_as_written() {
        let source = "one\ntwo\nthree";
        let starts = line_starts(source);
        assert_eq!(line_count(source), 3);
        assert_eq!(line_of(&starts, 3, 0), 1);
        assert_eq!(line_of(&starts, 3, 4), 2);
        assert_eq!(line_of(&starts, 3, 8), 3);
        // A trailing newline does not open a fourth line.
        assert_eq!(line_count("one\n"), 1);
        assert_eq!(line_of(&line_starts("one\n"), 1, 4), 1);
    }

    #[test]
    fn sentences_stop_at_the_punctuation_around_the_link() {
        let text = "Instant payouts settle in minutes. See the cutoffs page. Nothing else.";
        let offset = text.find("cutoffs").unwrap();
        assert_eq!(sentence_around(text, offset), "See the cutoffs page.");
        assert_eq!(
            sentence_around("Only one sentence.", 5),
            "Only one sentence."
        );
        // No trailing space after the break: still a break at the very end.
        assert_eq!(sentence_around("One. Two", 5), "Two");
    }

    #[test]
    fn percent_escapes_decode_only_when_they_are_escapes() {
        assert_eq!(decode_percent("weekly%20review.md"), "weekly review.md");
        assert_eq!(decode_percent("100%25.md"), "100%.md");
        assert_eq!(decode_percent("nothing%zz.md"), "nothing%zz.md");
        assert_eq!(decode_percent("plain.md"), "plain.md");
    }

    #[test]
    fn destinations_are_relative_markdown_or_text_files() {
        assert_eq!(
            destination("notes/ledger.md"),
            Some(Path::new("notes/ledger.md"))
        );
        assert_eq!(destination("cutoffs.txt"), Some(Path::new("cutoffs.txt")));
        assert_eq!(destination("https://example.com/page.md"), None);
        assert_eq!(destination("mailto:a@b.test"), None);
        assert_eq!(destination("assets/logo.png"), None);
        assert_eq!(destination("/etc/passwd.md"), None);
        assert_eq!(destination(""), None);
    }

    #[test]
    fn normalisation_is_lexical() {
        assert_eq!(
            normalize(Path::new("a/./b/../c.md")),
            PathBuf::from("a/c.md")
        );
        assert_eq!(normalize(Path::new("../a.md")), PathBuf::from("../a.md"));
        assert_eq!(
            normalize(Path::new("a/../../b.md")),
            PathBuf::from("../b.md")
        );
        assert_eq!(
            relative_to(
                Path::new("wiki/payments"),
                Path::new("wiki/notes/ledger.md")
            ),
            PathBuf::from("../notes/ledger.md")
        );
        assert!(escapes_root(Path::new("../outside.md")));
        assert!(!escapes_root(Path::new("notes/ledger.md")));
    }

    #[test]
    fn wikilink_parts_split_target_and_display_text() {
        assert_eq!(
            wikilink_parts("ledger"),
            Some(("ledger".to_string(), "ledger".to_string()))
        );
        assert_eq!(
            wikilink_parts("ledger|the ledger"),
            Some(("ledger".to_string(), "the ledger".to_string()))
        );
        assert_eq!(
            wikilink_parts("cutoffs#times"),
            Some(("cutoffs#times".to_string(), "cutoffs".to_string()))
        );
        assert_eq!(wikilink_parts(""), None);
    }

    #[test]
    fn frontmatter_needs_both_fences() {
        let (fields, title, body) = split_frontmatter("---\ntitle: Home\n---\nBody\n");
        assert_eq!(title.as_deref(), Some("Home"));
        assert_eq!(fields.len(), 1);
        assert_eq!(&"---\ntitle: Home\n---\nBody\n"[body..], "Body\n");

        // A lone `---` is a thematic break, not frontmatter.
        let (fields, title, body) = split_frontmatter("---\n\nBody\n");
        assert!(fields.is_empty());
        assert_eq!(title, None);
        assert_eq!(body, 0);
    }
}
