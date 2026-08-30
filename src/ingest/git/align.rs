use crate::core::AtomKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Moved,
}

impl ChangeKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Added => "added",
            ChangeKind::Modified => "modified",
            ChangeKind::Deleted => "deleted",
            ChangeKind::Renamed => "renamed",
            ChangeKind::Moved => "moved",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum BoundaryConfidence {
    High,
    Medium,
    Low,
}

impl BoundaryConfidence {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            BoundaryConfidence::High => "high",
            BoundaryConfidence::Medium => "medium",
            BoundaryConfidence::Low => "low",
        }
    }
}

pub(super) fn file_change_kind(status: char) -> ChangeKind {
    match status {
        'A' => ChangeKind::Added,
        'D' => ChangeKind::Deleted,
        'R' | 'C' => ChangeKind::Renamed,
        _ => ChangeKind::Modified,
    }
}

#[derive(Debug, Clone)]
pub(super) struct Hunk {
    pub(super) old_start: u32,
    pub(super) old_count: u32,
    pub(super) new_start: u32,
    pub(super) new_count: u32,
    pub(super) text: String,
    pub(super) start_offset: usize,
    pub(super) end_offset: usize,
}

fn parse_range(token: &str) -> (u32, u32) {
    let token = token.trim_start_matches(['-', '+']);
    match token.split_once(',') {
        Some((start, count)) => (start.parse().unwrap_or(0), count.parse().unwrap_or(0)),
        None => (token.parse().unwrap_or(0), 1),
    }
}

fn take_current(current: &mut Option<Hunk>, hunks: &mut Vec<Hunk>) {
    if let Some(mut hunk) = current.take() {
        hunk.end_offset = hunk.start_offset + hunk.text.len();
        hunks.push(hunk);
    }
}

pub(super) fn parse_hunks(patch: &str) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let mut current: Option<Hunk> = None;
    let mut position = 0usize;
    for chunk in patch.split_inclusive('\n') {
        let start = position;
        position += chunk.len();
        let line = chunk.trim_end();
        if let Some(rest) = line.strip_prefix("@@") {
            take_current(&mut current, &mut hunks);
            let header = rest.split("@@").next().unwrap_or("");
            let mut old = (0u32, 0u32);
            let mut new = (0u32, 0u32);
            for token in header.split_whitespace() {
                if token.starts_with('-') && token.len() > 1 && old == (0, 0) {
                    old = parse_range(token);
                } else if token.starts_with('+') && token.len() > 1 && new == (0, 0) {
                    new = parse_range(token);
                }
            }
            current = Some(Hunk {
                old_start: old.0,
                old_count: old.1,
                new_start: new.0,
                new_count: new.1,
                text: chunk.to_string(),
                start_offset: start,
                end_offset: 0,
            });
        } else if let Some(hunk) = current.as_mut() {
            hunk.text.push_str(chunk);
        }
    }
    take_current(&mut current, &mut hunks);
    hunks
}

fn total_lines(source: &str) -> u32 {
    let newlines = source.bytes().filter(|byte| *byte == b'\n').count() as u32;
    if source.ends_with('\n') {
        newlines
    } else {
        newlines + 1
    }
}

#[derive(Debug, Clone)]
pub(super) struct SymbolSpan {
    pub(super) name: String,
    pub(super) breadcrumb: String,
    pub(super) qualified: String,
    pub(super) kind: AtomKind,
    pub(super) start_line: u32,
    pub(super) end_line: u32,
}

fn parse_spans(source: &str, locator: &str) -> (Vec<SymbolSpan>, bool) {
    let boundaries = match crate::ingest::code::analyze_code(locator, source) {
        Ok(boundaries) => boundaries,
        Err(_) => return (Vec::new(), false),
    };
    let spans = boundaries
        .iter()
        .filter(|boundary| {
            matches!(
                boundary.kind,
                AtomKind::Function | AtomKind::Class | AtomKind::Module | AtomKind::Declaration
            )
        })
        .map(|boundary| SymbolSpan {
            name: boundary.display_name.clone(),
            breadcrumb: boundary.symbol_id.clone(),
            qualified: boundary.qualified_name.clone(),
            kind: boundary.kind,
            start_line: *boundary.line_range.start(),
            end_line: *boundary.line_range.end(),
        })
        .collect();
    (spans, true)
}
pub(super) fn changed_new_lines(hunk: &Hunk) -> Vec<u32> {
    let mut lines = Vec::new();
    let mut current = hunk.new_start;
    for line in hunk.text.lines().skip(1) {
        if line.starts_with('+') {
            lines.push(current);
            current += 1;
        } else if line.starts_with('-') || line.starts_with('\\') {
        } else {
            current += 1;
        }
    }
    lines
}

