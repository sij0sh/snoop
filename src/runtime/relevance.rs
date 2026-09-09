//! Post-retrieval relevance: weak evidence proves itself before occupying
//! a supporting lane (Phase D) or fill (Phase E). Required lanes stay
//! permissive; explicit history intent keeps its path through them.

use crate::core::RetrievalUnit;

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

fn concept_matches(concept: &Concept, haystack: &str) -> bool {
    if haystack.contains(&concept.raw) {
        return true;
    }
    // Separator-insensitive fallback: `force-rescan` matches `force rescan`.
    // Every piece must be present so one generic piece never matches alone.
    concept
        .pieces
        .iter()
        .all(|piece| haystack.contains(piece))
}

/// A supporting candidate is credible with a strong identifier hit, two
/// matched concepts, or an accepted anchor expansion. Mere multi-channel
/// agreement is not enough: one generic token surfacing in both the evidence
/// and routing representations is still one generic token.
pub(crate) fn credible(
    unit: &RetrievalUnit,
    concepts: &[Concept],
    expanded: bool,
) -> bool {
    if expanded {
        return true;
    }
    let haystack = format!("{}\n{}", unit.evidence_text, unit.routing_text).to_lowercase();
    if concepts
        .iter()
        .any(|concept| concept.is_identifier && concept_matches(concept, &haystack))
    {
        return true;
    }
    concepts
        .iter()
        .filter(|concept| concept_matches(concept, &haystack))
        .count()
        >= 2
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
