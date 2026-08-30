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
    }
}

pub(crate) fn preferred_role(facet: Facet) -> &'static str {
    match facet {
        Facet::CurrentBehavior => "current_truth",
        Facet::Rationale => "design_rationale",
        Facet::Evolution => "change_origin",
        Facet::PriorWork => "prior_work",
        Facet::Validation => "prior_work",
        Facet::Conflict => "prior_work",
        Facet::Invariant => "current_truth",
    }
}
