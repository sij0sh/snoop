//! Markdown adapter: heading-depth hierarchy, exact structural atoms.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::core::{AtomKind, ParsedAtom};

pub struct ParseOutput {
    pub atoms: Vec<ParsedAtom>,
}

enum FrameKind {
    Heading(u8),
    Para,
    List,
    Item,
    Quote,
    Code(Option<String>),
}

struct Frame {
    kind: FrameKind,
    start: usize,
    /// Reserved atom index; children reference it before emission.
    index: usize,
    parent: usize,
    heading_base: usize,
    text: String,
}

fn level_as_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn placeholder() -> ParsedAtom {
    ParsedAtom {
        kind: AtomKind::Paragraph,
        parent_index: None,
        ordinal: 0,
        start_offset: 0,
        end_offset: 0,
        text: String::new(),
        breadcrumb: String::new(),
        content_hash: ParsedAtom::content_hash_of(AtomKind::Paragraph, "", ""),
        metadata: serde_json::Value::Null,
    }
}

fn breadcrumb_of(title: &str, headings: &[(u8, usize)], atoms: &[ParsedAtom]) -> String {
    let mut parts = vec![title.to_string()];
    for (_, idx) in headings {
        let label = atoms[*idx].metadata["label"]
            .as_str()
            .unwrap_or_else(|| atoms[*idx].text.trim());
        if !label.is_empty() {
            parts.push(label.to_string());
        }
    }
    parts.join(" > ")
}

fn enclosing_or_heading(stack: &[Frame], headings: &[(u8, usize)], doc: usize) -> usize {
    stack
        .iter()
        .rev()
        .find(|f| matches!(f.kind, FrameKind::List | FrameKind::Item | FrameKind::Quote))
        .map(|f| f.index)
        .unwrap_or_else(|| headings.last().map(|(_, i)| *i).unwrap_or(doc))
}