pub(super) fn changed_old_lines(hunk: &Hunk) -> Vec<u32> {
    let mut lines = Vec::new();
    let mut current = hunk.old_start;
    for line in hunk.text.lines().skip(1) {
        if line.starts_with('-') {
            lines.push(current);
            current += 1;
        } else if line.starts_with('+') || line.starts_with('\\') {
        } else {
            current += 1;
        }
    }
    lines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    Old,
    New,
}

impl Side {
    fn range(self, hunk: &Hunk) -> (u32, u32) {
        match self {
            Side::Old => (hunk.old_start, hunk.old_count),
            Side::New => (hunk.new_start, hunk.new_count),
        }
    }

    fn changed_lines(self, hunk: &Hunk) -> Vec<u32> {
        match self {
            Side::Old => changed_old_lines(hunk),
            Side::New => changed_new_lines(hunk),
        }
    }
}

fn span_for_range(spans: &[SymbolSpan], start: u32, end: u32) -> Option<SymbolSpan> {
    spans
        .iter()
        .filter(|span| span.start_line <= end && span.end_line >= start)
        .min_by_key(|span| {
            (
                span.end_line.saturating_sub(span.start_line),
                span.breadcrumb.clone(),
            )
        })
        .cloned()
}

pub(super) fn align_hunks(
    hunks: &[Hunk],
    source: &str,
    locator: &str,
    side: Side,
) -> (Vec<(Option<SymbolSpan>, Vec<usize>)>, bool) {
    if hunks.is_empty() {
        return (Vec::new(), false);
    }
    let total = total_lines(source);
    let (spans, parsed) = parse_spans(source, locator);
    if spans.is_empty() {
        return (vec![(None, (0..hunks.len()).collect())], parsed);
    }

    let mut groups: Vec<(Option<SymbolSpan>, Vec<usize>)> = Vec::new();
    let mut assign = |span: Option<SymbolSpan>, index: usize| {
        let key = span
            .as_ref()
            .map(|value| value.breadcrumb.clone())
            .unwrap_or_default();
        match groups.iter_mut().find(|(existing, _)| {
            existing
                .as_ref()
                .map(|value| value.breadcrumb == key)
                .unwrap_or(key.is_empty())
        }) {
            Some((_, assigned)) => assigned.push(index),
            None => groups.push((span, vec![index])),
        }
    };

    for (index, hunk) in hunks.iter().enumerate() {
        let changed = side.changed_lines(hunk);
        let (start_line, count) = side.range(hunk);

        let covers_file = side == Side::New
            && if changed.is_empty() {
                hunks.len() == 1
                    && start_line <= 1
                    && start_line as u64 + count as u64 >= total as u64
            } else {
                changed.first() == Some(&1)
                    && changed.last().copied().unwrap_or(0) as u64 >= total as u64
            };
        if covers_file {
            assign(None, index);
            continue;
        }
        let spans_for_change: Vec<SymbolSpan> = if changed.is_empty() {
            Vec::new()
        } else {
            changed
                .iter()
                .filter_map(|line| span_for_range(&spans, *line, *line))
                .fold(Vec::new(), |mut acc: Vec<SymbolSpan>, span| {
                    if acc
                        .last()
                        .is_none_or(|last| last.breadcrumb != span.breadcrumb)
                    {
                        acc.push(span);
                    }
                    acc
                })
        };
        if !spans_for_change.is_empty() {
            for span in spans_for_change {
                assign(Some(span), index);
            }
            continue;
        }

        let range_start = start_line.max(1);
        let range_end = start_line
            .saturating_add(count.saturating_sub(1))
            .max(range_start);
        let span = span_for_range(&spans, range_start, range_end);
        assign(span, index);
    }
    (groups, parsed)
}

#[derive(Debug, Clone)]
pub(super) struct Alignment {
    pub(super) old_span: Option<SymbolSpan>,
    pub(super) new_span: Option<SymbolSpan>,
    pub(super) hunk_indices: Vec<usize>,
    pub(super) change_kind: ChangeKind,
    pub(super) strategy: &'static str,
    pub(super) confidence: BoundaryConfidence,
}

