//! Facet detection maps a query onto role-aware admission lanes.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Facet {
    Rationale,
    Evolution,
    Validation,
    PriorWork,
    Conflict,
    Invariant,
    CurrentBehavior,
    ReviewState,
}

fn query_tokens(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn detect_facets(query: &str) -> Vec<Facet> {
    let tokens = query_tokens(query);
    let has_token = |words: &[&str]| {
        words
            .iter()
            .any(|word| tokens.iter().any(|token| token == word))
    };
    let has_phrase = |phrases: &[&str]| {
        phrases.iter().any(|phrase| {
            let phrase_tokens = query_tokens(phrase);
            tokens
                .windows(phrase_tokens.len())
                .any(|window| window == phrase_tokens.as_slice())
        })
    };
    let mut facets = Vec::new();
    if has_token(&["why", "rationale"]) || has_phrase(&["what is the reason", "reasoning behind"]) {
        facets.push(Facet::Rationale);
    }
    if has_token(&[
        "when",
        "introduced",
        "renamed",
        "history",
        "originally",
        "previously",
        "legacy",
        "retired",
        "deprecated",
    ]) {
        facets.push(Facet::Evolution);
    }
    if has_token(&[
        "test",
        "tests",
        "tested",
        "pass",
        "passed",
        "passes",
        "invoked",
        "validated",
        "validation",
    ]) {
        facets.push(Facet::Validation);
    }
    if has_token(&[
        "prior",
        "previous",
        "attempt",
        "attempts",
        "fix",
        "fixes",
        "fixed",
        "investigated",
        "investigation",
    ]) {
        facets.push(Facet::PriorWork);
    }
    if has_token(&[
        "conflict",
        "conflicts",
        "contradict",
        "contradicts",
        "contradicted",
        "versus",
    ]) || has_phrase(&["which fix", "instead of"])
    {
        facets.push(Facet::Conflict);
    }
    if has_token(&[
        "invariant",
        "invariants",
        "across",
        "consistent",
        "consistently",
    ]) || has_phrase(&["same rule"])
    {
        facets.push(Facet::Invariant);
    }
    if has_token(&["current", "currently", "how", "now"]) {
        facets.push(Facet::CurrentBehavior);
    }
    if has_token(&[
        "unverified",
        "uncertain",
        "pending",
        "imported",
        "unconfirmed",
    ]) || has_phrase(&["needs review", "not confirmed", "not yet confirmed"])
    {
        facets.push(Facet::ReviewState);
    }
    if facets.is_empty() {
        facets.push(Facet::CurrentBehavior);
    }
    facets
}

pub(crate) fn role_of_kind(kind: crate::core::SourceKind) -> &'static str {
    match kind {
        crate::core::SourceKind::Code => "current_truth",
        crate::core::SourceKind::Markdown | crate::core::SourceKind::Text => "design_rationale",
        crate::core::SourceKind::GitCommit => "change_origin",
        crate::core::SourceKind::AgentSession => "prior_work",
        // Curated engineering memory is neither current implementation nor
        // raw history: it is interpreted, lifecycle-managed claims about
        // the evidence, so it gets its own role.
        crate::core::SourceKind::HindsightMemory => "curated_memory",
    }
}

/// Hindsight lifecycle visibility for one query. Ordinary queries see only
/// default material; Evolution unlocks history, Conflict unlocks disputed
/// and review material, and explicit review-state queries unlock
/// everything retrievable deliberately.
pub(crate) fn hindsight_visibility(facets: &[Facet]) -> super::options::HindsightVisibility {
    use super::options::HindsightVisibility;
    let evolution = facets.contains(&Facet::Evolution);
    let conflict = facets.contains(&Facet::Conflict);
    let review = facets.contains(&Facet::ReviewState);
    if review || (evolution && conflict) {
        HindsightVisibility::IncludeAll
    } else if evolution {
        HindsightVisibility::IncludeHistory
    } else if conflict {
        HindsightVisibility::IncludeReview
    } else {
        HindsightVisibility::Default
    }
}

/// Ordered admission lanes per facet. Most questions legitimately need two
/// evidence roles (for example current code plus the curated constraint
/// behind it), so facets name a primary and a secondary role.
pub(crate) fn preferred_roles(facet: Facet) -> &'static [&'static str] {
    match facet {
        Facet::CurrentBehavior => &["current_truth", "curated_memory"],
        Facet::Rationale => &["curated_memory", "design_rationale"],
        Facet::Evolution => &["change_origin", "curated_memory"],
        Facet::PriorWork => &["prior_work", "curated_memory"],
        Facet::Validation => &["prior_work", "current_truth"],
        Facet::Conflict => &["curated_memory", "prior_work"],
        Facet::Invariant => &["curated_memory", "current_truth"],
        Facet::ReviewState => &["curated_memory"],
    }
}