/// Parse Markdown into a deterministic atom hierarchy.
/// Heading depth derives nesting; block frames reserve indices up front so
/// parents always precede children in creation order.
pub fn parse_markdown(src: &str, title: &str) -> ParseOutput {
    let mut atoms = vec![ParsedAtom {
        kind: AtomKind::Document,
        parent_index: None,
        ordinal: 0,
        start_offset: 0,
        end_offset: src.len(),
        text: src.to_string(),
        breadcrumb: title.to_string(),
        content_hash: ParsedAtom::content_hash_of(AtomKind::Document, title, src),
        metadata: serde_json::Value::Null,
    }];
    let doc = 0usize;

    let mut stack: Vec<Frame> = Vec::new();
    // (depth, atom index) of the currently open heading chain.
    let mut headings: Vec<(u8, usize)> = Vec::new();

    let parser = Parser::new_ext(src, Options::empty());
    for (ev, range) in parser.into_offset_iter() {
        match ev {
            Event::Start(tag) => {
                let kind = match tag {
                    Tag::Heading { level, .. } => Some(FrameKind::Heading(level_as_u8(level))),
                    Tag::Paragraph => Some(FrameKind::Para),
                    Tag::List(_) => Some(FrameKind::List),
                    Tag::Item => Some(FrameKind::Item),
                    Tag::BlockQuote(_) => Some(FrameKind::Quote),
                    Tag::CodeBlock(cbk) => {
                        let lang = if let CodeBlockKind::Fenced(info) = &cbk {
                            info.split_whitespace().next().map(|s| s.to_string())
                        } else {
                            None
                        };
                        Some(FrameKind::Code(lang))
                    }
                    _ => None, // inline containers: no frame
                };
                let Some(kind) = kind else { continue };

                if let FrameKind::Heading(d) = kind {
                    while let Some((td, _)) = headings.last() {
                        if *td >= d {
                            headings.pop();
                        } else {
                            break;
                        }
                    }
                }
                let parent = if matches!(kind, FrameKind::Item) {
                    stack
                        .iter()
                        .rev()
                        .find(|f| matches!(f.kind, FrameKind::List))
                        .map(|f| f.index)
                        .unwrap_or(doc)
                } else {
                    enclosing_or_heading(&stack, &headings, doc)
                };
                let index = atoms.len();
                atoms.push(placeholder());
                stack.push(Frame {
                    kind,
                    start: range.start,
                    index,
                    parent,
                    heading_base: headings.len(),
                    text: String::new(),
                });
            }
            Event::End(tag_end) => {
                let is_block_end = matches!(
                    tag_end,
                    TagEnd::Heading(_)
                        | TagEnd::Paragraph
                        | TagEnd::List(_)
                        | TagEnd::Item
                        | TagEnd::BlockQuote(_)
                        | TagEnd::CodeBlock
                );
                if !is_block_end {
                    continue;
                }
                let Some(frame) = stack.pop() else { continue };
                let end = range.end.max(frame.start + 1).min(src.len());
                let closes_heading_scope = matches!(
                    &frame.kind,
                    FrameKind::List | FrameKind::Item | FrameKind::Quote
                );
                let heading_base = frame.heading_base;
                let text = frame.text.trim().to_string();
                let raw = src.get(frame.start..end).unwrap_or_default().to_string();

                match frame.kind {
                    FrameKind::Heading(d) => {
                        let bc = breadcrumb_of(title, &headings, &atoms);
                        let hash = ParsedAtom::content_hash_of(AtomKind::Heading, &bc, &raw);
                        atoms[frame.index] = ParsedAtom {
                            kind: AtomKind::Heading,
                            parent_index: Some(frame.parent),
                            ordinal: frame.index as u32,
                            start_offset: frame.start,
                            end_offset: end,
                            text: raw,
                            breadcrumb: bc,
                            content_hash: hash,
                            metadata: serde_json::json!({"label": text}),
                        };
                        headings.push((d, frame.index));
                    }
                    FrameKind::Para => {
                        if !text.is_empty() {
                            let bc = breadcrumb_of(title, &headings, &atoms);
                            let hash = ParsedAtom::content_hash_of(AtomKind::Paragraph, &bc, &raw);
                            atoms[frame.index] = ParsedAtom {
                                kind: AtomKind::Paragraph,
                                parent_index: Some(frame.parent),
                                ordinal: frame.index as u32,
                                start_offset: frame.start,
                                end_offset: end,
                                text: raw,
                                breadcrumb: bc,
                                content_hash: hash,
                                metadata: serde_json::Value::Null,
                            };
                        }
                    }
                    FrameKind::List => {
                        let bc = breadcrumb_of(title, &headings, &atoms);
                        let hash = ParsedAtom::content_hash_of(AtomKind::List, &bc, &raw);
                        atoms[frame.index] = ParsedAtom {
                            kind: AtomKind::List,
                            parent_index: Some(frame.parent),
                            ordinal: frame.index as u32,
                            start_offset: frame.start,
                            end_offset: end,
                            text: raw,
                            breadcrumb: bc,
                            content_hash: hash,
                            metadata: serde_json::Value::Null,
                        };
                    }
                    FrameKind::Item => {
                        if !text.is_empty() {
                            let bc = breadcrumb_of(title, &headings, &atoms);
                            let hash = ParsedAtom::content_hash_of(AtomKind::ListItem, &bc, &raw);
                            atoms[frame.index] = ParsedAtom {
                                kind: AtomKind::ListItem,
                                parent_index: Some(frame.parent),
                                ordinal: frame.index as u32,
                                start_offset: frame.start,
                                end_offset: end,
                                text: raw,
                                breadcrumb: bc,
                                content_hash: hash,
                                metadata: serde_json::Value::Null,
                            };
                        }
                    }
                    FrameKind::Quote => {
                        if !text.is_empty() {
                            let bc = breadcrumb_of(title, &headings, &atoms);
                            let hash = ParsedAtom::content_hash_of(AtomKind::BlockQuote, &bc, &raw);
                            atoms[frame.index] = ParsedAtom {
                                kind: AtomKind::BlockQuote,
                                parent_index: Some(frame.parent),
                                ordinal: frame.index as u32,
                                start_offset: frame.start,
                                end_offset: end,
                                text: raw,
                                breadcrumb: bc,
                                content_hash: hash,
                                metadata: serde_json::Value::Null,
                            };
                        }
                    }
                    FrameKind::Code(lang) => {
                        let bc = breadcrumb_of(title, &headings, &atoms);
                        let hash = ParsedAtom::content_hash_of(AtomKind::CodeBlock, &bc, &raw);
                        let meta = serde_json::json!({ "language": lang });
                        atoms[frame.index] = ParsedAtom {
                            kind: AtomKind::CodeBlock,
                            parent_index: Some(frame.parent),
                            ordinal: frame.index as u32,
                            start_offset: frame.start,
                            end_offset: end,
                            text: raw,
                            breadcrumb: bc,
                            content_hash: hash,
                            metadata: meta,
                        };
                    }
                }
                if closes_heading_scope {
                    headings.truncate(heading_base);
                }
            }
            Event::Text(t) => {
                for frame in stack
                    .iter_mut()
                    .filter(|frame| !matches!(frame.kind, FrameKind::List))
                {
                    frame.text.push_str(&t);
                }
            }
            Event::Code(t) => {
                for frame in stack
                    .iter_mut()
                    .filter(|frame| !matches!(frame.kind, FrameKind::List))
                {
                    frame.text.push('`');
                    frame.text.push_str(&t);
                    frame.text.push('`');
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                for frame in stack
                    .iter_mut()
                    .filter(|frame| !matches!(frame.kind, FrameKind::List | FrameKind::Code(_)))
                {
                    frame.text.push(' ');
                }
            }
            _ => {}
        }
    }

    ParseOutput {
        atoms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_blocks_keep_kinds_parents_offsets_and_inline_code() {
        let source = "> quoted `TokenStore`\n\n- first\n\n  continued\n";
        let parsed = parse_markdown(source, "Notes");
        let quote = parsed
            .atoms
            .iter()
            .find(|atom| atom.kind == AtomKind::BlockQuote)
            .unwrap();
        let item = parsed
            .atoms
            .iter()
            .find(|atom| atom.kind == AtomKind::ListItem)
            .unwrap();
        assert!(quote.text.contains("`TokenStore`"));
        assert!(quote.end_offset > quote.start_offset);
        assert!(!item.text.is_empty());
        assert!(item.parent_index.is_some());
        for atom in &parsed.atoms {
            assert_eq!(&source[atom.start_offset..atom.end_offset], atom.text);
        }
    }

    #[test]
    fn heading_inside_quote_does_not_leak_to_following_content() {
        let source = "> ## Quoted\n>\n> inside\n\nafter\n";
        let parsed = parse_markdown(source, "Notes");
        let after = parsed
            .atoms
            .iter()
            .find(|atom| atom.kind == AtomKind::Paragraph && atom.text.trim() == "after")
            .unwrap();
        assert_eq!(after.breadcrumb, "Notes");
        assert_eq!(after.parent_index, Some(0));
    }
}
