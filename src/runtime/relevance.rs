//! Post-retrieval relevance: weak evidence proves itself before occupying
//! a supporting lane (Phase D) or fill (Phase E). Required lanes stay
//! permissive; explicit history intent keeps its path through them.

use crate::core::RetrievalUnit;

/// Heuristic English stopwords for coverage counting. Intent words double
/// as facet signals elsewhere; here they would let any unit match on filler.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "in", "on", "for", "with", "from",
    "into", "over", "after", "before", "between", "through", "does", "do", "is",
    "are", "was", "were", "be", "been", "how", "what", "when", "where", "which",
    "why", "who", "find", "show", "get", "all", "any", "use", "using", "need",
];

/// One query concept. Compound identifiers stay whole: `cache_hits` is a
/// single concept with raw `cache_hits` and pieces `[cache, hits]`, so
/// identifier specificity survives FTS tokenization instead of degrading
/// into generic words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Concept {
    pub raw: String,
    pub pieces: Vec<String>,
    pub is_identifier: bool,
}

fn has_camel_hump(token: &str) -> bool {
    token
        .as_bytes()
        .windows(2)
        .any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase())
}

pub(crate) fn query_concepts(text: &str) -> Vec<Concept> {
    let mut concepts = Vec::new();
    for token in text.split_whitespace() {
        let raw = token.to_lowercase();
        let pieces: Vec<String> = raw
            .split(|character: char| !character.is_alphanumeric())
            .filter(|piece| !piece.is_empty())
            .map(str::to_string)
            .collect();
        if pieces.is_empty() {
            continue;
        }
        if STOPWORDS.contains(&raw.as_str()) {
            continue;
        }
        // Camel detection reads the original token; lowering erases humps.
        let is_identifier = token.contains(['_', '-', '.']) || has_camel_hump(token);
        if !concepts.iter().any(|concept: &Concept| concept.raw == raw) {
            concepts.push(Concept {
                raw,
                pieces,
                is_identifier,
            });
        }
    }
    concepts
}

/// Plain words match whole-word only, so `headers` never matches `head`.
/// Separator-bearing raws keep a substring check (`cache_hits` cannot
/// word-match) with a word-exact all-pieces fallback (`force rescan`).
fn concept_matches(
    concept: &Concept,
    words: &std::collections::HashSet<&str>,
    haystack: &str,
) -> bool {
    if concept
        .raw
        .contains(|character: char| !character.is_alphanumeric())
    {
        return haystack.contains(&concept.raw)
            || concept
                .pieces
                .iter()
                .all(|piece| words.contains(piece.as_str()));
    }
    words.contains(concept.raw.as_str())
}

/// A supporting or fill candidate is credible with a strong identifier hit,
/// enough matched concepts, or an accepted anchor expansion. Mere
/// multi-channel agreement is not enough: one generic token surfacing in
/// both the evidence and routing representations is still one generic token.
/// The coverage bar adapts to query length: a one-concept query cannot
/// discriminate by coverage, so any match stays credible and single-token
/// queries keep their legacy behavior.
pub(crate) fn credible(
    unit: &RetrievalUnit,
    concepts: &[Concept],
    expanded: bool,
) -> bool {
    if expanded {
        return true;
    }
    let haystack = format!("{}\n{}", unit.evidence_text, unit.routing_text).to_lowercase();
    let words: std::collections::HashSet<&str> = haystack
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    if concepts.iter().any(|concept| {
        concept.is_identifier && concept_matches(concept, &words, &haystack)
    }) {
        return true;
    }
    let needed = concepts.len().min(2);
    concepts
        .iter()
        .filter(|concept| concept_matches(concept, &words, &haystack))
        .count()
        >= needed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_identifiers_survive_as_single_concepts() {
        let concepts = query_concepts("incremental cache cache_hits force-rescan warmCache");
        let raws: Vec<&str> = concepts.iter().map(|concept| concept.raw.as_str()).collect();
        assert_eq!(
            raws,
            vec![
                "incremental",
                "cache",
                "cache_hits",
                "force-rescan",
                "warmcache"
            ]
        );
        let by_raw = |raw: &str| concepts.iter().find(|concept| concept.raw == raw).unwrap();
        assert!(by_raw("cache_hits").is_identifier);
        assert!(by_raw("force-rescan").is_identifier);
        assert!(by_raw("warmCache".to_lowercase().as_str()).is_identifier);
        assert!(!by_raw("cache").is_identifier);
        assert_eq!(by_raw("cache_hits").pieces, vec!["cache", "hits"]);
    }

    #[test]
    fn generic_single_piece_match_is_not_credible() {
        let concepts = query_concepts("incremental cache cache_hits");
        let unit = RetrievalUnit {
            id: crate::core::UnitId(1),
            source_id: crate::core::SourceId(1),
            source_kind: crate::core::SourceKind::GitCommit,
            locator: "git:junk".to_string(),
            kind: crate::core::UnitKind::Git,
            evidence_text: "commit junk tune cache sizes".to_string(),
            routing_text: "changed file: src/cache.py".to_string(),
            token_count: 10,
            content_hash: "hash".to_string(),
            timestamp: None,
            metadata: serde_json::json!({}),
        };
        assert!(!credible(&unit, &concepts, false));
    }

    #[test]
    fn stopwords_leave_concepts_and_plain_words_match_whole() {
        let concepts = query_concepts("find the CLI entry point");
        let raws: Vec<&str> = concepts.iter().map(|concept| concept.raw.as_str()).collect();
        assert_eq!(raws, vec!["cli", "entry", "point"]);
        let unit = RetrievalUnit {
            id: crate::core::UnitId(4),
            source_id: crate::core::SourceId(4),
            source_kind: crate::core::SourceKind::GitCommit,
            locator: "git:headers".to_string(),
            kind: crate::core::UnitKind::Git,
            evidence_text: "fix response headers handling".to_string(),
            routing_text: String::new(),
            token_count: 10,
            content_hash: "hash".to_string(),
            timestamp: None,
            metadata: serde_json::json!({}),
        };
        let head_only = query_concepts("implicit HEAD handling");
        assert!(!credible(&unit, &head_only, false));
    }

    #[test]
    fn identifier_verbatim_and_expansion_pass() {
        let concepts = query_concepts("incremental cache cache_hits");
        let strong = RetrievalUnit {
            id: crate::core::UnitId(2),
            source_id: crate::core::SourceId(2),
            source_kind: crate::core::SourceKind::Code,
            locator: "src/cache.py".to_string(),
            kind: crate::core::UnitKind::Code,
            evidence_text: "cache_hits and cache_misses counters".to_string(),
            routing_text: String::new(),
            token_count: 10,
            content_hash: "hash".to_string(),
            timestamp: None,
            metadata: serde_json::json!({}),
        };
        assert!(credible(&strong, &concepts, false));
        let weak = RetrievalUnit {
            id: crate::core::UnitId(3),
            source_id: crate::core::SourceId(3),
            source_kind: crate::core::SourceKind::GitCommit,
            locator: "git:weak".to_string(),
            kind: crate::core::UnitKind::Git,
            evidence_text: "unrelated".to_string(),
            routing_text: String::new(),
            token_count: 10,
            content_hash: "hash".to_string(),
            timestamp: None,
            metadata: serde_json::json!({}),
        };
        assert!(!credible(&weak, &concepts, false));
        assert!(credible(&weak, &concepts, true));
    }
}
