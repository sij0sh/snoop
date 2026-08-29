use std::path::Path;

use crate::core::{
    hash_segments, AnchorKind, AtomKind, BuiltAnchor, BuiltUnit, ParsedAtom, UnitKind,
};
use crate::ingest::units::{estimate_tokens, MAX_TOKENS};

pub const MAX_COMMITS: usize = 500;
pub const MAX_HUNKS_PER_FILE: usize = 64;
const MAX_PATCH_CHARS: usize = 256 * 1024;
const MAX_ALIGN_BYTES: usize = 10 * 1024 * 1024;
const GIT_UNIT_VERSION: &str = "git-unit-v3";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Moved,
}

impl ChangeKind {
    fn as_str(self) -> &'static str {
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
enum BoundaryConfidence {
    High,
    Medium,
    Low,
}

impl BoundaryConfidence {
    fn as_str(self) -> &'static str {
        match self {
            BoundaryConfidence::High => "high",
            BoundaryConfidence::Medium => "medium",
            BoundaryConfidence::Low => "low",
        }
    }
}

fn file_change_kind(status: char) -> ChangeKind {
    match status {
        'A' => ChangeKind::Added,
        'D' => ChangeKind::Deleted,
        'R' | 'C' => ChangeKind::Renamed,
        _ => ChangeKind::Modified,
    }
}

#[derive(Debug, Clone)]
pub struct CommitRef {
    pub oid: String,
    pub timestamp: i64,
    pub message: String,
    pub content_hash: String,
}

