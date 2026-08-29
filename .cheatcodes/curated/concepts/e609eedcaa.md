---
cheatcodes_id: e609eedcaa
type: Gotcha
title: Switching from mock to a real embedder purges mock vectors
description: When a real embedder is configured, vectors left by mock embedding mode are purged rather than retained alongside real vectors.
tags:
  - embeddings
  - vectors
  - mock-mode
  - data-cleanup
status: draft
generated:
  by: cheatcodes/0.2.0
  at: 2026-08-29T18:08:12.248Z
sources:
  - id: session-ef730a043d37fc6d
    resource: session:eb890f0a-d85a-40f0-9039-195582d9d43d#entries=6ba5cefb,b7ff8b25
    title: Session evidence
---

# Symptom

Existing vectors created with mock embedding remain associated with the repository until a real embedder is used.

# Cause

The application detects a configured non-mock embedder and deletes vectors created for the mock model before continuing.

# Fix

Expect mock vectors to be removed when switching to a real embedder; inspect the outcome's mock_vectors_purged field to determine whether any were deleted.

# Evidence

- [evidence-fcc0cd11c225e7163076056b] README.md:38:with a real embedder purges any vectors that mock mode left behind:
src/main.rs:237:            let purged_mock_vectors = if embedder.is_some() && !mock_embedder_requested() {
src/main.rs:246:            if purged_mock_vectors > 0 {
src/main.rs:247:                outcome_json["mock_vectors_purged"] = serde_json::json!(purged_mock_vectors);
src/store.rs:1260:    fn vector_models_lists_per_model_counts_and_purge_removes_only_that_model() {


[Hint: Prefer the grep tool for content search.]
- [evidence-84bb23e904e6a406b605708a] src/main.rs:144:    if url == "mock" {
src/main.rs:155:fn mock_embedder_requested() -> bool {
src/main.rs:156:    std::env::var("SNOOP_EMBED_URL").is_ok_and(|url| url == "mock")
src/main.rs:237:            let purged_mock_vectors = if embedder.is_some() && !mock_embedder_requested() {
src/main.rs:246:            if purged_mock_vectors > 0 {
src/main.rs:247:                outcome_json["mock_vectors_purged"] = serde_json::json!(purged_mock_vectors);
src/store.rs:1288:            .put_vector(unit_id, "evidence", "mock-v1", &[0.1, 0.2])
src/store.rs:1291:            .put_vector(unit_id, "routing", "mock-v1", &[0.3, 0.4])
src/store.rs:1305:                ("mock-v1".to_string(), 2),
src/store.rs:1308:        assert_eq!(store.delete_vectors_for_model(repo, "mock-v1").unwrap(), 2);


[Hint: Prefer the grep tool for content search.]