fn span_body(source: &str, span: &SymbolSpan) -> String {
    let start = span.start_line.saturating_sub(1) as usize;
    let end = span.end_line as usize;
    source
        .lines()
        .skip(start)
        .take(end.saturating_sub(start) + 1)
        .collect::<Vec<_>>()
        .join("\n")
}

fn matches_rename(before: &str, old_span: &SymbolSpan, after: &str, new_span: &SymbolSpan) -> bool {
    let old_length = old_span.end_line.saturating_sub(old_span.start_line);
    let new_length = new_span.end_line.saturating_sub(new_span.start_line);
    old_length == new_length
        && span_body(before, old_span).replace(&old_span.name, "\u{0}")
            == span_body(after, new_span).replace(&new_span.name, "\u{0}")
}

pub(super) fn reconcile_alignments(
    hunks: &[Hunk],
    before: &str,
    after: &str,
    old_locator: &str,
    new_locator: &str,
    file_renamed: bool,
    file_change: ChangeKind,
) -> Vec<Alignment> {
    let (new_groups, new_parsed) = align_hunks(hunks, after, new_locator, Side::New);
    let (old_groups, _old_parsed) = align_hunks(hunks, before, old_locator, Side::Old);
    let mut alignments = Vec::new();
    let mut consumed_old: Vec<usize> = Vec::new();

    for (new_span, indices) in &new_groups {
        let Some(new_span) = new_span else {
            alignments.push(Alignment {
                old_span: None,
                new_span: None,
                hunk_indices: indices.clone(),
                change_kind: file_change,
                strategy: "file",
                confidence: if new_parsed {
                    BoundaryConfidence::Medium
                } else {
                    BoundaryConfidence::Low
                },
            });
            continue;
        };
        let modified_index = old_groups
            .iter()
            .enumerate()
            .filter(|(index, _)| !consumed_old.contains(index))
            .find(|(_, (old_span, _))| {
                old_span.as_ref().is_some_and(|old| {
                    if file_renamed {
                        old.qualified == new_span.qualified
                    } else {
                        old.breadcrumb == new_span.breadcrumb
                    }
                })
            })
            .map(|(index, _)| index);
        if let Some(index) = modified_index {
            consumed_old.push(index);
            alignments.push(Alignment {
                old_span: old_groups[index].0.clone(),
                new_span: Some(new_span.clone()),
                hunk_indices: indices.clone(),
                change_kind: ChangeKind::Modified,
                strategy: "symbol",
                confidence: BoundaryConfidence::High,
            });
            continue;
        }
        let renamed_index = old_groups
            .iter()
            .enumerate()
            .filter(|(index, _)| !consumed_old.contains(index))
            .filter(|(_, (old_span, _))| {
                old_span
                    .as_ref()
                    .is_some_and(|old| matches_rename(before, old, after, new_span))
            })
            .min_by_key(|(index, (old_span, _))| {
                let old_start = old_span
                    .as_ref()
                    .map(|span| span.start_line)
                    .unwrap_or_default();
                (old_start.abs_diff(new_span.start_line), *index)
            })
            .map(|(index, _)| index);
        if let Some(index) = renamed_index {
            consumed_old.push(index);
            alignments.push(Alignment {
                old_span: old_groups[index].0.clone(),
                new_span: Some(new_span.clone()),
                hunk_indices: indices.clone(),
                change_kind: ChangeKind::Renamed,
                strategy: "symbol",
                confidence: BoundaryConfidence::High,
            });
            continue;
        }
        alignments.push(Alignment {
            old_span: None,
            new_span: Some(new_span.clone()),
            hunk_indices: indices.clone(),
            change_kind: ChangeKind::Added,
            strategy: "symbol",
            confidence: BoundaryConfidence::High,
        });
    }

    for (index, (old_span, indices)) in old_groups.iter().enumerate() {
        if consumed_old.contains(&index) {
            continue;
        }
        let Some(old_span) = old_span else {
            continue;
        };
        alignments.push(Alignment {
            old_span: Some(old_span.clone()),
            new_span: None,
            hunk_indices: indices.clone(),
            change_kind: ChangeKind::Deleted,
            strategy: "symbol",
            confidence: BoundaryConfidence::High,
        });
    }
    alignments
}