fn git(root: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.quotepath=false",
            "-c",
            "diff.algorithm=myers",
            "--no-pager",
        ])
        .args(args)
        .output()
        .map_err(|error| format!("git spawn failed: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn is_history_root(root: &Path) -> bool {
    root.join(".git").exists()
}

pub fn list_commits(
    root: &Path,
    max: usize,
) -> Result<Vec<CommitRef>, Box<dyn std::error::Error + Send + Sync>> {
    if git(root, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
        return Ok(Vec::new());
    }
    let max_arg = format!("--max-count={max}");
    let out = git(
        root,
        &["log", &max_arg, "--format=%H%x1f%ct%x1f%B%x1e", "HEAD"],
    )?;
    parse_log(&out)
}

pub fn list_commits_past(
    root: &Path,
    max: usize,
    boundary_tip: &str,
) -> Result<Vec<CommitRef>, Box<dyn std::error::Error + Send + Sync>> {
    if git(root, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
        return Ok(Vec::new());
    }
    let range = format!("{boundary_tip}..HEAD");
    let max_arg = format!("--max-count={max}");
    match git(
        root,
        &["log", &max_arg, "--format=%H%x1f%ct%x1f%B%x1e", &range],
    ) {
        Ok(out) => parse_log(&out),
        Err(_) => list_commits(root, max),
    }
}

fn parse_log(out: &str) -> Result<Vec<CommitRef>, Box<dyn std::error::Error + Send + Sync>> {
    let mut commits = Vec::new();
    for record in out.split('\x1e') {
        let record = record.trim_matches('\n');
        if record.trim().is_empty() {
            continue;
        }
        let mut fields = record.splitn(3, '\x1f');
        let (Some(oid), Some(timestamp), Some(message)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let oid = oid.trim();
        commits.push(CommitRef {
            oid: oid.to_string(),
            timestamp: timestamp.trim().parse().unwrap_or(0),
            message: message.trim_end().to_string(),
            content_hash: hash_segments(&["git-commit", oid]),
        });
    }
    Ok(commits)
}

struct ChangedFile {
    path: String,
    old_path: Option<String>,
    status: char,
    patch: String,
}

fn changed_files(
    root: &Path,
    oid: &str,
) -> Result<Vec<ChangedFile>, Box<dyn std::error::Error + Send + Sync>> {
    let out = git(
        root,
        &[
            "diff-tree",
            "--root",
            "--no-commit-id",
            "-r",
            "-M",
            "--name-status",
            oid,
        ],
    )?;
    let mut files = Vec::new();
    for line in out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let Some(status_field) = fields.next() else {
            continue;
        };
        let status = status_field.trim().chars().next().unwrap_or('M');
        if matches!(status, 'R' | 'C') {
            let score = status_field.trim()[1..].to_string();
            if score.is_empty() || !score.chars().all(|digit| digit.is_ascii_digit()) {
                continue;
            }
            let (Some(old_path), Some(new_path)) = (fields.next(), fields.next()) else {
                continue;
            };
            if old_path.is_empty() || new_path.is_empty() {
                continue;
            }
            let patch = git(
                root,
                &[
                    "diff-tree",
                    "--root",
                    "-p",
                    "-M",
                    "--no-ext-diff",
                    "-U3",
                    oid,
                    "--",
                    old_path,
                    new_path,
                ],
            )?;
            if patch.trim().is_empty() {
                continue;
            }
            files.push(ChangedFile {
                path: new_path.to_string(),
                old_path: Some(old_path.to_string()),
                status,
                patch,
            });
        } else {
            let Some(path) = fields.next() else {
                continue;
            };
            if path.is_empty() {
                continue;
            }
            let patch = git(
                root,
                &[
                    "diff-tree",
                    "--root",
                    "-p",
                    "--no-ext-diff",
                    "-U3",
                    oid,
                    "--",
                    path,
                ],
            )?;
            if patch.trim().is_empty() {
                continue;
            }
            files.push(ChangedFile {
                path: path.to_string(),
                old_path: None,
                status,
                patch,
            });
        }
    }
    Ok(files)
}

fn blob(root: &Path, rev: &str, path: &str) -> Option<String> {
    let rev_path = format!("{rev}:{path}");
    git(root, &["show", &rev_path])
        .ok()
        .filter(|content| !content.contains('\0'))
}

fn parent_oid(root: &Path, oid: &str) -> Option<String> {
    let out = git(root, &["show", "-s", "--format=%P", oid]).ok()?;
    out.lines()
        .next()?
        .split_whitespace()
        .next()
        .map(String::from)
}

#[derive(Debug, Clone)]
struct Hunk {
    old_start: u32,
    old_count: u32,
    new_start: u32,
    new_count: u32,
    text: String,
    start_offset: usize,
    end_offset: usize,
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

fn parse_hunks(patch: &str) -> Vec<Hunk> {
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
struct SymbolSpan {
    name: String,
    breadcrumb: String,
    qualified: String,
    kind: AtomKind,
    start_line: u32,
    end_line: u32,
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
fn changed_new_lines(hunk: &Hunk) -> Vec<u32> {
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

fn changed_old_lines(hunk: &Hunk) -> Vec<u32> {
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
enum Side {
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

fn align_hunks(
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
struct Alignment {
    old_span: Option<SymbolSpan>,
    new_span: Option<SymbolSpan>,
    hunk_indices: Vec<usize>,
    change_kind: ChangeKind,
    strategy: &'static str,
    confidence: BoundaryConfidence,
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

fn matches_rename(
    before: &str,
    old_span: &SymbolSpan,
    after: &str,
    new_span: &SymbolSpan,
) -> bool {
    let old_length = old_span.end_line.saturating_sub(old_span.start_line);
    let new_length = new_span.end_line.saturating_sub(new_span.start_line);
    old_length == new_length
        && span_body(before, old_span).replace(&old_span.name, "\u{0}")
            == span_body(after, new_span).replace(&new_span.name, "\u{0}")
}

fn reconcile_alignments(
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

fn git_routing(short: &str, subject: &str, path: &str, symbol: Option<&str>) -> String {
    format!(
        "source: git_change\ncommit: {short}\nmessage: {subject}\nchanged file: {path}\nchanged symbol: {}",
        symbol.unwrap_or("-")
    )
}

fn git_anchors(
    oid: &str,
    old_path: Option<&str>,
    new_path: &str,
    old_symbol: Option<&str>,
    new_symbol: Option<&str>,
) -> Vec<BuiltAnchor> {
    let mut anchors = vec![
        BuiltAnchor {
            kind: AnchorKind::Commit,
            value: oid.to_string(),
            relationship: "part_of".to_string(),
            confidence: "deterministic".to_string(),
        },
        BuiltAnchor {
            kind: AnchorKind::File,
            value: new_path.to_string(),
            relationship: "changes".to_string(),
            confidence: "deterministic".to_string(),
        },
    ];
    if let Some(old_path) = old_path.filter(|old| *old != new_path) {
        anchors.push(BuiltAnchor {
            kind: AnchorKind::File,
            value: old_path.to_string(),
            relationship: "renamed_from".to_string(),
            confidence: "deterministic".to_string(),
        });
    }
    match (new_symbol, old_symbol) {
        (Some(new), old) => {
            anchors.push(BuiltAnchor {
                kind: AnchorKind::Symbol,
                value: new.to_string(),
                relationship: "changes".to_string(),
                confidence: "deterministic".to_string(),
            });
            if let Some(old) = old.filter(|old| !old.is_empty()) {
                anchors.push(BuiltAnchor {
                    kind: AnchorKind::Symbol,
                    value: old.to_string(),
                    relationship: "renamed_from".to_string(),
                    confidence: "deterministic".to_string(),
                });
            }
        }
        (None, Some(old)) => {
            if !old.is_empty() {
                anchors.push(BuiltAnchor {
                    kind: AnchorKind::Symbol,
                    value: old.to_string(),
                    relationship: "deleted".to_string(),
                    confidence: "deterministic".to_string(),
                });
            }
        }
        (None, None) => {}
    }
    anchors
}

struct PushContext<'a> {
    output: &'a mut Vec<BuiltUnit>,
    atoms: &'a [ParsedAtom],
    atom_indices: Vec<usize>,
    header: String,
    routing: String,
    metadata: serde_json::Value,
    anchors: Vec<BuiltAnchor>,
}

fn split_hunk_text(text: &str, max_tokens: usize) -> Vec<String> {
    let max_chars = max_tokens.saturating_sub(1).max(1) * 4;
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for line in text.split_inclusive('\n') {
        let cost = estimate_tokens(line);
        if !current.is_empty() && used + cost > max_tokens {
            pieces.push(std::mem::take(&mut current));
            used = 0;
        }
        if cost > max_tokens {
            let budget_chars = max_chars;
            let mut start = 0usize;
            while start < line.len() {
                let mut limit = (start + budget_chars).min(line.len());
                while limit > start && !line.is_char_boundary(limit) {
                    limit -= 1;
                }
                let mut end = line[start..limit]
                    .char_indices()
                    .next_back()
                    .map(|(offset, _)| start + offset)
                    .unwrap_or(start);
                if end <= start {
                    end = line.len();
                }
                let piece = &line[start..end];
                pieces.push(piece.to_string());
                start = end;
            }
            used = 0;
            continue;
        }
        current.push_str(line);
        used += cost;
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    if pieces.is_empty() {
        pieces.push(String::new());
    }
    pieces
}

fn push_units(context: PushContext<'_>, hunk_texts: &[String]) {
    let PushContext {
        output,
        atoms,
        atom_indices,
        header,
        routing,
        metadata,
        anchors,
    } = context;
    let header_tokens = estimate_tokens(&header);
    let body_budget = MAX_TOKENS.saturating_sub(header_tokens + 1).max(1);
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for text in hunk_texts {
        let split: Vec<String> = if estimate_tokens(text) > body_budget {
            split_hunk_text(text, body_budget)
        } else {
            vec![text.clone()]
        };
        for piece in split {
            let cost = estimate_tokens(&piece);
            if !current.is_empty() && used + cost > body_budget {
                parts.push(std::mem::take(&mut current));
                used = 0;
            }
            current.push_str(&piece);
            used += cost;
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    let multi = parts.len() > 1;
    let hashes: Vec<&str> = atom_indices
        .iter()
        .map(|index| atoms[*index].content_hash.as_str())
        .collect();
    for (index, body) in parts.iter().enumerate() {
        let evidence = format!("{header}{body}");
        let mut unit_routing = routing.clone();
        let mut unit_metadata = metadata.clone();
        if multi {
            unit_routing.push_str(&format!("\npart: {}", index + 1));
            unit_metadata["part"] = serde_json::json!(index + 1);
        }
        output.push(BuiltUnit {
            kind: UnitKind::Git,
            token_count: estimate_tokens(&evidence),
            content_hash: hash_segments(&[
                GIT_UNIT_VERSION,
                "git",
                &hashes.join("\n"),
                &evidence,
                &unit_routing,
            ]),
            atom_indices: atom_indices.clone(),
            evidence_text: evidence,
            routing_text: unit_routing,
            metadata: unit_metadata,
            anchors: anchors.clone(),
        });
    }
}

pub fn ingest_commit(
    root: &Path,
    commit: &CommitRef,
) -> Result<(Vec<ParsedAtom>, Vec<BuiltUnit>), Box<dyn std::error::Error + Send + Sync>> {
    let files = changed_files(root, &commit.oid)?;
    let short = commit
        .oid
        .get(..8)
        .unwrap_or(commit.oid.as_str())
        .to_string();
    let subject = commit
        .message
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let commit_breadcrumb = format!("git:{short}");
    let mut atoms = vec![ParsedAtom {
        kind: AtomKind::Commit,
        parent_index: None,
        ordinal: 0,
        start_offset: 0,
        end_offset: commit.message.len(),
        text: commit.message.clone(),
        content_hash: ParsedAtom::content_hash_of(
            AtomKind::Commit,
            &commit_breadcrumb,
            &commit.message,
        ),
        breadcrumb: commit_breadcrumb.clone(),
        metadata: serde_json::json!({
            "commit": commit.oid,
            "timestamp": commit.timestamp,
            "subject": subject,
        }),
    }];
    let mut units = Vec::new();
    let mut ordinal = 1u32;

    for file in &files {
        let patch_text: String = if file.patch.chars().count() > MAX_PATCH_CHARS {
            file.patch.chars().take(MAX_PATCH_CHARS).collect()
        } else {
            file.patch.clone()
        };
        let file_breadcrumb = format!("{commit_breadcrumb} > {}", file.path);
        let fc_index = atoms.len();
        let fc_ordinal = ordinal;
        ordinal += 1;
        atoms.push(ParsedAtom {
            kind: AtomKind::FileChange,
            parent_index: Some(0),
            ordinal: fc_ordinal,
            start_offset: 0,
            end_offset: patch_text.len(),
            text: patch_text.clone(),
            content_hash: ParsedAtom::content_hash_of(
                AtomKind::FileChange,
                &file_breadcrumb,
                &file.patch,
            ),
            breadcrumb: file_breadcrumb.clone(),
            metadata: serde_json::json!({
                "path": file.path,
                "status": file.status.to_string(),
            }),
        });

        let hunks: Vec<Hunk> = parse_hunks(&file.patch)
            .into_iter()
            .take(MAX_HUNKS_PER_FILE)
            .collect();
        let mut hunk_indices = Vec::with_capacity(hunks.len());
        for (number, hunk) in hunks.iter().enumerate() {
            let hunk_breadcrumb = format!("{file_breadcrumb} > hunk {}", number + 1);
            let index = atoms.len();
            let hunk_ordinal = ordinal;
            ordinal += 1;
            atoms.push(ParsedAtom {
                kind: AtomKind::DiffHunk,
                parent_index: Some(fc_index),
                ordinal: hunk_ordinal,
                start_offset: hunk.start_offset,
                end_offset: hunk.end_offset,
                text: hunk.text.clone(),
                content_hash: ParsedAtom::content_hash_of(
                    AtomKind::DiffHunk,
                    &hunk_breadcrumb,
                    &hunk.text,
                ),
                breadcrumb: hunk_breadcrumb,
                metadata: serde_json::json!({
                    "path": file.path,
                    "old_start": hunk.old_start,
                    "old_count": hunk.old_count,
                    "new_start": hunk.new_start,
                    "new_count": hunk.new_count,
                }),
            });
            hunk_indices.push(index);
        }

        let file_change = file_change_kind(file.status);
        let old_path = file.old_path.clone().unwrap_or_else(|| file.path.clone());
        let supported = crate::ingest::code::supports_code_path(&file.path)
            || crate::ingest::code::supports_code_path(&old_path);
        let language = crate::ingest::code::language_name(&file.path)
            .or_else(|| crate::ingest::code::language_name(&old_path));
        let parent = parent_oid(root, &commit.oid);
        let aligned: Vec<Alignment> = if hunks.is_empty() {
            vec![Alignment {
                old_span: None,
                new_span: None,
                hunk_indices: Vec::new(),
                change_kind: if file.old_path.is_some() {
                    ChangeKind::Moved
                } else {
                    file_change
                },
                strategy: "file",
                confidence: BoundaryConfidence::Medium,
            }]
        } else if supported {
            let before = parent
                .as_deref()
                .and_then(|revision| blob(root, revision, &old_path))
                .filter(|content| content.len() <= MAX_ALIGN_BYTES)
                .unwrap_or_default();
            let after = if file.status == 'D' {
                String::new()
            } else {
                blob(root, &commit.oid, &file.path)
                    .filter(|content| content.len() <= MAX_ALIGN_BYTES)
                    .unwrap_or_default()
            };
            if before.is_empty() && after.is_empty() {
                vec![Alignment {
                    old_span: None,
                    new_span: None,
                    hunk_indices: (0..hunks.len()).collect(),
                    change_kind: file_change,
                    strategy: "file",
                    confidence: BoundaryConfidence::Medium,
                }]
            } else {
                reconcile_alignments(
                    &hunks,
                    &before,
                    &after,
                    &old_path,
                    &file.path,
                    file.old_path.is_some(),
                    file_change,
                )
            }
        } else {
            vec![Alignment {
                old_span: None,
                new_span: None,
                hunk_indices: (0..hunks.len()).collect(),
                change_kind: file_change,
                strategy: "file",
                confidence: BoundaryConfidence::Medium,
            }]
        };

        let mut base_metadata = serde_json::json!({
            "commit": commit.oid,
            "timestamp": commit.timestamp,
            "path": file.path,
        });
        if let Some(old) = &file.old_path {
            base_metadata["old_path"] = serde_json::json!(old);
        }
        for alignment in &aligned {
            let display = alignment
                .new_span
                .as_ref()
                .or(alignment.old_span.as_ref());
            let (header, routing, mut metadata) = match display {
                Some(span) => (
                    format!("commit {short} {subject}\n\n{}\n\n", span.breadcrumb),
                    git_routing(&short, &subject, &file.path, Some(&span.name)),
                    {
                        let mut metadata = base_metadata.clone();
                        metadata["strategy"] = serde_json::json!(alignment.strategy);
                        metadata["symbol"] = serde_json::json!(span.name);
                        metadata["symbol_id"] = serde_json::json!(span.breadcrumb);
                        metadata["declaration_kind"] = serde_json::json!(span.kind.as_str());
                        metadata
                    }
                ),
                None => (
                    format!("commit {short} {subject}\n\n{}\n\n", file.path),
                    git_routing(&short, &subject, &file.path, None),
                    {
                        let mut metadata = base_metadata.clone();
                        metadata["strategy"] = serde_json::json!(alignment.strategy);
                        metadata
                    }
                ),
            };
            metadata["change_kind"] = serde_json::json!(alignment.change_kind.as_str());
            metadata["boundary_confidence"] =
                serde_json::json!(alignment.confidence.as_str());
            metadata["hunks"] = serde_json::json!(alignment
                .hunk_indices
                .iter()
                .map(|index| format!("hunk {}", index + 1))
                .collect::<Vec<_>>());
            let evolution_symbol = matches!(
                alignment.change_kind,
                ChangeKind::Renamed | ChangeKind::Deleted
            );
            if evolution_symbol {
                if let Some(old_span) = &alignment.old_span {
                    metadata["old_symbol"] = serde_json::json!(old_span.breadcrumb);
                }
            }
            if let Some(language) = language {
                metadata["language"] = serde_json::json!(language);
            }
            let old_symbol_anchor = if evolution_symbol {
                alignment
                    .old_span
                    .as_ref()
                    .map(|span| span.breadcrumb.as_str())
            } else {
                None
            };
            let new_symbol_anchor = if matches!(alignment.change_kind, ChangeKind::Deleted) {
                None
            } else {
                display.map(|span| span.name.as_str())
            };
            let anchors = git_anchors(
                &commit.oid,
                file.old_path.as_deref(),
                &file.path,
                old_symbol_anchor,
                new_symbol_anchor,
            );
            let texts: Vec<String> = if alignment.hunk_indices.is_empty() {
                vec![patch_text.clone()]
            } else {
                alignment
                    .hunk_indices
                    .iter()
                    .map(|index| hunks[*index].text.clone())
                    .collect()
            };
            let atom_indices: Vec<usize> = alignment
                .hunk_indices
                .iter()
                .filter_map(|index| hunk_indices.get(*index).copied())
                .collect();
            push_units(
                PushContext {
                    output: &mut units,
                    atoms: &atoms,
                    atom_indices,
                    header,
                    routing,
                    metadata,
                    anchors,
                },
                &texts,
            );
        }
    }
    Ok((atoms, units))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_parser_reads_ranges_and_offsets() {
        let patch = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,3 +1,4 @@ fn a\n+line\n@@ -10 +10,2 @@\n line\n";
        let hunks = parse_hunks(patch);
        assert_eq!(hunks.len(), 2);
        assert_eq!((hunks[0].new_start, hunks[0].new_count), (1, 4));
        assert_eq!(
            (
                hunks[1].old_start,
                hunks[1].old_count,
                hunks[1].new_start,
                hunks[1].new_count
            ),
            (10, 1, 10, 2)
        );
        assert!(hunks[0].text.starts_with("@@"));
        assert_eq!(
            hunks[0].end_offset,
            hunks[0].start_offset + hunks[0].text.len()
        );
    }

    #[test]
    fn changed_new_lines_tracks_added_positions() {
        let text = "@@ -4,3 +4,4 @@\n context\n+added\n-removed\n+more\n".to_string();
        let hunk = Hunk {
            old_start: 4,
            old_count: 3,
            new_start: 4,
            new_count: 4,
            text,
            start_offset: 0,
            end_offset: 0,
        };
        assert_eq!(changed_new_lines(&hunk), vec![5, 6]);
    }

    #[test]
    fn alignment_uses_changed_lines_and_falls_back_per_hunk() {
        let after = "fn validate() {}\n\nfn refresh_session() {\n    validate();\n}\n";
        let hunks = vec![Hunk {
            old_start: 3,
            old_count: 3,
            new_start: 3,
            new_count: 3,
            text: "@@ -3,3 +3,3 @@\n fn refresh_session() {\n-    nothing();\n+    validate();\n"
                .to_string(),
            start_offset: 0,
            end_offset: 0,
        }];
        let (groups, parsed) = align_hunks(&hunks, after, "src/auth.rs", Side::New);
        assert!(parsed);
        assert_eq!(groups.len(), 1);
        let (span, indices) = &groups[0];
        assert!(span
            .as_ref()
            .unwrap()
            .breadcrumb
            .contains("refresh_session"));
        assert_eq!(indices, &vec![0]);
    }

    #[test]
    fn changed_old_lines_tracks_deleted_positions() {
        let text = "@@ -4,4 +4,3 @@\n context\n-removed\n+added\n-also_gone\n".to_string();
        let hunk = Hunk {
            old_start: 4,
            old_count: 4,
            new_start: 4,
            new_count: 3,
            text,
            start_offset: 0,
            end_offset: 0,
        };
        assert_eq!(changed_old_lines(&hunk), vec![5, 6]);
    }

    #[test]
    fn whole_file_hunk_falls_back() {
        let after = "fn one() {}\nfn two() {}\n";
        let hunks = vec![Hunk {
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count: 2,
            text: "@@\n".to_string(),
            start_offset: 0,
            end_offset: 3,
        }];
        let (groups, _parsed) = align_hunks(&hunks, after, "src/x.rs", Side::New);
        assert_eq!(groups.len(), 1);
        assert!(groups[0].0.is_none());
    }

    #[test]
    fn reconcile_keeps_modified_symbol_identity() {
        let before = "fn kept() {\n    old_call();\n}\n\nfn other() {}\n";
        let after = "fn kept() {\n    new_call();\n}\n\nfn other() {}\n";
        let hunks = vec![Hunk {
            old_start: 1,
            old_count: 3,
            new_start: 1,
            new_count: 3,
            text: "@@ -1,3 +1,3 @@\n fn kept() {\n-    old_call();\n+    new_call();\n"
                .to_string(),
            start_offset: 0,
            end_offset: 0,
        }];
        let alignments = reconcile_alignments(
            &hunks,
            before,
            after,
            "src/x.rs",
            "src/x.rs",
            false,
            ChangeKind::Modified,
        );
        let modified = alignments
            .iter()
            .find(|alignment| alignment.change_kind == ChangeKind::Modified)
            .expect("kept must be classified as modified");
        assert_eq!(modified.strategy, "symbol");
        let new_span = modified.new_span.as_ref().unwrap();
        assert_eq!(new_span.breadcrumb, "src/x.rs > kept");
        assert_eq!(modified.confidence, BoundaryConfidence::High);
        assert!(!alignments
            .iter()
            .any(|alignment| alignment.change_kind == ChangeKind::Added));
    }

    #[test]
    fn reconcile_classifies_add_and_delete() {
        let before = "fn kept() {}\n\nfn gone() {\n    validate();\n}\n";
        let after = "fn kept() {}\n\nfn added() {\n    load();\n}\n";
        let hunks = vec![Hunk {
            old_start: 3,
            old_count: 3,
            new_start: 3,
            new_count: 3,
            text: "@@ -3,3 +3,3 @@\n fn kept() {}\n-fn gone() {\n-    validate();\n-}\n+fn added() {\n+    load();\n+}\n"
                .to_string(),
            start_offset: 0,
            end_offset: 0,
        }];
        let alignments = reconcile_alignments(
            &hunks,
            before,
            after,
            "src/x.rs",
            "src/x.rs",
            false,
            ChangeKind::Modified,
        );
        assert!(
            alignments.iter().any(|alignment| {
                alignment.change_kind == ChangeKind::Added
                    && alignment
                        .new_span
                        .as_ref()
                        .is_some_and(|span| span.breadcrumb.ends_with("added"))
            }),
            "added must be classified: {alignments:?}"
        );
        assert!(
            alignments.iter().any(|alignment| {
                alignment.change_kind == ChangeKind::Deleted
                    && alignment
                        .old_span
                        .as_ref()
                        .is_some_and(|span| span.breadcrumb.ends_with("gone"))
            }),
            "gone must be classified: {alignments:?}"
        );
    }

    #[test]
    fn reconcile_detects_rename_by_normalized_body() {
        let before = "fn before_name() {\n    work();\n    work();\n}\n";
        let after = "fn after_name() {\n    work();\n    work();\n}\n";
        let hunks = vec![Hunk {
            old_start: 1,
            old_count: 4,
            new_start: 1,
            new_count: 4,
            text: "@@ -1,4 +1,4 @@\n-fn before_name() {\n+fn after_name() {\n     work();\n     work();\n}\n"
                .to_string(),
            start_offset: 0,
            end_offset: 0,
        }];
        let alignments = reconcile_alignments(
            &hunks,
            before,
            after,
            "src/x.rs",
            "src/x.rs",
            false,
            ChangeKind::Modified,
        );
        let renamed = alignments
            .iter()
            .find(|alignment| alignment.change_kind == ChangeKind::Renamed)
            .expect("same-body rename must be detected");
        assert!(renamed
            .old_span
            .as_ref()
            .is_some_and(|span| span.name == "before_name"));
        assert!(renamed
            .new_span
            .as_ref()
            .is_some_and(|span| span.name == "after_name"));
    }

    #[test]
    fn oversized_hunk_splits_within_the_limit() {
        let mut line = String::from("+    let value = ");
        for index in 0..2_000 {
            line.push_str(&format!("data{index}_"));
        }
        line.push('\n');
        let hunk_text = format!("@@ -1,2 +1,2 @@\n{line}");
        let pieces = split_hunk_text(&hunk_text, MAX_TOKENS - 20);
        assert!(pieces.len() > 1);
        assert!(pieces
            .iter()
            .all(|piece| estimate_tokens(piece) <= MAX_TOKENS - 20));
        assert_eq!(pieces.concat(), hunk_text);
    }

    #[test]
    fn split_hunk_text_handles_long_lines_without_looping() {
        let text = "+".repeat(5_000);
        let pieces = split_hunk_text(&text, 100);
        assert!(pieces.len() > 1);
        assert!(pieces.iter().all(|piece| estimate_tokens(piece) <= 101));
        assert_eq!(pieces.concat(), text);
    }

    #[test]
    fn push_units_bounds_every_unit() {
        let atoms = vec![ParsedAtom {
            kind: AtomKind::Commit,
            parent_index: None,
            ordinal: 0,
            start_offset: 0,
            end_offset: 3,
            text: "msg".to_string(),
            content_hash: "h".to_string(),
            breadcrumb: "git:abc".to_string(),
            metadata: serde_json::json!({}),
        }];
        let mut units = Vec::new();
        let oversized = "+".repeat(MAX_TOKENS * 8);
        push_units(
            PushContext {
                output: &mut units,
                atoms: &atoms,
                atom_indices: vec![0],
                header: "commit abc subject\n\npath\n\n".to_string(),
                routing: "source: git_change".to_string(),
                metadata: serde_json::json!({}),
                anchors: Vec::new(),
            },
            &[oversized],
        );
        assert!(units.len() > 1);
        assert!(units.iter().all(|unit| unit.token_count <= MAX_TOKENS));
        assert!(units
            .iter()
            .enumerate()
            .all(|(index, unit)| { unit.metadata["part"] == serde_json::json!(index + 1) }));
    }

    #[test]
    fn push_units_respects_header_budget() {
        let atoms = vec![ParsedAtom {
            kind: AtomKind::Commit,
            parent_index: None,
            ordinal: 0,
            start_offset: 0,
            end_offset: 3,
            text: "msg".to_string(),
            content_hash: "h".to_string(),
            breadcrumb: "git:abc".to_string(),
            metadata: serde_json::json!({}),
        }];
        let header = format!(
            "commit {} {}\n\n{}\n\n",
            "a".repeat(200),
            "b".repeat(200),
            "c".repeat(200)
        );
        let texts: Vec<String> = (0..10)
            .map(|index| {
                format!(
                    "@@ -{index},1 +{index},1 @@\n context\n+{}\n",
                    "x".repeat(600)
                )
            })
            .collect();
        let mut units = Vec::new();
        push_units(
            PushContext {
                output: &mut units,
                atoms: &atoms,
                atom_indices: vec![0],
                header,
                routing: "source: git_change".to_string(),
                metadata: serde_json::json!({}),
                anchors: Vec::new(),
            },
            &texts,
        );
        assert!(
            units.len() > 1,
            "medium hunks with a large header must split into parts"
        );
        assert!(units.iter().all(|unit| unit.token_count <= MAX_TOKENS));
    }
}
