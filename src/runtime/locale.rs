//! Locale-family collapsing for translated docs. Families derive from
//! locators at query time; the index is untouched.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use crate::core::{RetrievalUnit, SourceKind};

/// A locale-like path segment: 2-3 lowercase ASCII letters with an optional
/// `-`/`_` plus alphanumerics (`ja`, `zh-hant`, `pt_BR`).
fn is_locale_segment(segment: &str) -> bool {
    let cut = segment.find(['-', '_']);
    let (language, rest) = match cut {
        Some(index) => (&segment[..index], Some(&segment[index + 1..])),
        None => (segment, None),
    };
    (2..=3).contains(&language.len())
        && language.chars().all(|character| character.is_ascii_lowercase())
        && rest.is_none_or(|tail| {
            !tail.is_empty() && tail.chars().all(|character| character.is_ascii_alphanumeric())
        })
}

/// Family key for a doc locator: the locator with its locale-like segment
/// removed. Scoped to paths under a `docs` segment so version-like segments
/// elsewhere (`src/v2/...`) never collapse. Non-matching locators return
/// unchanged, and a lone source in its family always passes through.
pub(crate) fn doc_family(locator: &str) -> String {
    let mut segments: Vec<&str> = locator.split('/').collect();
    if !segments.contains(&"docs") {
        return locator.to_string();
    }
    let Some(position) = segments
        .iter()
        .position(|segment| is_locale_segment(segment))
    else {
        return locator.to_string();
    };
    segments.remove(position);
    segments.join("/")
}

fn query_tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect()
}

fn term_coverage(unit: &RetrievalUnit, tokens: &[String]) -> usize {
    let haystack = format!("{}\n{}", unit.evidence_text, unit.routing_text).to_lowercase();
    tokens
        .iter()
        .filter(|token| haystack.contains(token.as_str()))
        .count()
}

/// Pool unit ids to drop: for each multi-source markdown family keep one
/// representative source and suppress the rest for this query only. Chunks
/// from the chosen source stay fully eligible under the per-source cap.
/// Representative policy: best query-term coverage, then the segment-less
/// canonical sibling, then highest fused rank. No language is preferred.
pub(crate) fn suppress_locale_siblings(
    pool: &[(i64, u32, SourceKind)],
    units: &HashMap<i64, Option<RetrievalUnit>>,
    query_text: &str,
) -> HashSet<i64> {
    let tokens = query_tokens(query_text);
    // Per source in a family: best query-term coverage, best fused rank,
    // member unit ids.
    type SourceStats = (usize, u32, Vec<i64>);
    let mut families: HashMap<String, HashMap<String, SourceStats>> = HashMap::new();
    for (id, rank, kind) in pool {
        if *kind != SourceKind::Markdown {
            continue;
        }
        let Some(Some(unit)) = units.get(id) else {
            continue;
        };
        let family = doc_family(&unit.locator);
        let coverage = term_coverage(unit, &tokens);
        let entry = families
            .entry(family)
            .or_default()
            .entry(unit.locator.clone())
            .or_insert((0, u32::MAX, Vec::new()));
        entry.0 = entry.0.max(coverage);
        entry.1 = entry.1.min(*rank);
        entry.2.push(*id);
    }
    let mut suppressed = HashSet::new();
    for (family, sources) in &families {
        if sources.len() < 2 {
            continue;
        }
        let winner = sources
            .iter()
            .max_by_key(|(locator, (coverage, rank, _))| {
                (
                    *coverage,
                    (locator.as_str() == family.as_str()) as u8,
                    Reverse(*rank),
                )
            })
            .map(|(locator, _)| locator.clone());
        for (locator, (_, _, ids)) in sources {
            if Some(locator) != winner.as_ref() {
                suppressed.extend(ids.iter().copied());
            }
        }
    }
    suppressed
}
